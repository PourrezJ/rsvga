//! The SVGA II device: registers, VRAM, FIFO and PCI config, served over
//! vfio-user.

use std::io;
use std::os::fd::OwnedFd;
use std::sync::{Arc, Mutex, MutexGuard};

use svga_core::command::{Command, Rect};
use svga_core::{Config, Effect, FifoMemory, Limits, Mode, Registers, Snapshot, StopReason, run};
use svga_proto::{cap, fifo, fifo_cap, port};
use vfio_user::wire::{Errno, device_flags, pci_irq, pci_region, region_flags};
use vfio_user::{Device, DeviceInfo, DmaMapping, IrqInfo, IrqSet, RegionInfo};

use crate::log;
use crate::pci::{CONFIG_SIZE, PciConfig};
use crate::shm::SharedMem;

/// Device capabilities advertised in `SVGA_REG_CAPABILITIES` (M1: 2D only).
pub const CAPABILITIES: u32 = cap::RECT_COPY | cap::EXTENDED_FIFO | cap::PITCHLOCK;
/// FIFO capabilities written into `SVGA_FIFO_CAPABILITIES`.
pub const FIFO_CAPABILITIES: u32 = fifo_cap::FENCE;

/// Commands handled per FIFO pass, and passes per processing request: the
/// guest cannot keep the device busy forever.
const PASS_BUDGET: usize = 4096;
const MAX_PASSES: usize = 64;

/// Size of the I/O port BAR.
const IO_BAR_SIZE: u64 = 16;

/// Logs every trapped access when set (`RSVGA_TRACE=1`).
pub static TRACE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn tracing() -> bool {
    TRACE.load(std::sync::atomic::Ordering::Relaxed)
}

/// Device geometry chosen on the command line.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// VRAM BAR size in bytes (power of two).
    pub vram_size: u32,
    /// FIFO BAR size in bytes (power of two).
    pub fifo_size: u32,
    /// Largest mode width.
    pub max_width: u32,
    /// Largest mode height.
    pub max_height: u32,
}

/// Counters shown in the logs.
#[derive(Clone, Copy, Debug, Default)]
pub struct Stats {
    /// FIFO commands executed.
    pub commands: u64,
    /// `UPDATE` commands among them.
    pub updates: u64,
    /// Commands ignored because they are not implemented yet.
    pub unsupported: u64,
}

struct DmaWindow {
    address: u64,
    size: u64,
    _fd: Option<OwnedFd>,
}

/// Mutable device state, behind the device lock.
pub struct State {
    /// The register file.
    pub regs: Registers,
    pci: PciConfig,
    fifo_error: Option<String>,
    last_mode: Option<Mode>,
    dma: Vec<DmaWindow>,
    /// Counters.
    pub stats: Stats,
}

/// The device, shared between the vfio-user thread and the display loop.
pub struct Svga {
    vram: SharedMem,
    fifo: SharedMem,
    state: Mutex<State>,
    limits: Limits,
    /// Expansion ROM image (empty: no ROM) and the size of its BAR.
    rom: Vec<u8>,
    rom_bar: u32,
}

/// FIFO memory seen through the [`FifoMemory`] trait of `svga-core`.
struct FifoView<'a>(&'a SharedMem);

impl FifoMemory for FifoView<'_> {
    fn len_words(&self) -> usize {
        self.0.len() / 4
    }
    fn load(&self, index: usize) -> u32 {
        self.0.load_u32(index)
    }
    fn store(&mut self, index: usize, value: u32) {
        self.0.store_u32(index, value);
    }
}

fn reg_index(index: u32) -> usize {
    usize::try_from(index).unwrap_or(usize::MAX)
}

impl Svga {
    /// Allocates VRAM and FIFO memory and powers the device on. `rom` is the
    /// expansion ROM image (see [`crate::rom`]), or empty for none.
    pub fn new(opts: &Options, rom: Vec<u8>) -> io::Result<Arc<Self>> {
        let rom_bar = if rom.is_empty() {
            0
        } else {
            crate::rom::bar_size(rom.len())
        };
        let vram = SharedMem::new("rsvga-vram", opts.vram_size as usize)?;
        let fifo = SharedMem::new("rsvga-fifo", opts.fifo_size as usize)?;
        let regs = Registers::new(Config {
            vram_size: opts.vram_size,
            fifo_size: opts.fifo_size,
            max_width: opts.max_width,
            max_height: opts.max_height,
            capabilities: CAPABILITIES,
        });
        let state = State {
            regs,
            pci: PciConfig::new(opts.vram_size, opts.fifo_size, rom_bar),
            fifo_error: None,
            last_mode: None,
            dma: Vec::new(),
            stats: Stats::default(),
        };
        Ok(Arc::new(Self {
            vram,
            fifo,
            state: Mutex::new(state),
            limits: Limits::default(),
            rom,
            rom_bar,
        }))
    }

