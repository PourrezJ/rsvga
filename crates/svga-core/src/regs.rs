//! The SVGA register file, reached through the index/value I/O port pair.
//!
//! Pure state machine: port accesses in, [`Effect`]s out. The device model
//! acts on effects (process the FIFO, switch display mode...). Every guest
//! value is stored as-is and validated when used, in [`Registers::mode`].

use svga_proto::{fifo, id, port, reg};

/// Static properties of the emulated device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    /// Size of the VRAM BAR, in bytes.
    pub vram_size: u32,
    /// Size of the FIFO BAR, in bytes.
    pub fifo_size: u32,
    /// Largest mode width.
    pub max_width: u32,
    /// Largest mode height.
    pub max_height: u32,
    /// Value of [`reg::CAPABILITIES`].
    pub capabilities: u32,
    /// Value of [`reg::GMR_MAX_IDS`] (0 without GMR support).
    pub gmr_max_ids: u32,
    /// Value of [`reg::GMRS_MAX_PAGES`].
    pub gmr_max_pages: u32,
    /// Value of [`reg::MEMORY_SIZE`]: guest memory usable through GMRs, in
    /// bytes.
    pub memory_size: u32,
}

/// Number of scratch registers exposed.
pub const SCRATCH_REGS: u32 = 64;

/// A displayable mode, validated against VRAM.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mode {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Bits per pixel: 16 or 32.
    pub bits_per_pixel: u32,
    /// Bytes per scanline.
    pub pitch: u32,
    /// Offset of the first pixel in VRAM.
    pub offset: u32,
}

impl Mode {
    /// Bytes of VRAM covered by the mode.
    pub fn bytes(&self) -> u64 {
        u64::from(self.pitch).saturating_mul(u64::from(self.height))
    }
}

/// What a register write asks the device to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    /// Nothing beyond storing the value.
    None,
    /// The display mode may have changed.
    ModeChanged,
    /// The guest enabled or disabled the FIFO ([`reg::CONFIG_DONE`]).
    ConfigDone(bool),
    /// The guest asks for the FIFO to be processed ([`reg::SYNC`]).
    Sync,
    /// The guest acknowledged IRQ flags (written to the IRQ status port).
    IrqAck(u32),
    /// The guest changed [`reg::IRQMASK`].
    IrqMask,
}

/// The register file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Registers {
    cfg: Config,
    index: u32,
    id: u32,
    enable: u32,
    width: u32,
    height: u32,
    bits_per_pixel: u32,
    config_done: u32,
    guest_id: u32,
    cursor: [u32; 4],
    pitchlock: u32,
    irq_mask: u32,
    traces: u32,
    displays: [u32; 8],
    fb_start: u32,
    mem_start: u32,
    palette: Vec<u32>,
    scratch: Vec<u32>,
}

impl Registers {
    /// Power-on state.
    pub fn new(cfg: Config) -> Self {
        Self {
            cfg,
            index: 0,
            id: id::ID_2,
            enable: 0,
            width: 1024.min(cfg.max_width),
            height: 768.min(cfg.max_height),
            bits_per_pixel: 32,
            config_done: 0,
            guest_id: 0,
            cursor: [0; 4],
            pitchlock: 0,
            irq_mask: 0,
            traces: 0,
            displays: [0; 8],
            fb_start: 0,
            mem_start: 0,
            palette: vec![0; reg::NUM_PALETTE_REGS as usize],
            scratch: vec![0; SCRATCH_REGS as usize],
        }
    }

    /// Back to power-on state, keeping the BAR addresses.
    pub fn reset(&mut self) {
        let (fb_start, mem_start) = (self.fb_start, self.mem_start);
        *self = Self::new(self.cfg);
        self.fb_start = fb_start;
        self.mem_start = mem_start;
    }

    /// Static configuration.
    pub fn config(&self) -> &Config {
        &self.cfg
    }

    /// Records where the guest placed the VRAM and FIFO BARs.
    pub fn set_bar_addresses(&mut self, vram: u32, fifo: u32) {
        self.fb_start = vram;
        self.mem_start = fifo;
    }

    /// Interrupt sources the guest enabled ([`reg::IRQMASK`]).
    pub fn irq_mask(&self) -> u32 {
        self.irq_mask
    }

    /// Whether the guest enabled SVGA mode.
    pub fn enabled(&self) -> bool {
        self.enable & 1 != 0
    }

    /// Whether the guest enabled the FIFO.
    pub fn config_done(&self) -> bool {
        self.config_done != 0
    }

