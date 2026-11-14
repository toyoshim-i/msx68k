//! Instruction-boundary DMA events. No CPU pipeline or pin-level bus arbitration.
//! DMA accesses real bus endpoints and charges the shared emulated timeline.
use crate::{dmac::*, timing::MemoryRegion, X68kBus};
use msx68k_cpu::{Bus, CpuModel};

const TICK: u64 = 40; // 400 MHz time base, exact for 10/12.5 MHz DMA and 1/8-cycle refresh.

impl X68kBus {
    pub(crate) fn refresh_fdc_dma_request(&mut self) {
        let cycle = self.dma_cycle_unit();
        self.dmac
            .set_request_with_delay(0, self.fdc.dma_request(), cycle);
    }

    /// Idle path is one predictable test. Inactive channels are not scanned.
    #[inline(always)]
    pub(crate) fn advance_cpu_time(&mut self, ticks: u32) -> u32 {
        if !self.dmac.has_scheduled_work() {
            self.advance_devices(ticks);
            return 0;
        }
        self.advance_dma_time(ticks)
    }

    #[cold]
    pub(crate) fn advance_dma_time(&mut self, ticks: u32) -> u32 {
        // Keep the event horizon fixed at the instruction boundary.
        // DMA adds elapsed time without executing additional CPU instructions.
        let horizon = self.dmac.clock.saturating_add(ticks as u64 * TICK);
        self.dmac.clock = horizon;
        self.advance_devices(ticks);
        let mut stolen = 0;
        while self.dmac.next_event <= horizon {
            let channel = (0..4)
                .min_by_key(|&i| (self.dmac.deadlines[i], self.dmac.channels[i].cpr & 3, i))
                .unwrap();
            let requested = self.dmac.deadlines[channel];
            self.dmac.schedule(channel, u64::MAX);
            if !self.dmac.channels[channel].is_active() {
                continue;
            }
            let mut cost = 0;
            self.dma_in_bus = true;
            let result = self.dma_operand(channel, &mut cost);
            self.dma_in_bus = false;
            if let Err(code) = result {
                self.dmac.channels[channel].finish_error(code);
                self.dmac.schedule(channel, u64::MAX);
            }
            self.dmac.clock = self.dmac.clock.saturating_add(cost);
            let elapsed = cost + self.dmac.tick_fraction as u64;
            let dma_ticks = (elapsed / TICK) as u32;
            self.dmac.tick_fraction = (elapsed % TICK) as u32;
            self.advance_devices(dma_ticks);
            stolen += dma_ticks;
            if self.dmac.channels[channel].is_active() {
                let ch = &self.dmac.channels[channel];
                let when = match ch.ocr & 3 {
                    0 => {
                        if self.dmac.clock < self.dmac.burst_end[channel] {
                            requested
                        } else {
                            let span = self.dma_cycle_unit() * (16 << ((self.dmac.gcr >> 2) & 3));
                            let interval = span << (1 + (self.dmac.gcr & 3));
                            let next = self.dmac.burst_start[channel]
                                .saturating_add(interval)
                                .max(self.dmac.clock + 1);
                            self.dmac.burst_start[channel] = next;
                            self.dmac.burst_end[channel] = next + span;
                            next
                        }
                    }
                    1 => requested, // Maximum speed holds the CPU until completion.
                    _ if self.dmac.requests & (1 << channel) != 0 => {
                        self.dmac.clock + self.dma_cycle_unit()
                    }
                    _ => u64::MAX,
                };
                self.dmac.schedule(channel, when);
            }
        }
        stolen
    }

    fn dma_cycle_unit(&self) -> u64 {
        if self.cpu_model == CpuModel::Mc68030 {
            32
        } else {
            40
        }
    }

