//! Oki MSM6258V ADPCM voice synthesizer (`$E92000`–`$E93FFF`, DMAC Channel 3).
//!
//! All X68000 ADPCM sample rates (`3,906.25` to `15,625 Hz`, from 4/8 MHz divided by 1024/768/512)
//! are exact integer sub-multiples (`16, 12, 8, 6, 4`) of OPM's `62,500 Hz` sample rate (`160`
//! 10 MHz cycles/sample), so ADPCM mixes cleanly into the same `62,500 Hz` stereo stream.

use msx68k_cpu::BusFault;

use crate::opm::{OpmSynthMode, CYCLES_PER_SAMPLE_10MHZ};

const STEP_SIZE_TABLE: [i32; 49] = [
    16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60, 66,
    73, 80, 88, 97, 107, 118, 130, 143, 157, 173, 190, 209, 230, 253, 279, 307,
    337, 371, 408, 449, 494, 544, 598, 658, 724, 796, 876, 963, 1060, 1166, 1282, 1411,
    1552,
];

const STEP_INDEX_ADJ: [isize; 16] = [
    -1, -1, -1, -1, 2, 4, 6, 8, -1, -1, -1, -1, 2, 4, 6, 8,
];

/// Timestamped ADPCM output level change for offloaded audio-thread mixing.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct AdpcmSampleEvent {
    pub sample_time: u64,
    pub left: i16,
    pub right: i16,
}

/// 2nd-order Butterworth biquad low-pass filter (`fc = 6,800 Hz`, `fs = 62,500 Hz`, `Q = 1/sqrt(2)`).
///
/// Models the X68000 MSM6258V analog active reconstruction filter (`fc ≈ 0.85 * 15.625 kHz / 2`)
/// using Direct Form II Transposed arithmetic.
#[derive(Debug, Clone, Copy)]
pub struct AdpcmLpf {
    z1: f32,
    z2: f32,
}

impl Default for AdpcmLpf {
    fn default() -> Self {
        Self::new()
    }
}

impl AdpcmLpf {
    // Precomputed for fc = 6,800 Hz, fs = 62,500 Hz, Q = 1 / sqrt(2):
    // w0 = 2*pi*6800/62500, cos_w0 = 0.77530133, sin_w0 = 0.63159152,
    // alpha = sin_w0 / sqrt(2) = 0.44660264, a0 = 1 + alpha = 1.44660264.
    const B0: f32 = 0.077_664_27;
    const B1: f32 = 0.155_328_54;
    const B2: f32 = 0.077_664_27;
    const A1: f32 = -1.071_892_6;
    const A2: f32 = 0.382_549_67;

    pub const fn new() -> Self {
        Self { z1: 0.0, z2: 0.0 }
    }

    #[inline]
    pub fn reset(&mut self) {
        self.z1 = 0.0;
        self.z2 = 0.0;
    }

    #[inline]
    pub fn is_settled(&self) -> bool {
        self.z1 == 0.0 && self.z2 == 0.0
    }

    #[inline]
    pub fn process(&mut self, input: i16) -> i16 {
        if input == 0 && self.z1.abs() < 1e-3 && self.z2.abs() < 1e-3 {
            self.z1 = 0.0;
            self.z2 = 0.0;
            return 0;
        }
        let x = input as f32;
        let y = Self::B0 * x + self.z1;
        self.z1 = Self::B1 * x - Self::A1 * y + self.z2;
        self.z2 = Self::B2 * x - Self::A2 * y;
        y.round().clamp(i16::MIN as f32, i16::MAX as f32) as i16
    }
}

/// Oki MSM6258V ADPCM state.
#[derive(Debug, Clone)]
pub struct Adpcm {
    active: bool,
    recording: bool,
    /// Current latched byte from DMAC Ch3 (`$E92003`).
    data_byte: u8,
    has_byte: bool,
    /// `true` when the low nibble was already consumed and the high nibble is pending.
    high_nibble_next: bool,
    /// Decoded 12-bit signed PCM value (`-2048..=2047`).
    signal: i32,
    step_index: usize,
    /// PPI Port C (`$E9A005`) pan/rate/clock bits.
    pan_left: bool,
    pan_right: bool,
    period_10mhz: u32,
    sample_sub: u32,
    /// 62.5 kHz sample-clock tracking for mixing into OPM's 62.5 kHz stream.
    mix_sub: u32,
    sample_clock: u64,
    current_mono: i16,
    current_lr: (i16, i16),
    lpf_enabled: bool,
    lpf: AdpcmLpf,
    audio_enabled: bool,
    synth_mode: OpmSynthMode,
    samples: Vec<(i16, i16)>,
    events: Vec<AdpcmSampleEvent>,
}

