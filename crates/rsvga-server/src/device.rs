//! The SVGA II device: registers, VRAM, FIFO and PCI config, served over
//! vfio-user.

use std::io;
use std::os::fd::OwnedFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::Duration;

use svga_core::command::{Command, Rect};
use svga_core::{Config, Effect, FifoMemory, Limits, Mode, Registers, Snapshot, StopReason, run};
use svga_proto::{cap, cap2, cursor_on, fifo, fifo_cap, irq, port};
use vfio_user::wire::{
    Errno, device_flags, irq_info_flags, irq_set_flags, pci_irq, pci_region, region_flags,
};
use vfio_user::{Device, DeviceInfo, DmaMapping, IrqInfo, IrqSet, RegionInfo};

use crate::cmdbuf::CommandBuffers;
use crate::cursor::{self as guest_cursor, CursorImage};
use crate::damage::Damage;
use crate::guest::GuestMemory;
use crate::irq::Intx;
use crate::log;
use crate::pci::{CONFIG_SIZE, PciConfig};
use crate::shm::SharedMem;
use crate::twod::{Memory, TwoD};

/// Device capabilities advertised in `SVGA_REG_CAPABILITIES` without 3D:
/// 2D with GMR2 and screen objects, commands in FIFO or command buffers,
/// and guest-backed objects (MOBs, surfaces, screen targets). The VMware
/// WDDM driver then runs as a display-only driver on screen targets.
pub const CAPABILITIES: u32 = cap::RECT_COPY
    | cap::CURSOR
    | cap::ALPHA_CURSOR
    | cap::EXTENDED_FIFO
    | cap::PITCHLOCK
    | cap::IRQMASK
    | cap::GMR2
    | cap::SCREEN_OBJECT_2
    | cap::COMMAND_BUFFERS
    | cap::CMD_BUFFERS_2
    | cap::GBOBJECTS
    | cap::CAP2_REGISTER;

/// Value of `SVGA_REG_CAP2`: object tables may grow (the device keeps its
/// state elsewhere), GB memory is given in `GBOBJECT_MEM_SIZE_KB`, object
/// tables may use two-level page tables.
pub const CAPABILITIES_2: u32 = cap2::GROW_OTABLE | cap2::GB_MEMSIZE_2 | cap2::OTABLE_PTDEPTH_2;

/// Capabilities added with 3D (the default, see [`Options::three_d`]): 3D,
/// DX contexts, and the high-priority command queue, without which
/// Windows refuses the adapter in WDDM 2.0.
pub const CAPABILITIES_3D: u32 = cap::THREE_D | cap::DX | cap::HP_CMD_QUEUE;

/// `SVGA_REG_CAP2` bits added with 3D: DX2 and DX3 commands, and video
/// blits. VMware's kernel driver goes beyond WDDM 1.1 only with `DX3` and
/// `VIDEO_BLT`, and its Direct3D 9 driver needs WDDM 2.0. `DX3` alone does
/// not open feature level 11_0 (that also takes shader model 5 devcaps).
pub const CAPABILITIES_2_3D: u32 = cap2::DX2 | cap2::DX3 | cap2::VIDEO_BLT;

/// 3D hardware version with 3D (`SVGA3D_HWVERSION_WS8_B1`).
pub const HW_VERSION_3D: u32 = 0x2_0001;
/// GMR limits advertised to the guest (64 regions, 1 GiB of pages).
pub const GMR_MAX_IDS: u32 = 64;
/// Total pages across all GMRs.
pub const GMR_MAX_PAGES: u32 = 256 * 1024;
/// Guest memory usable through GMRs, in bytes.
pub const GMR_MEMORY_SIZE: u32 = 1 << 30;
/// Guest memory the guest is told it may turn into memory objects
/// (MOBs): 2 GiB. The driver sizes its graphics memory on it.
pub const GB_MEMORY_KB: u32 = 2 << 20;
/// Pages all MOBs together may hold: four times the budget above. The
/// guest may go past its budget a little (object tables, page tables,
/// frees still in flight); this bound only caps rsvga's bookkeeping
/// (8 bytes per page: 16 MiB at most).
pub const MOB_MAX_PAGES: u64 = 4 * (GB_MEMORY_KB as u64) * 1024 / 4096;
/// Largest single MOB, in bytes.
pub const MOB_MAX_SIZE: u32 = 256 << 20;
/// Host memory all device copies of guest-backed surfaces may use.
pub const GB_SURFACE_BYTES: u64 = 1 << 30;
/// Shader code all DX contexts together may hold.
pub const DX_CODE_BYTES: usize = 64 << 20;
/// Largest DX shader (code and signatures).
pub const DX_SHADER_BYTES: u32 = 4 << 20;

/// FIFO capabilities written into `SVGA_FIFO_CAPABILITIES`.
pub const FIFO_CAPABILITIES: u32 = fifo_cap::FENCE
    | fifo_cap::CURSOR_BYPASS_3
    | fifo_cap::SCREEN_OBJECT
    | fifo_cap::SCREEN_OBJECT_2;

/// Commands handled per FIFO pass, and passes per processing request: the
/// guest cannot keep the device busy forever.
const PASS_BUDGET: usize = 4096;
const MAX_PASSES: usize = 64;

/// Size of the I/O port BAR.
const IO_BAR_SIZE: u64 = 16;

/// Trace level (`RSVGA_TRACE`): 1 logs every SVGA register access by name
/// and every unsupported FIFO command; 2 also logs raw region accesses.
pub static TRACE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

pub(crate) fn trace_level() -> u8 {
    TRACE.load(std::sync::atomic::Ordering::Relaxed)
}

fn tracing() -> bool {
    trace_level() >= 2
}

/// Logs a trace line, folding identical consecutive lines (drivers poll).
pub(crate) fn trace(line: String) {
    static LAST: Mutex<(String, u64)> = Mutex::new((String::new(), 0));
    let mut last = LAST.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if last.0 == line {
        last.1 += 1;
        return;
    }
    if last.1 > 0 {
        log!("  (previous line repeated {} more times)", last.1);
    }
    log!("{line}");
    *last = (line, 0);
}

