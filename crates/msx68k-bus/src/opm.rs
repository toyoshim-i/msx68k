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

/// Envelope phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EgPhase {
    Off,
    Attack,
    Decay,
    Sustain,
    Release,
}

/// 1/4-wave log-sin table: `round(-log2(sin((i + 0.5) * pi / 512)) * 256)`.
const LOG_SIN_TABLE: [u16; 256] = [
    2137, 1731, 1543, 1419, 1326, 1252, 1190, 1137, 1091, 1050, 1013, 979, 949, 920, 894, 869,
    846, 825, 804, 785, 767, 749, 732, 717, 701, 687, 672, 659, 646, 633, 621, 609,
    598, 587, 576, 566, 556, 546, 536, 527, 518, 509, 501, 492, 484, 476, 468, 461,
    453, 446, 439, 432, 425, 418, 411, 405, 399, 392, 386, 380, 375, 369, 363, 358,
    352, 347, 341, 336, 331, 326, 321, 316, 311, 307, 302, 297, 293, 288, 284, 280,
    275, 271, 267, 263, 259, 255, 251, 247, 243, 239, 236, 232, 228, 225, 221, 218,
    214, 211, 207, 204, 201, 198, 194, 191, 188, 185, 182, 179, 176, 173, 170, 167,
    165, 162, 159, 156, 154, 151, 148, 146, 143, 141, 138, 136, 133, 131, 128, 126,
    124, 121, 119, 117, 115, 112, 110, 108, 106, 104, 102, 100, 98, 96, 94, 92,
    90, 88, 86, 84, 82, 80, 79, 77, 75, 73, 72, 70, 68, 67, 65, 63,
    62, 60, 59, 57, 56, 54, 53, 51, 50, 48, 47, 46, 44, 43, 42, 40,
    39, 38, 37, 35, 34, 33, 32, 31, 30, 29, 27, 26, 25, 24, 23, 22,
    21, 20, 19, 19, 18, 17, 16, 15, 14, 14, 13, 12, 11, 11, 10, 9,
    9, 8, 7, 7, 6, 6, 5, 5, 4, 4, 3, 3, 3, 2, 2, 2,
    1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
];

/// Exponential lookup table: `round(2^(11 - i / 256))` (`1025..=2048`).
const EXP_TABLE: [u16; 256] = [
    2048, 2042, 2037, 2031, 2026, 2020, 2015, 2010, 2004, 1999, 1993, 1988, 1983, 1977, 1972, 1967,
    1961, 1956, 1951, 1945, 1940, 1935, 1930, 1924, 1919, 1914, 1909, 1904, 1899, 1893, 1888, 1883,
    1878, 1873, 1868, 1863, 1858, 1853, 1848, 1843, 1838, 1833, 1828, 1823, 1818, 1813, 1808, 1804,
    1799, 1794, 1789, 1784, 1779, 1775, 1770, 1765, 1760, 1756, 1751, 1746, 1741, 1737, 1732, 1727,
    1723, 1718, 1713, 1709, 1704, 1699, 1695, 1690, 1686, 1681, 1677, 1672, 1668, 1663, 1659, 1654,
    1650, 1645, 1641, 1636, 1632, 1627, 1623, 1619, 1614, 1610, 1606, 1601, 1597, 1593, 1588, 1584,
    1580, 1575, 1571, 1567, 1563, 1558, 1554, 1550, 1546, 1542, 1538, 1533, 1529, 1525, 1521, 1517,
    1513, 1509, 1505, 1501, 1497, 1493, 1488, 1484, 1480, 1476, 1472, 1468, 1465, 1461, 1457, 1453,
    1449, 1445, 1441, 1437, 1433, 1429, 1425, 1422, 1418, 1414, 1410, 1406, 1403, 1399, 1395, 1391,
    1387, 1384, 1380, 1376, 1373, 1369, 1365, 1361, 1358, 1354, 1350, 1347, 1343, 1340, 1336, 1332,
    1329, 1325, 1322, 1318, 1314, 1311, 1307, 1304, 1300, 1297, 1293, 1290, 1286, 1283, 1279, 1276,
    1272, 1269, 1266, 1262, 1259, 1255, 1252, 1249, 1245, 1242, 1238, 1235, 1232, 1228, 1225, 1222,
    1219, 1215, 1212, 1209, 1205, 1202, 1199, 1196, 1192, 1189, 1186, 1183, 1180, 1176, 1173, 1170,
    1167, 1164, 1161, 1158, 1154, 1151, 1148, 1145, 1142, 1139, 1136, 1133, 1130, 1127, 1124, 1121,
    1118, 1115, 1112, 1109, 1106, 1103, 1100, 1097, 1094, 1091, 1088, 1085, 1082, 1079, 1076, 1073,
    1070, 1067, 1065, 1062, 1059, 1056, 1053, 1050, 1047, 1045, 1042, 1039, 1036, 1033, 1031, 1028,
];