impl Default for Adpcm {
    fn default() -> Self {
        Self::new()
    }
}

impl Adpcm {
    pub const fn new() -> Self {
        Self {
            active: false,
            recording: false,
            data_byte: 0,
            has_byte: false,
            high_nibble_next: false,
            signal: 0,
            step_index: 0,
            pan_left: false,  // PPI Port C default 0x0B -> bits 1..0 = 11 (muted)
            pan_right: false,
            period_10mhz: 640, // 8 MHz / 512 = 15,625 Hz default
            sample_sub: 0,
            mix_sub: 0,
            sample_clock: 0,
            current_mono: 0,
            current_lr: (0, 0),
            lpf_enabled: true,
            lpf: AdpcmLpf::new(),
            audio_enabled: false,
            synth_mode: OpmSynthMode::Inline,
            samples: Vec::new(),
            events: Vec::new(),
        }
    }

    pub fn reset(&mut self) {
        let audio_enabled = self.audio_enabled;
        let synth_mode = self.synth_mode;
        let lpf_enabled = self.lpf_enabled;
        *self = Self::new();
        self.audio_enabled = audio_enabled;
        self.synth_mode = synth_mode;
        self.lpf_enabled = lpf_enabled;
    }

    /// Dynamically enables or disables the 2nd-order Butterworth ADPCM low-pass filter.
    #[inline]
    pub fn set_lpf_enabled(&mut self, enabled: bool) {
        if self.lpf_enabled != enabled {
            self.lpf_enabled = enabled;
            self.lpf.reset();
        }
    }

    /// Returns whether the ADPCM low-pass filter is enabled.
    #[inline]
    pub const fn lpf_enabled(&self) -> bool {
        self.lpf_enabled
    }

    #[inline]
    pub const fn is_active(&self) -> bool {
        self.active
    }

    /// Returns `true` when MSM6258V is asserting DMAC Channel 3 request (`DRQ`).
    #[inline]
    pub const fn dma_request(&self) -> bool {
        self.active && !self.has_byte
    }

    pub fn set_audio_enabled(&mut self, enabled: bool) {
        self.audio_enabled = enabled;
        if !enabled {
            self.samples.clear();
            self.events.clear();
        }
    }

    pub fn set_synth_mode(&mut self, mode: OpmSynthMode) {
        self.synth_mode = mode;
        self.samples.clear();
        self.events.clear();
    }

    /// Updates ADPCM panning, divider, and clock from PPI Port C (`$E9A005`).
    pub fn set_ppi_port_c(&mut self, port_c: u8) {
        // Bits 1..0 are active-low mute bits: 0 = unmuted, 1 = muted.
        self.pan_left = (port_c & 0x01) == 0;
        self.pan_right = (port_c & 0x02) == 0;

        // Bits 3..2: divider (0 -> 1024, 1 -> 768, 2/3 -> 512).
        // Bit 4: clock select (0 -> 8 MHz, 1 -> 4 MHz).
        let div = match (port_c >> 2) & 0x03 {
            0 => 1024,
            1 => 768,
            _ => 512,
        };
        let period_4mhz = div * 10 / 4; // in 10 MHz cycles: 2560, 1920, 1280
        self.period_10mhz = if (port_c & 0x10) == 0 {
            period_4mhz / 2 // 8 MHz: 1280, 960, 640
        } else {
            period_4mhz
        };
        self.refresh_lr();
    }

    #[inline]
    fn refresh_lr(&mut self) {
        if !self.active {
            self.current_mono = 0;
            self.current_lr = (0, 0);
            return;
        }
        // Scale 12-bit signed (-2048..2047) by 8 -> -16384..16376 for clean OPM mix headroom.
        let mono = (self.signal << 3) as i16;
        self.current_mono = mono;
        self.current_lr = (
            if self.pan_left { mono } else { 0 },
            if self.pan_right { mono } else { 0 },
        );
    }

    #[inline]
    fn decode_nibble(&mut self, nibble: u8) {
        let step = STEP_SIZE_TABLE[self.step_index];
        let mut diff = step >> 3;
        if (nibble & 1) != 0 {
            diff += step >> 2;
        }
        if (nibble & 2) != 0 {
            diff += step >> 1;
        }
        if (nibble & 4) != 0 {
            diff += step;
        }
        if (nibble & 8) != 0 {
            self.signal = (self.signal - diff).max(-2048);
        } else {
            self.signal = (self.signal + diff).min(2047);
        }
        self.step_index =
            (self.step_index as isize + STEP_INDEX_ADJ[(nibble & 0x0F) as usize]).clamp(0, 48)
                as usize;
        self.refresh_lr();
    }