    fn dma_access_cost(&self, address: u32) -> u64 {
        let unit = self.dma_cycle_unit();
        let region = MemoryRegion::from_addr(address & 0x00ff_ffff);
        // DMA timing uses its own clock domain, independently of CPU speed
        // and privilege state. Timing coefficients still need hardware validation.
        let wait = region.timing_spec(false).sub_bus_10mhz_waits as u64;
        (5 + wait) * unit
            + if region == MemoryRegion::MainRam {
                unit / 8
            } else {
                0
            }
    }

    fn dma_read(
        &mut self,
        address: u32,
        width: u32,
        fc: u8,
        code: u8,
        cost: &mut u64,
    ) -> Result<u32, u8> {
        if width > 1 && address & 1 != 0 {
            return Err(code - 4);
        }
        let supervisor = fc & 4 != 0;
        *cost += self.dma_access_cost(address);
        match width {
            1 => self
                .read_u8(address, supervisor)
                .map(u32::from)
                .map_err(|_| code),
            2 => self
                .read_u16(address, supervisor)
                .map(u32::from)
                .map_err(|_| code),
            4 => {
                let hi = self.read_u16(address, supervisor).map_err(|_| code)? as u32;
                *cost += self.dma_access_cost(address.wrapping_add(2));
                let lo = self
                    .read_u16(address.wrapping_add(2), supervisor)
                    .map_err(|_| code)? as u32;
                Ok(hi << 16 | lo)
            }
            _ => unreachable!(),
        }
    }

    fn dma_write(
        &mut self,
        address: u32,
        width: u32,
        fc: u8,
        value: u32,
        code: u8,
        cost: &mut u64,
    ) -> Result<(), u8> {
        if width > 1 && address & 1 != 0 {
            return Err(code - 4);
        }
        let supervisor = fc & 4 != 0;
        *cost += self.dma_access_cost(address);
        match width {
            1 => self
                .write_u8(address, value as u8, supervisor)
                .map_err(|_| code),
            2 => self
                .write_u16(address, value as u16, supervisor)
                .map_err(|_| code),
            4 => {
                self.write_u16(address, (value >> 16) as u16, supervisor)
                    .map_err(|_| code)?;
                *cost += self.dma_access_cost(address.wrapping_add(2));
                self.write_u16(address.wrapping_add(2), value as u16, supervisor)
                    .map_err(|_| code)
            }
            _ => unreachable!(),
        }
    }

    fn dma_chain(&mut self, i: usize, first: bool, cost: &mut u64) -> Result<bool, u8> {
        let ch = self.dmac.channels[i].clone();
        match ch.ocr >> 2 & 3 {
            0 if !first && ch.ccr & CCR_CNT != 0 => {
                let dst = &mut self.dmac.channels[i];
                dst.mar = ch.bar;
                dst.mtc = ch.btc;
                dst.ccr &= !CCR_CNT;
                dst.csr |= CSR_BTC;
                if dst.ccr & CCR_ITE != 0 {
                    dst.irq_pending = true;
                }
                if dst.mtc == 0 {
                    return Err(CER_COUNT);
                }
                Ok(true)
            }
            0 => Ok(first && ch.mtc != 0),
            2 if ch.btc == 0 => {
                if first {
                    Err(CER_BASE_COUNT)
                } else {
                    Ok(false)
                }
            }
            3 if !first && ch.bar == 0 => Ok(false),
            mode @ (2 | 3) => {
                let mar = self.dma_read(ch.bar, 4, ch.bfc, CER_BUS_BASE, cost)?;
                let count =
                    self.dma_read(ch.bar.wrapping_add(4), 2, ch.bfc, CER_BUS_BASE, cost)? as u16;
                let next = if mode == 3 {
                    self.dma_read(ch.bar.wrapping_add(6), 4, ch.bfc, CER_BUS_BASE, cost)?
                } else {
                    ch.bar.wrapping_add(6)
                };
                let dst = &mut self.dmac.channels[i];
                dst.mar = mar;
                dst.mtc = count;
                dst.bar = next;
                if mode == 2 {
                    dst.btc -= 1;
                }
                if count == 0 {
                    return Err(CER_COUNT);
                }
                Ok(true)
            }
            _ => Err(CER_CONFIG),
        }
    }

