//! Serving one device to one client over a connected Unix stream socket.

use std::fmt;
use std::io::{self, IoSlice, IoSliceMut, Read, Write};
use std::mem::MaybeUninit;
use std::os::fd::{BorrowedFd, OwnedFd};
use std::os::unix::net::UnixStream;

use rustix::net::{
    RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, ReturnFlags, SendAncillaryBuffer,
    SendAncillaryMessage, SendFlags, recvmsg, sendmsg,
};

use crate::wire::{
    Errno, HEADER_LEN, HEADER_SIZE, Header, Payload, Request, error_reply, parse_request, reply,
};

/// Most file descriptors accepted in one message.
pub const MAX_FDS: usize = 8;
/// Largest data transfer accepted in one region access.
pub const MAX_XFER: u32 = 1024 * 1024;
/// Largest message accepted, header included.
pub const MAX_MESSAGE: u32 = MAX_XFER + 4096;

/// Capabilities sent in the `VERSION` reply. Each is a subset of what QEMU
/// proposes; `max_msg_fds` is what *this* side can receive.
pub const CAPABILITIES: &str = concat!(
    "{\"capabilities\":{",
    "\"max_msg_fds\":8,",
    "\"max_data_xfer_size\":1048576,",
    "\"pgsizes\":4096,",
    "\"max_dma_maps\":65535",
    "}}"
);

/// Answer to `DEVICE_GET_INFO`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeviceInfo {
    /// See [`crate::wire::device_flags`].
    pub flags: u32,
    /// Number of regions.
    pub num_regions: u32,
    /// Number of interrupt types.
    pub num_irqs: u32,
}

/// Answer to `DEVICE_GET_REGION_INFO`.
#[derive(Clone, Copy, Debug)]
pub struct RegionInfo<'a> {
    /// See [`crate::wire::region_flags`].
    pub flags: u32,
    /// Size of the region in bytes.
    pub size: u64,
    /// For mappable regions: the file to mmap, starting at offset 0.
    pub fd: Option<BorrowedFd<'a>>,
}

/// Answer to `DEVICE_GET_IRQ_INFO`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IrqInfo {
    /// See [`crate::wire::irq_info_flags`].
    pub flags: u32,
    /// Number of interrupts of this type.
    pub count: u32,
}

/// A window of client memory announced by `DMA_MAP`.
#[derive(Debug)]
pub struct DmaMapping {
    /// DMA address of the window.
    pub address: u64,
    /// Size of the window in bytes.
    pub size: u64,
    /// Offset of the window inside `fd`.
    pub offset: u64,
    /// See [`crate::wire::dma_flags`].
    pub flags: u32,
    /// Backing file, when the client shares its memory.
    pub fd: Option<OwnedFd>,
}

/// A `DEVICE_SET_IRQS` request.
#[derive(Debug)]
pub struct IrqSet {
    /// See [`crate::wire::irq_set_flags`].
    pub flags: u32,
    /// Interrupt type index.
    pub index: u32,
    /// First sub-index.
    pub start: u32,
    /// Number of sub-indices.
    pub count: u32,
    /// Inline data (booleans).
    pub data: Vec<u8>,
    /// Event file descriptors.
    pub fds: Vec<OwnedFd>,
}

/// A device served over vfio-user. Errors are reported to the client as
/// errno values; they never end the connection.
pub trait Device {
    /// Basic device description.
    fn info(&self) -> DeviceInfo;
    /// Description of region `index`, or `None` if it does not exist.
    fn region_info(&self, index: u32) -> Option<RegionInfo<'_>>;
    /// Description of interrupt type `index`, or `None` if it does not exist.
    fn irq_info(&self, index: u32) -> Option<IrqInfo>;
    /// Reads `data.len()` bytes at `offset` of `region`.
    fn region_read(&mut self, region: u32, offset: u64, data: &mut [u8]) -> Result<(), Errno>;
    /// Writes `data` at `offset` of `region`.
    fn region_write(&mut self, region: u32, offset: u64, data: &[u8]) -> Result<(), Errno>;
    /// Accepts a window of client memory.
    fn dma_map(&mut self, mapping: DmaMapping) -> Result<(), Errno>;
    /// Releases a window previously accepted by [`Device::dma_map`].
    fn dma_unmap(&mut self, address: u64, size: u64) -> Result<(), Errno>;
    /// Configures interrupts.
    fn set_irqs(&mut self, set: IrqSet) -> Result<(), Errno>;
    /// Resets the device.
    fn reset(&mut self) -> Result<(), Errno>;
}

