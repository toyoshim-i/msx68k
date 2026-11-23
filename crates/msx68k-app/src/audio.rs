//! Real-time host audio output for the YM2151 OPM sound chip.
//!
//! Uses a fixed rational resampling ratio (`62,500 Hz -> 48,000 Hz`, i.e. `125 / 96`)
//! so the pitch is strictly constant and never stretched or compressed.

use std::collections::VecDeque;
use std::ffi::{c_char, c_void, CString};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use msx68k_bus::adpcm::AdpcmSampleEvent;
use msx68k_bus::opm::{Opm, OpmRegEvent, OpmSynthMode, OPM_SAMPLE_RATE};

/// Host audio output sample rate (Hz).
pub const HOST_SAMPLE_RATE: u32 = 48_000;

/// Target OPM sample backlog in the ring buffer (~32 ms at 62,500 Hz).
pub const TARGET_OPM_SAMPLES: usize = 2_000;

/// Maximum OPM sample backlog before dropping stale samples (~128 ms at 62,500 Hz).
const MAX_OPM_SAMPLES: usize = 8_000;

/// Exact rational resampling ratio: 62500 / 48000 = 125 / 96.
const RESAMPLE_NUM: u32 = 125;
const RESAMPLE_DEN: u32 = 96;

/// Output chunk size written to the audio backend (~10 ms at 48,000 Hz).
const CHUNK_FRAMES: usize = 480;

/// Fixed-ratio rational linear resampler from 62,500 Hz OPM stereo PCM to 48,000 Hz.
struct Resampler {
    prev: (i16, i16),
    curr: (i16, i16),
    phase: u32,
    primed: bool,
}

impl Resampler {
    const fn new() -> Self {
        Self {
            prev: (0, 0),
            curr: (0, 0),
            phase: 0,
            primed: false,
        }
    }

    fn fill_chunk(&mut self, queue: &mut VecDeque<(i16, i16)>, out: &mut [i16]) {
        self.fill_chunk_with(|| queue.pop_front(), out);
    }

    fn fill_chunk_with(&mut self, mut next_sample: impl FnMut() -> Option<(i16, i16)>, out: &mut [i16]) {
        debug_assert_eq!(out.len() % 2, 0);
        if !self.primed {
            if let Some(first) = next_sample() {
                self.prev = first;
                self.curr = next_sample().unwrap_or(first);
                self.primed = true;
            } else {
                out.fill(0);
                return;
            }
        }

        for frame in out.chunks_exact_mut(2) {
            let p = self.phase as i32;
            let inv = (RESAMPLE_DEN as i32) - p;
            let l = (self.prev.0 as i32 * inv + self.curr.0 as i32 * p) / (RESAMPLE_DEN as i32);
            let r = (self.prev.1 as i32 * inv + self.curr.1 as i32 * p) / (RESAMPLE_DEN as i32);
            frame[0] = l.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
            frame[1] = r.clamp(i16::MIN as i32, i16::MAX as i32) as i16;

            self.phase += RESAMPLE_NUM;
            while self.phase >= RESAMPLE_DEN {
                self.phase -= RESAMPLE_DEN;
                self.prev = self.curr;
                self.curr = next_sample().unwrap_or((0, 0));
            }
        }
    }
}

struct SharedAudio {
    mode: OpmSynthMode,
    queue: Mutex<VecDeque<(i16, i16)>>,
    events: Mutex<VecDeque<OpmRegEvent>>,
    adpcm_events: Mutex<VecDeque<AdpcmSampleEvent>>,
    produced_clock: AtomicU64,
    queued_samples: AtomicUsize,
    stop: AtomicBool,
}

/// Non-blocking handle to the host audio playback thread.
pub struct AudioOutput {
    shared: Arc<SharedAudio>,
    thread: Option<JoinHandle<()>>,
    active: bool,
}

