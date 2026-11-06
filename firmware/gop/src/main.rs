//! UEFI Graphics Output Protocol driver for the rsvga card.
//!
//! OVMF's own video driver handles VMware cards through Bochs VBE ports that
//! vfio cannot forward, so rsvga ships this driver in its PCI option ROM. It
//! talks to the real SVGA registers and gives the firmware, the Windows boot
//! loader and Windows' basic display driver a linear framebuffer.
//!
//! Built for `x86_64-unknown-uefi`; on the host only the pure modules
//! (`modes`, `blt`) are compiled, for their tests.

#![cfg_attr(target_os = "uefi", no_std)]
#![cfg_attr(target_os = "uefi", no_main)]

mod blt;
mod modes;

#[cfg(target_os = "uefi")]
mod driver;

#[cfg(target_os = "uefi")]
#[unsafe(no_mangle)]
extern "efiapi" fn efi_main(
    image: r_efi::efi::Handle,
    system_table: *mut r_efi::efi::SystemTable,
) -> r_efi::efi::Status {
    driver::entry(image, system_table)
}

#[cfg(target_os = "uefi")]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}

#[cfg(not(target_os = "uefi"))]
fn main() {
    eprintln!("rsvga-gop is a UEFI driver: build it with --target x86_64-unknown-uefi");
}