/// Diagnosis aid: with `RSVGA_CURSOR_DUMP=DIR`, saves each distinct guest
/// cursor as `DIR/cursor-<fingerprint>.png` (over a checkerboard).
fn dump_cursor(image: &CursorImage, print: u32) {
    let Some(dir) = std::env::var_os("RSVGA_CURSOR_DUMP") else {
        return;
    };
    let path = std::path::Path::new(&dir).join(format!("cursor-{print:08x}.png"));
    if path.exists() {
        return;
    }
    let _ = std::fs::create_dir_all(&dir);
    let result = std::fs::File::create(&path)
        .and_then(|mut file| crate::display::write_png(&mut file, &image.on_checkerboard()));
    if let Err(error) = result {
        log!("cannot save {}: {error}", path.display());
    }
}

/// Short name of a command, for logs.
fn command_name(command: &Command) -> String {
    let text = format!("{command:?}");
    text.split([' ', '(', '{'])
        .next()
        .unwrap_or_default()
        .to_string()
}

fn reg_label(index: u32) -> String {
    use svga_proto::reg;
    if let Some(name) = reg::name(index) {
        return name.to_string();
    }
    if let Some(slot) = index.checked_sub(reg::SCRATCH_BASE) {
        return format!("SCRATCH[{slot}]");
    }
    if let Some(slot) = index.checked_sub(reg::PALETTE_BASE) {
        return format!("PALETTE[{slot}]");
    }
    format!("#{index}")
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
    /// Regular command buffers run on a worker thread rather than inside
    /// the guest's register write (see `cmdbuf.rs`).
    pub asynchronous: bool,
    /// Advertise 3D and run it on the host GPU ([`three_d_wanted`]); the
    /// device stays 2D if no Vulkan GPU opens.
    pub three_d: bool,
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
    /// DX context commands done.
    pub dx: u64,
    /// DX context commands that need the renderer (draws, clears, copies):
    /// accepted, not executed yet.
    pub dx_deferred: u64,
    /// DX context commands refused (malformed, or invalid for the state).
    pub dx_refused: u64,
    /// Legacy (Direct3D 9) context commands executed.
    pub legacy: u64,
    /// Legacy context commands refused.
    pub legacy_refused: u64,
}

/// Mutable device state, behind the device lock.
pub struct State {
    /// The register file.
    pub regs: Registers,
    pci: PciConfig,
    fifo_error: Option<String>,
    last_mode: Option<Mode>,
    /// Guest RAM shared by QEMU.
    pub guest: GuestMemory,
    /// GMRs and screen objects.
    pub twod: TwoD,
    /// Guest-backed objects: MOBs, surfaces, screen targets.
    pub gb: crate::gb::Gb,
    /// DirectX contexts.
    pub dx: crate::dxctx::DxContexts,
    /// Legacy (Direct3D 9) contexts and shaders.
    pub legacy: crate::ctx9::Legacy,
    /// The host GPU (with 3D), and where each surface's newest copy is.
    pub gpu: Option<crate::gpu::Gpu>,
    /// The INTx line.
    pub irq: Intx,
    /// Command buffer contexts.
    pub cb: CommandBuffers,
    /// Framebuffer area changed since the display last copied it.
    pub damage: Damage,
    /// Last cursor image defined by the guest, and a counter of definitions.
    cursor: Option<Arc<CursorImage>>,
    cursor_serial: u64,
    /// Stops the thread watching QEMU's INTx unmask eventfd.
    unmask_watch: Option<Arc<AtomicBool>>,
    /// Counters.
    pub stats: Stats,
    /// Kinds of unsupported commands already logged.
    unsupported_kinds: Vec<String>,
}

