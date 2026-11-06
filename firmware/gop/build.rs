//! Marks the PE image as a UEFI boot service driver (subsystem 11) rather
//! than an application: an application image is unloaded as soon as its
//! entry point returns, which would leave our protocol callbacks dangling.
//! Done here rather than in .cargo/config.toml so that it applies wherever
//! cargo is run from.
fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("uefi") {
        println!("cargo:rustc-link-arg-bins=/subsystem:efi_boot_service_driver");
    }
}