impl AudioOutput {
    /// Attempts to open a host audio stream (PulseAudio / PipeWire via `libpulse-simple.so.0`).
    /// Returns an inactive `AudioOutput` if no audio backend is available.
    pub fn open(mode: OpmSynthMode) -> Self {
        let shared = Arc::new(SharedAudio {
            mode,
            queue: Mutex::new(VecDeque::with_capacity(if mode == OpmSynthMode::Inline {
                MAX_OPM_SAMPLES
            } else {
                0
            })),
            events: Mutex::new(VecDeque::with_capacity(if mode == OpmSynthMode::Offload {
                1024
            } else {
                0
            })),
            adpcm_events: Mutex::new(VecDeque::with_capacity(if mode == OpmSynthMode::Offload {
                1024
            } else {
                0
            })),
            produced_clock: AtomicU64::new(0),
            queued_samples: AtomicUsize::new(0),
            stop: AtomicBool::new(false),
        });

        #[cfg(target_os = "linux")]
        {
            match PulseSimpleStream::open(HOST_SAMPLE_RATE) {
                Some(stream) => {
                    let worker_shared = Arc::clone(&shared);
                    let handle = thread::Builder::new()
                        .name("msx68k-audio".into())
                        .spawn(move || audio_worker(stream, worker_shared))
                        .ok();
                    if handle.is_some() {
                        return Self {
                            shared,
                            thread: handle,
                            active: true,
                        };
                    }
                }
                None => {
                    eprintln!("msx68k: audio backend unavailable; running muted");
                }
            }
        }

        Self {
            shared,
            thread: None,
            active: false,
        }
    }

    /// Returns the configured OPM synthesis mode.
    #[inline]
    pub fn mode(&self) -> OpmSynthMode {
        self.shared.mode
    }

    /// Returns `true` if host audio output is active.
    #[inline]
    pub const fn is_active(&self) -> bool {
        self.active
    }

    /// Returns the number of OPM samples (at 62,500 Hz) currently buffered for playback.
    #[inline]
    pub fn queued_opm_samples(&self) -> usize {
        self.shared.queued_samples.load(Ordering::Relaxed)
    }

    /// Returns how long the VM should wait before running the next frame to stay locked
    /// to the audio output clock without stretching or compressing waveforms.
    pub fn suggested_wait(&self) -> Duration {
        if !self.active {
            return Duration::ZERO;
        }
        let queued = self.queued_opm_samples();
        if queued <= TARGET_OPM_SAMPLES {
            Duration::ZERO
        } else {
            let excess = (queued - TARGET_OPM_SAMPLES) as u64;
            Duration::from_micros(excess * 1_000_000 / OPM_SAMPLE_RATE as u64)
        }
    }

    /// Appends newly generated OPM stereo samples (at 62,500 Hz) in `Inline` mode.
    pub fn push_samples(&self, samples: &[(i16, i16)]) {
        if !self.active || samples.is_empty() {
            return;
        }
        let mut queue = self.shared.queue.lock().unwrap();
        queue.extend(samples.iter().copied());
        if queue.len() > MAX_OPM_SAMPLES {
            let drop_count = queue.len() - MAX_OPM_SAMPLES;
            queue.drain(..drop_count);
        }
        self.shared
            .queued_samples
            .store(queue.len(), Ordering::Relaxed);
    }

    /// Appends timestamped OPM/ADPCM events and advances the produced sample clock
    /// in `Offload` mode.
    pub fn push_events(
        &self,
        events: &[OpmRegEvent],
        adpcm_events: &[AdpcmSampleEvent],
        produced_clock: u64,
    ) {
        if !self.active {
            return;
        }
        if !events.is_empty() {
            let mut queue = self.shared.events.lock().unwrap();
            queue.extend(events.iter().copied());
        }
        if !adpcm_events.is_empty() {
            let mut queue = self.shared.adpcm_events.lock().unwrap();
            queue.extend(adpcm_events.iter().copied());
        }
        self.shared
            .produced_clock
            .store(produced_clock, Ordering::Release);
    }
}

