//! Server side of the vfio-user protocol, which lets a VMM such as QEMU
//! (`-device vfio-user-pci`) use a PCI device emulated in another process.
//!
//! - [`wire`]: message formats, pure and allocation-bounded;
//! - [`server`]: framing, file descriptor passing and the request loop.
//!
//! Only the subset needed by a single-client device server is implemented:
//! no twin sockets, no server-to-client commands, no migration.

pub mod server;
pub mod wire;

pub use server::{
    Connection, Device, DeviceInfo, DmaMapping, IrqInfo, IrqSet, Message, RegionInfo, ServeError,
    serve,
};
pub use wire::{Errno, Header, Request};