/// YM2151 octave-7 base phase increments for the 768 steps (`12 semitones * 64 KF`).
///
/// Computed against the OPM's internal 3.579545 MHz / 64 (55,930.39 Hz) tuning reference so that
/// X68000's 4.000 MHz clock produces the exact hardware pitch offset drivers expect.
const fn build_phase_inc_table() -> [u32; 768] {
    // Octave 7 C#..C base f-numbers at 3.579545 MHz / 64, pre-scaled for 20-bit phase.
    // C#8 = 4434.92 Hz -> inc_oct7 = round(4434.92 * 1048576 / 55930.39) = 83148.
    // Approximated in const fn with exact 2^(k/768) polynomial / rational steps.
    let mut table = [0u32; 768];
    // Base C#8 (index 0) = 83148, C9 (index 768) = 166296.
    // Using 16.16 fixed-point iterative 2^(1/768) = 1.0009029098 -> 65595 / 65536 (very close: 65595.17).
    // Even better: exact 12-semitone base table (from 440 Hz * 2^((n-9)/12)) + 64-step linear/quad.
    const SEMI_BASE: [u32; 13] = [
        83148,  // C#8
        88092,  // D8
        93330,  // D#8
        98880,  // E8
        104760, // F8
        110989, // F#8
        117589, // G8
        124581, // G#8
        131989, // A8 (131989 / 16 = 8249.3 = A4)
        139837, // A#8
        148152, // B8
        156962, // C9
        166296, // C#9
    ];
    let mut s = 0;
    while s < 12 {
        let a = SEMI_BASE[s] as u64;
        let b = SEMI_BASE[s + 1] as u64;
        let mut kf = 0;
        while kf < 64 {
            // Linear interpolation within 1 semitone (max error < 0.02% = 0.3 cents).
            let val = a + ((b - a) * (kf as u64) + 32) / 64;
            table[s * 64 + kf] = val as u32;
            kf += 1;
        }
        s += 1;
    }
    table
}

const PHASE_INC_TABLE: [u32; 768] = build_phase_inc_table();

/// DT2 offsets in KF units (`0, +384, +500, +608`).
const DT2_TABLE: [u16; 4] = [0, 384, 500, 608];

