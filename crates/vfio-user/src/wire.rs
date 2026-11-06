//! Message formats of the vfio-user protocol (version 0.0).
//!
//! Everything here is pure: bytes in, typed values out, and back. Messages
//! come from the VMM, which is trusted more than the guest but still parsed
//! defensively: lengths are checked before any field is read, and data sizes
//! are bounded by the negotiated maximum transfer size.

use core::fmt;

/// Size of the header preceding every message.
pub const HEADER_LEN: usize = 16;
/// [`HEADER_LEN`] as the header's own size type.
pub const HEADER_SIZE: u32 = 16;

/// Command codes (`enum vfio_user_command`).
pub mod command {
    #![allow(missing_docs)]
    pub const VERSION: u16 = 1;
    pub const DMA_MAP: u16 = 2;
    pub const DMA_UNMAP: u16 = 3;
    pub const DEVICE_GET_INFO: u16 = 4;
    pub const DEVICE_GET_REGION_INFO: u16 = 5;
    pub const DEVICE_GET_REGION_IO_FDS: u16 = 6;
    pub const DEVICE_GET_IRQ_INFO: u16 = 7;
    pub const DEVICE_SET_IRQS: u16 = 8;
    pub const REGION_READ: u16 = 9;
    pub const REGION_WRITE: u16 = 10;
    pub const DMA_READ: u16 = 11;
    pub const DMA_WRITE: u16 = 12;
    pub const DEVICE_RESET: u16 = 13;
    pub const DIRTY_PAGES: u16 = 14;
    pub const REGION_WRITE_MULTI: u16 = 15;
    pub const DEVICE_FEATURE: u16 = 16;
}

/// Header flags.
pub mod flags {
    /// Message type: command.
    pub const REQUEST: u32 = 0x0;
    /// Message type: reply.
    pub const REPLY: u32 = 0x1;
    /// Mask of the message type bits.
    pub const TYPE_MASK: u32 = 0xf;
    /// The sender does not want a reply to this command.
    pub const NO_REPLY: u32 = 0x10;
    /// The reply reports an error; the header's error field holds an errno.
    pub const ERROR: u32 = 0x20;
}

/// `VFIO_DEVICE_FLAGS_*`.
pub mod device_flags {
    /// The device supports `DEVICE_RESET`.
    pub const RESET: u32 = 1 << 0;
    /// The device is a PCI device.
    pub const PCI: u32 = 1 << 1;
}

/// `VFIO_REGION_INFO_FLAG_*`.
pub mod region_flags {
    /// The client may read the region.
    pub const READ: u32 = 1 << 0;
    /// The client may write the region.
    pub const WRITE: u32 = 1 << 1;
    /// The client may mmap the region (a file descriptor comes with the reply).
    pub const MMAP: u32 = 1 << 2;
    /// The reply carries a capability chain.
    pub const CAPS: u32 = 1 << 3;
}

/// `VFIO_IRQ_INFO_*`.
pub mod irq_info_flags {
    #![allow(missing_docs)]
    pub const EVENTFD: u32 = 1 << 0;
    pub const MASKABLE: u32 = 1 << 1;
    pub const AUTOMASKED: u32 = 1 << 2;
    pub const NORESIZE: u32 = 1 << 3;
}

/// `VFIO_IRQ_SET_*`.
pub mod irq_set_flags {
    #![allow(missing_docs)]
    pub const DATA_NONE: u32 = 1 << 0;
    pub const DATA_BOOL: u32 = 1 << 1;
    pub const DATA_EVENTFD: u32 = 1 << 2;
    pub const ACTION_MASK: u32 = 1 << 3;
    pub const ACTION_UNMASK: u32 = 1 << 4;
    pub const ACTION_TRIGGER: u32 = 1 << 5;
}

/// Flags of `DMA_MAP`.
pub mod dma_flags {
    #![allow(missing_docs)]
    pub const READ: u32 = 1 << 0;
    pub const WRITE: u32 = 1 << 1;
    pub const MODE_MMAP: u32 = 1 << 2;
    pub const MODE_FILE_IO: u32 = 1 << 3;
}