impl Drop for AudioOutput {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(target_os = "linux")]
fn audio_worker(stream: PulseSimpleStream, shared: Arc<SharedAudio>) {
    let mut resampler = Resampler::new();
    let mut buf = vec![0i16; CHUNK_FRAMES * 2];
    let mut synth_opm = Opm::new();
    synth_opm.set_audio_enabled(true);
    let mut synth_clock: u64 = 0;
    let mut adpcm_lr: (i16, i16) = (0, 0);

    while !shared.stop.load(Ordering::Relaxed) {
        match shared.mode {
            OpmSynthMode::Inline => {
                let mut queue = shared.queue.lock().unwrap();
                resampler.fill_chunk(&mut queue, &mut buf);
                shared.queued_samples.store(queue.len(), Ordering::Relaxed);
            }
            OpmSynthMode::Offload => {
                let produced = shared.produced_clock.load(Ordering::Acquire);
                let mut events = shared.events.lock().unwrap();
                let mut adpcm_events = shared.adpcm_events.lock().unwrap();
                // Drop excess backlog if VM ever ran far ahead (e.g. pause/debugger).
                if produced.saturating_sub(synth_clock) > MAX_OPM_SAMPLES as u64 {
                    let skip_to = produced - MAX_OPM_SAMPLES as u64;
                    while let Some(ev) = events.front() {
                        if ev.sample_time <= skip_to {
                            synth_opm.write_reg(ev.reg, ev.val);
                            events.pop_front();
                        } else {
                            break;
                        }
                    }
                    while let Some(ev) = adpcm_events.front() {
                        if ev.sample_time <= skip_to {
                            adpcm_lr = (ev.left, ev.right);
                            adpcm_events.pop_front();
                        } else {
                            break;
                        }
                    }
                    synth_clock = skip_to;
                }
                resampler.fill_chunk_with(
                    || {
                        if synth_clock >= produced {
                            return None;
                        }
                        while let Some(ev) = events.front() {
                            if ev.sample_time <= synth_clock {
                                synth_opm.write_reg(ev.reg, ev.val);
                                events.pop_front();
                            } else {
                                break;
                            }
                        }
                        while let Some(ev) = adpcm_events.front() {
                            if ev.sample_time <= synth_clock {
                                adpcm_lr = (ev.left, ev.right);
                                adpcm_events.pop_front();
                            } else {
                                break;
                            }
                        }
                        synth_clock += 1;
                        let (ol, or) = synth_opm.next_sample();
                        Some((ol.saturating_add(adpcm_lr.0), or.saturating_add(adpcm_lr.1)))
                    },
                    &mut buf,
                );
                shared.queued_samples.store(
                    produced.saturating_sub(synth_clock) as usize,
                    Ordering::Relaxed,
                );
            }
        }
        if !stream.write(&buf) {
            break;
        }
    }
}

#[cfg(target_os = "linux")]
mod pulse_ffi {
    use super::*;

    const PA_STREAM_PLAYBACK: i32 = 1;
    const PA_SAMPLE_S16LE: i32 = 3;

    #[repr(C)]
    struct PaSampleSpec {
        format: i32,
        rate: u32,
        channels: u8,
    }

    #[repr(C)]
    struct PaBufferAttr {
        maxlength: u32,
        tlength: u32,
        prebuf: u32,
        minreq: u32,
        fragsize: u32,
    }

    type PaSimpleNewFn = unsafe extern "C" fn(
        server: *const c_char,
        name: *const c_char,
        dir: i32,
        dev: *const c_char,
        stream_name: *const c_char,
        ss: *const PaSampleSpec,
        map: *const c_void,
        attr: *const PaBufferAttr,
        error: *mut i32,
    ) -> *mut c_void;

    type PaSimpleWriteFn = unsafe extern "C" fn(
        s: *mut c_void,
        data: *const c_void,
        bytes: usize,
        error: *mut i32,
    ) -> i32;

    type PaSimpleFreeFn = unsafe extern "C" fn(s: *mut c_void);

    pub struct PulseSimpleStream {
        _lib: libloading::Library,
        handle: *mut c_void,
        write_fn: PaSimpleWriteFn,
        free_fn: PaSimpleFreeFn,
    }

    // SAFETY: `pa_simple*` is exclusively owned and accessed by the audio worker thread.
    unsafe impl Send for PulseSimpleStream {}