    #[inline]
    pub fn advance(&mut self, cycles_10mhz: u32) {
        if self.active {
            self.sample_sub = self.sample_sub.saturating_add(cycles_10mhz);
            while self.sample_sub >= self.period_10mhz {
                self.sample_sub -= self.period_10mhz;
                if self.has_byte {
                    let nibble = if self.high_nibble_next {
                        self.high_nibble_next = false;
                        self.has_byte = false;
                        self.data_byte >> 4
                    } else {
                        self.high_nibble_next = true;
                        self.data_byte & 0x0F
                    };
                    self.decode_nibble(nibble);
                    if self.audio_enabled && self.synth_mode == OpmSynthMode::Offload {
                        self.events.push(AdpcmSampleEvent {
                            sample_time: self.sample_clock,
                            left: self.current_lr.0,
                            right: self.current_lr.1,
                        });
                    }
                }
            }
        }

        if !self.audio_enabled {
            return;
        }

        self.mix_sub = self.mix_sub.saturating_add(cycles_10mhz);
        if self.mix_sub < CYCLES_PER_SAMPLE_10MHZ {
            return;
        }
        let mix_count = if self.mix_sub < CYCLES_PER_SAMPLE_10MHZ * 2 {
            self.mix_sub -= CYCLES_PER_SAMPLE_10MHZ;
            1
        } else {
            let n = self.mix_sub / CYCLES_PER_SAMPLE_10MHZ;
            self.mix_sub %= CYCLES_PER_SAMPLE_10MHZ;
            n
        };
        self.sample_clock += mix_count as u64;

        if self.synth_mode == OpmSynthMode::Inline {
            if !self.lpf_enabled {
                self.samples
                    .resize(self.samples.len() + mix_count as usize, self.current_lr);
            } else if self.current_mono == 0 && self.lpf.is_settled() {
                self.samples
                    .resize(self.samples.len() + mix_count as usize, (0, 0));
            } else {
                self.samples.reserve(mix_count as usize);
                for _ in 0..mix_count {
                    let y = self.lpf.process(self.current_mono);
                    self.samples.push((
                        if self.pan_left { y } else { 0 },
                        if self.pan_right { y } else { 0 },
                    ));
                }
            }
        }
    }

    /// Mixes accumulated 62.5 kHz ADPCM samples into `opm_out` at a 0.50:0.50 full-scale ratio
    /// (OPM `>> 1` + ADPCM `signal << 3`), aligning to `opm_out`'s length.
    pub fn mix_into(&mut self, opm_out: &mut [(i16, i16)]) {
        let n = self.samples.len().min(opm_out.len());
        for (dst, &(al, ar)) in opm_out[..n].iter_mut().zip(&self.samples[..n]) {
            dst.0 = (dst.0 >> 1).saturating_add(al);
            dst.1 = (dst.1 >> 1).saturating_add(ar);
        }
        for dst in &mut opm_out[n..] {
            dst.0 >>= 1;
            dst.1 >>= 1;
        }
        self.samples.drain(..n);
    }

    /// Drains timestamped ADPCM level changes for `Offload` mode.
    pub fn take_events(&mut self, out: &mut Vec<AdpcmSampleEvent>) {
        out.append(&mut self.events);
    }

    pub fn read_u8(&self, addr: u32) -> Result<u8, BusFault> {
        let a = addr & 0x00FF_FFFF;
        if (a & 1) == 0 {
            return Err(BusFault::BusError {
                addr: a,
                is_write: false,
            });
        }
        if (a & 3) == 1 {
            Ok(if self.active { 0xC0 } else { 0x40 })
        } else {
            Ok(0xFF)
        }
    }