/// Region indices of a PCI device (`VFIO_PCI_*_REGION_INDEX`).
pub mod pci_region {
    #![allow(missing_docs)]
    pub const BAR0: u32 = 0;
    pub const BAR1: u32 = 1;
    pub const BAR2: u32 = 2;
    pub const BAR3: u32 = 3;
    pub const BAR4: u32 = 4;
    pub const BAR5: u32 = 5;
    pub const ROM: u32 = 6;
    pub const CONFIG: u32 = 7;
    pub const VGA: u32 = 8;
    /// Number of standard PCI regions.
    pub const COUNT: u32 = 9;
}

/// Interrupt indices of a PCI device (`VFIO_PCI_*_IRQ_INDEX`).
pub mod pci_irq {
    #![allow(missing_docs)]
    pub const INTX: u32 = 0;
    pub const MSI: u32 = 1;
    pub const MSIX: u32 = 2;
    pub const ERR: u32 = 3;
    pub const REQ: u32 = 4;
    /// Number of standard PCI interrupt types.
    pub const COUNT: u32 = 5;
}

/// An errno value carried by error replies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Errno(pub u32);

impl Errno {
    /// No such entry.
    pub const ENOENT: Self = Self(2);
    /// Argument list too long.
    pub const E2BIG: Self = Self(7);
    /// Bad address.
    pub const EFAULT: Self = Self(14);
    /// Already exists.
    pub const EEXIST: Self = Self(17);
    /// Invalid argument.
    pub const EINVAL: Self = Self(22);
    /// Function not implemented.
    pub const ENOSYS: Self = Self(38);
    /// Operation not supported.
    pub const ENOTSUP: Self = Self(95);
}

/// The 16-byte message header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    /// Message ID, echoed in the reply.
    pub id: u16,
    /// Command code.
    pub command: u16,
    /// Size of the whole message, header included.
    pub size: u32,
    /// See [`flags`].
    pub flags: u32,
    /// Errno of an error reply.
    pub error: u32,
}

impl Header {
    /// Decodes a header.
    pub fn parse(bytes: &[u8; HEADER_LEN]) -> Self {
        let mut r = Le(bytes);
        Self {
            id: r.u16(),
            command: r.u16(),
            size: r.u32(),
            flags: r.u32(),
            error: r.u32(),
        }
    }

    /// Encodes the header.
    pub fn encode(&self) -> [u8; HEADER_LEN] {
        let mut out = [0u8; HEADER_LEN];
        out[0..2].copy_from_slice(&self.id.to_le_bytes());
        out[2..4].copy_from_slice(&self.command.to_le_bytes());
        out[4..8].copy_from_slice(&self.size.to_le_bytes());
        out[8..12].copy_from_slice(&self.flags.to_le_bytes());
        out[12..16].copy_from_slice(&self.error.to_le_bytes());
        out
    }

    /// Whether this is a reply rather than a command.
    pub fn is_reply(&self) -> bool {
        self.flags & flags::TYPE_MASK == flags::REPLY
    }

    /// Whether the sender asked not to be answered.
    pub fn no_reply(&self) -> bool {
        self.flags & flags::NO_REPLY != 0
    }
}