/// The device, shared between the vfio-user thread and the display loop.
pub struct Svga {
    vram: SharedMem,
    fifo: SharedMem,
    state: Mutex<State>,
    pub(crate) limits: Limits,
    /// Whether a VMM is connected, and whether one ever was.
    client_connected: AtomicBool,
    client_seen: AtomicBool,
    /// Expansion ROM image (empty: no ROM) and the size of its BAR.
    rom: Vec<u8>,
    rom_bar: u32,
    /// Whether command buffers run on the worker.
    pub(crate) asynchronous: bool,
    /// Wakes the worker.
    pub(crate) work: std::sync::Condvar,
    /// Signalled when the worker runs out of buffers.
    pub(crate) idle: std::sync::Condvar,
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

/// Diagnostic override from the environment (hexadecimal), to study how a
/// guest driver reacts to capabilities the device does not implement.
fn probe_value(name: &str) -> Option<u32> {
    let value = std::env::var(name).ok()?;
    u32::from_str_radix(value.trim_start_matches("0x"), 16).ok()
}

/// [`probe_value`], logged.
fn probe_env(name: &str) -> Option<u32> {
    let value = probe_value(name)?;
    log!("PROBE {name} = {value:#x}: advertised, not implemented");
    Some(value)
}

/// Whether 3D is wanted: yes, unless `RSVGA_3D=0`.
pub fn three_d_wanted() -> bool {
    std::env::var_os("RSVGA_3D").is_none_or(|v| v != "0")
}

/// Whether this run gets 3D: wanted and a Vulkan GPU opens.
pub fn three_d_available() -> bool {
    three_d_wanted() && crate::gpu::Gpu::new().is_ok()
}

/// The host GPU, when 3D is wanted (or probed): rendering needs it.
fn open_gpu(three_d: bool) -> Option<crate::gpu::Gpu> {
    let probed = probe_value("RSVGA_PROBE_CAPS").is_some_and(|c| c & cap::THREE_D != 0);
    if !three_d && !probed {
        return None;
    }
    match crate::gpu::Gpu::new() {
        Ok(gpu) => {
            log!("GPU: {}", gpu.name());
            Some(gpu)
        }
        Err(e) => {
            log!("GPU unavailable, the device stays 2D: {e}");
            None
        }
    }
}

/// Capabilities this run advertises: [`CAPABILITIES`], with
/// [`CAPABILITIES_3D`] if `three_d`, unless `RSVGA_PROBE_CAPS` overrides
/// them for driver probing.
pub fn capabilities(three_d: bool) -> u32 {
    let own = if three_d {
        CAPABILITIES | CAPABILITIES_3D
    } else {
        CAPABILITIES
    };
    probe_env("RSVGA_PROBE_CAPS").unwrap_or(own)
}

/// Value of `SVGA_REG_CAP2`: [`CAPABILITIES_2`], with
/// [`CAPABILITIES_2_3D`] if `three_d`, unless `RSVGA_PROBE_CAPS2`
/// overrides it.
pub fn caps2(three_d: bool) -> u32 {
    let own = if three_d {
        CAPABILITIES_2 | CAPABILITIES_2_3D
    } else {
        CAPABILITIES_2
    };
    probe_env("RSVGA_PROBE_CAPS2").unwrap_or(own)
}

/// 3D hardware version written into the FIFO: [`HW_VERSION_3D`] if
/// `three_d`, else 0, unless `RSVGA_PROBE_HW3D` overrides it.
pub fn hw_version_3d(three_d: bool) -> u32 {
    probe_env("RSVGA_PROBE_HW3D").unwrap_or(if three_d { HW_VERSION_3D } else { 0 })
}

/// QEMU arguments (one per element) for its VMware backdoor (`vmport`):
/// answer `BDOOR_CMD_GET_SVGA_CAPABILITIES` exactly like this device (with
/// 3D if `three_d`), and open the GuestRPC channel the VMware drivers log
/// through. Needs the rsvga QEMU patch `0002-vmport-...`.
pub fn vmport_args(three_d: bool) -> Vec<String> {
    let caps = capabilities(three_d);
    let caps2 = caps2(three_d);
    [
        "vmport.svga-backdoor=on".to_string(),
        format!("vmport.svga-caps={caps:#x}"),
        format!("vmport.svga-fifo-caps={FIFO_CAPABILITIES:#x}"),
        format!("vmport.svga-3d-hwversion={:#x}", hw_version_3d(three_d)),
        format!("vmport.svga-caps2={caps2:#x}"),
        "vmport.guestrpc=on".to_string(),
    ]
    .into_iter()
    .flat_map(|global| ["-global".to_string(), global])
    .collect()
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
        // 3D only with a GPU to run it.
        let gpu = open_gpu(opts.three_d);
        let with_3d = opts.three_d && gpu.is_some();
        let mut regs = Registers::new(Config {
            vram_size: opts.vram_size,
            fifo_size: opts.fifo_size,
            max_width: opts.max_width,
            max_height: opts.max_height,
            capabilities: capabilities(with_3d),
            gmr_max_ids: GMR_MAX_IDS,
            gmr_max_pages: GMR_MAX_PAGES,
            memory_size: GMR_MEMORY_SIZE,
            gb_memory_kb: GB_MEMORY_KB,
            mob_max_size: MOB_MAX_SIZE,
            caps2: caps2(with_3d),
        });
        regs.set_devcaps(crate::devcaps::table(
            regs.config().capabilities & cap::THREE_D != 0,
        ));
        let state = State {
            regs,
            pci: PciConfig::new(opts.vram_size, opts.fifo_size, rom_bar),
            fifo_error: None,
            last_mode: None,
            guest: GuestMemory::default(),
            twod: TwoD::new(GMR_MAX_IDS, GMR_MAX_PAGES, opts.max_width, opts.max_height),
            gb: crate::gb::Gb::new(crate::gb::Limits {
                mob_pages: MOB_MAX_PAGES,
                mob_size: MOB_MAX_SIZE,
                surface_bytes: GB_SURFACE_BYTES,
                max_width: opts.max_width,
                max_height: opts.max_height,
            }),
            gpu,
            dx: crate::dxctx::DxContexts::new(crate::dxctx::Limits {
                contexts: crate::devcaps::MAX_CONTEXT_IDS,
                code_bytes: DX_CODE_BYTES,
                shader_bytes: DX_SHADER_BYTES,
            }),
            legacy: crate::ctx9::Legacy::default(),
            irq: Intx::default(),
            cb: CommandBuffers::default(),
            damage: Damage::default(),
            cursor: None,
            cursor_serial: 0,
            unmask_watch: None,
            stats: Stats::default(),
            unsupported_kinds: Vec::new(),
        };
        let svga = Arc::new(Self {
            vram,
            fifo,
            state: Mutex::new(state),
            limits: Limits::default(),
            rom,
            rom_bar,
            client_connected: AtomicBool::new(false),
            client_seen: AtomicBool::new(false),
            asynchronous: opts.asynchronous,
            work: std::sync::Condvar::new(),
            idle: std::sync::Condvar::new(),
        });
        if opts.asynchronous {
            let worker = Arc::clone(&svga);
            thread::Builder::new()
                .name("rsvga-cb".into())
                .spawn(move || worker.work())?;
        }
        Ok(svga)
    }

    /// Locks the device state. A panic while holding the lock cannot leave
    /// memory unsafe, so a poisoned lock is simply taken over.
    pub fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Records that the VMM connected or disconnected.
    pub fn set_client(&self, connected: bool) {
        self.client_connected.store(connected, Ordering::Relaxed);
        if connected {
            self.client_seen.store(true, Ordering::Relaxed);
        }
    }

    /// Whether the VMM was connected and has gone (the VM stopped).
    #[cfg_attr(not(feature = "window"), allow(dead_code))]
    pub fn client_gone(&self) -> bool {
        self.client_seen.load(Ordering::Relaxed) && !self.client_connected.load(Ordering::Relaxed)
    }

    /// One-line summary of the FIFO registers, for traces.
    pub fn fifo_status(&self) -> String {
        let r = |i: u32| self.fifo.load_u32(reg_index(i));
        let state = self.lock();
        format!(
            "fifo min={:#x} max={:#x} next={:#x} stop={:#x} fence={:#x} busy={} caps={:#x} | cursor on={} x={} y={} count={} | commands={} dx={} deferred={} refused={} shaders={}/{} cb ok={} failed={} error={:?} | irq mask={:#x} pending={:#x} signals={} eventfd={}",
            r(fifo::MIN),
            r(fifo::MAX),
            r(fifo::NEXT_CMD),
            r(fifo::STOP),
            r(fifo::FENCE),
            r(fifo::BUSY),
            r(fifo::CAPABILITIES),
            r(fifo::CURSOR_ON),
            r(fifo::CURSOR_X),
            r(fifo::CURSOR_Y),
            r(fifo::CURSOR_COUNT),
            state.stats.commands,
            state.stats.dx,
            state.stats.dx_deferred,
            state.stats.dx_refused,
            state.dx.translated,
            state.dx.translated.saturating_add(state.dx.refused),
            state.cb.completed,
            state.cb.failed,
            state.fifo_error,
            state.regs.irq_mask(),
            state.irq.pending(),
            state.irq.signals(),
            state.irq.connected()
        )
    }