/// Why serving stopped.
#[derive(Debug)]
pub enum ServeError {
    /// The socket failed.
    Io(io::Error),
    /// The client broke the protocol; the connection is closed.
    Protocol(String),
}

impl fmt::Display for ServeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "vfio-user socket error: {error}"),
            Self::Protocol(reason) => write!(f, "vfio-user protocol violation: {reason}"),
        }
    }
}

impl std::error::Error for ServeError {}

impl From<io::Error> for ServeError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// One received message.
#[derive(Debug)]
pub struct Message {
    /// Its header.
    pub header: Header,
    /// Everything after the header.
    pub payload: Vec<u8>,
    /// File descriptors that came with it.
    pub fds: Vec<OwnedFd>,
}

/// Message framing over a stream socket, with file descriptor passing.
pub struct Connection {
    stream: UnixStream,
}

impl Connection {
    /// Wraps a connected socket.
    pub fn new(stream: UnixStream) -> Self {
        Self { stream }
    }

    /// Receives the next message, or `None` once the peer has hung up.
    pub fn recv(&mut self) -> Result<Option<Message>, ServeError> {
        let mut header = [0u8; HEADER_LEN];
        let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(MAX_FDS))];
        let mut control = RecvAncillaryBuffer::new(&mut space);
        let received = recvmsg(
            &self.stream,
            &mut [IoSliceMut::new(&mut header)],
            &mut control,
            RecvFlags::CMSG_CLOEXEC,
        )
        .map_err(io::Error::from)?;
        if received.bytes == 0 {
            return Ok(None);
        }
        let mut fds = Vec::new();
        for message in control.drain() {
            if let RecvAncillaryMessage::ScmRights(received_fds) = message {
                fds.extend(received_fds);
            }
        }
        if received.flags.contains(ReturnFlags::CTRUNC) {
            return Err(ServeError::Protocol(format!(
                "more than {MAX_FDS} file descriptors"
            )));
        }
        if let Some(rest) = header.get_mut(received.bytes..) {
            self.stream.read_exact(rest)?;
        }

        let header = Header::parse(&header);
        if header.is_reply() {
            return Err(ServeError::Protocol(
                "unexpected reply from the client".into(),
            ));
        }
        if header.size < HEADER_SIZE || header.size > MAX_MESSAGE {
            return Err(ServeError::Protocol(format!(
                "bad message size {}",
                header.size
            )));
        }
        let mut payload = vec![0u8; (header.size as usize).saturating_sub(HEADER_LEN)];
        self.stream.read_exact(&mut payload)?;
        Ok(Some(Message {
            header,
            payload,
            fds,
        }))
    }

    /// Sends `bytes`, attaching `fds` to the first byte.
    pub fn send(&mut self, bytes: &[u8], fds: &[BorrowedFd<'_>]) -> io::Result<()> {
        if fds.is_empty() {
            return self.stream.write_all(bytes);
        }
        let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(MAX_FDS))];
        let mut control = SendAncillaryBuffer::new(&mut space);
        if fds.len() > MAX_FDS || !control.push(SendAncillaryMessage::ScmRights(fds)) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "too many fds"));
        }
        let sent = sendmsg(
            &self.stream,
            &[IoSlice::new(bytes)],
            &mut control,
            SendFlags::empty(),
        )
        .map_err(io::Error::from)?;
        self.stream.write_all(bytes.get(sent..).unwrap_or_default())
    }
}

/// What a handled request produces.
enum Outcome<'a> {
    Reply(Vec<u8>, Option<BorrowedFd<'a>>),
    Error(Errno),
}