/// A decoded command from the client.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    /// `VERSION`: protocol negotiation, always the first message.
    Version {
        /// Proposed major version.
        major: u16,
        /// Proposed minor version.
        minor: u16,
        /// Optional JSON capabilities, without the terminating NUL.
        capabilities: Option<String>,
    },
    /// `DMA_MAP`: a window of client memory the server may access.
    DmaMap {
        /// See [`dma_flags`].
        flags: u32,
        /// Offset in the accompanying file descriptor.
        offset: u64,
        /// DMA address of the window.
        address: u64,
        /// Size of the window.
        size: u64,
    },
    /// `DMA_UNMAP`: a previously mapped window goes away.
    DmaUnmap {
        /// Reply buffer size wanted by the client.
        argsz: u32,
        /// Unused in this protocol version.
        flags: u32,
        /// DMA address of the window.
        address: u64,
        /// Size of the window.
        size: u64,
    },
    /// `DEVICE_GET_INFO`.
    DeviceGetInfo {
        /// Reply buffer size wanted by the client.
        argsz: u32,
    },
    /// `DEVICE_GET_REGION_INFO`.
    GetRegionInfo {
        /// Reply buffer size wanted by the client.
        argsz: u32,
        /// Region index.
        index: u32,
    },
    /// `DEVICE_GET_IRQ_INFO`.
    GetIrqInfo {
        /// Reply buffer size wanted by the client.
        argsz: u32,
        /// Interrupt type index.
        index: u32,
    },
    /// `DEVICE_SET_IRQS`.
    SetIrqs {
        /// See [`irq_set_flags`].
        flags: u32,
        /// Interrupt type index.
        index: u32,
        /// First sub-index.
        start: u32,
        /// Number of sub-indices.
        count: u32,
        /// Optional data (booleans); event fds travel as ancillary data.
        data: Vec<u8>,
    },
    /// `REGION_READ`.
    RegionRead {
        /// Offset inside the region.
        offset: u64,
        /// Region index.
        region: u32,
        /// Number of bytes to read.
        count: u32,
    },
    /// `REGION_WRITE`.
    RegionWrite {
        /// Offset inside the region.
        offset: u64,
        /// Region index.
        region: u32,
        /// Bytes to write.
        data: Vec<u8>,
    },
    /// `DEVICE_RESET`.
    DeviceReset,
    /// A command this server does not implement.
    Unsupported(u16),
}

/// Why a message could not be decoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireError {
    /// The payload is shorter than the command's fixed part.
    ShortPayload {
        /// Command code.
        command: u16,
        /// Actual payload length.
        len: usize,
    },
    /// A data transfer exceeds the negotiated maximum.
    TransferTooLarge(u64),
    /// A `REGION_WRITE` count does not match its data.
    CountMismatch {
        /// Announced count.
        count: u32,
        /// Bytes actually present.
        data: usize,
    },
    /// The version capabilities are not NUL-terminated UTF-8.
    BadCapabilities,
}

impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::ShortPayload { command, len } => {
                write!(
                    f,
                    "payload of {len} bytes is too short for command {command}"
                )
            }
            Self::TransferTooLarge(count) => write!(f, "transfer of {count} bytes is too large"),
            Self::CountMismatch { count, data } => {
                write!(f, "write announces {count} bytes but carries {data}")
            }
            Self::BadCapabilities => write!(f, "version capabilities are malformed"),
        }
    }
}

impl std::error::Error for WireError {}

/// Little-endian reader over a byte slice whose length was checked first.
struct Le<'a>(&'a [u8]);

impl Le<'_> {
    fn take<const N: usize>(&mut self) -> [u8; N] {
        let mut out = [0u8; N];
        if let Some((head, rest)) = self.0.split_at_checked(N) {
            out.copy_from_slice(head);
            self.0 = rest;
        }
        out
    }
    fn u16(&mut self) -> u16 {
        u16::from_le_bytes(self.take())
    }
    fn u32(&mut self) -> u32 {
        u32::from_le_bytes(self.take())
    }
    fn u64(&mut self) -> u64 {
        u64::from_le_bytes(self.take())
    }
}

fn need(command: u16, payload: &[u8], len: usize) -> Result<(), WireError> {
    if payload.len() < len {
        return Err(WireError::ShortPayload {
            command,
            len: payload.len(),
        });
    }
    Ok(())
}

