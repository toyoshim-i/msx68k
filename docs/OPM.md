# Yamaha YM2151 (OPM) Sound Emulation

This document describes the architecture, hardware fidelity, and known differences from the real Yamaha YM2151 (OPM) + YM3012 DAC silicon of `crates/msx68k-bus/src/opm.rs`.

---

## 1. Architecture Overview

On the X68000, the YM2151 OPM is clocked at **4.000 MHz** and mapped at `$E90001` (address latch) and `$E90003` (data write / status read), producing stereo output at **62,500 Hz** (`4,000,000 / 64`, or 160 cycles at 10 MHz per sample).

Instead of simulating the chip's internal 32-cycle time-division-multiplexed (TDM) gate pipeline one clock at a time, `msx68k-bus::opm::Opm` evaluates all active channels once per 62.5 kHz sample while retaining the chip's arithmetic tables, bit widths, and inter-slot sample delays.

- **Synthesis modes (`OpmSynthMode`)**:
  - `Inline`: Synthesizes 62.5 kHz PCM directly on the VM thread during `Opm::advance()`.
  - `Offload`: Runs timers, Busy flag, IRQ (`GPIP3`), and timestamped register event recording (`OpmRegEvent`) on the VM thread; a dedicated audio thread (`crates/msx68k-app/src/audio.rs`) replays events and runs FM synthesis bit-identically.
- **Host resampling**: `crates/msx68k-app/src/audio.rs` resamples 62,500 Hz to 48,000 Hz using an exact `125 / 96` rational linear resampler.
- **Active-channel skipping**: `active_mask` tracks channels with non-`Off` operators so completely silent channels (and an idle chip) skip FM synthesis.

---

## 2. Hardware-Faithful Waveform & Control Logic

The following YM2151 hardware behaviors are modeled directly from the chip's digital logic:

- **Sine / Exponential ROMs & Operator Amplitude**:
  - Uses the exact 256-entry 1/4-wave `logsinrom` (`0x859..0x000`) and `exprom` (`0x7fa..0x400`) lookup tables.
  - Operator outputs are 14-bit signed (`(exp << 2) >> pow`, peak `±8168`), matching hardware FM modulation depth (`±8π` radians at full scale) and 2-sample `M1` self-feedback (`((m1[0] + m1[1]) >> 1) >> (9 - FL)`).
- **Slot Register & Key-On Mapping + TDM Pipeline Delays**:
  - Slot registers `$40/$48/$50/$58` map to `M1 / M2 / C1 / C2`, while Key-On register `$08` maps bits `3..6` to `M1 / C1 / M2 / C2` (bits 4 and 5 swapped relative to slot order, as on real hardware).
  - `Channel::eval()` evaluates slots in hardware TDM order (`M1 -> M2 -> C1 -> C2`) with a 1-sample latch (`op_c1_prev`, `op1_hist`), reproducing the silicon's 1-sample delay on `C1 -> M2` (`CON=0, 1, 2`), `M1 -> M2` (`CON=1, 5`), and `C1 -> C2` (`CON=3`) while keeping 0-sample latency on `M1 -> C1` and `M2 -> C2`.
- **Phase Generator (`KC`, `KF`, `DT1`, `DT2`, `MUL`, LFO PM)**:
  - Uses the hardware 64-entry piecewise-linear F-number ROM (`PG_FREQ_TABLE`, `kc_to_fnum`) and carry-skipping keycode arithmetic (`calc_kcode`) to combine `KC`, `KF`, `DT2`, and LFO PM.
  - Applies non-linear `PMS` scaling (`channel_pm_offset`) matching the chip's PMS barrel-shifter logic.
  - Uses the post-`DT2`/PM `pg_kcode` for both `DT1` detune lookup and Envelope Generator key scaling (`KS`).
- **Envelope Generator (EG)**:
  - Runs on a 3-sample divider (`20,833.3 Hz`) with silicon `eg_timer` trailing-zero rate stepping for rates `1..47`, `EG_STEPHI` substepping for rates `48..63`, 10-bit exponential attack (`(~eg_level * step) >> 4`), instant attack at `rate >= 62`, and `0x3F0` mute/off cutoff.