/// Serves `device` until the client disconnects or breaks the protocol.
pub fn serve<D: Device + ?Sized>(stream: UnixStream, device: &mut D) -> Result<(), ServeError> {
    let mut conn = Connection::new(stream);
    let mut negotiated = false;

    while let Some(Message {
        header,
        payload,
        fds,
    }) = conn.recv()?
    {
        let request = match parse_request(header.command, &payload, MAX_XFER) {
            Ok(request) => request,
            Err(error) => {
                if !negotiated {
                    return Err(ServeError::Protocol(error.to_string()));
                }
                if !header.no_reply() {
                    conn.send(&error_reply(&header, Errno::EINVAL), &[])?;
                }
                continue;
            }
        };

        if let Request::Version {
            major,
            minor: _,
            capabilities: _,
        } = request
        {
            if negotiated || major != 0 {
                conn.send(&error_reply(&header, Errno::EINVAL), &[])?;
                return Err(ServeError::Protocol(format!("unsupported version {major}")));
            }
            let mut body = Payload::default()
                .u16(0)
                .u16(0)
                .bytes(CAPABILITIES.as_bytes())
                .finish();
            body.push(0);
            conn.send(&reply(&header, &body), &[])?;
            negotiated = true;
            continue;
        }
        if !negotiated {
            return Err(ServeError::Protocol(
                "the first message must be VERSION".into(),
            ));
        }

        let outcome = handle(device, request, fds);
        if header.no_reply() {
            continue;
        }
        match outcome {
            Outcome::Reply(body, fd) => {
                let fds: Vec<BorrowedFd<'_>> = fd.into_iter().collect();
                conn.send(&reply(&header, &body), &fds)?;
            }
            Outcome::Error(errno) => conn.send(&error_reply(&header, errno), &[])?,
        }
    }
    Ok(())
}

fn done(result: Result<(), Errno>) -> Outcome<'static> {
    match result {
        Ok(()) => Outcome::Reply(Vec::new(), None),
        Err(errno) => Outcome::Error(errno),
    }
}

fn handle<D: Device + ?Sized>(
    device: &mut D,
    request: Request,
    mut fds: Vec<OwnedFd>,
) -> Outcome<'_> {
    match request {
        Request::DeviceGetInfo { argsz: _ } => {
            let info = device.info();
            let body = Payload::default()
                .u32(16)
                .u32(info.flags)
                .u32(info.num_regions)
                .u32(info.num_irqs)
                .finish();
            Outcome::Reply(body, None)
        }
        Request::GetRegionInfo { argsz: _, index } => match device.region_info(index) {
            Some(info) => {
                let body = Payload::default()
                    .u32(32)
                    .u32(info.flags)
                    .u32(index)
                    .u32(0) // no capability chain
                    .u64(info.size)
                    .u64(0) // mmap offset inside `fd`
                    .finish();
                Outcome::Reply(body, info.fd)
            }
            None => Outcome::Error(Errno::EINVAL),
        },
        Request::GetIrqInfo { argsz: _, index } => match device.irq_info(index) {
            Some(info) => {
                let body = Payload::default()
                    .u32(16)
                    .u32(info.flags)
                    .u32(index)
                    .u32(info.count)
                    .finish();
                Outcome::Reply(body, None)
            }
            None => Outcome::Error(Errno::EINVAL),
        },
        Request::RegionRead {
            offset,
            region,
            count,
        } => {
            let mut data = vec![0u8; count as usize];
            match device.region_read(region, offset, &mut data) {
                Ok(()) => {
                    let body = Payload::default()
                        .u64(offset)
                        .u32(region)
                        .u32(count)
                        .bytes(&data)
                        .finish();
                    Outcome::Reply(body, None)
                }
                Err(errno) => Outcome::Error(errno),
            }
        }
        Request::RegionWrite {
            offset,
            region,
            data,
        } => match device.region_write(region, offset, &data) {
            Ok(()) => {
                let count = u32::try_from(data.len()).unwrap_or(u32::MAX);
                Outcome::Reply(
                    Payload::default()
                        .u64(offset)
                        .u32(region)
                        .u32(count)
                        .finish(),
                    None,
                )
            }
            Err(errno) => Outcome::Error(errno),
        },
        Request::DmaMap {
            flags,
            offset,
            address,
            size,
        } => {
            if fds.len() > 1 {
                return Outcome::Error(Errno::EINVAL);
            }
            done(device.dma_map(DmaMapping {
                address,
                size,
                offset,
                flags,
                fd: fds.pop(),
            }))
        }
        Request::DmaUnmap {
            argsz,
            flags,
            address,
            size,
        } => match device.dma_unmap(address, size) {
            Ok(()) => {
                let body = Payload::default()
                    .u32(argsz)
                    .u32(flags)
                    .u64(address)
                    .u64(size)
                    .finish();
                Outcome::Reply(body, None)
            }
            Err(errno) => Outcome::Error(errno),
        },
        Request::SetIrqs {
            flags,
            index,
            start,
            count,
            data,
        } => done(device.set_irqs(IrqSet {
            flags,
            index,
            start,
            count,
            data,
            fds,
        })),
        Request::DeviceReset => done(device.reset()),
        Request::Version { .. } => Outcome::Error(Errno::EINVAL),
        Request::Unsupported(_) => Outcome::Error(Errno::ENOSYS),
    }
}