/// Decodes the payload of a command. `max_xfer` bounds region transfers.
pub fn parse_request(command: u16, payload: &[u8], max_xfer: u32) -> Result<Request, WireError> {
    let mut r = Le(payload);
    match command {
        command::VERSION => {
            need(command, payload, 4)?;
            let (major, minor) = (r.u16(), r.u16());
            let rest = payload.get(4..).unwrap_or_default();
            let capabilities = if rest.is_empty() {
                None
            } else {
                let (nul, text) = rest.split_last().ok_or(WireError::BadCapabilities)?;
                if *nul != 0 || text.contains(&0) {
                    return Err(WireError::BadCapabilities);
                }
                let text = core::str::from_utf8(text).map_err(|_| WireError::BadCapabilities)?;
                Some(text.to_owned())
            };
            Ok(Request::Version {
                major,
                minor,
                capabilities,
            })
        }
        command::DMA_MAP => {
            need(command, payload, 32)?;
            let (_argsz, flags) = (r.u32(), r.u32());
            Ok(Request::DmaMap {
                flags,
                offset: r.u64(),
                address: r.u64(),
                size: r.u64(),
            })
        }
        command::DMA_UNMAP => {
            need(command, payload, 24)?;
            let (argsz, flags) = (r.u32(), r.u32());
            Ok(Request::DmaUnmap {
                argsz,
                flags,
                address: r.u64(),
                size: r.u64(),
            })
        }
        command::DEVICE_GET_INFO => {
            need(command, payload, 4)?;
            Ok(Request::DeviceGetInfo { argsz: r.u32() })
        }
        command::DEVICE_GET_REGION_INFO => {
            need(command, payload, 12)?;
            let (argsz, _flags, index) = (r.u32(), r.u32(), r.u32());
            Ok(Request::GetRegionInfo { argsz, index })
        }
        command::DEVICE_GET_IRQ_INFO => {
            need(command, payload, 12)?;
            let (argsz, _flags, index) = (r.u32(), r.u32(), r.u32());
            Ok(Request::GetIrqInfo { argsz, index })
        }
        command::DEVICE_SET_IRQS => {
            need(command, payload, 20)?;
            let (_argsz, flags, index, start, count) =
                (r.u32(), r.u32(), r.u32(), r.u32(), r.u32());
            let data = payload.get(20..).unwrap_or_default();
            if data.len() > max_xfer as usize {
                return Err(WireError::TransferTooLarge(data.len() as u64));
            }
            Ok(Request::SetIrqs {
                flags,
                index,
                start,
                count,
                data: data.to_vec(),
            })
        }
        command::REGION_READ => {
            need(command, payload, 16)?;
            let (offset, region, count) = (r.u64(), r.u32(), r.u32());
            if count > max_xfer {
                return Err(WireError::TransferTooLarge(u64::from(count)));
            }
            Ok(Request::RegionRead {
                offset,
                region,
                count,
            })
        }
        command::REGION_WRITE => {
            need(command, payload, 16)?;
            let (offset, region, count) = (r.u64(), r.u32(), r.u32());
            if count > max_xfer {
                return Err(WireError::TransferTooLarge(u64::from(count)));
            }
            let data = payload.get(16..).unwrap_or_default();
            if data.len() != count as usize {
                return Err(WireError::CountMismatch {
                    count,
                    data: data.len(),
                });
            }
            Ok(Request::RegionWrite {
                offset,
                region,
                data: data.to_vec(),
            })
        }
        command::DEVICE_RESET => Ok(Request::DeviceReset),
        other => Ok(Request::Unsupported(other)),
    }
}

/// Builds a reply to `request` carrying `payload`.
pub fn reply(request: &Header, payload: &[u8]) -> Vec<u8> {
    let size = u32::try_from(HEADER_LEN.saturating_add(payload.len())).unwrap_or(u32::MAX);
    let header = Header {
        id: request.id,
        command: request.command,
        size,
        flags: flags::REPLY,
        error: 0,
    };
    let mut out = Vec::with_capacity(HEADER_LEN.saturating_add(payload.len()));
    out.extend_from_slice(&header.encode());
    out.extend_from_slice(payload);
    out
}

/// Builds an error reply to `request`. Error replies carry no payload.
pub fn error_reply(request: &Header, errno: Errno) -> Vec<u8> {
    let header = Header {
        id: request.id,
        command: request.command,
        size: HEADER_SIZE,
        flags: flags::REPLY | flags::ERROR,
        error: errno.0,
    };
    header.encode().to_vec()
}