- **LFO & Noise Generator**:
  - LFO frequency accumulator uses the 22-bit (`0x40_0000`) divisor matching the YM2151's ~60 Hz maximum rate at 4 MHz, with hardware polarity and phase for Sawtooth (`0`), Square (`1`), and Triangle (`2`) AM/PM waveforms and full 10-bit AM attenuation depth (`AMS=3, AMD=127` ≈ 96 dB).
  - 17-bit LFSR noise (taps bit 0 ^ bit 3) clocks at 125 kHz (2 ticks per 62.5 kHz sample, period `32 - NFRQ`), driving Channel 7 `C2` with linear EG attenuation (`±2046` max) rather than `EXP_TABLE`.

---

## 3. Known Differences from Real YM2151 (OPM) Hardware

These differences are deliberate trade-offs of the sample-level design or unimplemented edge features:

1. **YM3012 Floating-Point DAC Quantization & L/R Half-Sample Offset**
   - Real hardware serializes the 16-bit channel sum to an external **YM3012** DAC in 3-bit exponent + 10-bit mantissa floating-point format, truncating low-order bits at higher amplitudes (~0.1% NRMSE). `opm.rs` outputs full-precision 16-bit signed linear PCM clamped to `i16`.
   - Real hardware clears the Left and Right channel accumulators 16 internal cycles (half a sample) apart, so on the Left channel `M1`/`M2` and `C1`/`C2` cross a sample boundary relative to each other. `opm.rs` sums all 4 slots into Left and Right on the same sample.
2. **Internal Counters Paused When Silent (`active_mask == 0`)**
   - To avoid idle CPU overhead, `step_sample()` is skipped when all 8 channels are inactive, pausing `eg_div`/`eg_counter`, `lfo_counter`/`lfo_phase`, and `noise_lfsr`. On real hardware these counters free-run continuously, so the initial EG step alignment (0–2 samples) and LFO phase immediately after silence can differ from a continuously running chip.
3. **LFO Noise Waveform (`wave == 3`) & Fractional Divider Jitter**
   - On real hardware, LFO waveform `3` shifts 1 bit per LFO clock from the noise LFSR into an internal sample-and-hold register. `opm.rs` reads `noise_lfsr & 0xFF` directly.
   - Real hardware derives LFO ticks from a discrete counter chain (`lfo_counter1`/`2`/`3` bit-masking) with slight step jitter; `opm.rs` uses a uniform phase accumulator (`(16 | lo) << hi` mod `0x40_0000`).
4. **Register Write Pipeline Latency & Test Register (`$01`)**
   - Real hardware latches register writes through its 32-cycle TDM pipeline (1–3 sample latency before affecting output) and implements test register `$01` bits `0..7` (EG/PG test modes, serial test readback). `opm.rs` applies register writes immediately on the next sample and only implements `$01` bit 1 (LFO reset).
5. **CSM Mode (`$14` bit 7) When `AR < 62`**
   - On real hardware, Timer A overflow asserts an internal 1-sample `kon_csm` pulse; if an EG tick coincides with that sample and `AR < 62`, the operator performs one Attack step before entering Release on the following sample. `opm.rs` jumps `eg_level` to `0` when `rate >= 62` and enters `Release` directly.
6. **Ch 7 `C2` Noise Amplitude Granularity**
   - Real hardware outputs noise with 4-unit steps (`+2040 / -2044` peak via `((nc_out & ~1) << 2)`), whereas `opm.rs` uses 2-unit steps (`((0x3FF - total_eg) << 1)`, `±2046` peak).

---

## 4. Verification

Unit tests in `crates/msx68k-bus/src/opm.rs` (`cargo test -p msx68k-bus --lib opm`) cover:

- Timer A/B overflow, Busy flag duration, and IRQ assertion/clearing.
- Key-On bit mapping (`$08` bit 4 = `C1`, bit 5 = `M2`) across all 4 slots.
- 4.000 MHz pitch accuracy (`A4` `KC=$4A` → ~491.7 Hz), `DT1`/`DT2`, and `PMS` consistency.
- Bit-exact 4-operator `CON=2` pipeline output against silicon reference samples (`[81, -2396, 740, 798, 2410, -1658]`).
- Exponential attack/decay/sustain/release progression, operator peak amplitude (`±8168`), LFO AM/PM waveforms and reset, 125 kHz noise LFSR stepping and linear amplitude, stereo panning, and bit-for-bit `Inline` vs `Offload` parity.