/// DT1 magnitudes for `dt1 & 3` across 32 keycodes (0..31).
const DT1_TABLE: [[u8; 32]; 4] = [
    [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    [0, 0, 0, 0, 1, 1, 1, 1, 1, 1, 1, 1, 2, 2, 2, 2, 2, 3, 3, 3, 4, 4, 4, 5, 5, 6, 6, 7, 8, 8, 8, 8],
    [1, 1, 1, 1, 2, 2, 2, 2, 2, 3, 3, 3, 4, 4, 4, 5, 5, 6, 6, 7, 8, 8, 9, 10, 11, 12, 13, 14, 16, 16, 16, 16],
    [2, 2, 2, 2, 2, 3, 3, 3, 4, 4, 4, 5, 5, 6, 6, 7, 8, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 20, 22, 22, 22, 22],
];

/// EG counter shift amount per rate (0..63).
const EG_RATE_SHIFT: [u8; 64] = [
    11, 11, 11, 11, 10, 10, 10, 10, 9, 9, 9, 9, 8, 8, 8, 8,
    7, 7, 7, 7, 6, 6, 6, 6, 5, 5, 5, 5, 4, 4, 4, 4,
    3, 3, 3, 3, 2, 2, 2, 2, 1, 1, 1, 1, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
];

/// EG increment step row index per rate (0..63).
const EG_RATE_ROW: [u8; 64] = [
    0, 0, 0, 0, 1, 1, 2, 2, 1, 1, 2, 2, 1, 1, 2, 2,
    1, 1, 2, 2, 1, 1, 2, 2, 1, 1, 2, 2, 1, 1, 2, 2,
    1, 1, 2, 2, 1, 1, 2, 2, 1, 1, 2, 2, 1, 1, 2, 2,
    3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 15, 15, 15,
];

/// EG step values across 8 substeps (indexed by `EG_RATE_ROW`).
const EG_INC_TABLE: [[u8; 8]; 16] = [
    [0, 0, 0, 0, 0, 0, 0, 0], // 0: rate 0..3 (no change)
    [0, 1, 0, 1, 0, 1, 0, 1], // 1: 4/8
    [0, 1, 1, 1, 0, 1, 1, 1], // 2: 6/8
    [0, 1, 1, 1, 1, 1, 1, 1], // 3: 7/8 (rate 48)
    [1, 1, 1, 1, 1, 1, 1, 1], // 4: 1
    [1, 1, 1, 2, 1, 1, 1, 2], // 5: 1.25
    [1, 2, 1, 2, 1, 2, 1, 2], // 6: 1.5
    [1, 2, 2, 2, 1, 2, 2, 2], // 7: 1.75
    [2, 2, 2, 2, 2, 2, 2, 2], // 8: 2
    [2, 2, 2, 4, 2, 2, 2, 4], // 9: 2.5
    [2, 4, 2, 4, 2, 4, 2, 4], // 10: 3
    [2, 4, 4, 4, 2, 4, 4, 4], // 11: 3.5
    [4, 4, 4, 4, 4, 4, 4, 4], // 12: 4
    [4, 4, 4, 8, 4, 4, 4, 8], // 13: 5
    [4, 8, 4, 8, 4, 8, 4, 8], // 14: 6
    [8, 8, 8, 8, 8, 8, 8, 8], // 15: 8
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
        let rate = self.effective_rate(self.ar, kc);
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
        self.update_phase_inc(kc, kf, pm_offset);
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
        let rate = self.effective_rate(self.ar, kc);
        if rate >= 62 {
            self.eg_level = 0;
        }
        self.eg_phase = EgPhase::Release;
        self.update_phase_inc(kc, kf, pm_offset);
    }

    #[inline]
    fn keycode(kc: u8) -> u8 {
        let oct = (kc >> 4) & 7;
        let note = kc & 0x0F;
        let semi = note.saturating_sub(note >> 2).min(11);
        (oct << 2) | (semi >> 2)
    }

    #[inline]
    fn effective_rate(&self, base_5bit: u8, kc: u8) -> u8 {
        if base_5bit == 0 {
            return 0;
        }
        let kcode = Self::keycode(kc) >> (3 - self.ks);
        ((base_5bit << 1) + kcode).min(63)
    }

    fn update_phase_inc(&mut self, kc: u8, kf: u8, pm_offset: i32) {
        let oct = ((kc >> 4) & 7) as i32;
        let note = (kc & 0x0F) as i32;
        let semi = (note - (note >> 2)).min(11);
        let kf6 = (kf >> 2) as i32;
        let dt2 = DT2_TABLE[self.dt2 as usize] as i32;

        let pitch = (oct * 768 + semi * 64 + kf6 + dt2 + pm_offset).clamp(0, 8 * 768 - 1);
        let eff_oct = (pitch / 768) as u32;
        let step_idx = (pitch % 768) as usize;

        let mut inc = PHASE_INC_TABLE[step_idx] >> (7 - eff_oct);
        let kcode = Self::keycode(kc) as usize;
        let dt1_mag = DT1_TABLE[(self.dt1 & 3) as usize][kcode] as u32;
        if (self.dt1 & 4) != 0 {
            inc = inc.wrapping_sub(dt1_mag);
        } else {
            inc = inc.wrapping_add(dt1_mag);
        }
        inc &= 0xFFFFF;
        if self.mul == 0 {
            inc >>= 1;
        } else {
            inc = (inc * (self.mul as u32)) & 0xFFFFF;
        }
        self.phase_inc = inc;
    }

    fn step_eg(&mut self, eg_counter: u32, kc: u8) {
        let rate = match self.eg_phase {
            EgPhase::Off => return,
            EgPhase::Attack => self.effective_rate(self.ar, kc),
            EgPhase::Decay => self.effective_rate(self.d1r, kc),
            EgPhase::Sustain => self.effective_rate(self.d2r, kc),
            EgPhase::Release => self.effective_rate((self.rr << 1) | 1, kc),
        };
        if rate < 4 {
            return;
        }
        let shift = EG_RATE_SHIFT[rate as usize];
        if (eg_counter & ((1u32 << shift) - 1)) != 0 {
            return;
        }
        let substep = ((eg_counter >> shift) & 7) as usize;
        let step = EG_INC_TABLE[EG_RATE_ROW[rate as usize] as usize][substep] as u16;
        if step == 0 {
            return;
        }
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
                if self.eg_level >= self.d1l {
                    self.eg_phase = EgPhase::Sustain;
                }
            }
            EgPhase::Sustain => {
                self.eg_level = (self.eg_level + step).min(EG_SILENT);
            }
            EgPhase::Release => {
                self.eg_level = (self.eg_level + step).min(EG_SILENT);
                if self.eg_level >= EG_SILENT {
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
            ((EXP_TABLE[(atten & 0xFF) as usize] << 1) >> shift) as i32
        };
        self.out = if (phase_10 & 0x200) != 0 { -val } else { val };
        self.out
    }

    #[inline]
    fn compute_noise_sample(&mut self, noise_bit: bool, am_atten: u16) -> i32 {
        self.phase = (self.phase + self.phase_inc) & 0xFFFFF;
        let total_eg = (self.eg_level + ((self.tl as u16) << 3) + if self.ams_en { am_atten } else { 0 })
            .min(EG_SILENT);
        if total_eg >= EG_SILENT {
            self.out = 0;
            return 0;
        }
        let atten = total_eg << 2;
        let shift = atten >> 8;
        let val = if shift >= 13 {
            0
        } else {
            ((EXP_TABLE[(atten & 0xFF) as usize] << 1) >> shift) as i32
        };
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
        let pm = if self.pms == 0 {
            0
        } else if self.pms <= 5 {
            (pm_lfo * (self.pms as i32)) >> 5
        } else {
            (pm_lfo << (self.pms - 5)) >> 2
        };
        for op in &mut self.ops {
            op.update_phase_inc(self.kc, self.kf, pm);
        }
    }

    fn step_eg(&mut self, eg_counter: u32) {
        for op in &mut self.ops {
            op.step_eg(eg_counter, self.kc);
        }
    }

    #[inline]
    fn eval(&mut self, am_lfo: u16, use_noise: bool, noise_bit: bool) -> (i32, i32) {
        let am_atten = if self.ams == 0 {
            0
        } else {
            am_lfo >> AMS_SHIFT[self.ams as usize]
        };

        // M1 (op 0) with 2-sample feedback (FB=1..7 -> pi/16 .. 4*pi)
        let fb = if self.fl == 0 {
            0
        } else {
            (self.op1_hist[0] + self.op1_hist[1]) >> (9 - self.fl)
        };
        let m1 = self.ops[0].compute_sample(fb, am_atten);
        self.op1_hist[1] = self.op1_hist[0];
        self.op1_hist[0] = m1;

        // Slot evaluation order matches OPM pipeline: M1 (0) -> C1 (2) -> M2 (1) -> C2 (3).
        let (c1, m2, c2, out) = match self.con {
            0 => {
                // M1 -> C1 -> M2 -> C2
                let c1 = self.ops[2].compute_sample(m1 >> 1, am_atten);
                let m2 = self.ops[1].compute_sample(c1 >> 1, am_atten);
                let c2 = self.eval_c2(m2 >> 1, am_atten, use_noise, noise_bit);
                (c1, m2, c2, c2)
            }
            1 => {
                // (M1 + C1) -> M2 -> C2
                let c1 = self.ops[2].compute_sample(0, am_atten);
                let m2 = self.ops[1].compute_sample((m1 + c1) >> 1, am_atten);
                let c2 = self.eval_c2(m2 >> 1, am_atten, use_noise, noise_bit);
                (c1, m2, c2, c2)
            }
            2 => {
                // (M1 + (C1 -> M2)) -> C2
                let c1 = self.ops[2].compute_sample(0, am_atten);
                let m2 = self.ops[1].compute_sample(c1 >> 1, am_atten);
                let c2 = self.eval_c2((m1 + m2) >> 1, am_atten, use_noise, noise_bit);
                (c1, m2, c2, c2)
            }
            3 => {
                // ((M1 -> C1) + M2) -> C2
                let c1 = self.ops[2].compute_sample(m1 >> 1, am_atten);
                let m2 = self.ops[1].compute_sample(0, am_atten);
                let c2 = self.eval_c2((c1 + m2) >> 1, am_atten, use_noise, noise_bit);
                (c1, m2, c2, c2)
            }
            4 => {
                // (M1 -> C1) + (M2 -> C2)
                let c1 = self.ops[2].compute_sample(m1 >> 1, am_atten);
                let m2 = self.ops[1].compute_sample(0, am_atten);
                let c2 = self.eval_c2(m2 >> 1, am_atten, use_noise, noise_bit);
                (c1, m2, c2, c1 + c2)
            }
            5 => {
                // M1 -> (C1 + M2 + C2)
                let c1 = self.ops[2].compute_sample(m1 >> 1, am_atten);
                let m2 = self.ops[1].compute_sample(m1 >> 1, am_atten);
                let c2 = self.eval_c2(m1 >> 1, am_atten, use_noise, noise_bit);
                (c1, m2, c2, c1 + m2 + c2)
            }
            6 => {
                // (M1 -> C1) + M2 + C2
                let c1 = self.ops[2].compute_sample(m1 >> 1, am_atten);
                let m2 = self.ops[1].compute_sample(0, am_atten);
                let c2 = self.eval_c2(0, am_atten, use_noise, noise_bit);
                (c1, m2, c2, c1 + m2 + c2)
            }
            _ => {
                // M1 + C1 + M2 + C2
                let c1 = self.ops[2].compute_sample(0, am_atten);
                let m2 = self.ops[1].compute_sample(0, am_atten);
                let c2 = self.eval_c2(0, am_atten, use_noise, noise_bit);
                (c1, m2, c2, m1 + c1 + m2 + c2)
            }
        };
        let _ = (c1, m2, c2);

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
    samples: Vec<(i16, i16)>,
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
            samples: Vec::new(),
        }
    }

    pub fn reset(&mut self) {
        let audio_enabled = self.audio_enabled;
        *self = Self::new();
        self.audio_enabled = audio_enabled;
    }

    /// Enables or disables 62.5 kHz sample buffering (disabled in headless/test runs).
    pub fn set_audio_enabled(&mut self, enabled: bool) {
        self.audio_enabled = enabled;
        if !enabled {
            self.samples.clear();
        }
    }

    /// Drains accumulated 62.5 kHz stereo samples into `out`.
    pub fn take_samples(&mut self, out: &mut Vec<(i16, i16)>) {
        out.append(&mut self.samples);
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
                self.timer_a_sub %= period_10mhz;
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
                self.timer_b_sub %= period_10mhz;
                if (ctrl & 0x08) != 0 {
                    self.status |= 0x02;
                }
            }
        }

        if csm_trigger {
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

        if !self.audio_enabled {
            return;
        }

        self.sample_sub = self.sample_sub.saturating_add(cycles_10mhz);
        if self.sample_sub < CYCLES_PER_SAMPLE_10MHZ {
            return;
        }
        let sample_count = self.sample_sub / CYCLES_PER_SAMPLE_10MHZ;
        self.sample_sub %= CYCLES_PER_SAMPLE_10MHZ;

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

    #[inline]
    fn channel_pm_offset(pms: u8, lfo_pm: i32) -> i32 {
        match pms {
            0 => 0,
            1..=5 => lfo_pm >> (6 - pms),
            6 => lfo_pm << 1,
            _ => lfo_pm << 2,
        }
    }

    fn step_lfo_and_noise(&mut self) {
        // 17-bit LFSR noise (taps bit 0 ^ bit 3)
        let nfrq = (self.regs[0x0F] & 0x1F) as u32;
        let noise_period = (32 - nfrq).max(1);
        self.noise_counter += 1;
        if self.noise_counter >= noise_period {
            self.noise_counter = 0;
            let bit = ((self.noise_lfsr ^ (self.noise_lfsr >> 3)) & 1) << 16;
            self.noise_lfsr = (self.noise_lfsr >> 1) | bit;
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
            if self.lfo_counter >= 0x40000 {
                self.lfo_counter -= 0x40000;
                self.lfo_phase = self.lfo_phase.wrapping_add(1);
            }
        }

        let wave = self.regs[0x1B] & 0x03;
        let (pm_wave, am_wave): (i32, u32) = match wave {
            0 => {
                // Sawtooth
                let pm = ((self.lfo_phase as i8) as i32) * 2;
                let am = (self.lfo_phase ^ 0xFF) as u32;
                (pm, am)
            }
            1 => {
                // Square
                let hi = (self.lfo_phase & 0x80) != 0;
                (if hi { -128 } else { 127 }, if hi { 0 } else { 255 })
            }
            2 => {
                // Triangle
                let p = self.lfo_phase;
                let tri = if (p & 0x80) == 0 {
                    (p as i32) * 2 - 128
                } else {
                    127 - ((p & 0x7F) as i32) * 2
                };
                let am = if (p & 0x80) == 0 {
                    (p as u32) * 2
                } else {
                    (255 - p as u32) * 2
                };
                (tri, am)
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

        // Scale 8-channel sum cleanly into i16 headroom.
        let left = (left_acc >> 1).clamp(i16::MIN as i32, i16::MAX as i32) as i16;
        let right = (right_acc >> 1).clamp(i16::MIN as i32, i16::MAX as i32) as i16;
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
            self.apply_register(r as u8, val);
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
                // Bits 3..6 = M1(0), M2(1), C1(2), C2(3)
                for slot in 0..4 {
                    if (val & (1 << (3 + slot))) != 0 {
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
}