    fn dma_operand(&mut self, i: usize, cost: &mut u64) -> Result<(), u8> {
        if self.dmac.starting[i] {
            self.dmac.starting[i] = false;
            if !self.dma_chain(i, true, cost)? {
                return Err(CER_COUNT);
            }
            let span = self.dma_cycle_unit() * (16 << ((self.dmac.gcr >> 2) & 3));
            self.dmac.burst_start[i] = self.dmac.clock;
            self.dmac.burst_end[i] = self.dmac.clock + span;
            if self.dmac.channels[i].ocr & 3 == 2 && self.dmac.requests & (1 << i) == 0 {
                return Ok(());
            }
        }
        let ch = self.dmac.channels[i].clone();
        let size = ch.ocr >> 4 & 3;
        let bytes = match size {
            1 => 2,
            2 => 4,
            _ => 1,
        };
        let to_memory = ch.is_device_to_memory();
        let data = self.dma_read_side(i, !to_memory, bytes, size == 0, cost)?;
        self.dma_write_side(i, to_memory, bytes, size == 0, data, cost)?;
        // DMA-side writes may abort/halt a controller through its mapped registers.
        if self.dmac.channels[i].csr & CSR_ACT == 0 {
            return Ok(());
        }
        self.dmac.channels[i].step_mar(bytes);
        self.dmac.channels[i].step_dar(bytes * if ch.dcr & 8 == 0 { 2 } else { 1 });
        self.dmac.channels[i].mtc -= 1;
        let mac = ch.scr >> 2 & 3;
        let extra = if ch.dcr & 8 == 0 {
            match size {
                0 if mac == 0 => {
                    if ch.mar & 1 == 0 {
                        6
                    } else {
                        8
                    }
                }
                0 => {
                    if to_memory {
                        10
                    } else {
                        8
                    }
                }
                1 | 2 if !to_memory => 4,
                _ => 0,
            }
        } else if size == 0 {
            4 + u64::from(ch.mar & 1 != 0 && mac == 0)
                + u64::from(ch.dar & 1 != 0 && ch.scr & 3 == 0)
        } else {
            0
        };
        *cost += extra * self.dma_cycle_unit();
        if self.dmac.channels[i].mtc == 0 && !self.dma_chain(i, false, cost)? {
            let dst = &mut self.dmac.channels[i];
            if ch.ocr & 0x0c != 0 {
                dst.csr |= CSR_BTC;
            }
            dst.finish_normal();
            if i == 0 {
                self.fdc.terminal_count(&mut self.ioi);
                self.dmac.set_request(0, false);
            }
        }
        Ok(())
    }

    fn dma_read_side(
        &mut self,
        i: usize,
        memory: bool,
        bytes: u32,
        packed: bool,
        cost: &mut u64,
    ) -> Result<u32, u8> {
        let ch = self.dmac.channels[i].clone();
        let (address, fc, code, direction) = if memory {
            (ch.mar, ch.mfc, CER_BUS_MEM, ch.scr >> 2 & 3)
        } else {
            (ch.dar, ch.dfc, CER_BUS_DEV, ch.scr & 3)
        };
        let port8 = !memory && ch.dcr & 8 == 0;
        if packed && !port8 {
            let carry = if memory {
                self.dmac.memory_carry[i].take()
            } else {
                self.dmac.device_carry[i].take()
            };
            if let Some(value) = carry {
                return Ok(value as u32);
            }
            if ch.mtc >= 2
                && ((direction == 1 && address & 1 == 0) || (direction == 2 && address & 1 != 0))
            {
                let word = self.dma_read(address & !1, 2, fc, code, cost)?;
                let (value, carry) = if direction == 1 {
                    (word >> 8, word as u8)
                } else {
                    (word & 255, (word >> 8) as u8)
                };
                if memory {
                    self.dmac.memory_carry[i] = Some(carry);
                } else {
                    self.dmac.device_carry[i] = Some(carry);
                }
                return Ok(value);
            }
        }
        if port8 && bytes > 1 {
            let mut data = 0;
            for offset in 0..bytes {
                data = data << 8
                    | self.dma_read(address.wrapping_add(offset * 2), 1, fc, code, cost)?;
            }
            Ok(data)
        } else {
            self.dma_read(address, bytes, fc, code, cost)
        }
    }

