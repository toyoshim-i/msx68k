//! Yamaha YM2151 (OPM) FM Operator Type-M sound generator.
//!
//! Models the chip's internal 62,500 Hz (`4 MHz / 64`, 160 cycles at 10 MHz) pipeline:
//! - Sample-accurate register latching with Busy flag and Timer A/B + CSM mode
//! - 20-bit phase generator (`KC`, `KF`, `DT1`, `DT2`, `MUL`, LFO PM)
//! - 10-bit envelope generator with 3-sample divider, key scale (`KS`), and exponential attack
//! - 1/4-wave log-sin and `2^-x` exp lookup tables (no linear multiplier)
//! - M1 2-sample self-feedback, 8 algorithms (`CON`), 17-bit LFSR noise (Ch 7 C2), and 4-waveform LFO
//! - Per-channel active tracking so completely silent channels/chips skip FM synthesis

use msx68k_cpu::BusFault;

/// 10 MHz CPU cycles per OPM native sample (`10_000_000 / 62_500 = 160`).
pub const CYCLES_PER_SAMPLE_10MHZ: u32 = 160;

/// OPM native sample rate at X68000's 4.000 MHz master clock (`4_000_000 / 64`).
pub const OPM_SAMPLE_RATE: u32 = 62_500;

/// Busy duration in 10 MHz cycles after a register data write (32 OPM clocks at 4 MHz = 8 us).
const BUSY_CYCLES_10MHZ: u32 = 80;

/// Maximum EG attenuation (10-bit, silent).
const EG_SILENT: u16 = 0x3FF;

/// EG attenuation threshold where Decay/Sustain/Release transitions to Off/Mute (`0x3F0` in silicon).
const EG_OFF_LEVEL: u16 = 0x3F0;

/// Where YM2151 FM synthesis runs relative to the VM execution thread.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Default)]
pub enum OpmSynthMode {
    /// Synthesize 62.5 kHz FM samples inline on the VM thread during `advance()`.
    #[default]
    Inline,
    /// Only run timers, Busy, IRQ, and timestamped register event capture on the VM thread;
    /// FM synthesis runs on the audio thread.
    Offload,
}

/// Timestamped OPM register write event for offloaded synthesis.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct OpmRegEvent {
    pub sample_time: u64,
    pub reg: u8,
    pub val: u8,
}

/// Envelope phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EgPhase {
    Off,
    Attack,
    Decay,
    Sustain,
    Release,
}

/// 1/4-wave log-sin table (`logsinrom` from YM2151 silicon).
const LOG_SIN_TABLE: [u16; 256] = [
    0x859, 0x6c3, 0x607, 0x58b, 0x52e, 0x4e4, 0x4a6, 0x471,
    0x443, 0x41a, 0x3f5, 0x3d3, 0x3b5, 0x398, 0x37e, 0x365,
    0x34e, 0x339, 0x324, 0x311, 0x2ff, 0x2ed, 0x2dc, 0x2cd,
    0x2bd, 0x2af, 0x2a0, 0x293, 0x286, 0x279, 0x26d, 0x261,
    0x256, 0x24b, 0x240, 0x236, 0x22c, 0x222, 0x218, 0x20f,
    0x206, 0x1fd, 0x1f5, 0x1ec, 0x1e4, 0x1dc, 0x1d4, 0x1cd,
    0x1c5, 0x1be, 0x1b7, 0x1b0, 0x1a9, 0x1a2, 0x19b, 0x195,
    0x18f, 0x188, 0x182, 0x17c, 0x177, 0x171, 0x16b, 0x166,
    0x160, 0x15b, 0x155, 0x150, 0x14b, 0x146, 0x141, 0x13c,
    0x137, 0x133, 0x12e, 0x129, 0x125, 0x121, 0x11c, 0x118,
    0x114, 0x10f, 0x10b, 0x107, 0x103, 0x0ff, 0x0fb, 0x0f8,
    0x0f4, 0x0f0, 0x0ec, 0x0e9, 0x0e5, 0x0e2, 0x0de, 0x0db,
    0x0d7, 0x0d4, 0x0d1, 0x0cd, 0x0ca, 0x0c7, 0x0c4, 0x0c1,
    0x0be, 0x0bb, 0x0b8, 0x0b5, 0x0b2, 0x0af, 0x0ac, 0x0a9,
    0x0a7, 0x0a4, 0x0a1, 0x09f, 0x09c, 0x099, 0x097, 0x094,
    0x092, 0x08f, 0x08d, 0x08a, 0x088, 0x086, 0x083, 0x081,
    0x07f, 0x07d, 0x07a, 0x078, 0x076, 0x074, 0x072, 0x070,
    0x06e, 0x06c, 0x06a, 0x068, 0x066, 0x064, 0x062, 0x060,
    0x05e, 0x05c, 0x05b, 0x059, 0x057, 0x055, 0x053, 0x052,
    0x050, 0x04e, 0x04d, 0x04b, 0x04a, 0x048, 0x046, 0x045,
    0x043, 0x042, 0x040, 0x03f, 0x03e, 0x03c, 0x03b, 0x039,
    0x038, 0x037, 0x035, 0x034, 0x033, 0x031, 0x030, 0x02f,
    0x02e, 0x02d, 0x02b, 0x02a, 0x029, 0x028, 0x027, 0x026,
    0x025, 0x024, 0x023, 0x022, 0x021, 0x020, 0x01f, 0x01e,
    0x01d, 0x01c, 0x01b, 0x01a, 0x019, 0x018, 0x017, 0x017,
    0x016, 0x015, 0x014, 0x014, 0x013, 0x012, 0x011, 0x011,
    0x010, 0x00f, 0x00f, 0x00e, 0x00d, 0x00d, 0x00c, 0x00c,
    0x00b, 0x00a, 0x00a, 0x009, 0x009, 0x008, 0x008, 0x007,
    0x007, 0x007, 0x006, 0x006, 0x005, 0x005, 0x005, 0x004,
    0x004, 0x004, 0x003, 0x003, 0x003, 0x002, 0x002, 0x002,
    0x002, 0x001, 0x001, 0x001, 0x001, 0x001, 0x001, 0x001,
    0x000, 0x000, 0x000, 0x000, 0x000, 0x000, 0x000, 0x000,
];

/// Exponential lookup table (`exprom` from YM2151 silicon: `round(2^((255 - i) / 256) * 1024)`, `1024..=2042`).
const EXP_TABLE: [u16; 256] = [
    0x7fa, 0x7f5, 0x7ef, 0x7ea, 0x7e4, 0x7df, 0x7da, 0x7d4,
    0x7cf, 0x7c9, 0x7c4, 0x7bf, 0x7b9, 0x7b4, 0x7ae, 0x7a9,
    0x7a4, 0x79f, 0x799, 0x794, 0x78f, 0x78a, 0x784, 0x77f,
    0x77a, 0x775, 0x770, 0x76a, 0x765, 0x760, 0x75b, 0x756,
    0x751, 0x74c, 0x747, 0x742, 0x73d, 0x738, 0x733, 0x72e,
    0x729, 0x724, 0x71f, 0x71a, 0x715, 0x710, 0x70b, 0x706,
    0x702, 0x6fd, 0x6f8, 0x6f3, 0x6ee, 0x6e9, 0x6e5, 0x6e0,
    0x6db, 0x6d6, 0x6d2, 0x6cd, 0x6c8, 0x6c4, 0x6bf, 0x6ba,
    0x6b5, 0x6b1, 0x6ac, 0x6a8, 0x6a3, 0x69e, 0x69a, 0x695,
    0x691, 0x68c, 0x688, 0x683, 0x67f, 0x67a, 0x676, 0x671,
    0x66d, 0x668, 0x664, 0x65f, 0x65b, 0x657, 0x652, 0x64e,
    0x649, 0x645, 0x641, 0x63c, 0x638, 0x634, 0x630, 0x62b,
    0x627, 0x623, 0x61e, 0x61a, 0x616, 0x612, 0x60e, 0x609,
    0x605, 0x601, 0x5fd, 0x5f9, 0x5f5, 0x5f0, 0x5ec, 0x5e8,
    0x5e4, 0x5e0, 0x5dc, 0x5d8, 0x5d4, 0x5d0, 0x5cc, 0x5c8,
    0x5c4, 0x5c0, 0x5bc, 0x5b8, 0x5b4, 0x5b0, 0x5ac, 0x5a8,
    0x5a4, 0x5a0, 0x59c, 0x599, 0x595, 0x591, 0x58d, 0x589,
    0x585, 0x581, 0x57e, 0x57a, 0x576, 0x572, 0x56f, 0x56b,
    0x567, 0x563, 0x560, 0x55c, 0x558, 0x554, 0x551, 0x54d,
    0x549, 0x546, 0x542, 0x53e, 0x53b, 0x537, 0x534, 0x530,
    0x52c, 0x529, 0x525, 0x522, 0x51e, 0x51b, 0x517, 0x514,
    0x510, 0x50c, 0x509, 0x506, 0x502, 0x4ff, 0x4fb, 0x4f8,
    0x4f4, 0x4f1, 0x4ed, 0x4ea, 0x4e7, 0x4e3, 0x4e0, 0x4dc,
    0x4d9, 0x4d6, 0x4d2, 0x4cf, 0x4cc, 0x4c8, 0x4c5, 0x4c2,
    0x4be, 0x4bb, 0x4b8, 0x4b5, 0x4b1, 0x4ae, 0x4ab, 0x4a8,
    0x4a4, 0x4a1, 0x49e, 0x49b, 0x498, 0x494, 0x491, 0x48e,
    0x48b, 0x488, 0x485, 0x482, 0x47e, 0x47b, 0x478, 0x475,
    0x472, 0x46f, 0x46c, 0x469, 0x466, 0x463, 0x460, 0x45d,
    0x45a, 0x457, 0x454, 0x451, 0x44e, 0x44b, 0x448, 0x445,
    0x442, 0x43f, 0x43c, 0x439, 0x436, 0x433, 0x430, 0x42d,
    0x42a, 0x428, 0x425, 0x422, 0x41f, 0x41c, 0x419, 0x416,
    0x414, 0x411, 0x40e, 0x40b, 0x408, 0x406, 0x403, 0x400,
];