    /// Locks the device state. A panic while holding the lock cannot leave
    /// memory unsafe, so a poisoned lock is simply taken over.
    pub fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The VRAM, for display.
    pub fn vram(&self) -> &SharedMem {
        &self.vram
    }

    fn fail(&self, state: &mut State, reason: String) {
        log!("FIFO stopped: {reason}");
        state.fifo_error = Some(reason);
    }

    fn apply(&self, state: &mut State, effect: Effect) {
        match effect {
            Effect::None | Effect::IrqAck(_) => {}
            Effect::ModeChanged => {
                let mode = state.regs.mode();
                if mode != state.last_mode {
                    match mode {
                        Some(m) => log!(
                            "mode {}x{} {} bpp, pitch {}",
                            m.width,
                            m.height,
                            m.bits_per_pixel,
                            m.pitch
                        ),
                        None => log!("SVGA mode off"),
                    }
                    state.last_mode = mode;
                }
            }
            Effect::ConfigDone(true) => {
                state.fifo_error = None;
                let mut view = FifoView(&self.fifo);
                match Snapshot::capture(&view) {
                    Ok(snap) => {
                        for (index, value) in [
                            (fifo::CAPABILITIES, FIFO_CAPABILITIES),
                            (fifo::FLAGS, 0),
                            (fifo::HWVERSION_3D, 0),
                        ] {
                            if snap.has_register(index) {
                                view.store(reg_index(index), value);
                            }
                        }
                        log!("FIFO on: ring {:#x}..{:#x}", snap.min(), snap.max());
                    }
                    Err(error) => self.fail(state, error.to_string()),
                }
                self.process_fifo(state);
            }
            Effect::ConfigDone(false) => log!("FIFO off"),
            Effect::Sync => self.process_fifo(state),
        }
    }

    /// Runs the FIFO until it is empty, waits for the guest, or fails.
    pub fn process_fifo(&self, state: &mut State) {
        if !state.regs.config_done() || state.fifo_error.is_some() {
            return;
        }
        let mut view = FifoView(&self.fifo);
        for _ in 0..MAX_PASSES {
            let mut commands = Vec::new();
            let pass = run(&mut view, &self.limits, PASS_BUDGET, |command| {
                commands.push(command)
            });
            for command in commands {
                self.execute(state, command);
            }
            match pass.stop {
                StopReason::Budget => continue,
                StopReason::Empty | StopReason::Incomplete => return,
                StopReason::Invalid(error) => return self.fail(state, error.to_string()),
                StopReason::BadConfig(error) => return self.fail(state, error.to_string()),
            }
        }
    }

    fn execute(&self, state: &mut State, command: Command) {
        state.stats.commands = state.stats.commands.saturating_add(1);
        match command {
            Command::Update(_) | Command::UpdateVerbose { .. } => {
                // The display loop refreshes the whole framebuffer anyway.
                state.stats.updates = state.stats.updates.saturating_add(1);
            }
            Command::RectCopy { src_x, src_y, dst } => {
                if let Some(mode) = state.regs.mode() {
                    self.rect_copy(mode, src_x, src_y, dst);
                }
            }
            Command::Fence(value) => {
                let mut view = FifoView(&self.fifo);
                if Snapshot::capture(&view).is_ok_and(|snap| snap.has_register(fifo::FENCE)) {
                    view.store(reg_index(fifo::FENCE), value);
                }
            }
            Command::Nop => {}
            Command::NopError => self.fail(state, "the guest sent NOP_ERROR".into()),
            other => {
                if state.stats.unsupported == 0 {
                    log!(
                        "ignoring unsupported command {other:?} (further ones are counted silently)"
                    );
                }
                state.stats.unsupported = state.stats.unsupported.saturating_add(1);
            }
        }
    }

