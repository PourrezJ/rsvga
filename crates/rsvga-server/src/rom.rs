//! PCI expansion ROM holding the card's UEFI driver.
//!
//! Layout (PCI Firmware Specification 3.0, UEFI specification §14.4.2):
//!
//! ```text
//! 0x000  EFI PCI expansion ROM header (0x55AA, 0x0EF1, subsystem, machine...)
//! 0x01C  PCI data structure "PCIR" (vendor, device, class, code type 3 = EFI)
//! 0x200  the PE/COFF driver image, uncompressed
//! ```
//!
//! The whole image is padded to 512-byte units.

use std::fmt;

const EFI_ROM_SIGNATURE: u32 = 0x0ef1;
const SUBSYSTEM_BOOT_SERVICE_DRIVER: u16 = 11;
const MACHINE_X64: u16 = 0x8664;
const PCIR_OFFSET: usize = 0x1c;
const PCIR_LENGTH: u16 = 0x1c;
const IMAGE_OFFSET: usize = 0x200;
const CODE_TYPE_EFI: u8 = 3;
const LAST_IMAGE: u8 = 0x80;
const UNIT: usize = 512;

/// Why a driver image cannot be packed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RomError(String);

impl fmt::Display for RomError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for RomError {}

fn u16_at(data: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        data.get(at..at.checked_add(2)?)?.try_into().ok()?,
    ))
}

fn u32_at(data: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        data.get(at..at.checked_add(4)?)?.try_into().ok()?,
    ))
}

/// Checks that `pe` is an x86-64 UEFI boot service driver.
fn check_driver(pe: &[u8]) -> Result<(), RomError> {
    let err = |what: &str| RomError(format!("not a UEFI x64 driver: {what}"));
    if pe.get(..2) != Some(b"MZ") {
        return Err(err("no MZ header"));
    }
    let header = u32_at(pe, 0x3c).ok_or_else(|| err("truncated"))? as usize;
    if pe.get(header..header.saturating_add(4)) != Some(b"PE\0\0") {
        return Err(err("no PE signature"));
    }
    if u16_at(pe, header + 4) != Some(MACHINE_X64) {
        return Err(err("not x86-64"));
    }
    let optional = header + 24;
    if u16_at(pe, optional) != Some(0x20b) {
        return Err(err("not PE32+"));
    }
    if u16_at(pe, optional + 68) != Some(SUBSYSTEM_BOOT_SERVICE_DRIVER) {
        return Err(err("subsystem is not EFI boot service driver (11)"));
    }
    Ok(())
}

/// Packs `pe` into an option ROM for the given PCI function.
pub fn build(pe: &[u8], vendor: u16, device: u16, class_code: u32) -> Result<Vec<u8>, RomError> {
    check_driver(pe)?;
    let total = (IMAGE_OFFSET + pe.len()).next_multiple_of(UNIT);
    let units =
        u16::try_from(total / UNIT).map_err(|_| RomError("driver image is too large".into()))?;

    let mut rom = vec![0u8; total];
    rom[0] = 0x55;
    rom[1] = 0xaa;
    rom[0x02..0x04].copy_from_slice(&units.to_le_bytes()); // initialization size
    rom[0x04..0x08].copy_from_slice(&EFI_ROM_SIGNATURE.to_le_bytes());
    rom[0x08..0x0a].copy_from_slice(&SUBSYSTEM_BOOT_SERVICE_DRIVER.to_le_bytes());
    rom[0x0a..0x0c].copy_from_slice(&MACHINE_X64.to_le_bytes());
    // 0x0c: compression type 0 (none); 0x0e..0x16 reserved.
    rom[0x16..0x18].copy_from_slice(&(IMAGE_OFFSET as u16).to_le_bytes());
    rom[0x18..0x1a].copy_from_slice(&(PCIR_OFFSET as u16).to_le_bytes());

    let pcir = &mut rom[PCIR_OFFSET..PCIR_OFFSET + usize::from(PCIR_LENGTH)];
    pcir[0x00..0x04].copy_from_slice(b"PCIR");
    pcir[0x04..0x06].copy_from_slice(&vendor.to_le_bytes());
    pcir[0x06..0x08].copy_from_slice(&device.to_le_bytes());
    pcir[0x0a..0x0c].copy_from_slice(&PCIR_LENGTH.to_le_bytes());
    pcir[0x0c] = 3; // PCI data structure revision 3.0
    pcir[0x0d..0x10].copy_from_slice(&class_code.to_le_bytes()[..3]);
    pcir[0x10..0x12].copy_from_slice(&units.to_le_bytes()); // image length
    pcir[0x14] = CODE_TYPE_EFI;
    pcir[0x15] = LAST_IMAGE;

    rom[IMAGE_OFFSET..IMAGE_OFFSET + pe.len()].copy_from_slice(pe);
    Ok(rom)
}

/// Size of the ROM BAR for a ROM of `len` bytes: a power of two, at least
/// the 2 KiB granularity of the PCI ROM base address register.
pub fn bar_size(len: usize) -> u32 {
    u32::try_from(len.max(2048).next_power_of_two()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Smallest PE32+ header the checks accept.
    fn fake_driver(subsystem: u16, len: usize) -> Vec<u8> {
        let mut pe = vec![0u8; len];
        pe[0..2].copy_from_slice(b"MZ");
        pe[0x3c..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        pe[0x80..0x84].copy_from_slice(b"PE\0\0");
        pe[0x84..0x86].copy_from_slice(&MACHINE_X64.to_le_bytes());
        pe[0x98..0x9a].copy_from_slice(&0x20bu16.to_le_bytes());
        pe[0x98 + 68..0x98 + 70].copy_from_slice(&subsystem.to_le_bytes());
        pe
    }

    #[test]
    fn layout() {
        let pe = fake_driver(11, 7168);
        let rom = build(&pe, 0x15ad, 0x0405, 0x03_0000).unwrap();
        assert_eq!(rom.len(), 0x200 + 7168);
        assert_eq!(&rom[..2], &[0x55, 0xaa]);
        assert_eq!(u16_at(&rom, 2), Some((rom.len() / 512) as u16));
        assert_eq!(u32_at(&rom, 4), Some(0x0ef1));
        assert_eq!(u16_at(&rom, 8), Some(11));
        assert_eq!(u16_at(&rom, 0x16), Some(0x200));
        let pcir = u16_at(&rom, 0x18).unwrap() as usize;
        assert_eq!(&rom[pcir..pcir + 4], b"PCIR");
        assert_eq!(
            (u16_at(&rom, pcir + 4), u16_at(&rom, pcir + 6)),
            (Some(0x15ad), Some(0x0405))
        );
        assert_eq!(&rom[pcir + 0x0d..pcir + 0x10], &[0, 0, 3]);
        assert_eq!((rom[pcir + 0x14], rom[pcir + 0x15]), (3, 0x80));
        assert_eq!(&rom[0x200..0x202], b"MZ");
    }

    #[test]
    fn applications_and_garbage_are_refused() {
        assert!(build(&fake_driver(10, 4096), 0x15ad, 0x0405, 0x03_0000).is_err());
        assert!(build(b"not a PE", 0x15ad, 0x0405, 0x03_0000).is_err());
        let mut pe = fake_driver(11, 4096);
        pe[0x3c..0x40].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(build(&pe, 0x15ad, 0x0405, 0x03_0000).is_err());
    }

    #[test]
    fn bar_sizes() {
        assert_eq!(bar_size(100), 2048);
        assert_eq!(bar_size(7680), 8192);
        assert_eq!(bar_size(8192), 8192);
    }
}