    /// Bytes per pixel of the current depth, rounded up.
    fn bytes_per_pixel(&self) -> u32 {
        self.bits_per_pixel.div_ceil(8)
    }

    /// Bytes per scanline: the pitch lock if set, else the packed width
    /// rounded up to 4 bytes.
    pub fn bytes_per_line(&self) -> u32 {
        if self.pitchlock != 0 {
            return self.pitchlock;
        }
        let packed = u64::from(self.width).saturating_mul(u64::from(self.bytes_per_pixel()));
        u32::try_from(packed.next_multiple_of(4)).unwrap_or(u32::MAX)
    }

    fn fb_size(&self) -> u32 {
        let size = u64::from(self.bytes_per_line()).saturating_mul(u64::from(self.height));
        u32::try_from(size).unwrap_or(u32::MAX)
    }

    /// The current mode, if SVGA mode is on and the mode fits in VRAM.
    pub fn mode(&self) -> Option<Mode> {
        if !self.enabled() {
            return None;
        }
        let supported_depth = matches!(self.bits_per_pixel, 16 | 32);
        let in_bounds = (1..=self.cfg.max_width).contains(&self.width)
            && (1..=self.cfg.max_height).contains(&self.height);
        let pitch = self.bytes_per_line();
        let packed = u64::from(self.width).saturating_mul(u64::from(self.bytes_per_pixel()));
        let mode = Mode {
            width: self.width,
            height: self.height,
            bits_per_pixel: self.bits_per_pixel,
            pitch,
            offset: 0,
        };
        let fits = u64::from(pitch) >= packed && mode.bytes() <= u64::from(self.cfg.vram_size);
        (supported_depth && in_bounds && fits).then_some(mode)
    }

    /// Reads a port of BAR 0 (`offset` from [`svga_proto::port`]).
    pub fn port_read(&self, offset: u16) -> u32 {
        match offset {
            port::INDEX => self.index,
            port::VALUE => self.read(self.index),
            _ => 0,
        }
    }

    /// Writes a port of BAR 0.
    pub fn port_write(&mut self, offset: u16, value: u32) -> Effect {
        match offset {
            port::INDEX => {
                self.index = value;
                Effect::None
            }
            port::VALUE => self.write(self.index, value),
            port::IRQ_STATUS => Effect::IrqAck(value),
            _ => Effect::None,
        }
    }

    fn depth(&self) -> u32 {
        match self.bits_per_pixel {
            32 => 24,
            other => other,
        }
    }

    fn color_masks(&self) -> (u32, u32, u32) {
        match self.bits_per_pixel {
            16 => (0xf800, 0x07e0, 0x001f),
            24 | 32 => (0x00ff_0000, 0x0000_ff00, 0x0000_00ff),
            _ => (0, 0, 0),
        }
    }

    /// Reads register `index`. Unknown registers read as 0.
    pub fn read(&self, index: u32) -> u32 {
        if let Some(value) = self.banked(index) {
            return value;
        }
        let (red, green, blue) = self.color_masks();
        match index {
            reg::ID => self.id,
            reg::ENABLE => self.enable,
            reg::WIDTH => self.width,
            reg::HEIGHT => self.height,
            reg::MAX_WIDTH => self.cfg.max_width,
            reg::MAX_HEIGHT => self.cfg.max_height,
            reg::DEPTH => self.depth(),
            reg::BITS_PER_PIXEL | reg::HOST_BITS_PER_PIXEL => self.bits_per_pixel,
            reg::PSEUDOCOLOR => u32::from(self.bits_per_pixel == 8),
            reg::RED_MASK => red,
            reg::GREEN_MASK => green,
            reg::BLUE_MASK => blue,
            reg::BYTES_PER_LINE => self.bytes_per_line(),
            reg::FB_START => self.fb_start,
            reg::FB_OFFSET => 0,
            reg::VRAM_SIZE | reg::MAX_PRIMARY_MEM => self.cfg.vram_size,
            reg::FB_SIZE => self.fb_size(),
            reg::CAPABILITIES => self.cfg.capabilities,
            reg::MEM_START => self.mem_start,
            reg::MEM_SIZE => self.cfg.fifo_size,
            reg::CONFIG_DONE => self.config_done,
            reg::SYNC | reg::BUSY => 0, // the FIFO is processed synchronously
            reg::GUEST_ID => self.guest_id,
            reg::CURSOR_X..=reg::CURSOR_ON => self.cursor_reg(index),
            reg::SCRATCH_SIZE => SCRATCH_REGS,
            reg::MEM_REGS => fifo::NUM_REGS,
            reg::NUM_DISPLAYS => 1,
            reg::PITCHLOCK => self.pitchlock,
            reg::IRQMASK => self.irq_mask,
            reg::NUM_GUEST_DISPLAYS..=reg::DISPLAY_HEIGHT => self.display_reg(index),
            reg::TRACES => self.traces,
            reg::GMR_MAX_IDS => self.cfg.gmr_max_ids,
            reg::GMRS_MAX_PAGES => self.cfg.gmr_max_pages,
            reg::MEMORY_SIZE => self.cfg.memory_size,
            _ => 0,
        }
    }