    /// Copies a rectangle inside the framebuffer. Rectangles that leave the
    /// visible mode are ignored rather than clipped, like the real device.
    fn rect_copy(&self, mode: Mode, src_x: u32, src_y: u32, dst: Rect) {
        let fits = |x: u32, y: u32| {
            u64::from(x) + u64::from(dst.width) <= u64::from(mode.width)
                && u64::from(y) + u64::from(dst.height) <= u64::from(mode.height)
        };
        if !fits(src_x, src_y) || !fits(dst.x, dst.y) {
            return;
        }
        let bpp = u64::from(mode.bits_per_pixel / 8);
        let row_bytes = u64::from(dst.width) * bpp;
        let at = |x: u32, y: u32| {
            u64::from(mode.offset) + u64::from(y) * u64::from(mode.pitch) + u64::from(x) * bpp
        };
        let (Ok(row_bytes), true) = (usize::try_from(row_bytes), dst.height > 0) else {
            return;
        };
        let mut rows: Box<dyn Iterator<Item = u32>> = Box::new(0..dst.height);
        if src_y < dst.y {
            rows = Box::new((0..dst.height).rev());
        }
        for row in rows {
            let (Ok(src), Ok(target)) = (
                usize::try_from(at(src_x, src_y + row)),
                usize::try_from(at(dst.x, dst.y + row)),
            ) else {
                return;
            };
            self.vram.copy_within(src, target, row_bytes);
        }
    }
}

/// The vfio-user face of the device.
pub struct Handle(pub Arc<Svga>);

impl Device for Handle {
    fn info(&self) -> DeviceInfo {
        DeviceInfo {
            flags: device_flags::PCI | device_flags::RESET,
            num_regions: pci_region::COUNT,
            num_irqs: pci_irq::COUNT,
        }
    }