/// YM2151 silicon piecewise-linear F-number ROM (`pg_freqtable[64]`: `(basefreq, approxtype, slope)`).
const PG_FREQ_TABLE: [(i32, u8, i32); 64] = [
    (1299, 1, 19), (1318, 1, 19), (1337, 1, 19), (1356, 1, 20),
    (1376, 1, 20), (1396, 1, 20), (1416, 1, 21), (1437, 1, 20),
    (1458, 1, 21), (1479, 1, 21), (1501, 1, 22), (1523, 1, 22),
    (0,    0, 16), (0,    0, 16), (0,    0, 16), (0,    0, 16),
    (1545, 1, 22), (1567, 1, 22), (1590, 1, 23), (1613, 1, 23),
    (1637, 1, 23), (1660, 1, 24), (1685, 1, 24), (1709, 1, 24),
    (1734, 1, 25), (1759, 1, 25), (1785, 1, 26), (1811, 1, 26),
    (0,    0, 16), (0,    0, 16), (0,    0, 16), (0,    0, 16),
    (1837, 1, 26), (1864, 1, 27), (1891, 1, 27), (1918, 1, 28),
    (1946, 1, 28), (1975, 1, 28), (2003, 1, 29), (2032, 1, 30),
    (2062, 1, 30), (2092, 1, 30), (2122, 1, 31), (2153, 1, 31),
    (0,    0, 16), (0,    0, 16), (0,    0, 16), (0,    0, 16),
    (2185, 1, 31), (2216, 0, 31), (2249, 0, 31), (2281, 0, 31),
    (2315, 0, 31), (2348, 0, 31), (2382, 0, 30), (2417, 0, 30),
    (2452, 0, 30), (2488, 0, 30), (2524, 0, 30), (2561, 0, 30),
    (0,    0, 16), (0,    0, 16), (0,    0, 16), (0,    0, 16),
];

/// Converts YM2151 internal keycode (`OPM_CalcKCode` output) to 12-bit F-number.
#[inline]
fn kc_to_fnum(kcode: i32) -> u32 {
    let kcode_h = ((kcode >> 4) & 63) as usize;
    let kcode_l = kcode & 15;
    let (basefreq, approxtype, raw_slope) = PG_FREQ_TABLE[kcode_h];
    let mut sum = 0i32;
    if approxtype != 0 {
        for i in 0..4 {
            if (kcode_l & (1 << i)) != 0 {
                sum += raw_slope >> (3 - i);
            }
        }
    } else {
        let slope = raw_slope | 1;
        if (kcode_l & 1) != 0 {
            sum += (slope >> 3) + 2;
        }
        if (kcode_l & 2) != 0 {
            sum += 8;
        }
        if (kcode_l & 4) != 0 {
            sum += slope >> 1;
        }
        if (kcode_l & 8) != 0 {
            sum += slope + 1;
        }
        if (kcode_l & 12) == 12 && (raw_slope & 1) == 0 {
            sum += 4;
        }
    }
    (basefreq + (sum >> 1)) as u32
}

/// Combines channel `KC`/`KF`, LFO PM offset (`lfo_mag`, `lfo_neg`), and `DT2` using YM2151's
/// hardware carry-skipping keycode arithmetic (`OPM_CalcKCode`).
#[inline]
fn calc_kcode(kcf: i32, lfo_mag: i32, lfo_neg: bool, dt: u8) -> i32 {
    let lfo_sign = !lfo_neg;
    let lfo = if !lfo_sign { !lfo_mag } else { lfo_mag };
    let mut sum = (kcf & 8191) + (lfo & 8191) + (!lfo_sign as i32);
    let cr = ((kcf & 255) + (lfo & 255) + (!lfo_sign as i32)) >> 8;
    let overflow1 = (sum & (1 << 13)) != 0;
    sum &= 8191;
    if lfo_sign && ((((sum >> 6) & 3) == 3) || cr != 0) {
        sum += 64;
    }
    let mut negoverflow = false;
    if !lfo_sign && cr == 0 && (lfo & 192) != 0 {
        sum += (-64i32) & 8191;
        negoverflow = true;
    }
    let overflow2 = (sum & (1 << 13)) != 0;
    sum &= 8191;
    if (!lfo_sign && !overflow1) || (negoverflow && !overflow2) {
        sum = 0;
    }
    if lfo_sign && (overflow1 || overflow2) {
        sum = 8127;
    }

    let mut t2 = sum & 63;
    if dt == 2 {
        t2 += 20;
    }
    if dt == 2 || dt == 3 {
        t2 += 32;
    }

    let b0 = ((t2 >> 6) & 1) != 0;
    let b1 = dt == 2;
    let b2 = ((sum >> 6) & 1) != 0;
    let b3 = ((sum >> 7) & 1) != 0;

    let w2 = b0 && b1 && b2;
    let w3 = b0 && b3;
    let w6 = (b0 && !w2 && !w3) || (b3 && !b0 && b1);

    t2 &= 63;
    let mut t3 = (sum >> 6)
        + (w6 as i32)
        + (b1 as i32)
        + ((w2 || w3) as i32) * 2
        + ((dt == 3) as i32) * 4
        + ((dt != 0) as i32) * 8;
    if (t3 & 128) != 0 {
        t2 = 63;
        t3 = 126;
    }
    t3 * 64 + t2
}