/// Little-endian writer for reply payloads.
#[derive(Default)]
pub struct Payload(Vec<u8>);

impl Payload {
    /// Appends a `u16`.
    pub fn u16(mut self, value: u16) -> Self {
        self.0.extend_from_slice(&value.to_le_bytes());
        self
    }
    /// Appends a `u32`.
    pub fn u32(mut self, value: u32) -> Self {
        self.0.extend_from_slice(&value.to_le_bytes());
        self
    }
    /// Appends a `u64`.
    pub fn u64(mut self, value: u64) -> Self {
        self.0.extend_from_slice(&value.to_le_bytes());
        self
    }
    /// Appends raw bytes.
    pub fn bytes(mut self, value: &[u8]) -> Self {
        self.0.extend_from_slice(value);
        self
    }
    /// The encoded payload.
    pub fn finish(self) -> Vec<u8> {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_round_trip() {
        let header = Header {
            id: 7,
            command: 5,
            size: 48,
            flags: flags::NO_REPLY,
            error: 0,
        };
        assert_eq!(Header::parse(&header.encode()), header);
        assert!(header.no_reply() && !header.is_reply());
    }

    #[test]
    fn version_with_capabilities() {
        let mut payload = Payload::default().u16(0).u16(0).finish();
        payload.extend_from_slice(b"{\"capabilities\":{}}\0");
        assert_eq!(
            parse_request(command::VERSION, &payload, 4096),
            Ok(Request::Version {
                major: 0,
                minor: 0,
                capabilities: Some("{\"capabilities\":{}}".into())
            })
        );
        let unterminated = Payload::default().u16(0).u16(0).bytes(b"{}").finish();
        assert_eq!(
            parse_request(command::VERSION, &unterminated, 4096),
            Err(WireError::BadCapabilities)
        );
    }

    #[test]
    fn short_payloads_are_rejected() {
        for command in [
            command::VERSION,
            command::DMA_MAP,
            command::DMA_UNMAP,
            command::DEVICE_GET_INFO,
            command::DEVICE_GET_REGION_INFO,
            command::DEVICE_GET_IRQ_INFO,
            command::DEVICE_SET_IRQS,
            command::REGION_READ,
            command::REGION_WRITE,
        ] {
            assert!(matches!(
                parse_request(command, &[0; 3], 4096),
                Err(WireError::ShortPayload { .. })
            ));
        }
    }

    #[test]
    fn region_write_count_must_match_data() {
        let ok = Payload::default()
            .u64(1)
            .u32(0)
            .u32(4)
            .u32(0xdead_beef)
            .finish();
        assert_eq!(
            parse_request(command::REGION_WRITE, &ok, 4096),
            Ok(Request::RegionWrite {
                offset: 1,
                region: 0,
                data: 0xdead_beef_u32.to_le_bytes().to_vec()
            })
        );
        let bad = Payload::default().u64(1).u32(0).u32(8).u32(0).finish();
        assert_eq!(
            parse_request(command::REGION_WRITE, &bad, 4096),
            Err(WireError::CountMismatch { count: 8, data: 4 })
        );
    }

    #[test]
    fn transfers_are_bounded() {
        let read = Payload::default().u64(0).u32(1).u32(8192).finish();
        assert_eq!(
            parse_request(command::REGION_READ, &read, 4096),
            Err(WireError::TransferTooLarge(8192))
        );
    }

    #[test]
    fn replies() {
        let request = Header {
            id: 3,
            command: 4,
            size: 20,
            flags: 0,
            error: 0,
        };
        let ok = reply(&request, &[1, 2, 3, 4]);
        let header = Header::parse(ok[..16].try_into().unwrap());
        assert_eq!(
            (header.id, header.size, header.flags),
            (3, 20, flags::REPLY)
        );
        let err = error_reply(&request, Errno::EINVAL);
        let header = Header::parse(err[..16].try_into().unwrap());
        assert_eq!(
            (header.size, header.flags, header.error),
            (16, flags::REPLY | flags::ERROR, 22)
        );
    }
}