    /// The VRAM, for display.
    pub fn vram(&self) -> &SharedMem {
        &self.vram
    }

    fn fail(&self, state: &mut State, reason: String) {
        log!("FIFO stopped: {reason}");
        state.fifo_error = Some(reason);
        let mask = state.regs.irq_mask();
        state.irq.raise(irq::ERROR, mask);
    }

    fn apply(&self, state: &mut State, effect: Effect) {
        match effect {
            Effect::None => {}
            Effect::IrqAck(flags) => {
                let mask = state.regs.irq_mask();
                state.irq.ack(flags, mask);
            }
            Effect::IrqMask => {
                let mask = state.regs.irq_mask();
                state.irq.update(mask);
            }
            Effect::ModeChanged => {
                // Writing the legacy mode registers destroys screen objects.
                state.twod.clear_screens();
                state.damage.all();
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
                let three_d = state.regs.config().capabilities & cap::THREE_D != 0;
                match Snapshot::capture(&view) {
                    Ok(snap) => {
                        for (index, value) in [
                            (fifo::CAPABILITIES, FIFO_CAPABILITIES),
                            (fifo::FLAGS, 0),
                            (fifo::HWVERSION_3D, hw_version_3d(three_d)),
                            (fifo::HWVERSION_3D_REVISED, hw_version_3d(three_d)),
                        ] {
                            if snap.has_register(index) {
                                view.store(reg_index(index), value);
                            }
                        }
                        // The 3D caps record, for drivers that read it from
                        // the FIFO rather than through SVGA_REG_DEV_CAP.
                        if snap.has_register(fifo::CAPS_3D_LAST) {
                            let record = crate::devcaps::fifo_record(state.regs.devcaps());
                            for (i, word) in record.into_iter().enumerate() {
                                view.store(reg_index(fifo::CAPS_3D).saturating_add(i), word);
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
            Effect::CommandBuffer { header, context } => {
                self.submit_command_buffer(state, header, context);
            }
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
            let progressed = !commands.is_empty();
            for command in commands {
                self.execute(state, command, None);
            }
            if let Some(gpu) = state.gpu.as_mut() {
                gpu.flush(&mut state.gb, &state.guest);
            }
            if progressed {
                let mask = state.regs.irq_mask();
                state.irq.raise(irq::FIFO_PROGRESS, mask);
            }
            match pass.stop {
                StopReason::Budget => continue,
                StopReason::Empty | StopReason::Incomplete => return,
                StopReason::Invalid(error) => return self.fail(state, error.to_string()),
                StopReason::BadConfig(error) => return self.fail(state, error.to_string()),
            }
        }
    }

    /// Executes a command; `dx_context`: the DX context of its command
    /// buffer, if any.
    pub(crate) fn execute(&self, state: &mut State, command: Command, dx_context: Option<u32>) {
        state.stats.commands = state.stats.commands.saturating_add(1);
        if trace_level() >= 1 {
            match &command {
                Command::Fence(value) => trace(format!("FIFO Fence({value:#x})")),
                // Guest-backed commands are traced by name below.
                Command::Command3d { id, size, payload }
                    if svga_core::gb::parse(*id, payload).is_some()
                        || svga_core::dx::parse(*id, *size, payload).is_some()
                        || svga_core::legacy::parse(*id, payload).is_some() => {}
                other => trace(format!("FIFO {}", command_name(other))),
            }
        }
        if let Command::Command3d { id, payload, .. } = &command
            && let Some(parsed) = svga_core::gb::parse(*id, payload)
        {
            let _span = crate::prof::span(crate::prof::Cat::Gb);
            let State { gb, guest, gpu, .. } = state;
            let result = parsed
                .map_err(|short| short.to_string())
                .and_then(|gb_command| {
                    if trace_level() >= 1 {
                        let mut text = format!("{gb_command:?}");
                        text.truncate(160);
                        trace(format!("GB {text}"));
                    }
                    // GPU copies the command reads come back first.
                    let Some(gpu) = gpu.as_mut() else {
                        return gb.execute(gb_command, guest);
                    };
                    if gpu.execute_gb(&gb_command, gb, guest)? {
                        return Ok(());
                    }
                    gpu.before_gb(&gb_command, gb, guest)?;
                    let effects = gb_command.clone();
                    gb.execute(gb_command, guest)?;
                    gpu.after_gb(&effects);
                    Ok(())
                });
            if let Err(error) = result {
                trace(format!("GB command {id} failed: {error}"));
            }
            return;
        }
        if let Command::Command3d { id, size, payload } = &command
            && let Some(parsed) = svga_core::dx::parse(*id, *size, payload)
        {
            let _span = crate::prof::span(crate::prof::Cat::Dx);
            let State {
                dx,
                gb,
                guest,
                stats,
                gpu,
                ..
            } = state;
            let result = parsed.map_err(|e| e.to_string()).and_then(|dx_command| {
                if trace_level() >= 1 {
                    let mut text = format!("{dx_command:?}");
                    text.truncate(200);
                    let context = dx_context.map_or("-".to_string(), |c| c.to_string());
                    trace(format!("DX {context} {text}"));
                }
                let Some(gpu) = gpu.as_mut() else {
                    return dx.execute(dx_command, dx_context, gb, guest);
                };
                gpu.before_dx(&dx_command, gb, guest)?;
                let command = dx_command.clone();
                let outcome = dx.execute(dx_command, dx_context, gb, guest)?;
                gpu.after_dx(&command);
                if outcome == crate::dxctx::Outcome::Deferred && crate::gpu::runs_on_gpu(&command) {
                    let context = dx_context
                        .and_then(|cid| dx.context(cid))
                        .ok_or("DX command outside a DX context")?;
                    gpu.execute_dx(&command, context, gb, guest)?;
                    return Ok(crate::dxctx::Outcome::Done);
                }
                Ok(outcome)
            });
            match result {
                Ok(crate::dxctx::Outcome::Done) => stats.dx = stats.dx.saturating_add(1),
                Ok(crate::dxctx::Outcome::Deferred) => {
                    stats.dx_deferred = stats.dx_deferred.saturating_add(1);
                }
                Err(error) => {
                    stats.dx_refused = stats.dx_refused.saturating_add(1);
                    let name = svga_proto::cmd3d_names::name(*id).unwrap_or("?");
                    trace(format!("DX {name} refused: {error}"));
                }
            }
            return;
        }
        if let Command::Command3d { id, payload, .. } = &command
            && let Some(parsed) = svga_core::legacy::parse(*id, payload)
        {
            let State {
                legacy,
                gb,
                guest,
                stats,
                gpu,
                ..
            } = state;
            let result = parsed.map_err(|e| e.to_string()).and_then(|command| {
                if trace_level() >= 1 {
                    let mut text = format!("{command:?}");
                    text.truncate(300);
                    trace(format!("D3D9 {text}"));
                }
                let outcome = {
                    let _span = crate::prof::span(crate::prof::Cat::D3d9State);
                    legacy.execute(&command, gb, guest, gpu.is_some())?
                };
                if outcome == crate::ctx9::Outcome::Gpu
                    && let Some(gpu) = gpu.as_mut()
                {
                    let _span = crate::prof::span(crate::prof::Cat::D3d9Gpu);
                    gpu.execute_legacy(&command, legacy, gb, guest)?;
                }
                Ok(())
            });
            match result {
                Ok(()) => stats.legacy = stats.legacy.saturating_add(1),
                Err(error) => {
                    stats.legacy_refused = stats.legacy_refused.saturating_add(1);
                    let name = svga_proto::cmd3d_names::name(*id).unwrap_or("?");
                    trace(format!("D3D9 {name} refused: {error}"));
                }
            }
            return;
        }
        let State { twod, guest, .. } = state;
        match twod.execute(
            &command,
            &Memory {
                guest,
                vram: &self.vram,
            },
        ) {
            Ok(true) => return,
            Ok(false) => {}
            Err(error) => {
                trace(format!(
                    "2D command failed: {error} ({})",
                    command_name(&command)
                ));
                return;
            }
        }
        match command {
            Command::Update(rect) | Command::UpdateVerbose { rect, .. } => {
                state.stats.updates = state.stats.updates.saturating_add(1);
                state.damage.add(rect.x, rect.y, rect.width, rect.height);
            }
            Command::RectCopy { src_x, src_y, dst } => {
                if let Some(mode) = state.regs.mode() {
                    self.rect_copy(mode, src_x, src_y, dst);
                    state.damage.add(dst.x, dst.y, dst.width, dst.height);
                }
            }
            Command::DefineAlphaCursor {
                hotspot_x,
                hotspot_y,
                width,
                height,
                pixels,
            } => {
                let image = guest_cursor::from_alpha(width, height, hotspot_x, hotspot_y, &pixels);
                self.set_cursor(state, image);
            }
            Command::DefineCursor(cursor) => {
                let image = guest_cursor::from_masks(&cursor);
                self.set_cursor(state, image);
            }
            Command::Fence(value) => {
                let mut view = FifoView(&self.fifo);
                let mut flags = irq::ANY_FENCE;
                if let Ok(snap) = Snapshot::capture(&view) {
                    if snap.has_register(fifo::FENCE) {
                        view.store(reg_index(fifo::FENCE), value);
                    }
                    if snap.has_register(fifo::FENCE_GOAL)
                        && view.load(reg_index(fifo::FENCE_GOAL)) == value
                    {
                        flags |= irq::FENCE_GOAL;
                    }
                }
                let mask = state.regs.irq_mask();
                state.irq.raise(flags, mask);
            }
            Command::Nop => {}
            Command::NopError => self.fail(state, "the guest sent NOP_ERROR".into()),
            other => {
                let kind = match &other {
                    Command::Command3d { id, .. } => format!("3D {id}"),
                    _ => command_name(&other),
                };
                // Without tracing, each kind once (at most 64 kinds).
                let first =
                    !state.unsupported_kinds.contains(&kind) && state.unsupported_kinds.len() < 64;
                if trace_level() >= 1 || first {
                    let text = match &other {
                        Command::Command3d { id, size, payload } => {
                            // The first words, to see what an unknown
                            // command carries.
                            let words: Vec<String> =
                                payload.iter().take(128).map(|w| format!("{w:x}")).collect();
                            format!(
                                "3D {} ({id}), {size} bytes: {}",
                                svga_proto::cmd3d_names::name(*id).unwrap_or("?"),
                                words.join(" ")
                            )
                        }
                        _ => {
                            let mut text = format!("{other:?}");
                            text.truncate(240);
                            text
                        }
                    };
                    if trace_level() >= 1 {
                        trace(format!("FIFO unsupported: {text}"));
                    } else {
                        log!("ignoring unsupported command {text} (again: counted silently)");
                    }
                }
                if first {
                    state.unsupported_kinds.push(kind);
                }
                state.stats.unsupported = state.stats.unsupported.saturating_add(1);
            }
        }
    }

    fn set_cursor(&self, state: &mut State, image: Option<CursorImage>) {
        match image {
            Some(image) => {
                let print = image.fingerprint();
                trace(format!(
                    "guest cursor {}x{}, hotspot ({}, {}), image {print:08x}",
                    image.width, image.height, image.hot_x, image.hot_y
                ));
                dump_cursor(&image, print);
                state.cursor = Some(Arc::new(image));
                state.cursor_serial = state.cursor_serial.wrapping_add(1);
            }
            None => trace("cursor definition ignored (unsupported format)".into()),
        }
    }

    /// The guest cursor: a change counter, the image, and whether the guest
    /// shows it. Visibility comes from the FIFO cursor registers once the
    /// guest has used them (`CURSOR_COUNT` != 0); before that, shown.
    #[cfg_attr(not(feature = "window"), allow(dead_code))]
    pub fn cursor(&self) -> (u64, Option<Arc<CursorImage>>, bool) {
        let state = self.lock();
        let view = FifoView(&self.fifo);
        let visible = match Snapshot::capture(&view) {
            Ok(snap)
                if state.regs.config_done()
                    && snap.has_register(fifo::CURSOR_COUNT)
                    && view.load(reg_index(fifo::CURSOR_COUNT)) != 0 =>
            {
                view.load(reg_index(fifo::CURSOR_ON)) != cursor_on::HIDE
            }
            _ => true,
        };
        (state.cursor_serial, state.cursor.clone(), visible)
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

/// Watches the eventfd KVM signals when the guest EOIs the INTx line, and
/// unmasks the line each time (re-signalling it if still asserted). Polls
/// with a timeout so that setting the returned flag stops the thread.
fn watch_unmask(svga: Arc<Svga>, fd: OwnedFd) -> Arc<AtomicBool> {
    use rustix::event::{PollFd, PollFlags, Timespec, poll};
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    thread::spawn(move || {
        let timeout = Timespec {
            tv_sec: 0,
            tv_nsec: 100_000_000,
        };
        while !flag.load(Ordering::Relaxed) {
            let mut fds = [PollFd::new(&fd, PollFlags::IN)];
            match poll(&mut fds, Some(&timeout)) {
                Ok(0) => {}
                Ok(_) => {
                    let mut counter = [0u8; 8];
                    let _ = rustix::io::read(&fd, &mut counter);
                    let mut state = svga.lock();
                    let mask = state.regs.irq_mask();
                    state.irq.unmask(mask);
                }
                Err(rustix::io::Errno::INTR) => {}
                Err(_) => {
                    thread::sleep(Duration::from_millis(100));
                }
            }
        }
    });
    stop
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
        match index {
            pci_irq::INTX => Some(IrqInfo {
                flags: irq_info_flags::EVENTFD
                    | irq_info_flags::MASKABLE
                    | irq_info_flags::AUTOMASKED,
                count: 1,
            }),
            _ => (index < pci_irq::COUNT).then_some(IrqInfo { flags: 0, count: 0 }),
        }
    }

    fn region_read(&mut self, region: u32, offset: u64, data: &mut [u8]) -> Result<(), Errno> {
        let svga = &self.0;
        let ok = match region {
            pci_region::BAR0 => {
                let state = svga.lock();
                let value = match (offset, data.len()) {
                    (8, 4) => state.irq.pending(),
                    (0..=2, 4) => state.regs.port_read(offset as u16),
                    _ => u32::MAX,
                };
                if trace_level() >= 1 && offset == u64::from(port::VALUE) {
                    let index = state.regs.port_read(port::INDEX);
                    trace(format!("reg {} -> {value:#x}", reg_label(index)));
                }
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
                    if trace_level() >= 1 && offset == u64::from(port::VALUE) {
                        let index = state.regs.port_read(port::INDEX);
                        trace(format!(
                            "reg {} <- {:#x}",
                            reg_label(index),
                            u32::from_le_bytes(bytes)
                        ));
                    }
                    let effect = state
                        .regs
                        .port_write(offset as u16, u32::from_le_bytes(bytes));
                    if offset == u64::from(port::VALUE) || offset == u64::from(port::IRQ_STATUS) {
                        // What must follow the buffers the worker holds.
                        let ordered = match effect {
                            Effect::CommandBuffer { context, .. } => {
                                context == svga_proto::cb::context::DEVICE
                            }
                            Effect::Sync | Effect::ConfigDone(_) => true,
                            _ => false,
                        };
                        if svga.asynchronous && ordered {
                            state = svga.drain(state);
                        }
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
        let shared = mapping.fd.is_some();
        let writable = mapping.flags & vfio_user::wire::dma_flags::WRITE != 0;
        let mut state = self.0.lock();
        state.guest.add(
            mapping.address,
            mapping.size,
            mapping.fd,
            mapping.offset,
            writable,
        )?;
        if trace_level() >= 1 {
            let kind = if shared { "shared" } else { "not shared" };
            log!(
                "DMA window {:#x}+{:#x} ({kind})",
                mapping.address,
                mapping.size
            );
        }
        Ok(())
    }

    fn dma_unmap(&mut self, address: u64, size: u64) -> Result<(), Errno> {
        if self.0.lock().guest.remove(address, size) {
            Ok(())
        } else {
            Err(Errno::EINVAL)
        }
    }

    fn set_irqs(&mut self, set: IrqSet) -> Result<(), Errno> {
        use irq_set_flags::{
            ACTION_MASK, ACTION_TRIGGER, ACTION_UNMASK, DATA_BOOL, DATA_EVENTFD, DATA_NONE,
        };
        if set.index != pci_irq::INTX {
            // Only INTx exists; disabling the others is a no-op.
            return if set.count == 0 {
                Ok(())
            } else {
                Err(Errno::EINVAL)
            };
        }
        if set.start != 0 || set.count > 1 {
            return Err(Errno::EINVAL);
        }
        let data = set.flags & (DATA_NONE | DATA_BOOL | DATA_EVENTFD);
        let action = set.flags & (ACTION_MASK | ACTION_UNMASK | ACTION_TRIGGER);
        // DATA_BOOL carries one byte per sub-index: act only if it is set.
        let selected = match data {
            DATA_BOOL => set.data.first().is_some_and(|&b| b != 0),
            _ => set.count == 1,
        };
        let mut fds = set.fds.into_iter();
        let mut state = self.0.lock();
        let mask = state.regs.irq_mask();
        match (action, data) {
            (ACTION_TRIGGER, DATA_EVENTFD) => {
                let fd = if set.count == 1 {
                    Some(fds.next().ok_or(Errno::EINVAL)?)
                } else {
                    None
                };
                if trace_level() >= 1 {
                    log!(
                        "INTx trigger eventfd {}",
                        if fd.is_some() { "connected" } else { "removed" }
                    );
                }
                state.irq.set_trigger(fd, mask);
            }
            (ACTION_TRIGGER, DATA_NONE) if set.count == 0 => state.irq.set_trigger(None, mask),
            (ACTION_TRIGGER, DATA_NONE | DATA_BOOL) => {}
            (ACTION_MASK, DATA_NONE | DATA_BOOL) => {
                if selected {
                    state.irq.mask();
                }
            }
            (ACTION_UNMASK, DATA_NONE | DATA_BOOL) => {
                if selected {
                    state.irq.unmask(mask);
                }
            }
            (ACTION_UNMASK, DATA_EVENTFD) => {
                if let Some(stop) = state.unmask_watch.take() {
                    stop.store(true, Ordering::Relaxed);
                }
                if set.count == 1 {
                    let fd = fds.next().ok_or(Errno::EINVAL)?;
                    state.unmask_watch = Some(watch_unmask(self.0.clone(), fd));
                }
            }
            _ => return Err(Errno::EINVAL),
        }
        Ok(())
    }

    fn reset(&mut self) -> Result<(), Errno> {
        let mut state = self.0.lock();
        state.regs.reset();
        state.irq.reset();
        state.twod.reset();
        state.gb.reset();
        state.dx.reset();
        state.legacy.reset();
        if let Some(gpu) = state.gpu.as_mut() {
            gpu.reset();
        }
        state.cb.reset();
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
                    asynchronous: false,
                    three_d: false,
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
            asynchronous: false,
            three_d: false,
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
    fn fences_raise_the_intx_line() {
        use rustix::event::{EventfdFlags, eventfd};
        let mut h = device();
        let fd = eventfd(0, EventfdFlags::CLOEXEC | EventfdFlags::NONBLOCK).unwrap();
        let trigger = IrqSet {
            flags: irq_set_flags::ACTION_TRIGGER | irq_set_flags::DATA_EVENTFD,
            index: pci_irq::INTX,
            start: 0,
            count: 1,
            data: Vec::new(),
            fds: vec![fd.try_clone().unwrap()],
        };
        h.set_irqs(trigger).unwrap();
        set(&mut h, reg::IRQMASK, irq::ANY_FENCE);
        enable_fifo(&mut h);
        push(&h, &[cmd::FENCE, 7]);
        set(&mut h, reg::SYNC, 1);
        let mut counter = [0u8; 8];
        assert_eq!(rustix::io::read(&fd, &mut counter), Ok(8), "line signalled");
        let mut status = [0u8; 4];
        h.region_read(0, 8, &mut status).unwrap();
        assert_eq!(u32::from_le_bytes(status) & irq::ANY_FENCE, irq::ANY_FENCE);
        // The guest acknowledges; an unmask then finds the line low.
        h.region_write(0, 8, &irq::ANY_FENCE.to_le_bytes()).unwrap();
        let unmask = IrqSet {
            flags: irq_set_flags::ACTION_UNMASK | irq_set_flags::DATA_NONE,
            index: pci_irq::INTX,
            start: 0,
            count: 1,
            data: Vec::new(),
            fds: Vec::new(),
        };
        h.set_irqs(unmask).unwrap();
        assert!(
            rustix::io::read(&fd, &mut counter).is_err(),
            "no new signal"
        );
        assert_eq!(h.irq_info(pci_irq::INTX).unwrap().count, 1);
    }

    #[test]
    fn only_reported_damage_is_copied() {
        use crate::display::Frame;
        let mut h = device();
        set(&mut h, reg::WIDTH, 64);
        set(&mut h, reg::HEIGHT, 64);
        set(&mut h, reg::BITS_PER_PIXEL, 32);
        set(&mut h, reg::ENABLE, 1);
        enable_fifo(&mut h);
        let mut frame = Frame::default();
        assert!(
            h.0.capture(&mut frame).unwrap().changed,
            "first frame: everything"
        );
        assert!(
            !h.0.capture(&mut frame).unwrap().changed,
            "nothing reported"
        );
        h.0.vram.store_u32(64 + 1, 0x11); // (1, 1): inside the update
        h.0.vram.store_u32(10 * 64 + 10, 0x22); // (10, 10): not reported
        push(&h, &[cmd::UPDATE, 1, 1, 2, 2]);
        set(&mut h, reg::SYNC, 1);
        assert!(h.0.capture(&mut frame).unwrap().changed);
        assert_eq!(frame.pixels[64 + 1], 0x11);
        assert_eq!(
            frame.pixels[10 * 64 + 10],
            0,
            "outside the update: not copied"
        );
    }

    #[test]
    fn the_guest_cursor_and_its_visibility() {
        let mut h = device();
        enable_fifo(&mut h);
        assert_eq!(h.0.cursor().0, 0);
        push(&h, &[cmd::DEFINE_ALPHA_CURSOR, 0, 0, 0, 1, 1, 0xff00_00ff]);
        set(&mut h, reg::SYNC, 1);
        let (serial, image, visible) = h.0.cursor();
        assert_eq!((serial, visible), (1, true));
        assert_eq!(image.unwrap().rgba, [0, 0, 255, 255]);
        h.0.fifo.store_u32(fifo::CURSOR_COUNT as usize, 1);
        h.0.fifo.store_u32(fifo::CURSOR_ON as usize, 0);
        assert!(!h.0.cursor().2, "hidden by the guest");
    }

    #[test]
    fn command_buffers_run_report_and_stop_on_errors() {
        use svga_proto::cb;
        let mut h = device();
        enable_fifo(&mut h);
        set(&mut h, reg::IRQMASK, irq::COMMAND_BUFFER);
        // Guest RAM at 0x10_0000, shared like QEMU does.
        let ram = SharedMem::new("ram", 0x4000).unwrap();
        h.dma_map(DmaMapping {
            address: 0x10_0000,
            size: 0x4000,
            offset: 0,
            flags: 3,
            fd: Some(ram.fd().try_clone_to_owned().unwrap()),
        })
        .unwrap();
        let put = |at: usize, words: &[u32]| {
            for (i, w) in words.iter().enumerate() {
                assert!(ram.write(at + 4 * i, &w.to_le_bytes()));
            }
        };
        let word = |at: usize| {
            let mut b = [0u8; 4];
            assert!(ram.read(at, &mut b));
            u32::from_le_bytes(b)
        };
        // A header at `at` for `length` bytes of commands at `data`.
        let header = |at: usize, length: u32, data: u32| {
            put(at, &[cb::status::NONE, 0, 1, 0, 0, length, data, 0]);
            put(at + 32, &[0; 8]);
        };
        let submit = |h: &mut Handle, at: usize, context: u32| {
            set(h, reg::COMMAND_HIGH, 0);
            set(h, reg::COMMAND_LOW, (0x10_0000 + at as u32) | context);
        };
        let irq_status = |h: &mut Handle| {
            let mut b = [0u8; 4];
            h.region_read(0, 8, &mut b).unwrap();
            u32::from_le_bytes(b)
        };

        // An update then a fence: completed, fence visible, IRQ raised.
        put(0x1000, &[cmd::UPDATE, 0, 0, 8, 8, cmd::FENCE, 0x42]);
        header(0, 28, 0x10_1000);
        submit(&mut h, 0, cb::context::CONTEXT_0);
        assert_eq!(word(0), cb::status::COMPLETED);
        assert_eq!(h.0.fifo.load_u32(fifo::FENCE as usize), 0x42);
        assert_ne!(irq_status(&mut h) & irq::COMMAND_BUFFER, 0);

        // A bad command after a fence: error at its offset, context stopped.
        put(0x1100, &[cmd::FENCE, 0x43, 12345]);
        header(0x40, 12, 0x10_1100);
        submit(&mut h, 0x40, cb::context::CONTEXT_0);
        assert_eq!(word(0x40), cb::status::COMMAND_ERROR);
        assert_eq!(word(0x44), 8, "errorOffset");
        assert_eq!(
            h.0.fifo.load_u32(fifo::FENCE as usize),
            0x43,
            "ran up to it"
        );

        // While stopped, buffers wait; restarting the context runs them.
        put(0x1200, &[cmd::FENCE, 0x44]);
        header(0x80, 8, 0x10_1200);
        submit(&mut h, 0x80, cb::context::CONTEXT_0);
        assert_eq!(word(0x80), cb::status::NONE, "queued");
        put(
            0x1300,
            &[cb::dc::START_STOP_CONTEXT, 1, cb::context::CONTEXT_0],
        );
        header(0xc0, 12, 0x10_1300);
        submit(&mut h, 0xc0, cb::context::DEVICE);
        assert_eq!(word(0xc0), cb::status::COMPLETED, "device command");
        assert_eq!(word(0x80), cb::status::COMPLETED, "queued buffer ran");
        assert_eq!(h.0.fifo.load_u32(fifo::FENCE as usize), 0x44);

        // DX commands of a DX context's buffer apply to that context; one
        // with nine render targets is refused.
        let mut dx = vec![svga_proto::cmd3d::DX_DEFINE_CONTEXT, 4, 7];
        dx.extend([svga_proto::cmd3d::DX_SET_TOPOLOGY, 4, 1]);
        dx.extend([svga_proto::cmd3d::DX_SET_RENDERTARGETS, 40]);
        dx.extend([u32::MAX; 10]);
        put(0x1400, &dx);
        let length = 4 * dx.len() as u32;
        put(
            0x180,
            &[
                cb::status::NONE,
                0,
                2,
                0,
                cb::flag::DX_CONTEXT,
                length,
                0x10_1400,
                0,
            ],
        );
        put(0x180 + 32, &[0, 7, 0, 0, 0, 0, 0, 0]);
        submit(&mut h, 0x180, cb::context::CONTEXT_0);
        assert_eq!(word(0x180), cb::status::COMPLETED);
        {
            let state = h.0.lock();
            assert_eq!((state.stats.dx, state.stats.dx_refused), (2, 1));
            let context = state.dx.context(7).expect("context 7");
            assert_eq!(context.bindings.topology, 1);
        }

        // Contents inside a MOB (SVGA_CB_FLAG_MOB, WDDM 2.0 drivers): MOB 9
        // covers the page at 0x10_2000, the commands sit at its byte 0x10.
        put(
            0x1500,
            &[
                svga_proto::cmd3d::DEFINE_GB_MOB64,
                20,
                9,
                svga_proto::mobfmt::RANGE,
                0x102,
                0,
                4096,
            ],
        );
        header(0x1c0, 28, 0x10_1500);
        submit(&mut h, 0x1c0, cb::context::CONTEXT_0);
        assert_eq!(word(0x1c0), cb::status::COMPLETED, "MOB defined");
        put(0x2010, &[cmd::FENCE, 0x45]);
        put(
            0x200,
            &[cb::status::NONE, 0, 3, 0, cb::flag::MOB, 8, 9, 0x10],
        );
        put(0x200 + 32, &[0; 8]);
        submit(&mut h, 0x200, cb::context::CONTEXT_0);
        assert_eq!(word(0x200), cb::status::COMPLETED, "buffer in a MOB");
        assert_eq!(h.0.fifo.load_u32(fifo::FENCE as usize), 0x45);

        // Bad headers and contexts are refused, not run.
        header(0x100, 6, 0x10_1200);
        submit(&mut h, 0x100, cb::context::CONTEXT_0);
        assert_eq!(word(0x100), cb::status::CB_HEADER_ERROR);
        header(0x140, 8, 0x10_1200);
        submit(&mut h, 0x140, 5);
        assert_eq!(word(0x140), cb::status::SUBMISSION_ERROR);
        let state = h.0.lock();
        assert_eq!((state.cb.completed, state.cb.failed), (6, 3));
    }

    #[test]
    fn vmport_answers_like_the_device() {
        if std::env::var_os("RSVGA_PROBE_CAPS").is_some() {
            return; // probing overrides the advertised capabilities
        }
        for (three_d, caps) in [
            (false, CAPABILITIES),
            (true, CAPABILITIES | CAPABILITIES_3D),
        ] {
            let args = vmport_args(three_d);
            assert!(args.chunks(2).all(|pair| pair[0] == "-global"));
            let globals: Vec<&str> = args.iter().skip(1).step_by(2).map(String::as_str).collect();
            assert!(globals.contains(&"vmport.svga-backdoor=on"));
            assert!(globals.contains(&format!("vmport.svga-caps={caps:#x}").as_str()));
            assert!(
                globals.contains(&format!("vmport.svga-fifo-caps={FIFO_CAPABILITIES:#x}").as_str())
            );
        }
        let mut h = device();
        assert_eq!(get(&mut h, svga_proto::reg::CAPABILITIES), CAPABILITIES);
        assert_eq!(get(&mut h, svga_proto::reg::CAP2), CAPABILITIES_2);
    }

    /// 3D is advertised when a GPU runs it (WDDM 2.0's bits included), and
    /// the device stays 2D without one.
    #[test]
    fn three_d_comes_with_a_gpu() {
        if std::env::var_os("RSVGA_PROBE_CAPS").is_some() {
            return;
        }
        let opts = Options {
            vram_size: 16 << 20,
            fifo_size: 64 << 10,
            max_width: 2560,
            max_height: 1600,
            asynchronous: false,
            three_d: true,
        };
        let mut h = Handle(Svga::new(&opts, Vec::new()).unwrap());
        let gpu = h.0.lock().gpu.is_some();
        let (caps, caps2) = (
            get(&mut h, svga_proto::reg::CAPABILITIES),
            get(&mut h, svga_proto::reg::CAP2),
        );
        if gpu {
            assert_eq!(caps, CAPABILITIES | CAPABILITIES_3D);
            assert_eq!(caps2, CAPABILITIES_2 | CAPABILITIES_2_3D);
            assert_eq!(hw_version_3d(true), HW_VERSION_3D);
        } else {
            eprintln!("no GPU: checked that the device stays 2D");
            assert_eq!((caps, caps2), (CAPABILITIES, CAPABILITIES_2));
        }
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