    fn cursor_reg(&self, index: u32) -> u32 {
        let slot = index.wrapping_sub(reg::CURSOR_X) as usize;
        self.cursor.get(slot).copied().unwrap_or(0)
    }

    fn display_reg(&self, index: u32) -> u32 {
        let slot = index.wrapping_sub(reg::NUM_GUEST_DISPLAYS) as usize;
        self.displays.get(slot).copied().unwrap_or(0)
    }

    /// Palette and scratch registers.
    fn banked(&self, index: u32) -> Option<u32> {
        let palette = index
            .checked_sub(reg::PALETTE_BASE)
            .and_then(|i| self.palette.get(i as usize));
        let scratch = index
            .checked_sub(reg::SCRATCH_BASE)
            .and_then(|i| self.scratch.get(i as usize));
        scratch.or(palette).copied()
    }

    fn banked_mut(&mut self, index: u32) -> Option<&mut u32> {
        if let Some(slot) = index.checked_sub(reg::SCRATCH_BASE) {
            return self.scratch.get_mut(slot as usize);
        }
        let slot = index.checked_sub(reg::PALETTE_BASE)?;
        self.palette.get_mut(slot as usize)
    }

    /// Writes register `index`. Read-only and unknown registers ignore writes.
    pub fn write(&mut self, index: u32, value: u32) -> Effect {
        if let Some(slot) = self.banked_mut(index) {
            *slot = value;
            return Effect::None;
        }
        match index {
            reg::ID => {
                if matches!(value, id::ID_0 | id::ID_1 | id::ID_2) {
                    self.id = value;
                }
                Effect::None
            }
            reg::ENABLE => {
                self.enable = value;
                Effect::ModeChanged
            }
            reg::WIDTH => {
                self.width = value;
                Effect::ModeChanged
            }
            reg::HEIGHT => {
                self.height = value;
                Effect::ModeChanged
            }
            reg::BITS_PER_PIXEL => {
                self.bits_per_pixel = value;
                Effect::ModeChanged
            }
            reg::PITCHLOCK => {
                self.pitchlock = value;
                Effect::ModeChanged
            }
            reg::CONFIG_DONE => {
                self.config_done = value;
                Effect::ConfigDone(value != 0)
            }
            reg::SYNC => Effect::Sync,
            reg::GUEST_ID => {
                self.guest_id = value;
                Effect::None
            }
            reg::CURSOR_X..=reg::CURSOR_ON => {
                if let Some(slot) = self
                    .cursor
                    .get_mut(index.wrapping_sub(reg::CURSOR_X) as usize)
                {
                    *slot = value;
                }
                Effect::None
            }
            reg::IRQMASK => {
                self.irq_mask = value;
                Effect::IrqMask
            }
            reg::NUM_GUEST_DISPLAYS..=reg::DISPLAY_HEIGHT => {
                let slot = index.wrapping_sub(reg::NUM_GUEST_DISPLAYS) as usize;
                if let Some(slot) = self.displays.get_mut(slot) {
                    *slot = value;
                }
                Effect::None
            }
            reg::TRACES => {
                self.traces = value;
                Effect::None
            }
            _ => Effect::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use svga_proto::cap;

    fn regs() -> Registers {
        Registers::new(Config {
            vram_size: 16 << 20,
            fifo_size: 2 << 20,
            max_width: 2560,
            max_height: 1600,
            capabilities: cap::EXTENDED_FIFO,
            gmr_max_ids: 64,
            gmr_max_pages: 65536,
            memory_size: 256 << 20,
        })
    }

    fn set(r: &mut Registers, index: u32, value: u32) -> Effect {
        r.port_write(port::INDEX, index);
        r.port_write(port::VALUE, value)
    }

    fn get(r: &mut Registers, index: u32) -> u32 {
        r.port_write(port::INDEX, index);
        r.port_read(port::VALUE)
    }

    #[test]
    fn id_negotiation_accepts_only_known_versions() {
        let mut r = regs();
        assert_eq!(get(&mut r, reg::ID), id::ID_2);
        set(&mut r, reg::ID, id::ID_1);
        assert_eq!(get(&mut r, reg::ID), id::ID_1);
        set(&mut r, reg::ID, 0x9000_0007);
        assert_eq!(get(&mut r, reg::ID), id::ID_1);
    }

    #[test]
    fn a_mode_like_ovmf_sets_it() {
        let mut r = regs();
        assert_eq!(r.mode(), None);
        set(&mut r, reg::WIDTH, 1280);
        set(&mut r, reg::HEIGHT, 800);
        set(&mut r, reg::BITS_PER_PIXEL, 32);
        assert_eq!(set(&mut r, reg::ENABLE, 1), Effect::ModeChanged);
        assert_eq!(get(&mut r, reg::BYTES_PER_LINE), 5120);
        assert_eq!(get(&mut r, reg::FB_SIZE), 5120 * 800);
        assert_eq!(get(&mut r, reg::RED_MASK), 0xff_0000);
        assert_eq!(get(&mut r, reg::DEPTH), 24);
        assert_eq!(
            r.mode(),
            Some(Mode {
                width: 1280,
                height: 800,
                bits_per_pixel: 32,
                pitch: 5120,
                offset: 0
            })
        );
    }

    #[test]
    fn modes_that_do_not_fit_are_not_displayed() {
        let mut r = Registers::new(Config {
            vram_size: 8 << 20,
            ..*regs().config()
        });
        set(&mut r, reg::ENABLE, 1);
        set(&mut r, reg::WIDTH, 2560);
        set(&mut r, reg::HEIGHT, 1600);
        set(&mut r, reg::BITS_PER_PIXEL, 32);
        assert_eq!(r.mode(), None, "8 MiB VRAM < 2560*1600*4");
        set(&mut r, reg::HEIGHT, 1600 / 2);
        assert!(r.mode().is_some());
        set(&mut r, reg::BITS_PER_PIXEL, 24);
        assert_eq!(r.mode(), None, "24 bpp is not supported");
        set(&mut r, reg::BITS_PER_PIXEL, 32);
        set(&mut r, reg::WIDTH, u32::MAX);
        assert_eq!(r.mode(), None);
        set(&mut r, reg::WIDTH, 1024);
        set(&mut r, reg::PITCHLOCK, 16);
        assert_eq!(r.mode(), None, "pitch smaller than a scanline");
    }

    #[test]
    fn fifo_and_sync_effects() {
        let mut r = regs();
        assert_eq!(set(&mut r, reg::CONFIG_DONE, 1), Effect::ConfigDone(true));
        assert!(r.config_done());
        assert_eq!(set(&mut r, reg::SYNC, 1), Effect::Sync);
        assert_eq!(get(&mut r, reg::BUSY), 0);
        assert_eq!(get(&mut r, reg::MEM_REGS), fifo::NUM_REGS);
        assert_eq!(r.port_write(port::IRQ_STATUS, 3), Effect::IrqAck(3));
    }

    #[test]
    fn palette_scratch_and_unknown_registers() {
        let mut r = regs();
        set(&mut r, reg::PALETTE_BASE + 5, 0xab);
        assert_eq!(get(&mut r, reg::PALETTE_BASE + 5), 0xab);
        set(&mut r, reg::SCRATCH_BASE + SCRATCH_REGS - 1, 7);
        assert_eq!(get(&mut r, reg::SCRATCH_BASE + SCRATCH_REGS - 1), 7);
        set(&mut r, reg::SCRATCH_BASE + SCRATCH_REGS, 7);
        assert_eq!(get(&mut r, reg::SCRATCH_BASE + SCRATCH_REGS), 0);
        set(&mut r, u32::MAX, 1);
        assert_eq!(get(&mut r, u32::MAX), 0);
        set(&mut r, reg::VRAM_SIZE, 1);
        assert_eq!(get(&mut r, reg::VRAM_SIZE), 16 << 20, "read-only");
    }

    #[test]
    fn reset_keeps_bar_addresses() {
        let mut r = regs();
        r.set_bar_addresses(0xe000_0000, 0xf000_0000);
        set(&mut r, reg::ENABLE, 1);
        r.reset();
        assert!(!r.enabled());
        assert_eq!(get(&mut r, reg::FB_START), 0xe000_0000);
        assert_eq!(get(&mut r, reg::MEM_START), 0xf000_0000);
    }
}
