//! PCI configuration space of the SVGA II device (256 bytes, no capabilities).

use svga_proto::pci::{DEVICE_ID_SVGA2, VENDOR_ID_VMWARE};

/// Size of conventional PCI configuration space.
pub const CONFIG_SIZE: usize = 256;

const COMMAND: usize = 0x04;
const BAR0: usize = 0x10;
const SUBSYSTEM_VENDOR: usize = 0x2c;
const INTERRUPT_LINE: usize = 0x3c;

/// Writable bits of the command register: I/O, memory, bus master.
const COMMAND_WRITABLE: u16 = 0x0007;

#[derive(Clone, Copy)]
struct Bar {
    size: u32,
    flags: u32,
}

impl Bar {
    const NONE: Self = Self { size: 0, flags: 0 };

    /// Bits that hold the address (the rest reads back as `flags`).
    fn address_mask(&self) -> u32 {
        if self.size == 0 { 0 } else { !(self.size - 1) }
    }
}

/// Configuration space with BAR sizing semantics.
pub struct PciConfig {
    space: [u8; CONFIG_SIZE],
    bars: [Bar; 6],
}

impl PciConfig {
    /// BAR 0: 16 bytes of I/O ports. BAR 1: prefetchable VRAM. BAR 2: FIFO.
    /// Both memory sizes must be powers of two.
    pub fn new(vram_size: u32, fifo_size: u32) -> Self {
        assert!(vram_size.is_power_of_two() && fifo_size.is_power_of_two());
        let mut space = [0u8; CONFIG_SIZE];
        space[0..2].copy_from_slice(&VENDOR_ID_VMWARE.to_le_bytes());
        space[2..4].copy_from_slice(&DEVICE_ID_SVGA2.to_le_bytes());
        space[0x0b] = 0x03; // class: display controller, subclass 0 (VGA)
        space[SUBSYSTEM_VENDOR..SUBSYSTEM_VENDOR + 2]
            .copy_from_slice(&VENDOR_ID_VMWARE.to_le_bytes());
        space[SUBSYSTEM_VENDOR + 2..SUBSYSTEM_VENDOR + 4]
            .copy_from_slice(&DEVICE_ID_SVGA2.to_le_bytes());
        let mut bars = [Bar::NONE; 6];
        bars[0] = Bar {
            size: 16,
            flags: 0x1,
        }; // I/O space
        bars[1] = Bar {
            size: vram_size,
            flags: 0x8,
        }; // 32-bit, prefetchable
        bars[2] = Bar {
            size: fifo_size,
            flags: 0x0,
        }; // 32-bit
        let mut config = Self { space, bars };
        for index in 0..6 {
            config.store_bar(index, 0);
        }
        config
    }

    fn store_bar(&mut self, index: usize, value: u32) {
        let bar = self.bars[index];
        let value = (value & bar.address_mask()) | bar.flags;
        let at = BAR0 + 4 * index;
        self.space[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn bar_value(&self, index: usize) -> u32 {
        let at = BAR0 + 4 * index;
        u32::from_le_bytes(self.space[at..at + 4].try_into().unwrap_or_default())
    }

    /// Guest-assigned address of a memory or I/O BAR.
    pub fn bar_address(&self, index: usize) -> u32 {
        match self.bars.get(index) {
            Some(bar) => self.bar_value(index) & bar.address_mask(),
            None => 0,
        }
    }

    /// Reads `out.len()` bytes at `offset`; false if out of range.
    pub fn read(&self, offset: u64, out: &mut [u8]) -> bool {
        let Some(range) = range(offset, out.len()) else {
            return false;
        };
        out.copy_from_slice(&self.space[range]);
        true
    }

    /// Writes `data` at `offset`, keeping read-only bits; false if out of range.
    pub fn write(&mut self, offset: u64, data: &[u8]) -> bool {
        let Some(range) = range(offset, data.len()) else {
            return false;
        };
        for (at, &byte) in range.zip(data) {
            match at {
                COMMAND => self.space[at] = byte & COMMAND_WRITABLE.to_le_bytes()[0],
                0x05 => self.space[at] = byte & COMMAND_WRITABLE.to_le_bytes()[1],
                0x0c | 0x0d | INTERRUPT_LINE => self.space[at] = byte, // cache line, latency, IRQ line
                0x10..=0x27 => self.space[at] = byte,
                _ => {}
            }
        }
        for index in 0..6 {
            let value = self.bar_value(index);
            self.store_bar(index, value);
        }
        true
    }
}

fn range(offset: u64, len: usize) -> Option<std::ops::Range<usize>> {
    let start = usize::try_from(offset).ok()?;
    let end = start.checked_add(len)?;
    (end <= CONFIG_SIZE).then_some(start..end)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read32(config: &PciConfig, offset: u64) -> u32 {
        let mut out = [0u8; 4];
        assert!(config.read(offset, &mut out));
        u32::from_le_bytes(out)
    }

    #[test]
    fn identity() {
        let config = PciConfig::new(64 << 20, 2 << 20);
        assert_eq!(read32(&config, 0), 0x0405_15ad);
        assert_eq!(read32(&config, 8) >> 8, 0x03_0000);
        assert_eq!(read32(&config, 0x2c), 0x0405_15ad);
    }

    #[test]
    fn bar_sizing_and_assignment() {
        let mut config = PciConfig::new(64 << 20, 2 << 20);
        config.write(0x14, &u32::MAX.to_le_bytes());
        assert_eq!(read32(&config, 0x14), !((64u32 << 20) - 1) | 0x8);
        config.write(0x14, &0xe000_0000u32.to_le_bytes());
        assert_eq!(config.bar_address(1), 0xe000_0000);
        config.write(0x10, &u32::MAX.to_le_bytes());
        assert_eq!(read32(&config, 0x10), 0xffff_fff1);
        config.write(0x10, &0xc040u32.to_le_bytes());
        assert_eq!(config.bar_address(0), 0xc040);
        config.write(0x1c, &u32::MAX.to_le_bytes());
        assert_eq!(read32(&config, 0x1c), 0, "BAR 3 does not exist");
    }

    #[test]
    fn read_only_fields_and_bounds() {
        let mut config = PciConfig::new(64 << 20, 2 << 20);
        config.write(0, &[0xff; 4]);
        assert_eq!(read32(&config, 0), 0x0405_15ad);
        config.write(4, &[0xff, 0xff]);
        assert_eq!(read32(&config, 4) & 0xffff, 0x0007);
        assert!(!config.write(255, &[0, 0]));
        assert!(!config.read(u64::MAX, &mut [0]));
    }
}