    pub fn write_u8(&mut self, addr: u32, val: u8) -> Result<(), BusFault> {
        let a = addr & 0x00FF_FFFF;
        if (a & 1) == 0 {
            return Err(BusFault::BusError {
                addr: a,
                is_write: true,
            });
        }
        if (a & 3) == 1 {
            if (val & 0x01) != 0 {
                self.active = false;
                self.recording = false;
                self.has_byte = false;
                self.high_nibble_next = false;
                self.signal = 0;
                self.step_index = 0;
                self.sample_sub = 0;
                self.refresh_lr();
                if self.audio_enabled && self.synth_mode == OpmSynthMode::Offload {
                    self.events.push(AdpcmSampleEvent {
                        sample_time: self.sample_clock,
                        left: 0,
                        right: 0,
                    });
                }
            } else if (val & 0x02) != 0 {
                self.active = true;
                self.recording = false;
                self.has_byte = false;
                self.high_nibble_next = false;
                self.signal = 0;
                self.step_index = 0;
                self.sample_sub = 0;
                self.refresh_lr();
            } else if (val & 0x04) != 0 {
                self.active = true;
                self.recording = true;
            }
        } else if (a & 3) == 3 {
            self.data_byte = val;
            self.has_byte = true;
            self.high_nibble_next = false;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dma_request_handshake_clears_on_write_and_reasserts_after_two_samples() {
        let mut adpcm = Adpcm::new();
        adpcm.set_ppi_port_c(0x08); // 8 MHz (bit4=0) / 512 (640 cycles), L+R unmuted
        assert!(!adpcm.dma_request());

        // Start playback -> immediately requests first byte via DMAC Ch3.
        adpcm.write_u8(0xE92001, 0x02).unwrap();
        assert!(adpcm.dma_request());

        // DMAC Ch3 writes byte -> clears request.
        adpcm.write_u8(0xE92003, 0x73).unwrap();
        assert!(!adpcm.dma_request());

        // After 1 sample period (low nibble consumed), high nibble still pending -> no DRQ yet.
        adpcm.advance(640);
        assert!(!adpcm.dma_request());

        // After 2nd sample period (high nibble consumed), DRQ re-asserts.
        adpcm.advance(640);
        assert!(adpcm.dma_request());
    }

    #[test]
    fn ppi_port_c_controls_rate_and_stereo_panning() {
        let mut adpcm = Adpcm::new();
        adpcm.set_audio_enabled(true);
        // 4 MHz (bit4=1) / 1024 = 2560 cycles, Left only (bit 0 = 0, bit 1 = 1 -> 0x12)
        adpcm.set_ppi_port_c(0x12);
        adpcm.write_u8(0xE92001, 0x02).unwrap();
        adpcm.write_u8(0xE92003, 0x55).unwrap();
        adpcm.advance(2560);

        let mut out = vec![(0i16, 0i16); 16];
        adpcm.mix_into(&mut out);
        assert!(out.iter().any(|&(l, _)| l > 0));
        assert!(out.iter().all(|&(_, r)| r == 0));
    }

    #[test]
    fn biquad_lpf_preserves_dc_and_attenuates_high_frequency() {
        // DC gain should converge to 1.0 (10000 -> ~10000).
        let mut lpf = AdpcmLpf::new();
        let mut y = 0i16;
        for _ in 0..256 {
            y = lpf.process(10_000);
        }
        assert!((9_995..=10_005).contains(&y), "DC output={y}");

        // 15.625 kHz square wave (period = 4 samples at 62.5 kHz) should be strongly attenuated
        // by the 6.8 kHz 2nd-order Butterworth filter (at least -12 dB -> peak < 3500).
        lpf.reset();
        let mut peak = 0i16;
        for i in 0..256 {
            let x = if (i & 2) == 0 { 10_000 } else { -10_000 };
            let out = lpf.process(x);
            if i >= 64 {
                peak = peak.max(out.abs());
            }
        }
        assert!(peak < 3_500, "15.625 kHz peak={peak} was not attenuated");
    }

    #[test]
    fn adpcm_lpf_can_be_toggled_dynamically_and_mixes_at_half_opm() {
        let mut adpcm = Adpcm::new();
        adpcm.set_audio_enabled(true);
        adpcm.set_ppi_port_c(0x08); // 15.625 kHz, L+R
        adpcm.write_u8(0xE92001, 0x02).unwrap();
        adpcm.write_u8(0xE92003, 0x77).unwrap();

        // With LPF ON (default), first 62.5 kHz sample after step is smoothed (< raw level).
        assert!(adpcm.lpf_enabled());
        adpcm.advance(640); // decodes first nibble and pushes 4 samples
        let raw_level = adpcm.current_mono;
        assert!(raw_level > 0);
        let mut out = vec![(2000i16, 2000i16); 4];
        adpcm.mix_into(&mut out);
        // OPM 2000 >> 1 = 1000; filtered ADPCM first sample is smaller than raw_level.
        assert!(out[0].0 > 1000 && out[0].0 < 1000 + raw_level);

        // Toggle LPF OFF dynamically -> subsequent samples output raw level directly.
        adpcm.set_lpf_enabled(false);
        assert!(!adpcm.lpf_enabled());
        adpcm.advance(160);
        let mut out_off = vec![(2000i16, 2000i16); 1];
        adpcm.mix_into(&mut out_off);
        assert_eq!(out_off[0].0, 1000 + raw_level);
    }
}
