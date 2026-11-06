//! End-to-end exchange over a socket pair: a scripted client against
//! `serve()` running a fake device, including file descriptor passing.

#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    missing_docs
)]

use std::io::{IoSliceMut, Read, Seek, SeekFrom, Write};
use std::mem::MaybeUninit;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::sync::mpsc;
use std::thread;

use rustix::net::{RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, recvmsg};
use vfio_user::wire::{self, Errno, Header, Payload, command, flags, region_flags};
use vfio_user::{Connection, Device, DeviceInfo, DmaMapping, IrqInfo, IrqSet, RegionInfo, serve};

/// One 4 KiB mappable region (index 0) and one 16-byte register region (1).
struct Fake {
    shared: OwnedFd,
    regs: [u8; 16],
    mapped: mpsc::Sender<(u64, u64, bool)>,
    resets: u32,
}

impl Device for Fake {
    fn info(&self) -> DeviceInfo {
        DeviceInfo {
            flags: wire::device_flags::PCI | wire::device_flags::RESET,
            num_regions: 2,
            num_irqs: 1,
        }
    }
    fn region_info(&self, index: u32) -> Option<RegionInfo<'_>> {
        match index {
            0 => Some(RegionInfo {
                flags: region_flags::READ | region_flags::WRITE | region_flags::MMAP,
                size: 4096,
                fd: Some(self.shared.as_fd()),
            }),
            1 => Some(RegionInfo {
                flags: region_flags::READ | region_flags::WRITE,
                size: 16,
                fd: None,
            }),
            _ => None,
        }
    }
    fn irq_info(&self, index: u32) -> Option<IrqInfo> {
        (index == 0).then_some(IrqInfo { flags: 0, count: 0 })
    }
    fn region_read(&mut self, region: u32, offset: u64, data: &mut [u8]) -> Result<(), Errno> {
        let start = offset as usize;
        let source = self
            .regs
            .get(start..start + data.len())
            .ok_or(Errno::EINVAL)?;
        if region != 1 {
            return Err(Errno::EINVAL);
        }
        data.copy_from_slice(source);
        Ok(())
    }
    fn region_write(&mut self, region: u32, offset: u64, data: &[u8]) -> Result<(), Errno> {
        let start = offset as usize;
        if region != 1 {
            return Err(Errno::EINVAL);
        }
        self.regs
            .get_mut(start..start + data.len())
            .ok_or(Errno::EINVAL)?
            .copy_from_slice(data);
        Ok(())
    }
    fn dma_map(&mut self, mapping: DmaMapping) -> Result<(), Errno> {
        self.mapped
            .send((mapping.address, mapping.size, mapping.fd.is_some()))
            .unwrap();
        Ok(())
    }
    fn dma_unmap(&mut self, _address: u64, _size: u64) -> Result<(), Errno> {
        Ok(())
    }
    fn set_irqs(&mut self, _set: IrqSet) -> Result<(), Errno> {
        Err(Errno::EINVAL)
    }
    fn reset(&mut self) -> Result<(), Errno> {
        self.resets += 1;
        Ok(())
    }
}

struct Client {
    conn: Connection,
    raw: UnixStream,
    next_id: u16,
}

impl Client {
    fn request(&mut self, cmd: u16, payload: &[u8], fds: &[std::os::fd::BorrowedFd<'_>]) -> u16 {
        let id = self.next_id;
        self.next_id += 1;
        let header = Header {
            id,
            command: cmd,
            size: (16 + payload.len()) as u32,
            flags: flags::REQUEST,
            error: 0,
        };
        let mut bytes = header.encode().to_vec();
        bytes.extend_from_slice(payload);
        self.conn.send(&bytes, fds).unwrap();
        id
    }

    fn reply(&mut self) -> (Header, Vec<u8>, Vec<OwnedFd>) {
        let mut head = [0u8; 16];
        let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(2))];
        let mut control = RecvAncillaryBuffer::new(&mut space);
        let got = recvmsg(
            &self.raw,
            &mut [IoSliceMut::new(&mut head)],
            &mut control,
            RecvFlags::empty(),
        )
        .unwrap();
        let mut fds = Vec::new();
        for message in control.drain() {
            if let RecvAncillaryMessage::ScmRights(received) = message {
                fds.extend(received);
            }
        }
        self.raw.read_exact(&mut head[got.bytes..]).unwrap();
        let header = Header::parse(&head);
        let mut payload = vec![0u8; header.size as usize - 16];
        self.raw.read_exact(&mut payload).unwrap();
        (header, payload, fds)
    }
}

type Mapped = mpsc::Receiver<(u64, u64, bool)>;

fn start() -> (Client, Mapped, thread::JoinHandle<Result<u32, String>>) {
    let (server_end, client_end) = UnixStream::pair().unwrap();
    let (tx, rx) = mpsc::channel();
    let shared: OwnedFd = tempfile_with(b"shared region").into();
    let server = thread::spawn(move || {
        let mut fake = Fake {
            shared,
            regs: [0; 16],
            mapped: tx,
            resets: 0,
        };
        serve(server_end, &mut fake)
            .map(|()| fake.resets)
            .map_err(|e| e.to_string())
    });
    let raw = client_end.try_clone().unwrap();
    (
        Client {
            conn: Connection::new(client_end),
            raw,
            next_id: 1,
        },
        rx,
        server,
    )
}

fn tempfile_with(contents: &[u8]) -> std::fs::File {
    let fd = rustix::fs::memfd_create("vfio-user-test", rustix::fs::MemfdFlags::CLOEXEC).unwrap();
    let mut file = std::fs::File::from(fd);
    file.write_all(contents).unwrap();
    file
}