/// DT1 magnitudes for `dt1 & 3` across 32 keycodes (0..31).
const DT1_TABLE: [[u8; 32]; 4] = [
    [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    [0, 0, 0, 0, 1, 1, 1, 1, 1, 1, 1, 1, 2, 2, 2, 2, 2, 3, 3, 3, 4, 4, 4, 5, 5, 6, 6, 7, 8, 8, 8, 8],
    [1, 1, 1, 1, 2, 2, 2, 2, 2, 3, 3, 3, 4, 4, 4, 5, 5, 6, 6, 7, 8, 8, 9, 10, 11, 12, 13, 14, 16, 16, 16, 16],
    [2, 2, 2, 2, 2, 3, 3, 3, 4, 4, 4, 5, 5, 6, 6, 7, 8, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 20, 22, 22, 22, 22],
];

/// High-rate EG substep pattern (`eg_stephi` from YM2151 silicon, rates 48..63).
const EG_STEPHI: [[u8; 4]; 4] = [
    [0, 0, 0, 0],
    [1, 0, 0, 0],
    [1, 0, 1, 0],
    [1, 1, 1, 0],
];

/// LFO AM sensitivity shift (`AMS` 0..3).
const AMS_SHIFT: [u8; 4] = [8, 2, 1, 0];

#[derive(Debug, Clone)]
struct Operator {
    // Registers
    dt1: u8,
    mul: u8,
    tl: u8,
    ks: u8,
    ar: u8,
    ams_en: bool,
    d1r: u8,
    dt2: u8,
    d2r: u8,
    d1l: u16, // pre-shifted sustain threshold (0..=992)
    rr: u8,

    // Phase & Envelope state
    phase: u32,
    phase_inc: u32,
    pg_kcode: u8,
    eg_phase: EgPhase,
    eg_level: u16,
    key_on: bool,
    out: i32,
}

impl Operator {
    const fn new() -> Self {
        Self {
            dt1: 0,
            mul: 0,
            tl: 0,
            ks: 0,
            ar: 0,
            ams_en: false,
            d1r: 0,
            dt2: 0,
            d2r: 0,
            d1l: 0,
            rr: 0,
            phase: 0,
            phase_inc: 0,
            pg_kcode: 0,
            eg_phase: EgPhase::Off,
            eg_level: EG_SILENT,
            key_on: false,
            out: 0,
        }
    }

    fn key_on(&mut self, kc: u8, kf: u8, pm_offset: i32) {
        if self.key_on {
            return;
        }
        self.key_on = true;
        self.phase = 0;
        self.update_phase_inc(kc, kf, pm_offset);
        let rate = self.effective_rate(self.ar);
        if rate >= 62 {
            self.eg_level = 0;
            self.eg_phase = if self.d1l == 0 {
                EgPhase::Sustain
            } else {
                EgPhase::Decay
            };
        } else {
            self.eg_phase = EgPhase::Attack;
        }
    }

    fn key_off(&mut self) {
        if !self.key_on {
            return;
        }
        self.key_on = false;
        if self.eg_phase != EgPhase::Off {
            self.eg_phase = EgPhase::Release;
        }
    }

    fn csm_key_on(&mut self, kc: u8, kf: u8, pm_offset: i32) {
        if self.key_on {
            return;
        }
        self.phase = 0;
        self.update_phase_inc(kc, kf, pm_offset);
        let rate = self.effective_rate(self.ar);
        if rate >= 62 {
            self.eg_level = 0;
        }
        self.eg_phase = EgPhase::Release;
    }

    #[inline]
    fn effective_rate(&self, base_5bit: u8) -> u8 {
        if base_5bit == 0 {
            return 0;
        }
        let ksv = self.pg_kcode >> (3 - self.ks);
        ((base_5bit << 1) + ksv).min(63)
    }

    fn update_phase_inc(&mut self, kc: u8, kf: u8, pm_offset: i32) {
        let kcf = ((kc as i32) << 6) + ((kf >> 2) as i32);
        let lfo_neg = pm_offset < 0;
        let lfo_mag = pm_offset.abs();
        let kcode = calc_kcode(kcf, lfo_mag, lfo_neg, self.dt2);
        let fnum = kc_to_fnum(kcode);
        let kcode_h = ((kcode >> 8) & 0x1F) as u32;
        self.pg_kcode = kcode_h as u8;

        let block = kcode_h >> 2;
        let mut basefreq = (fnum << block) >> 2;
        let dt1_kcode = kcode_h.min(0x1C) as usize;
        let dt1_mag = DT1_TABLE[(self.dt1 & 3) as usize][dt1_kcode] as u32;
        if (self.dt1 & 4) != 0 {
            basefreq = basefreq.wrapping_sub(dt1_mag);
        } else {
            basefreq = basefreq.wrapping_add(dt1_mag);
        }
        basefreq &= 0x1FFFF;
        let inc = if self.mul == 0 {
            basefreq >> 1
        } else {
            basefreq.wrapping_mul(self.mul as u32)
        };
        self.phase_inc = inc & 0xFFFFF;
    }

    fn step_eg(&mut self, eg_counter: u32) {
        let rate = match self.eg_phase {
            EgPhase::Off => return,
            EgPhase::Attack => self.effective_rate(self.ar),
            EgPhase::Decay => self.effective_rate(self.d1r),
            EgPhase::Sustain => self.effective_rate(self.d2r),
            EgPhase::Release => self.effective_rate((self.rr << 1) | 1),
        };
        if rate == 0 {
            return;
        }
        let step: u16 = if rate >= 48 {
            let inchi = EG_STEPHI[(rate & 3) as usize][(eg_counter & 3) as usize];
            let inc = (inchi + (rate >> 2) - 11).min(4);
            1u16 << (inc - 1)
        } else {
            let timer16 = (eg_counter & 0xFFFF) as u16;
            if timer16 == 0 {
                return;
            }
            let shift = ((timer16.trailing_zeros() + 1) + (rate >> 2) as u32) & 15;
            let hit = match shift {
                12 => true,
                13 => ((rate >> 1) & 1) != 0,
                14 => (rate & 1) != 0,
                _ => false,
            };
            if hit { 1 } else { return }
        };
        match self.eg_phase {
            EgPhase::Attack => {
                if self.eg_level > 0 {
                    // 10-bit exponential attack: eg_level += (~eg_level * step) >> 4
                    let delta = ((!(self.eg_level as i32)) * (step as i32)) >> 4;
                    self.eg_level = (self.eg_level as i32 + delta).clamp(0, EG_SILENT as i32) as u16;
                }
                if self.eg_level == 0 {
                    self.eg_phase = if self.d1l == 0 {
                        EgPhase::Sustain
                    } else {
                        EgPhase::Decay
                    };
                }
            }
            EgPhase::Decay => {
                self.eg_level = (self.eg_level + step).min(EG_SILENT);
                if self.eg_level >= EG_OFF_LEVEL {
                    self.eg_level = EG_SILENT;
                    self.eg_phase = EgPhase::Off;
                } else if self.eg_level >= self.d1l {
                    self.eg_phase = EgPhase::Sustain;
                }
            }
            EgPhase::Sustain => {
                self.eg_level = (self.eg_level + step).min(EG_SILENT);
                if self.eg_level >= EG_OFF_LEVEL {
                    self.eg_level = EG_SILENT;
                    self.eg_phase = EgPhase::Off;
                }
            }
            EgPhase::Release => {
                self.eg_level = (self.eg_level + step).min(EG_SILENT);
                if self.eg_level >= EG_OFF_LEVEL {
                    self.eg_level = EG_SILENT;
                    self.eg_phase = EgPhase::Off;
                }
            }
            EgPhase::Off => {}
        }
    }

    #[inline]
    fn compute_sample(&mut self, modulation: i32, am_atten: u16) -> i32 {
        let phase_10 = (((self.phase >> 10) as i32 + modulation) & 0x3FF) as u32;
        self.phase = (self.phase + self.phase_inc) & 0xFFFFF;

        let total_eg = (self.eg_level + ((self.tl as u16) << 3) + if self.ams_en { am_atten } else { 0 })
            .min(EG_SILENT);
        if total_eg >= EG_SILENT {
            self.out = 0;
            return 0;
        }

        let quarter = (phase_10 & 0xFF) as usize;
        let idx = if (phase_10 & 0x100) != 0 { 255 - quarter } else { quarter };
        let log_sin = LOG_SIN_TABLE[idx];
        let atten = log_sin + (total_eg << 2);
        let shift = atten >> 8;
        let val = if shift >= 13 {
            0
        } else {
            ((EXP_TABLE[(atten & 0xFF) as usize] << 2) >> shift) as i32
        };
        self.out = if (phase_10 & 0x200) != 0 { -val } else { val };
        self.out
    }

    #[inline]
    fn compute_noise_sample(&mut self, noise_bit: bool, am_atten: u16) -> i32 {
        self.phase = (self.phase + self.phase_inc) & 0xFFFFF;
        let total_eg = (self.eg_level + ((self.tl as u16) << 3) + if self.ams_en { am_atten } else { 0 })
            .min(EG_SILENT);
        if total_eg >= EG_OFF_LEVEL {
            self.out = 0;
            return 0;
        }
        // Hardware YM2151 noise channel uses linear EG attenuation (±2044 max), not EXP_TABLE.
        let val = ((EG_SILENT - total_eg) as i32) << 1;
        self.out = if noise_bit { val } else { -val };
        self.out
    }

    #[inline]
    fn is_active(&self) -> bool {
        self.eg_phase != EgPhase::Off
    }
}

#[derive(Debug, Clone)]
struct Channel {
    rl: u8,  // bits 1..0 = R/L enable (from reg bits 7..6)
    fl: u8,  // feedback 0..7
    con: u8, // algorithm 0..7
    kc: u8,
    kf: u8,
    pms: u8,
    ams: u8,
    op1_hist: [i32; 2],
    op_c1_prev: i32,
    ops: [Operator; 4], // 0: M1, 1: M2, 2: C1, 3: C2
}

impl Channel {
    const fn new() -> Self {
        Self {
            rl: 0,
            fl: 0,
            con: 0,
            kc: 0,
            kf: 0,
            pms: 0,
            ams: 0,
            op1_hist: [0; 2],
            op_c1_prev: 0,
            ops: [
                Operator::new(),
                Operator::new(),
                Operator::new(),
                Operator::new(),
            ],
        }
    }

    #[inline]
    fn is_active(&self) -> bool {
        self.ops[0].is_active()
            || self.ops[1].is_active()
            || self.ops[2].is_active()
            || self.ops[3].is_active()
    }

    fn update_pitch(&mut self, pm_lfo: i32) {
        let pm = Opm::channel_pm_offset(self.pms, pm_lfo);
        for op in &mut self.ops {
            op.update_phase_inc(self.kc, self.kf, pm);
        }
    }

    fn step_eg(&mut self, eg_counter: u32) {
        for op in &mut self.ops {
            op.step_eg(eg_counter);
        }
    }

    #[inline]
    fn eval(&mut self, am_lfo: u16, use_noise: bool, noise_bit: bool) -> (i32, i32) {
        let am_atten = if self.ams == 0 {
            0
        } else {
            am_lfo >> AMS_SHIFT[self.ams as usize]
        };

        // Hardware TDM slot order: M1(0) -> M2(1) -> C1(2) -> C2(3) with 16-cycle pipeline.
        // M1 uses op1_hist[0..2]; M2 uses previous M1/C1; C1 uses current M1;
        // C2 uses current M1/M2 and previous C1.
        let m1_prev = self.op1_hist[0];
        let c1_prev = self.op_c1_prev;

        let fb = if self.fl == 0 {
            0
        } else {
            ((self.op1_hist[0] + self.op1_hist[1]) >> 1) >> (9 - self.fl)
        };
        let m1 = self.ops[0].compute_sample(fb, am_atten);

        let m2_mod = match self.con {
            0 | 2 => c1_prev >> 1,
            1 => (m1_prev + c1_prev) >> 1,
            5 => m1_prev >> 1,
            _ => 0,
        };
        let m2 = self.ops[1].compute_sample(m2_mod, am_atten);

        let c1_mod = match self.con {
            0 | 3 | 4 | 5 | 6 => m1 >> 1,
            _ => 0,
        };
        let c1 = self.ops[2].compute_sample(c1_mod, am_atten);

        let c2_mod = match self.con {
            0 | 1 | 4 => m2 >> 1,
            2 => (m1 + m2) >> 1,
            3 => (c1_prev + m2) >> 1,
            5 => m1 >> 1,
            _ => 0,
        };
        let c2 = self.eval_c2(c2_mod, am_atten, use_noise, noise_bit);

        self.op1_hist[1] = self.op1_hist[0];
        self.op1_hist[0] = m1;
        self.op_c1_prev = c1;

        let out = match self.con {
            0 | 1 | 2 | 3 => c2,
            4 => c1 + c2,
            5 | 6 => m2 + c1 + c2,
            _ => m1 + m2 + c1 + c2,
        };

        let left = if (self.rl & 0x01) != 0 { out } else { 0 };
        let right = if (self.rl & 0x02) != 0 { out } else { 0 };
        (left, right)
    }

    #[inline]
    fn eval_c2(&mut self, modulation: i32, am_atten: u16, use_noise: bool, noise_bit: bool) -> i32 {
        if use_noise {
            self.ops[3].compute_noise_sample(noise_bit, am_atten)
        } else {
            self.ops[3].compute_sample(modulation, am_atten)
        }
    }
}

/// Yamaha YM2151 FM Operator Type-M (`$E90001` addr, `$E90003` data/status).
#[derive(Debug, Clone)]
pub struct Opm {
    addr_latch: u8,
    regs: [u8; 256],
    status: u8,
    busy_cycles: u32,
    sample_sub: u32,
    timer_a_sub: u32,
    timer_b_sub: u32,
    eg_div: u8,
    eg_counter: u32,
    lfo_counter: u32,
    lfo_phase: u8,
    lfo_pm: i32,
    prev_lfo_pm: i32,
    lfo_am: u16,
    pmd: i32,
    amd: u16,
    noise_lfsr: u32,
    noise_counter: u32,
    channels: [Channel; 8],
    active_mask: u8,
    audio_enabled: bool,
    synth_mode: OpmSynthMode,
    sample_clock: u64,
    samples: Vec<(i16, i16)>,
    events: Vec<OpmRegEvent>,
}

impl Default for Opm {
    fn default() -> Self {
        Self::new()
    }
}

impl Opm {
    pub const fn new() -> Self {
        Self {
            addr_latch: 0,
            regs: [0; 256],
            status: 0,
            busy_cycles: 0,
            sample_sub: 0,
            timer_a_sub: 0,
            timer_b_sub: 0,
            eg_div: 0,
            eg_counter: 0,
            lfo_counter: 0,
            lfo_phase: 0,
            lfo_pm: 0,
            prev_lfo_pm: 0,
            lfo_am: 0,
            pmd: 0,
            amd: 0,
            noise_lfsr: 1,
            noise_counter: 0,
            channels: [
                Channel::new(),
                Channel::new(),
                Channel::new(),
                Channel::new(),
                Channel::new(),
                Channel::new(),
                Channel::new(),
                Channel::new(),
            ],
            active_mask: 0,
            audio_enabled: false,
            synth_mode: OpmSynthMode::Inline,
            sample_clock: 0,
            samples: Vec::new(),
            events: Vec::new(),
        }
    }

    pub fn reset(&mut self) {
        let audio_enabled = self.audio_enabled;
        let synth_mode = self.synth_mode;
        *self = Self::new();
        self.audio_enabled = audio_enabled;
        self.synth_mode = synth_mode;
    }

    /// Enables or disables 62.5 kHz sample buffering / event recording (disabled in headless/test runs).
    pub fn set_audio_enabled(&mut self, enabled: bool) {
        self.audio_enabled = enabled;
        if !enabled {
            self.samples.clear();
            self.events.clear();
        }
    }

    /// Sets whether FM synthesis runs inline on the VM thread or is offloaded via register events.
    pub fn set_synth_mode(&mut self, mode: OpmSynthMode) {
        self.synth_mode = mode;
        self.samples.clear();
        self.events.clear();
    }

    /// Returns the configured synthesis mode.
    #[inline]
    pub const fn synth_mode(&self) -> OpmSynthMode {
        self.synth_mode
    }

    /// Drains accumulated 62.5 kHz stereo samples into `out` (used in `Inline` mode).
    pub fn take_samples(&mut self, out: &mut Vec<(i16, i16)>) {
        out.append(&mut self.samples);
    }

    /// Drains timestamped register write events into `out` and returns the current 62.5 kHz
    /// sample clock (used in `Offload` mode).
    pub fn take_events(&mut self, out: &mut Vec<OpmRegEvent>) -> u64 {
        out.append(&mut self.events);
        self.sample_clock
    }

    /// Writes an OPM register directly (used by an offloaded synthesis instance).
    #[inline]
    pub fn write_reg(&mut self, reg: u8, val: u8) {
        self.regs[reg as usize] = val;
        self.apply_register(reg, val);
    }

    /// Advances the chip by exactly one 62.5 kHz sample (including CSM Timer A if active) and
    /// returns the stereo PCM sample, skipping FM math when `active_mask == 0`.
    #[inline]
    pub fn next_sample(&mut self) -> (i16, i16) {
        let ctrl = self.regs[0x14];
        if (ctrl & 0x85) == 0x85 {
            let ta = ((self.regs[0x10] as u32) << 2) | ((self.regs[0x11] & 0x03) as u32);
            let period_10mhz = (1024 - ta).max(1) * CYCLES_PER_SAMPLE_10MHZ;
            self.timer_a_sub += CYCLES_PER_SAMPLE_10MHZ;
            if self.timer_a_sub >= period_10mhz {
                self.timer_a_sub -= period_10mhz;
                self.trigger_csm();
            }
        }
        if self.active_mask == 0 {
            return (0, 0);
        }
        self.step_sample()
    }

    /// Returns `true` if OPM `CT2` (register `$1B` bit 6) is high, which forces FDC Ready.
    #[inline]
    pub const fn ct2_fdc_enforced_ready(&self) -> bool {
        (self.regs[0x1B] & 0x40) != 0
    }

    /// Returns `true` if OPM is asserting its interrupt line (`GPIP3`).
    #[inline]
    pub const fn irq_active(&self) -> bool {
        (self.status & 0x03) != 0
    }

    /// Advances OPM timers, Busy flag, and (when audio is enabled) 62.5 kHz FM synthesis.
    #[inline]
    pub fn advance(&mut self, cycles_10mhz: u32) {
        if self.busy_cycles != 0 {
            self.busy_cycles = self.busy_cycles.saturating_sub(cycles_10mhz);
        }

        let ctrl = self.regs[0x14];
        if (ctrl & 0x03) == 0 && !self.audio_enabled {
            return;
        }
        // Timer A: 10-bit value from regs[0x10] (hi 8) and regs[0x11] (lo 2)
        let mut csm_trigger = false;
        if (ctrl & 0x01) != 0 {
            let ta = ((self.regs[0x10] as u32) << 2) | ((self.regs[0x11] & 0x03) as u32);
            let period_10mhz = (1024 - ta).max(1) * CYCLES_PER_SAMPLE_10MHZ;
            self.timer_a_sub = self.timer_a_sub.saturating_add(cycles_10mhz);
            if self.timer_a_sub >= period_10mhz {
                self.timer_a_sub -= period_10mhz;
                if self.timer_a_sub >= period_10mhz {
                    self.timer_a_sub %= period_10mhz;
                }
                if (ctrl & 0x04) != 0 {
                    self.status |= 0x01;
                }
                if (ctrl & 0x80) != 0 {
                    csm_trigger = true;
                }
            }
        }
        // Timer B: 8-bit value from regs[0x12]
        if (ctrl & 0x02) != 0 {
            let tb = self.regs[0x12] as u32;
            let period_10mhz = (256 - tb).max(1) * (CYCLES_PER_SAMPLE_10MHZ * 16);
            self.timer_b_sub = self.timer_b_sub.saturating_add(cycles_10mhz);
            if self.timer_b_sub >= period_10mhz {
                self.timer_b_sub -= period_10mhz;
                if self.timer_b_sub >= period_10mhz {
                    self.timer_b_sub %= period_10mhz;
                }
                if (ctrl & 0x08) != 0 {
                    self.status |= 0x02;
                }
            }
        }

        if !self.audio_enabled {
            return;
        }

        self.sample_sub = self.sample_sub.saturating_add(cycles_10mhz);
        if self.sample_sub < CYCLES_PER_SAMPLE_10MHZ {
            if csm_trigger && self.synth_mode == OpmSynthMode::Inline {
                self.trigger_csm();
            }
            return;
        }
        let sample_count = if self.sample_sub < CYCLES_PER_SAMPLE_10MHZ * 2 {
            self.sample_sub -= CYCLES_PER_SAMPLE_10MHZ;
            1
        } else {
            let n = self.sample_sub / CYCLES_PER_SAMPLE_10MHZ;
            self.sample_sub %= CYCLES_PER_SAMPLE_10MHZ;
            n
        };

        if self.synth_mode == OpmSynthMode::Offload {
            self.sample_clock += sample_count as u64;
            return;
        }

        if csm_trigger {
            self.trigger_csm();
        }

        self.sample_clock += sample_count as u64;
        if self.active_mask == 0 {
            self.samples.resize(self.samples.len() + sample_count as usize, (0, 0));
            return;
        }

        for i in 0..sample_count as usize {
            let sample = self.step_sample();
            self.samples.push(sample);
            if self.active_mask == 0 {
                let remaining = (sample_count as usize) - 1 - i;
                if remaining > 0 {
                    self.samples.resize(self.samples.len() + remaining, (0, 0));
                }
                break;
            }
        }
    }

    fn trigger_csm(&mut self) {
        for (ch_idx, ch) in self.channels.iter_mut().enumerate() {
            let pm = Self::channel_pm_offset(ch.pms, self.lfo_pm);
            for op in &mut ch.ops {
                op.csm_key_on(ch.kc, ch.kf, pm);
            }
            if ch.is_active() {
                self.active_mask |= 1 << ch_idx;
            }
        }
    }

    #[inline]
    fn channel_pm_offset(pms: u8, lfo_pm: i32) -> i32 {
        if pms == 0 || lfo_pm == 0 {
            return 0;
        }
        let lfo = lfo_pm.unsigned_abs().min(127) as i32;
        let mut top = (lfo >> 4) & 7;
        if pms != 7 {
            top >>= 1;
        }
        let t = (((top & 6) == 6 && pms == 7) || ((top & 3) == 3 && pms >= 6)) as i32;
        let mut out = top + ((top >> 2) & 1) + t;
        out = out * 2 + ((lfo >> 4) & 1);
        if pms == 7 {
            out >>= 1;
        }
        out &= 15;
        out = (lfo & 15) + out * 16;
        let mag = match pms {
            1 => (out >> 5) & 3,
            2 => (out >> 4) & 7,
            3 => (out >> 3) & 15,
            4 => (out >> 2) & 31,
            5 => (out >> 1) & 63,
            6 => (out & 255) << 1,
            _ => (out & 255) << 2,
        };
        if lfo_pm < 0 { -mag } else { mag }
    }

    fn step_lfo_and_noise(&mut self) {
        // 17-bit LFSR noise (taps bit 0 ^ bit 3), clocked at 125 kHz (2x per 62.5 kHz sample)
        let nfrq = (self.regs[0x0F] & 0x1F) as u32;
        let noise_period = (32 - nfrq).max(1);
        for _ in 0..2 {
            self.noise_counter += 1;
            if self.noise_counter >= noise_period {
                self.noise_counter = 0;
                let bit = ((self.noise_lfsr ^ (self.noise_lfsr >> 3)) & 1) << 16;
                self.noise_lfsr = (self.noise_lfsr >> 1) | bit;
            }
        }

        // LFO
        if (self.regs[0x01] & 0x02) != 0 {
            self.lfo_phase = 0;
            self.lfo_counter = 0;
            self.lfo_pm = 0;
            self.lfo_am = 0;
            return;
        } else {
            let lfrq = self.regs[0x18];
            let shift = (lfrq >> 4) as u32;
            let delta = (16 | (lfrq & 0x0F)) as u32;
            self.lfo_counter += delta << shift;
            if self.lfo_counter >= 0x40_0000 {
                let steps = self.lfo_counter >> 22;
                self.lfo_counter &= 0x3F_FFFF;
                self.lfo_phase = self.lfo_phase.wrapping_add(steps as u8);
            }
        }

        let wave = self.regs[0x1B] & 0x03;
        let (pm_wave, am_wave): (i32, u32) = match wave {
            0 => {
                // Sawtooth: PM 0..+127 then -128..-1; AM 255..0
                let pm = (self.lfo_phase as i8) as i32;
                let am = (self.lfo_phase ^ 0xFF) as u32;
                (pm, am)
            }
            1 => {
                // Square
                let hi = (self.lfo_phase & 0x80) != 0;
                (if hi { -128 } else { 127 }, if hi { 0 } else { 255 })
            }
            2 => {
                // Triangle: PM 0..+127..0..-127..0; AM 255..0..255
                let p = self.lfo_phase;
                let pm = match p >> 6 {
                    0 => ((p & 0x3F) as i32) * 2,
                    1 => 127 - ((p & 0x3F) as i32) * 2,
                    2 => -(((p & 0x3F) as i32) * 2),
                    _ => -127 + ((p & 0x3F) as i32) * 2,
                };
                let am = if (p & 0x80) == 0 {
                    255 - (p as u32) * 2
                } else {
                    ((p & 0x7F) as u32) * 2
                };
                (pm, am)
            }
            _ => {
                // Noise
                let n = (self.noise_lfsr & 0xFF) as u32;
                (((n as i8) as i32), n)
            }
        };

        self.lfo_pm = (pm_wave * self.pmd) >> 7;
        self.lfo_am = ((am_wave * (self.amd as u32)) >> 5) as u16;
    }

    fn step_sample(&mut self) -> (i16, i16) {
        self.step_lfo_and_noise();
        let pm_changed = self.lfo_pm != self.prev_lfo_pm;
        self.prev_lfo_pm = self.lfo_pm;

        self.eg_div += 1;
        let run_eg = if self.eg_div >= 3 {
            self.eg_div = 0;
            self.eg_counter = self.eg_counter.wrapping_add(1);
            true
        } else {
            false
        };

        let noise_en = (self.regs[0x0F] & 0x80) != 0;
        let noise_bit = (self.noise_lfsr & 1) != 0;

        let mut left_acc: i32 = 0;
        let mut right_acc: i32 = 0;
        let mut new_active: u8 = 0;

        for ch_idx in 0..8 {
            if (self.active_mask & (1 << ch_idx)) == 0 {
                continue;
            }
            let ch = &mut self.channels[ch_idx];
            if pm_changed {
                ch.update_pitch(self.lfo_pm);
            }
            if run_eg {
                ch.step_eg(self.eg_counter);
            }
            let (l, r) = ch.eval(self.lfo_am, noise_en && ch_idx == 7, noise_bit);
            left_acc += l;
            right_acc += r;
            if ch.is_active() {
                new_active |= 1 << ch_idx;
            }
        }
        self.active_mask = new_active;

        // Clamp 8-channel sum into i16 (matches YM2151 / YM3012 1x DAC range).
        let left = left_acc.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
        let right = right_acc.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
        (left, right)
    }

    pub fn read_u8(&self, addr: u32) -> Result<u8, BusFault> {
        let a = addr & 0x00FF_FFFF;
        if (a & 1) == 0 {
            return Err(BusFault::BusError {
                addr: a,
                is_write: false,
            });
        }
        if (a & 3) == 3 {
            let busy = if self.busy_cycles != 0 { 0x80 } else { 0 };
            Ok((self.status & 0x03) | busy)
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
            self.addr_latch = val;
        } else if (a & 3) == 3 {
            let r = self.addr_latch as usize;
            self.regs[r] = val;
            self.busy_cycles = BUSY_CYCLES_10MHZ;
            if self.synth_mode == OpmSynthMode::Offload {
                if self.audio_enabled {
                    self.events.push(OpmRegEvent {
                        sample_time: self.sample_clock,
                        reg: r as u8,
                        val,
                    });
                }
                if r == 0x14 {
                    if (val & 0x10) != 0 {
                        self.status &= !0x01;
                    }
                    if (val & 0x20) != 0 {
                        self.status &= !0x02;
                    }
                }
            } else {
                self.apply_register(r as u8, val);
            }
        }
        Ok(())
    }

    fn apply_register(&mut self, reg: u8, val: u8) {
        match reg {
            0x01 => {
                if (val & 0x02) != 0 {
                    self.lfo_phase = 0;
                    self.lfo_counter = 0;
                }
            }
            0x08 => {
                let ch_idx = (val & 0x07) as usize;
                let ch = &mut self.channels[ch_idx];
                let pm = Self::channel_pm_offset(ch.pms, self.lfo_pm);
                // Bits 3..6 = M1(0), C1(2), M2(1), C2(3)
                const KON_SLOT: [usize; 4] = [0, 2, 1, 3];
                for (bit, &slot) in KON_SLOT.iter().enumerate() {
                    if (val & (1 << (3 + bit))) != 0 {
                        ch.ops[slot].key_on(ch.kc, ch.kf, pm);
                    } else {
                        ch.ops[slot].key_off();
                    }
                }
                if ch.is_active() {
                    self.active_mask |= 1 << ch_idx;
                }
            }
            0x14 => {
                if (val & 0x10) != 0 {
                    self.status &= !0x01;
                }
                if (val & 0x20) != 0 {
                    self.status &= !0x02;
                }
            }
            0x19 => {
                if (val & 0x80) != 0 {
                    self.pmd = (val & 0x7F) as i32;
                } else {
                    self.amd = (val & 0x7F) as u16;
                }
            }
            0x20..=0x27 => {
                let ch = &mut self.channels[(reg & 0x07) as usize];
                ch.rl = (val >> 6) & 0x03;
                ch.fl = (val >> 3) & 0x07;
                ch.con = val & 0x07;
            }
            0x28..=0x2F => {
                let ch = &mut self.channels[(reg & 0x07) as usize];
                ch.kc = val & 0x7F;
                ch.update_pitch(self.lfo_pm);
            }
            0x30..=0x37 => {
                let ch = &mut self.channels[(reg & 0x07) as usize];
                ch.kf = val & 0xFC;
                ch.update_pitch(self.lfo_pm);
            }
            0x38..=0x3F => {
                let ch = &mut self.channels[(reg & 0x07) as usize];
                ch.pms = (val >> 4) & 0x07;
                ch.ams = val & 0x03;
                ch.update_pitch(self.lfo_pm);
            }
            0x40..=0x5F => {
                let ch_idx = (reg & 0x07) as usize;
                let slot = ((reg >> 3) & 0x03) as usize;
                let ch = &mut self.channels[ch_idx];
                let op = &mut ch.ops[slot];
                op.dt1 = (val >> 4) & 0x07;
                op.mul = val & 0x0F;
                let pm = Self::channel_pm_offset(ch.pms, self.lfo_pm);
                op.update_phase_inc(ch.kc, ch.kf, pm);
            }
            0x60..=0x7F => {
                let ch_idx = (reg & 0x07) as usize;
                let slot = ((reg >> 3) & 0x03) as usize;
                self.channels[ch_idx].ops[slot].tl = val & 0x7F;
            }
            0x80..=0x9F => {
                let ch_idx = (reg & 0x07) as usize;
                let slot = ((reg >> 3) & 0x03) as usize;
                let op = &mut self.channels[ch_idx].ops[slot];
                op.ks = (val >> 6) & 0x03;
                op.ar = val & 0x1F;
            }
            0xA0..=0xBF => {
                let ch_idx = (reg & 0x07) as usize;
                let slot = ((reg >> 3) & 0x03) as usize;
                let op = &mut self.channels[ch_idx].ops[slot];
                op.ams_en = (val & 0x80) != 0;
                op.d1r = val & 0x1F;
            }
            0xC0..=0xDF => {
                let ch_idx = (reg & 0x07) as usize;
                let slot = ((reg >> 3) & 0x03) as usize;
                let ch = &mut self.channels[ch_idx];
                let op = &mut ch.ops[slot];
                op.dt2 = (val >> 6) & 0x03;
                op.d2r = val & 0x1F;
                let pm = Self::channel_pm_offset(ch.pms, self.lfo_pm);
                op.update_phase_inc(ch.kc, ch.kf, pm);
            }
            0xE0..=0xFF => {
                let ch_idx = (reg & 0x07) as usize;
                let slot = ((reg >> 3) & 0x03) as usize;
                let op = &mut self.channels[ch_idx].ops[slot];
                let d1l = (val >> 4) & 0x0F;
                op.d1l = if d1l == 15 { 0x3E0 } else { (d1l as u16) << 5 };
                op.rr = val & 0x0F;
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_reg(opm: &mut Opm, reg: u8, val: u8) {
        opm.write_u8(0xE90001, reg).unwrap();
        opm.write_u8(0xE90003, val).unwrap();
    }

    #[test]
    fn timer_a_and_b_irq_and_busy_behave_as_hardware() {
        let mut opm = Opm::new();
        write_reg(&mut opm, 0x10, 0xFF);
        write_reg(&mut opm, 0x11, 0x03); // TA = 1023 -> 1 sample = 160 cycles
        assert_ne!(opm.read_u8(0xE90003).unwrap() & 0x80, 0, "busy after write");
        opm.advance(160);
        assert_eq!(opm.read_u8(0xE90003).unwrap() & 0x80, 0, "busy clears");

        write_reg(&mut opm, 0x14, 0x05); // Run TA + IRQ enable
        opm.advance(160);
        assert!(opm.irq_active());
        assert_eq!(opm.read_u8(0xE90003).unwrap() & 0x03, 0x01);

        write_reg(&mut opm, 0x14, 0x10); // Reset TA flag
        assert!(!opm.irq_active());
    }

    #[test]
    fn key_on_generates_stereo_waveform_and_skips_when_silent() {
        let mut opm = Opm::new();
        opm.set_audio_enabled(true);

        // Silent before key-on
        opm.advance(160 * 16);
        let mut out = Vec::new();
        opm.take_samples(&mut out);
        assert_eq!(out.len(), 16);
        assert!(out.iter().all(|&s| s == (0, 0)));
        assert_eq!(opm.active_mask, 0);

        // Program ch 0 algorithm 7 (all carriers), pan both L+R, A440-ish KC, fast AR, sustain
        write_reg(&mut opm, 0x20, 0xC7); // RL=both, FL=0, CON=7
        write_reg(&mut opm, 0x28, 0x4A); // Oct 4, A
        write_reg(&mut opm, 0x40, 0x01); // M1 MUL=1
        write_reg(&mut opm, 0x60, 0x00); // M1 TL=0
        write_reg(&mut opm, 0x80, 0x1F); // M1 AR=31 (instant attack)
        write_reg(&mut opm, 0xE0, 0x0F); // M1 D1L=0, RR=15
        write_reg(&mut opm, 0x08, 0x08); // Key-on M1 on ch 0
        assert_ne!(opm.active_mask, 0);

        out.clear();
        opm.advance(160 * 128);
        opm.take_samples(&mut out);
        assert_eq!(out.len(), 128);
        assert!(out.iter().any(|&(l, r)| l != 0 && l == r), "both L and R must sound");

        // Key-off and let fast RR=15 decay to silence -> active_mask returns to 0
        write_reg(&mut opm, 0x08, 0x00);
        opm.advance(160 * 2000);
        out.clear();
        opm.take_samples(&mut out);
        assert_eq!(opm.active_mask, 0, "channel deactivates after release");
        assert_eq!(*out.last().unwrap(), (0, 0));
    }

    #[test]
    fn panning_routes_left_and_right_independently() {
        let mut opm = Opm::new();
        opm.set_audio_enabled(true);
        write_reg(&mut opm, 0x20, 0x47); // Left only (bit 6 = 1, bit 7 = 0 in reg -> rl=1)
        write_reg(&mut opm, 0x28, 0x4A);
        write_reg(&mut opm, 0x40, 0x01);
        write_reg(&mut opm, 0x80, 0x1F);
        write_reg(&mut opm, 0x08, 0x08);

        let mut out = Vec::new();
        opm.advance(160 * 64);
        opm.take_samples(&mut out);
        assert!(out.iter().any(|&(l, _)| l != 0));
        assert!(out.iter().all(|&(_, r)| r == 0));

        write_reg(&mut opm, 0x20, 0x87); // Right only
        out.clear();
        opm.advance(160 * 64);
        opm.take_samples(&mut out);
        assert!(out.iter().all(|&(l, _)| l == 0));
        assert!(out.iter().any(|&(_, r)| r != 0));
    }

    #[test]
    fn noise_lfsr_drives_ch7_c2_when_enabled() {
        let mut opm = Opm::new();
        opm.set_audio_enabled(true);
        write_reg(&mut opm, 0x0F, 0x80 | 0x1F); // Noise enable, max rate
        write_reg(&mut opm, 0x27, 0xC7); // Ch 7 both L+R, CON=7
        write_reg(&mut opm, 0x7F, 0x00); // Ch 7 C2 TL=0
        write_reg(&mut opm, 0x9F, 0x1F); // Ch 7 C2 AR=31
        write_reg(&mut opm, 0x08, 0x47); // Key-on C2 (bit 6) on ch 7

        let mut out = Vec::new();
        opm.advance(160 * 64);
        opm.take_samples(&mut out);
        assert!(out.iter().any(|&(l, _)| l > 0) && out.iter().any(|&(l, _)| l < 0));
    }

    #[test]
    fn exponential_attack_ramps_gradually_and_decays_to_release() {
        let mut opm = Opm::new();
        opm.set_audio_enabled(true);
        write_reg(&mut opm, 0x20, 0xC7); // Ch 0 L+R, CON=7
        write_reg(&mut opm, 0x28, 0x4A); // KC = A4
        write_reg(&mut opm, 0x40, 0x01); // M1 MUL=1
        write_reg(&mut opm, 0x60, 0x00); // M1 TL=0
        write_reg(&mut opm, 0x80, 12);   // M1 AR=12 (gradual attack)
        write_reg(&mut opm, 0xA0, 16);   // M1 D1R=16
        write_reg(&mut opm, 0xE0, 0x48); // M1 D1L=4, RR=8
        write_reg(&mut opm, 0x08, 0x08); // Key-on M1

        // After 300 samples (~100 EG ticks), eg_level should have decreased partially, not jumped to 0.
        opm.advance(160 * 300);
        let eg_early = opm.channels[0].ops[0].eg_level;
        assert!(eg_early > 0 && eg_early < EG_SILENT, "eg_early={eg_early}");
        assert_eq!(opm.channels[0].ops[0].eg_phase, EgPhase::Attack);

        // Advance enough to reach Decay / Sustain.
        opm.advance(160 * 16384);
        assert!(matches!(
            opm.channels[0].ops[0].eg_phase,
            EgPhase::Decay | EgPhase::Sustain
        ));

        // Key-off transitions to Release and eventually silences active_mask.
        write_reg(&mut opm, 0x08, 0x00);
        assert_eq!(opm.channels[0].ops[0].eg_phase, EgPhase::Release);
        opm.advance(160 * 62500);
        assert_eq!(opm.active_mask & 1, 0);
    }

    #[test]
    fn feedback_and_all_algorithms_produce_valid_output() {
        for con in 0..=7u8 {
            let mut opm = Opm::new();
            opm.set_audio_enabled(true);
            write_reg(&mut opm, 0x20, 0xC0 | (7 << 3) | con); // L+R, FB=7, CON=con
            write_reg(&mut opm, 0x28, 0x4A);
            for slot_base in [0x40u8, 0x48, 0x50, 0x58] {
                write_reg(&mut opm, slot_base, 0x01);        // MUL=1
                write_reg(&mut opm, slot_base + 0x20, 0x00); // TL=0
                write_reg(&mut opm, slot_base + 0x40, 0x1F); // AR=31
            }
            write_reg(&mut opm, 0x08, 0x78); // Key-on all 4 slots on ch 0

            let mut out = Vec::new();
            opm.advance(160 * 128);
            opm.take_samples(&mut out);
            assert!(
                out.iter().any(|&(l, r)| l != 0 && r != 0),
                "CON={con} produced silence"
            );
        }
    }

    #[test]
    fn lfo_pm_and_am_modulate_and_reset_cleanly() {
        let mut opm = Opm::new();
        opm.set_audio_enabled(true);
        write_reg(&mut opm, 0x18, 0xFF); // LFRQ max
        write_reg(&mut opm, 0x19, 0x80 | 127); // PMD=127
        write_reg(&mut opm, 0x19, 127);        // AMD=127
        write_reg(&mut opm, 0x1B, 0x02); // Triangle LFO
        write_reg(&mut opm, 0x20, 0xC7); // Ch 0 L+R, CON=7
        write_reg(&mut opm, 0x28, 0x4A);
        write_reg(&mut opm, 0x38, 0x73); // PMS=7, AMS=3
        write_reg(&mut opm, 0x40, 0x01); // MUL=1
        write_reg(&mut opm, 0x60, 0x00); // TL=0
        write_reg(&mut opm, 0x80, 0x1F); // AR=31
        write_reg(&mut opm, 0xA0, 0x80); // AMON=1
        write_reg(&mut opm, 0x08, 0x08); // Key-on M1

        opm.advance(160 * 64);
        assert!(opm.lfo_pm != 0 || opm.lfo_am != 0);

        // Reset LFO via reg $01 bit 1 and step; PM/AM should return to 0 and update channel pitch.
        let base_inc = {
            let mut op = opm.channels[0].ops[0].clone();
            op.update_phase_inc(opm.channels[0].kc, opm.channels[0].kf, 0);
            op.phase_inc
        };
        write_reg(&mut opm, 0x01, 0x02);
        opm.advance(160 * 4);
        assert_eq!(opm.lfo_pm, 0);
        assert_eq!(opm.channels[0].ops[0].phase_inc, base_inc);
    }

    #[test]
    fn a4_kc_produces_expected_4mhz_frequency() {
        let mut opm = Opm::new();
        opm.set_audio_enabled(true);
        write_reg(&mut opm, 0x20, 0xC7); // Ch 0 L+R, CON=7
        write_reg(&mut opm, 0x28, 0x4A); // KC = A4
        write_reg(&mut opm, 0x40, 0x01); // MUL=1
        write_reg(&mut opm, 0x60, 0x00); // TL=0
        write_reg(&mut opm, 0x80, 0x1F); // AR=31
        write_reg(&mut opm, 0x08, 0x08); // Key-on M1

        let mut out = Vec::new();
        opm.advance(160 * OPM_SAMPLE_RATE); // 1 second at 62,500 Hz
        opm.take_samples(&mut out);

        // Count rising zero-crossings: 440 * (4.000 / 3.579545) = ~491.7 Hz.
        let cycles = out
            .windows(2)
            .filter(|w| w[0].0 < 0 && w[1].0 >= 0)
            .count();
        assert!((488..=495).contains(&cycles), "expected ~492 Hz, got {cycles} Hz");
    }

    #[test]
    fn offload_events_match_inline_samples_bit_for_bit() {
        let mut inline = Opm::new();
        inline.set_audio_enabled(true);
        inline.set_synth_mode(OpmSynthMode::Inline);

        let mut offload = Opm::new();
        offload.set_audio_enabled(true);
        offload.set_synth_mode(OpmSynthMode::Offload);

        for opm in [&mut inline, &mut offload] {
            write_reg(opm, 0x20, 0xC4); // L+R, CON=4
            write_reg(opm, 0x28, 0x4A);
            write_reg(opm, 0x40, 0x01);
            write_reg(opm, 0x50, 0x01);
            write_reg(opm, 0x60, 0x18);
            write_reg(opm, 0x70, 0x00);
            write_reg(opm, 0x80, 0x1F);
            write_reg(opm, 0x90, 0x1F);
            write_reg(opm, 0x08, 0x78); // Key-on
        }

        inline.advance(160 * 100);
        offload.advance(160 * 100);

        write_reg(&mut inline, 0x28, 0x4E); // Pitch change mid-stream
        write_reg(&mut offload, 0x28, 0x4E);

        inline.advance(160 * 100);
        let target_clock = offload.advance(160 * 100);
        let _ = target_clock;

        let mut inline_samples = Vec::new();
        inline.take_samples(&mut inline_samples);

        let mut events = Vec::new();
        let total_samples = offload.take_events(&mut events);
        assert_eq!(total_samples as usize, inline_samples.len());

        let mut synth = Opm::new();
        let mut ev_idx = 0;
        let mut offload_samples = Vec::with_capacity(total_samples as usize);
        for t in 0..total_samples {
            while ev_idx < events.len() && events[ev_idx].sample_time <= t {
                synth.write_reg(events[ev_idx].reg, events[ev_idx].val);
                ev_idx += 1;
            }
            offload_samples.push(synth.next_sample());
        }
        assert_eq!(inline_samples, offload_samples);
    }

    #[test]
    fn operator_peak_and_lfo_am_match_nuked_scale() {
        let mut opm = Opm::new();
        opm.set_audio_enabled(true);
        write_reg(&mut opm, 0x20, 0xC7); // Ch 0 L+R, CON=7
        write_reg(&mut opm, 0x28, 0x4A); // KC = A4
        write_reg(&mut opm, 0x40, 0x01); // M1 MUL=1
        write_reg(&mut opm, 0x60, 0x00); // M1 TL=0
        write_reg(&mut opm, 0x80, 0x1F); // M1 AR=31 (instant)
        write_reg(&mut opm, 0x08, 0x08); // Key-on M1

        let mut out = Vec::new();
        opm.advance(160 * 256);
        opm.take_samples(&mut out);
        let peak = out.iter().map(|&(l, _)| l.abs()).max().unwrap();
        // Nuked-OPM peak operator output: exprom[0] << 2 = 0x7fa << 2 = 8168.
        assert!((8140..=8168).contains(&peak), "unexpected peak {peak}");

        // LFO max AM attenuation at AMD=127, AMS=3 matches Nuked-OPM (lfo_am_lock=253, x4=1012).
        write_reg(&mut opm, 0x18, 0xFF);
        write_reg(&mut opm, 0x19, 127); // AMD=127
        write_reg(&mut opm, 0x1B, 0x01); // Square LFO
        opm.lfo_phase = 0;
        opm.step_lfo_and_noise();
        assert!((1000..=1016).contains(&opm.lfo_am), "lfo_am={}", opm.lfo_am);
        assert!(opm.lfo_counter < 0x40_0000, "lfo_counter must not accumulate past 0x40_0000");
    }

    #[test]
    fn pitch_update_matches_channel_pm_offset_and_noise_clocks_at_2x() {
        let mut opm = Opm::new();
        opm.set_audio_enabled(true);
        write_reg(&mut opm, 0x28, 0x4A);
        write_reg(&mut opm, 0x38, 0x40); // PMS=4
        write_reg(&mut opm, 0x40, 0x01); // M1 MUL=1

        opm.channels[0].update_pitch(64);
        let inc_via_update = opm.channels[0].ops[0].phase_inc;
        let pm = Opm::channel_pm_offset(4, 64);
        let mut op_ref = opm.channels[0].ops[0].clone();
        op_ref.update_phase_inc(opm.channels[0].kc, opm.channels[0].kf, pm);
        assert_eq!(inc_via_update, op_ref.phase_inc);

        // At NFRQ=31 (period=1), 1 sample clocks LFSR twice (state 1 -> 0x10000 -> 0x08000).
        write_reg(&mut opm, 0x0F, 0x9F);
        opm.noise_lfsr = 1;
        opm.noise_counter = 0;
        opm.step_lfo_and_noise();
        assert_eq!(opm.noise_lfsr, 0x08000);
    }

    #[test]
    fn key_on_bits_map_c1_to_bit4_and_m2_to_bit5() {
        // YM2151 reg $08: bit3=M1($40), bit4=C1($50), bit5=M2($48), bit6=C2($58).
        for (bit, slot_reg_off) in [(3u8, 0x00u8), (4, 0x10), (5, 0x08), (6, 0x18)] {
            let mut opm = Opm::new();
            opm.set_audio_enabled(true);
            write_reg(&mut opm, 0x20, 0xC7); // CON=7 (all carriers)
            write_reg(&mut opm, 0x28, 0x4A);
            write_reg(&mut opm, 0x40 + slot_reg_off, 0x01); // MUL=1
            write_reg(&mut opm, 0x60 + slot_reg_off, 0x00); // TL=0
            write_reg(&mut opm, 0x80 + slot_reg_off, 0x1F); // AR=31
            write_reg(&mut opm, 0x08, 1 << bit);

            let mut peak = 0i16;
            for _ in 0..128 {
                peak = peak.max(opm.next_sample().0.abs());
            }
            assert!(
                (8140..=8168).contains(&peak),
                "KON bit {bit} (slot reg +0x{slot_reg_off:02X}) peak={peak}"
            );
        }
    }

    #[test]
    fn con2_pipeline_and_logsin_match_nuked_reference() {
        // Minimal repro from differential fuzzing (CON=2, KC=0x5E, 1-sample C1->M2 delay + logsinrom).
        let mut opm = Opm::new();
        opm.set_audio_enabled(true);
        let regs: &[(u8, u8)] = &[
            (0x20, 0xC2), (0x28, 0x5E), (0x30, 0x00),
            (0x40, 3), (0x60, 15), (0x80, 31), (0xE0, 15),
            (0x48, 3), (0x68, 19), (0x88, 31), (0xE8, 15),
            (0x50, 2), (0x70, 8),  (0x90, 31), (0xF0, 15),
            (0x58, 3), (0x78, 14), (0x98, 31), (0xF8, 15),
            (0x08, 0x78),
        ];
        for &(r, v) in regs {
            write_reg(&mut opm, r, v);
        }
        let samples: Vec<i16> = (0..6).map(|_| opm.next_sample().1).collect();
        // Nuked-OPM op_out[5] for C2 on samples 0..6: [81, -2396, 740, 798, 2410, -1658]
        assert_eq!(samples, vec![81, -2396, 740, 798, 2410, -1658]);
    }

    #[test]
    fn lfo_waveforms_and_noise_amplitude_match_nuked() {
        // Triangle LFO starts at max AM attenuation (~1012 at AMD=127) and 0 PM after reset.
        let mut opm = Opm::new();
        opm.set_audio_enabled(true);
        write_reg(&mut opm, 0x01, 0x02);
        write_reg(&mut opm, 0x18, 0x80); // Slow LFO
        write_reg(&mut opm, 0x19, 0x80 | 127); // PMD=127
        write_reg(&mut opm, 0x19, 127);        // AMD=127
        write_reg(&mut opm, 0x1B, 0x02); // Triangle
        write_reg(&mut opm, 0x01, 0x00);
        opm.step_lfo_and_noise();
        assert_eq!(opm.lfo_pm, 0);
        assert!((1000..=1016).contains(&opm.lfo_am), "tri initial lfo_am={}", opm.lfo_am);

        // Noise Ch 7 C2 linear peak is <= 2046 (Nuked-OPM: ±2044).
        let mut nopm = Opm::new();
        nopm.set_audio_enabled(true);
        write_reg(&mut nopm, 0x0F, 0x9F);
        write_reg(&mut nopm, 0x27, 0xC7);
        write_reg(&mut nopm, 0x7F, 0x00);
        write_reg(&mut nopm, 0x9F, 0x1F);
        write_reg(&mut nopm, 0x08, 0x47);
        let peak = (0..64).map(|_| nopm.next_sample().0.abs()).max().unwrap();
        assert!((2040..=2046).contains(&peak), "noise peak={peak}");
    }
}