    impl PulseSimpleStream {
        pub fn open(sample_rate: u32) -> Option<Self> {
            let lib = unsafe { libloading::Library::new("libpulse-simple.so.0") }.ok()?;
            let new_fn: PaSimpleNewFn = unsafe { *lib.get::<PaSimpleNewFn>(b"pa_simple_new\0").ok()? };
            let write_fn: PaSimpleWriteFn =
                unsafe { *lib.get::<PaSimpleWriteFn>(b"pa_simple_write\0").ok()? };
            let free_fn: PaSimpleFreeFn =
                unsafe { *lib.get::<PaSimpleFreeFn>(b"pa_simple_free\0").ok()? };

            let app_name = CString::new("msx68k").ok()?;
            let stream_name = CString::new("YM2151 OPM").ok()?;
            let spec = PaSampleSpec {
                format: PA_SAMPLE_S16LE,
                rate: sample_rate,
                channels: 2,
            };
            // Target ~20 ms playback buffer in PulseAudio / PipeWire.
            let tlength = sample_rate * 4 * 20 / 1000;
            let attr = PaBufferAttr {
                maxlength: u32::MAX,
                tlength,
                prebuf: u32::MAX,
                minreq: u32::MAX,
                fragsize: u32::MAX,
            };
            let mut err = 0i32;
            let handle = unsafe {
                new_fn(
                    std::ptr::null(),
                    app_name.as_ptr(),
                    PA_STREAM_PLAYBACK,
                    std::ptr::null(),
                    stream_name.as_ptr(),
                    &spec,
                    std::ptr::null(),
                    &attr,
                    &mut err,
                )
            };
            if handle.is_null() {
                return None;
            }
            Some(Self {
                _lib: lib,
                handle,
                write_fn,
                free_fn,
            })
        }

        pub fn write(&self, samples: &[i16]) -> bool {
            let mut err = 0i32;
            let bytes = std::mem::size_of_val(samples);
            let rc = unsafe {
                (self.write_fn)(
                    self.handle,
                    samples.as_ptr() as *const c_void,
                    bytes,
                    &mut err,
                )
            };
            rc >= 0
        }
    }

    impl Drop for PulseSimpleStream {
        fn drop(&mut self) {
            unsafe {
                (self.free_fn)(self.handle);
            }
        }
    }
}

#[cfg(target_os = "linux")]
use pulse_ffi::PulseSimpleStream;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resampler_maintains_exact_62500_to_48000_ratio() {
        let mut resampler = Resampler::new();
        // 1 second of 62,500 Hz input -> 48,000 output frames.
        let mut queue: VecDeque<(i16, i16)> = (0..OPM_SAMPLE_RATE as usize)
            .map(|i| {
                let phase = (i as f64) * 2.0 * std::f64::consts::PI * 1000.0 / OPM_SAMPLE_RATE as f64;
                let s = (phase.sin() * 16000.0) as i16;
                (s, -s)
            })
            .collect();

        let mut out = vec![0i16; HOST_SAMPLE_RATE as usize * 2];
        resampler.fill_chunk(&mut queue, &mut out);

        // Consumed almost the entire 1-second input buffer (within 2 samples for priming/rounding).
        assert!(queue.len() <= 2, "remaining input samples: {}", queue.len());

        // Count zero-crossings on left channel: 1 kHz * 1 s = ~2000 crossings.
        let mut crossings = 0usize;
        for w in out.chunks_exact(2).collect::<Vec<_>>().windows(2) {
            if (w[0][0] < 0 && w[1][0] >= 0) || (w[0][0] >= 0 && w[1][0] < 0) {
                crossings += 1;
            }
        }
        assert!(
            (1995..=2005).contains(&crossings),
            "expected ~2000 zero crossings for 1 kHz, got {crossings}"
        );
    }

    #[test]
    fn resampler_decays_cleanly_to_silence_on_underrun() {
        let mut resampler = Resampler::new();
        let mut queue: VecDeque<(i16, i16)> = [(10_000, -10_000), (10_000, -10_000)].into();
        let mut out = vec![0i16; 32 * 2];
        resampler.fill_chunk(&mut queue, &mut out);
        assert_eq!(out[out.len() - 2], 0);
        assert_eq!(out[out.len() - 1], 0);
    }
}