    fn dma_write_side(
        &mut self,
        i: usize,
        memory: bool,
        bytes: u32,
        packed: bool,
        data: u32,
        cost: &mut u64,
    ) -> Result<(), u8> {
        let ch = self.dmac.channels[i].clone();
        let (address, fc, code, direction) = if memory {
            (ch.mar, ch.mfc, CER_BUS_MEM, ch.scr >> 2 & 3)
        } else {
            (ch.dar, ch.dfc, CER_BUS_DEV, ch.scr & 3)
        };
        let port8 = !memory && ch.dcr & 8 == 0;
        if packed && !port8 {
            let carry = if memory {
                self.dmac.memory_carry[i].take()
            } else {
                self.dmac.device_carry[i].take()
            };
            if let Some(carry) = carry {
                let word = if address & 1 != 0 {
                    (carry as u32) << 8 | data
                } else {
                    data << 8 | carry as u32
                };
                return self.dma_write(address & !1, 2, fc, word, code, cost);
            }
            if ch.mtc >= 2
                && ((direction == 1 && address & 1 == 0) || (direction == 2 && address & 1 != 0))
            {
                if memory {
                    self.dmac.memory_carry[i] = Some(data as u8);
                } else {
                    self.dmac.device_carry[i] = Some(data as u8);
                }
                return Ok(());
            }
        }
        if port8 && bytes > 1 {
            for offset in 0..bytes {
                self.dma_write(
                    address.wrapping_add(offset * 2),
                    1,
                    fc,
                    data >> ((bytes - offset - 1) * 8),
                    code,
                    cost,
                )?;
            }
            Ok(())
        } else {
            self.dma_write(address, bytes, fc, data, code, cost)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::X68kVm;

    fn machine(ocr: u8, count: u16, memory: u32, device: u32) -> X68kBus {
        let mut bus = X68kVm::new_app_vm().bus;
        let ch = &mut bus.dmac.channels[2];
        ch.dcr = 8; // 16-bit device port
        ch.ocr = ocr;
        ch.scr = 5; // both increment
        ch.mtc = count;
        ch.mar = memory;
        ch.dar = device;
        ch.mfc = 5;
        ch.dfc = 5;
        ch.bfc = 5;
        bus
    }
    fn start(bus: &mut X68kBus) {
        bus.dmac.write_u8(0xe84087, CCR_STR | CCR_ITE).unwrap();
    }
    fn drain(bus: &mut X68kBus) {
        for _ in 0..1000 {
            bus.advance_cpu_time(4);
            if !bus.dmac.has_scheduled_work() {
                return;
            }
        }
        panic!("DMA did not finish");
    }

    #[test]
    fn idle_does_not_advance_or_scan_the_dma_scheduler() {
        let mut bus = machine(0x11, 1, 0x2000, 0xe00000);
        // Also covers an external channel waiting for DRQ: ACT alone is not work.
        bus.dmac.channels[2].csr |= CSR_ACT;
        for _ in 0..100 {
            assert_eq!(bus.advance_cpu_time(4), 0);
        }
        assert_eq!(bus.dmac.clock, 0);
        assert_eq!(bus.dmac.deadlines, [u64::MAX; 4]);
        assert_eq!(bus.dmac.channels[2].mtc, 1);
    }

    #[test]
    fn transfer_uses_tvram_write_logic_and_charges_time_before_irq() {
        let mut bus = machine(0x11, 2, 0x2000, 0xe00000);
        bus.write_u32(0x2000, 0xcafe1234, true).unwrap();
        bus.write_u16(0xe8002a, 0x0150, true).unwrap(); // planes 0 and 2
        start(&mut bus);
        assert_eq!(bus.dmac.channels[2].mtc, 2);
        assert!(!bus.dmac.channels[2].irq_pending);
        let stolen = bus.advance_cpu_time(4);
        assert_eq!(stolen, 24); // 2 * (RAM 5+1/8 + TVRAM 5+2), fractional remainder retained
        assert_eq!(bus.dmac.tick_fraction, 10);
        for address in [0xe00000, 0xe40000] {
            assert_eq!(bus.read_u32(address, true).unwrap(), 0xcafe1234);
        }
        assert_eq!(bus.dmac.channels[2].mtc, 0);
        assert!(bus.dmac.channels[2].irq_pending);
        assert!(!bus.dmac.has_scheduled_work());
    }

    #[test]
    fn external_requests_halt_resume_and_abort_are_not_instant_copies() {
        let mut bus = machine(0xb2, 3, 0x3000, 0x2000); // unpacked byte, device -> memory
        bus.write_u32(0x2000, 0x12345678, true).unwrap();
        start(&mut bus);
        assert_eq!(bus.advance_cpu_time(4), 0); // initialize, wait for request
        assert!(!bus.dmac.has_scheduled_work());
        bus.dmac.set_request(2, true);
        bus.advance_cpu_time(4);
        assert_eq!(bus.dmac.channels[2].mtc, 2);
        assert_eq!(bus.read_u8(0x3000, true).unwrap(), 0x12);
        bus.dmac.write_u8(0xe84087, CCR_HLT | CCR_ITE).unwrap();
        assert_eq!(bus.advance_cpu_time(100), 0);
        assert_eq!(bus.dmac.channels[2].mtc, 2);
        bus.dmac.write_u8(0xe84087, CCR_ITE).unwrap();
        bus.advance_cpu_time(4);
        assert_eq!(bus.dmac.channels[2].mtc, 1);
        bus.dmac.write_u8(0xe84087, CCR_SAB | CCR_ITE).unwrap();
        assert_eq!(bus.dmac.channels[2].cer, CER_SW_ABORT);
        assert!(!bus.dmac.has_scheduled_work());
        assert_eq!(bus.read_u8(0x3002, true).unwrap(), 0);
    }

    #[test]
    fn packed_odd_byte_count_and_long_operands_preserve_byte_order() {
        for (ocr, count, length) in [(0x01, 7, 7), (0x21, 2, 8)] {
            let mut bus = machine(ocr, count, 0x2000, 0xe00000);
            for i in 0..8 {
                bus.write_u8(0x2000 + i, (i + 1) as u8, true).unwrap();
            }
            start(&mut bus);
            drain(&mut bus);
            for i in 0..length {
                assert_eq!(bus.read_u8(0xe00000 + i, true).unwrap(), (i + 1) as u8);
            }
            assert_eq!(bus.read_u8(0xe00000 + length, true).unwrap(), 0);
        }
    }

    #[test]
    fn array_chain_descriptors_are_read_from_the_bus_using_bfc() {
        let mut bus = machine(0x19, 0, 0, 0x3000); // word, max, array
        bus.write_u32(0xe00000, 0x2000, true).unwrap();
        bus.write_u16(0xe00004, 2, true).unwrap();
        bus.write_u32(0x2000, 0x76543210, true).unwrap();
        bus.dmac.channels[2].bar = 0xe00000;
        bus.dmac.channels[2].btc = 1;
        start(&mut bus);
        drain(&mut bus);
        assert_eq!(bus.read_u32(0x3000, true).unwrap(), 0x76543210);
        assert_eq!(bus.dmac.channels[2].csr & CSR_BTC, CSR_BTC);
        let mut denied = machine(0x19, 0, 0, 0x3000);
        denied.dmac.channels[2].bar = 0xe00000;
        denied.dmac.channels[2].btc = 1;
        denied.dmac.channels[2].bfc = 1;
        start(&mut denied);
        drain(&mut denied);
        assert_eq!(denied.dmac.channels[2].cer, CER_BUS_BASE);
    }

    #[test]
    fn memory_and_device_faults_use_their_own_function_codes() {
        for (mfc, dfc, mar, dar, error) in [
            (5, 1, 0x2000, 0xe00000, CER_BUS_DEV),
            (1, 5, 0xe00000, 0x2000, CER_BUS_MEM),
            (5, 5, 0x2001, 0x3000, CER_ADDR_MEM),
            (5, 5, 0x2000, 0x3001, CER_ADDR_DEV),
        ] {
            let mut bus = machine(0x11, 1, mar, dar);
            bus.dmac.channels[2].mfc = mfc;
            bus.dmac.channels[2].dfc = dfc;
            start(&mut bus);
            drain(&mut bus);
            assert_eq!(bus.dmac.channels[2].cer, error);
            assert_eq!(bus.dmac.channels[2].mtc, 1);
        }
    }

    #[test]
    fn x68030_dma_uses_12_5mhz_and_retains_fractional_ticks() {
        let mut bus = machine(0x11, 2, 0x2000, 0xe00000);
        bus.cpu_model = CpuModel::Mc68030;
        start(&mut bus);
        assert_eq!(bus.advance_cpu_time(4), 19);
        assert_eq!(bus.dmac.tick_fraction, 16);
        assert_eq!(bus.dmac.channels[2].mtc, 0);
    }

    #[test]
    fn eight_bit_device_port_splits_words_and_longs_on_odd_addresses() {
        for (ocr, bytes) in [(0x11, 2), (0x21, 4)] {
            let mut bus = machine(ocr, 2, 0x2000, 0x3001);
            bus.dmac.channels[2].dcr = 0;
            bus.write_u32(0x2000, 0x12345678, true).unwrap();
            bus.write_u32(0x2004, 0x9abcdef0, true).unwrap();
            start(&mut bus);
            drain(&mut bus);
            for i in 0..bytes * 2 {
                assert_eq!(
                    bus.read_u8(0x3001 + i * 2, true).unwrap(),
                    bus.read_u8(0x2000 + i, true).unwrap()
                );
                assert_eq!(bus.read_u8(0x3000 + i * 2, true).unwrap(), 0);
            }
        }
    }

    #[test]
    fn linked_descriptors_follow_bus_addresses_and_stop_at_null() {
        let mut bus = machine(0x1d, 0, 0, 0x3000);
        bus.write_u32(0xe00000, 0x2000, true).unwrap();
        bus.write_u16(0xe00004, 1, true).unwrap();
        bus.write_u32(0xe00006, 0xe00020, true).unwrap();
        bus.write_u32(0xe00020, 0x2100, true).unwrap();
        bus.write_u16(0xe00024, 1, true).unwrap();
        bus.write_u32(0xe00026, 0, true).unwrap();
        bus.write_u16(0x2000, 0xabcd, true).unwrap();
        bus.write_u16(0x2100, 0x1234, true).unwrap();
        bus.dmac.channels[2].bar = 0xe00000;
        start(&mut bus);
        drain(&mut bus);
        assert_eq!(bus.read_u32(0x3000, true).unwrap(), 0xabcd1234);
        assert_eq!(bus.dmac.channels[2].bar, 0);
        assert_eq!(bus.dmac.channels[2].cer, CER_NONE);
    }

    #[test]
    fn limited_auto_releases_cpu_between_bursts() {
        let mut bus = machine(0x10, 32, 0x2000, 0x3000);
        start(&mut bus);
        assert!(bus.advance_cpu_time(4) > 0);
        let remaining = bus.dmac.channels[2].mtc;
        assert!(remaining > 0 && remaining < 32);
        assert!(bus.dmac.next_event > bus.dmac.clock);
        drain(&mut bus);
        assert_eq!(bus.dmac.channels[2].mtc, 0);
    }
}