    fn region_info(&self, index: u32) -> Option<RegionInfo<'_>> {
        let rw = region_flags::READ | region_flags::WRITE;
        fn mapped(mem: &SharedMem) -> RegionInfo<'_> {
            RegionInfo {
                flags: region_flags::READ | region_flags::WRITE | region_flags::MMAP,
                size: mem.len() as u64,
                fd: Some(mem.fd()),
            }
        }
        Some(match index {
            pci_region::BAR0 => RegionInfo {
                flags: rw,
                size: IO_BAR_SIZE,
                fd: None,
            },
            pci_region::BAR1 => mapped(&self.0.vram),
            pci_region::BAR2 => mapped(&self.0.fifo),
            pci_region::CONFIG => RegionInfo {
                flags: rw,
                size: CONFIG_SIZE as u64,
                fd: None,
            },
            pci_region::ROM if self.0.rom_bar != 0 => RegionInfo {
                flags: region_flags::READ,
                size: u64::from(self.0.rom_bar),
                fd: None,
            },
            pci_region::BAR3..=pci_region::ROM | pci_region::VGA => RegionInfo {
                flags: 0,
                size: 0,
                fd: None,
            },
            _ => return None,
        })
    }

    fn irq_info(&self, index: u32) -> Option<IrqInfo> {
        // No interrupts yet: the drivers poll when SVGA_CAP_IRQMASK is absent.
        (index < pci_irq::COUNT).then_some(IrqInfo { flags: 0, count: 0 })
    }

    fn region_read(&mut self, region: u32, offset: u64, data: &mut [u8]) -> Result<(), Errno> {
        let svga = &self.0;
        let ok = match region {
            pci_region::BAR0 => {
                let state = svga.lock();
                let value = match (offset, data.len()) {
                    (0 | 1 | 2 | 8, 4) => state.regs.port_read(offset as u16),
                    _ => u32::MAX,
                };
                for (byte, value) in data.iter_mut().zip(value.to_le_bytes().into_iter().cycle()) {
                    *byte = value;
                }
                true
            }
            pci_region::BAR1 => svga
                .vram
                .read(usize::try_from(offset).map_err(|_| Errno::EINVAL)?, data),
            pci_region::BAR2 => svga
                .fifo
                .read(usize::try_from(offset).map_err(|_| Errno::EINVAL)?, data),
            pci_region::CONFIG => svga.lock().pci.read(offset, data),
            pci_region::ROM if svga.rom_bar != 0 => {
                let fits = offset
                    .checked_add(data.len() as u64)
                    .is_some_and(|end| end <= u64::from(svga.rom_bar));
                if fits {
                    // Bytes past the image read as 0xff, like erased flash.
                    let start = offset as usize;
                    for (i, byte) in data.iter_mut().enumerate() {
                        *byte = svga.rom.get(start + i).copied().unwrap_or(0xff);
                    }
                }
                fits
            }
            _ => false,
        };
        if tracing() && region != pci_region::BAR1 {
            log!("read  region {region} +{offset:#x} {data:02x?} ok={ok}");
        }
        if ok { Ok(()) } else { Err(Errno::EINVAL) }
    }

    fn region_write(&mut self, region: u32, offset: u64, data: &[u8]) -> Result<(), Errno> {
        let svga = &self.0;
        if tracing() && region != pci_region::BAR1 {
            log!("write region {region} +{offset:#x} {data:02x?}");
        }
        let ok = match region {
            pci_region::BAR0 => {
                if let (0 | 1 | 2 | 8, Ok(bytes)) = (offset, <[u8; 4]>::try_from(data)) {
                    let mut state = svga.lock();
                    let effect = state
                        .regs
                        .port_write(offset as u16, u32::from_le_bytes(bytes));
                    if offset == u64::from(port::VALUE) {
                        svga.apply(&mut state, effect);
                    }
                }
                true
            }
            pci_region::BAR1 => svga
                .vram
                .write(usize::try_from(offset).map_err(|_| Errno::EINVAL)?, data),
            pci_region::BAR2 => svga
                .fifo
                .write(usize::try_from(offset).map_err(|_| Errno::EINVAL)?, data),
            pci_region::CONFIG => {
                let mut state = svga.lock();
                let written = state.pci.write(offset, data);
                let (vram, fifo) = (state.pci.bar_address(1), state.pci.bar_address(2));
                state.regs.set_bar_addresses(vram, fifo);
                written
            }
            _ => false,
        };
        if ok { Ok(()) } else { Err(Errno::EINVAL) }
    }

    fn dma_map(&mut self, mapping: DmaMapping) -> Result<(), Errno> {
        let end = mapping
            .address
            .checked_add(mapping.size)
            .ok_or(Errno::EINVAL)?;
        if mapping.size == 0 {
            return Err(Errno::EINVAL);
        }
        let mut state = self.0.lock();
        let overlaps = state
            .dma
            .iter()
            .any(|w| mapping.address < w.address + w.size && w.address < end);
        if overlaps {
            return Err(Errno::EEXIST);
        }
        log!(
            "DMA window {:#x}+{:#x}{}",
            mapping.address,
            mapping.size,
            if mapping.fd.is_some() {
                " (shared)"
            } else {
                ""
            }
        );
        state.dma.push(DmaWindow {
            address: mapping.address,
            size: mapping.size,
            _fd: mapping.fd,
        });
        Ok(())
    }

    fn dma_unmap(&mut self, address: u64, size: u64) -> Result<(), Errno> {
        let mut state = self.0.lock();
        let before = state.dma.len();
        state
            .dma
            .retain(|w| !(w.address == address && w.size == size));
        if state.dma.len() == before {
            Err(Errno::EINVAL)
        } else {
            Ok(())
        }
    }

    fn set_irqs(&mut self, set: IrqSet) -> Result<(), Errno> {
        if set.count == 0 {
            Ok(())
        } else {
            Err(Errno::EINVAL)
        }
    }

    fn reset(&mut self) -> Result<(), Errno> {
        let mut state = self.0.lock();
        state.regs.reset();
        state.fifo_error = None;
        state.last_mode = None;
        log!("device reset");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use svga_proto::{cmd, id, reg};

    fn device() -> Handle {
        Handle(
            Svga::new(
                &Options {
                    vram_size: 16 << 20,
                    fifo_size: 64 << 10,
                    max_width: 2560,
                    max_height: 1600,
                },
                Vec::new(),
            )
            .unwrap(),
        )
    }

    fn set(h: &mut Handle, index: u32, value: u32) {
        h.region_write(0, 0, &index.to_le_bytes()).unwrap();
        h.region_write(0, 1, &value.to_le_bytes()).unwrap();
    }

    fn get(h: &mut Handle, index: u32) -> u32 {
        h.region_write(0, 0, &index.to_le_bytes()).unwrap();
        let mut out = [0u8; 4];
        h.region_read(0, 1, &mut out).unwrap();
        u32::from_le_bytes(out)
    }

    #[test]
    fn registers_through_the_io_bar() {
        let mut h = device();
        set(&mut h, reg::ID, id::ID_2);
        assert_eq!(get(&mut h, reg::ID), id::ID_2);
        assert_eq!(get(&mut h, reg::VRAM_SIZE), 16 << 20);
        assert_eq!(get(&mut h, reg::CAPABILITIES), CAPABILITIES);
    }

    #[test]
    fn bar_addresses_reach_the_registers() {
        let mut h = device();
        h.region_write(pci_region::CONFIG, 0x14, &0xe000_0000u32.to_le_bytes())
            .unwrap();
        h.region_write(pci_region::CONFIG, 0x18, &0xfe00_0000u32.to_le_bytes())
            .unwrap();
        assert_eq!(get(&mut h, reg::FB_START), 0xe000_0000);
        assert_eq!(get(&mut h, reg::MEM_START), 0xfe00_0000);
    }

    /// Sets up the FIFO like a driver: registers below MIN, ring after it.
    fn enable_fifo(h: &mut Handle) -> u32 {
        let svga = h.0.clone();
        let min = fifo::NUM_REGS * 4;
        svga.fifo.store_u32(0, min);
        svga.fifo.store_u32(1, 64 << 10);
        svga.fifo.store_u32(2, min);
        svga.fifo.store_u32(3, min);
        set(h, reg::CONFIG_DONE, 1);
        min
    }

    fn push(h: &Handle, words: &[u32]) {
        let fifo = &h.0.fifo;
        let mut next = fifo.load_u32(2);
        for &word in words {
            fifo.store_u32(next as usize / 4, word);
            next += 4;
        }
        fifo.store_u32(2, next);
    }

    #[test]
    fn config_done_publishes_fifo_capabilities_and_fences_work() {
        let mut h = device();
        enable_fifo(&mut h);
        assert_eq!(
            h.0.fifo.load_u32(fifo::CAPABILITIES as usize),
            FIFO_CAPABILITIES
        );
        push(&h, &[cmd::FENCE, 42]);
        set(&mut h, reg::SYNC, 1);
        assert_eq!(h.0.fifo.load_u32(fifo::FENCE as usize), 42);
        assert_eq!(
            h.0.fifo.load_u32(3),
            h.0.fifo.load_u32(2),
            "STOP caught up with NEXT_CMD"
        );
    }

    #[test]
    fn rect_copy_moves_pixels() {
        let mut h = device();
        set(&mut h, reg::WIDTH, 64);
        set(&mut h, reg::HEIGHT, 64);
        set(&mut h, reg::BITS_PER_PIXEL, 32);
        set(&mut h, reg::ENABLE, 1);
        let vram = &h.0.vram;
        vram.store_u32(0, 0x00ff_0000); // pixel (0, 0)
        enable_fifo(&mut h);
        push(&h, &[cmd::RECT_COPY, 0, 0, 10, 5, 1, 1]);
        set(&mut h, reg::SYNC, 1);
        assert_eq!(h.0.vram.load_u32(5 * 64 + 10), 0x00ff_0000);
        // Out of the visible mode: ignored, not clipped.
        push(&h, &[cmd::RECT_COPY, 0, 0, 63, 63, 2, 2]);
        set(&mut h, reg::SYNC, 1);
        assert_eq!(h.0.lock().stats.commands, 2);
    }

    #[test]
    fn a_bad_command_stops_the_fifo_until_reenabled() {
        let mut h = device();
        enable_fifo(&mut h);
        push(&h, &[2]); // not a command
        set(&mut h, reg::SYNC, 1);
        assert!(h.0.lock().fifo_error.is_some());
        set(&mut h, reg::CONFIG_DONE, 0);
        h.0.fifo.store_u32(3, h.0.fifo.load_u32(2));
        set(&mut h, reg::CONFIG_DONE, 1);
        assert!(h.0.lock().fifo_error.is_none());
    }

    #[test]
    fn the_rom_is_served_through_region_6() {
        let opts = Options {
            vram_size: 16 << 20,
            fifo_size: 64 << 10,
            max_width: 2560,
            max_height: 1600,
        };
        let mut h = Handle(Svga::new(&opts, vec![0x55, 0xaa, 1]).unwrap());
        let info = h.region_info(pci_region::ROM).unwrap();
        assert_eq!((info.flags, info.size), (region_flags::READ, 2048));
        let mut out = [0u8; 4];
        h.region_read(pci_region::ROM, 0, &mut out).unwrap();
        assert_eq!(out, [0x55, 0xaa, 1, 0xff]);
        assert_eq!(
            h.region_read(pci_region::ROM, 2046, &mut out),
            Err(Errno::EINVAL)
        );
        assert_eq!(device().region_info(pci_region::ROM).unwrap().size, 0);
    }

    #[test]
    fn dma_windows_cannot_overlap() {
        let mut h = device();
        let map = |address, size| DmaMapping {
            address,
            size,
            offset: 0,
            flags: 3,
            fd: None,
        };
        assert_eq!(h.dma_map(map(0, 0x1000)), Ok(()));
        assert_eq!(h.dma_map(map(0x800, 0x1000)), Err(Errno::EEXIST));
        assert_eq!(h.dma_map(map(u64::MAX, 2)), Err(Errno::EINVAL));
        assert_eq!(h.dma_unmap(0, 0x800), Err(Errno::EINVAL));
        assert_eq!(h.dma_unmap(0, 0x1000), Ok(()));
    }
}