fn version(client: &mut Client) {
    let mut body = Payload::default()
        .u16(0)
        .u16(0)
        .bytes(b"{\"capabilities\":{\"max_msg_fds\":16}}")
        .finish();
    body.push(0);
    client.request(command::VERSION, &body, &[]);
    let (header, payload, _) = client.reply();
    assert!(header.is_reply());
    assert_eq!(header.flags & flags::ERROR, 0);
    assert_eq!(&payload[..4], &[0, 0, 0, 0]);
    assert_eq!(payload.last(), Some(&0));
    let json = std::str::from_utf8(&payload[4..payload.len() - 1]).unwrap();
    assert!(json.contains("\"max_data_xfer_size\":1048576"));
}

#[test]
fn a_full_session() {
    let (mut client, mapped, server) = start();
    version(&mut client);

    // Device info.
    let id = client.request(
        command::DEVICE_GET_INFO,
        &Payload::default().u32(16).u32(0).u32(0).u32(0).finish(),
        &[],
    );
    let (header, payload, _) = client.reply();
    assert_eq!(header.id, id);
    assert_eq!(&payload[8..12], &2u32.to_le_bytes());

    // The mappable region comes with its file descriptor.
    let info = Payload::default()
        .u32(32)
        .u32(0)
        .u32(0)
        .u32(0)
        .u64(0)
        .u64(0)
        .finish();
    client.request(command::DEVICE_GET_REGION_INFO, &info, &[]);
    let (_, payload, fds) = client.reply();
    assert_eq!(
        u32::from_le_bytes(payload[4..8].try_into().unwrap()) & region_flags::MMAP,
        region_flags::MMAP
    );
    assert_eq!(
        u64::from_le_bytes(payload[16..24].try_into().unwrap()),
        4096
    );
    assert_eq!(fds.len(), 1);
    let mut file = std::fs::File::from(fds.into_iter().next().unwrap());
    file.seek(SeekFrom::Start(0)).unwrap();
    let mut text = String::new();
    file.read_to_string(&mut text).unwrap();
    assert_eq!(text, "shared region");

    // Unknown region.
    let info = Payload::default()
        .u32(32)
        .u32(0)
        .u32(9)
        .u32(0)
        .u64(0)
        .u64(0)
        .finish();
    client.request(command::DEVICE_GET_REGION_INFO, &info, &[]);
    let (header, _, _) = client.reply();
    assert_eq!(
        (header.flags & flags::ERROR, header.error),
        (flags::ERROR, 22)
    );

    // Register write then read back.
    let write = Payload::default()
        .u64(4)
        .u32(1)
        .u32(4)
        .u32(0x1234_5678)
        .finish();
    client.request(command::REGION_WRITE, &write, &[]);
    let (header, payload, _) = client.reply();
    assert_eq!((header.flags, payload.len()), (flags::REPLY, 16));
    client.request(
        command::REGION_READ,
        &Payload::default().u64(4).u32(1).u32(4).finish(),
        &[],
    );
    let (_, payload, _) = client.reply();
    assert_eq!(&payload[16..], &0x1234_5678u32.to_le_bytes());

    // A posted write gets no reply; the next request's reply comes next.
    let mut posted = Header {
        id: 99,
        command: command::REGION_WRITE,
        size: 36,
        flags: flags::NO_REPLY,
        error: 0,
    }
    .encode()
    .to_vec();
    posted.extend(Payload::default().u64(0).u32(1).u32(4).u32(7).finish());
    client.conn.send(&posted, &[]).unwrap();
    client.request(
        command::REGION_READ,
        &Payload::default().u64(0).u32(1).u32(4).finish(),
        &[],
    );
    let (header, payload, _) = client.reply();
    assert_ne!(header.id, 99);
    assert_eq!(&payload[16..], &7u32.to_le_bytes());

    // DMA map with a file descriptor.
    let memory = tempfile_with(&[0u8; 64]);
    let map = Payload::default()
        .u32(32)
        .u32(3)
        .u64(0)
        .u64(0x1000)
        .u64(64)
        .finish();
    client.request(command::DMA_MAP, &map, &[memory.as_fd()]);
    let (header, _, _) = client.reply();
    assert_eq!(header.flags, flags::REPLY);
    assert_eq!(mapped.recv().unwrap(), (0x1000, 64, true));

    // Unsupported command and reset.
    client.request(command::DIRTY_PAGES, &[], &[]);
    let (header, _, _) = client.reply();
    assert_eq!(header.error, Errno::ENOSYS.0);
    client.request(command::DEVICE_RESET, &[], &[]);
    client.reply();

    drop(client);
    assert_eq!(server.join().unwrap(), Ok(1));
}

#[test]
fn the_first_message_must_be_version() {
    let (mut client, _, server) = start();
    client.request(
        command::DEVICE_GET_INFO,
        &Payload::default().u32(16).u32(0).u32(0).u32(0).finish(),
        &[],
    );
    let result = server.join().unwrap();
    assert!(result.unwrap_err().contains("VERSION"));
}

#[test]
fn oversized_messages_close_the_connection() {
    let (mut client, _, server) = start();
    version(&mut client);
    let header = Header {
        id: 1,
        command: command::REGION_WRITE,
        size: u32::MAX,
        flags: 0,
        error: 0,
    };
    client.conn.send(&header.encode(), &[]).unwrap();
    assert!(
        server
            .join()
            .unwrap()
            .unwrap_err()
            .contains("bad message size")
    );
}
