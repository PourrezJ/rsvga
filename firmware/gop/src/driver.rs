//! UEFI driver: binds to the rsvga PCI function and produces a Graphics
//! Output Protocol on a child handle, driving the real SVGA registers.

use core::ffi::c_void;
use core::ptr::{self, null_mut};
use core::sync::atomic::{AtomicPtr, Ordering};

use r_efi::efi::{self, Guid, Handle, Status};
use r_efi::protocols::{device_path, driver_binding, graphics_output as gop, pci_io};
use svga_proto::{id, pci, reg};

use crate::blt::{self, Op, Surface};
use crate::modes::ModeList;

static BOOT_SERVICES: AtomicPtr<efi::BootServices> = AtomicPtr::new(null_mut());

fn bs() -> &'static efi::BootServices {
    // SAFETY: set once in `entry` from the system table before any other
    // function of this driver can run; boot services stay valid while the
    // driver is in use (it is a boot service driver).
    unsafe { &*BOOT_SERVICES.load(Ordering::Relaxed) }
}

/// `ACPI_DISPLAY_ADR(1, 0, 0, 1, 0, ACPI_ADR_DISPLAY_TYPE_VGA, 0, 0)`: the
/// same display node OVMF puts in `ConOut` for a PCI VGA controller, so the
/// console finds our output.
const DISPLAY_ADR: u32 = (1 << 31) | (1 << 16) | (1 << 8);
const ACPI_ADR_DP: u8 = 0x03;
const END_ENTIRE: u8 = 0xff;

// --- Port I/O ---------------------------------------------------------------

/// The SVGA register file behind the I/O BAR.
#[derive(Clone, Copy)]
struct Svga {
    base: u16,
}

impl Svga {
    fn out32(port: u16, value: u32) {
        // SAFETY: plain port write to the device's own I/O BAR.
        unsafe {
            core::arch::asm!("out dx, eax", in("dx") port, in("eax") value, options(nostack, preserves_flags))
        }
    }

    fn in32(port: u16) -> u32 {
        let value: u32;
        // SAFETY: plain port read from the device's own I/O BAR.
        unsafe {
            core::arch::asm!("in eax, dx", in("dx") port, out("eax") value, options(nostack, preserves_flags))
        }
        value
    }

    fn write(self, index: u32, value: u32) {
        Self::out32(self.base, index);
        Self::out32(self.base.wrapping_add(1), value); // unaligned value port
    }

    fn read(self, index: u32) -> u32 {
        Self::out32(self.base, index);
        Self::in32(self.base.wrapping_add(1))
    }
}

// --- Per-device state -------------------------------------------------------

/// Everything a started device needs. `gop` must stay the first field: the
/// protocol callbacks receive a pointer to it and cast it back to `Private`.
#[repr(C)]
struct Private {
    gop: gop::Protocol,
    mode: gop::Mode,
    info: gop::ModeInformation,
    svga: Svga,
    fb_base: u64,
    modes: ModeList,
    pci_io: *mut pci_io::Protocol,
    controller: Handle,
    child: Handle,
    device_path: *mut device_path::Protocol,
}

fn alloc(size: usize) -> Result<*mut c_void, Status> {
    let mut out = null_mut();
    // SAFETY: boot service call with a valid out pointer.
    let status = unsafe { (bs().allocate_pool)(efi::BOOT_SERVICES_DATA, size, &mut out) };
    if status.is_error() {
        Err(status)
    } else {
        Ok(out)
    }
}

fn free(ptr: *mut c_void) {
    if !ptr.is_null() {
        // SAFETY: `ptr` came from `alloc`.
        unsafe { (bs().free_pool)(ptr) };
    }
}

fn guid(g: &Guid) -> *mut Guid {
    ptr::from_ref(g).cast_mut()
}

// --- PCI helpers ------------------------------------------------------------

fn config_read32(pci: *mut pci_io::Protocol, offset: u32) -> Option<u32> {
    let mut value = 0u32;
    // SAFETY: `pci` is an opened PciIo protocol; we read one dword.
    let status = unsafe {
        ((*pci).pci.read)(
            pci,
            pci_io::WIDTH_UINT32,
            offset,
            1,
            ptr::from_mut(&mut value).cast(),
        )
    };
    (!status.is_error()).then_some(value)
}

fn is_rsvga(pci: *mut pci_io::Protocol) -> bool {
    let ids = u32::from(pci::VENDOR_ID_VMWARE) | (u32::from(pci::DEVICE_ID_SVGA2) << 16);
    config_read32(pci, 0) == Some(ids)
}

fn open_pci(
    this: &driver_binding::Protocol,
    controller: Handle,
) -> Result<*mut pci_io::Protocol, Status> {
    let mut pci = null_mut();
    // SAFETY: boot service call with valid pointers.
    let status = unsafe {
        (bs().open_protocol)(
            controller,
            guid(&pci_io::PROTOCOL_GUID),
            &mut pci,
            this.driver_binding_handle,
            controller,
            efi::OPEN_PROTOCOL_BY_DRIVER,
        )
    };
    if status.is_error() {
        Err(status)
    } else {
        Ok(pci.cast())
    }
}

fn close_pci(this: &driver_binding::Protocol, controller: Handle, user: Handle) {
    // SAFETY: closes an open done by this driver.
    unsafe {
        (bs().close_protocol)(
            controller,
            guid(&pci_io::PROTOCOL_GUID),
            this.driver_binding_handle,
            user,
        );
    }
}

/// Size of a device path in bytes, end node excluded.
///
/// # Safety
/// `path` must be a valid, end-terminated device path.
unsafe fn path_len(path: *const device_path::Protocol) -> usize {
    let mut len = 0usize;
    let mut node = path.cast::<u8>();
    loop {
        // SAFETY: walking a valid device path node by node.
        let (kind, sub, size) = unsafe {
            let header = &*node.cast::<device_path::Protocol>();
            (
                header.r#type,
                header.sub_type,
                usize::from(u16::from_le_bytes(header.length)),
            )
        };
        if kind == device_path::TYPE_END && sub == END_ENTIRE {
            return len;
        }
        if size < 4 {
            return len; // malformed: stop rather than loop forever
        }
        len += size;
        // SAFETY: the next node starts right after this one.
        node = unsafe { node.add(size) };
    }
}

// --- Driver binding ---------------------------------------------------------

unsafe extern "efiapi" fn supported(
    this: *mut driver_binding::Protocol,
    controller: Handle,
    remaining: *mut device_path::Protocol,
) -> Status {
    // SAFETY: firmware passes our own binding protocol.
    let this = unsafe { &*this };
    if !remaining.is_null() {
        // SAFETY: firmware passes a valid node when non-null.
        let node = unsafe { &*remaining };
        let end = node.r#type == device_path::TYPE_END;
        let adr = node.r#type == device_path::TYPE_ACPI && node.sub_type == ACPI_ADR_DP;
        if !end && !adr {
            return Status::UNSUPPORTED;
        }
    }
    let pci = match open_pci(this, controller) {
        Ok(pci) => pci,
        Err(status) => return status,
    };
    let ours = is_rsvga(pci);
    close_pci(this, controller, controller);
    if ours {
        Status::SUCCESS
    } else {
        Status::UNSUPPORTED
    }
}

unsafe extern "efiapi" fn start(
    this: *mut driver_binding::Protocol,
    controller: Handle,
    _remaining: *mut device_path::Protocol,
) -> Status {
    // SAFETY: firmware passes our own binding protocol.
    let this = unsafe { &*this };
    let pci = match open_pci(this, controller) {
        Ok(pci) => pci,
        Err(status) => return status,
    };
    // SAFETY: `pci` stays open BY_DRIVER for the lifetime of the device.
    match unsafe { start_device(this, controller, pci) } {
        Ok(()) => Status::SUCCESS,
        Err(status) => {
            close_pci(this, controller, controller);
            status
        }
    }
}

/// # Safety
/// `pci` must be the PciIo protocol of `controller`, opened by this driver.
unsafe fn start_device(
    this: &driver_binding::Protocol,
    controller: Handle,
    pci: *mut pci_io::Protocol,
) -> Result<(), Status> {
    let enable = pci_io::ATTRIBUTE_IO | pci_io::ATTRIBUTE_MEMORY;
    // SAFETY: valid PciIo protocol.
    let status =
        unsafe { ((*pci).attributes)(pci, pci_io::ATTRIBUTE_OPERATION_ENABLE, enable, null_mut()) };
    if status.is_error() {
        return Err(status);
    }

    // The PCI bus driver has assigned the BARs; read them back.
    let bar0 = config_read32(pci, 0x10).ok_or(Status::DEVICE_ERROR)?;
    let bar1 = config_read32(pci, 0x14).ok_or(Status::DEVICE_ERROR)?;
    let io = u16::try_from(bar0 & 0xffff_fffc).map_err(|_| Status::DEVICE_ERROR)?;
    let fb_base = u64::from(bar1 & 0xffff_fff0);
    if bar0 & 1 == 0 || io == 0 || fb_base == 0 {
        return Err(Status::DEVICE_ERROR);
    }
    let svga = Svga { base: io };
    svga.write(reg::ID, id::ID_2);
    if svga.read(reg::ID) != id::ID_2 {
        return Err(Status::DEVICE_ERROR);
    }
    let modes = ModeList::new(
        svga.read(reg::MAX_WIDTH),
        svga.read(reg::MAX_HEIGHT),
        svga.read(reg::VRAM_SIZE),
    );
    let preferred = modes.preferred().ok_or(Status::DEVICE_ERROR)?;

    // Child device path: controller path + ACPI display node + end.
    let mut parent: *mut c_void = null_mut();
    // SAFETY: boot service call with valid pointers.
    let status = unsafe {
        (bs().open_protocol)(
            controller,
            guid(&device_path::PROTOCOL_GUID),
            &mut parent,
            this.driver_binding_handle,
            controller,
            efi::OPEN_PROTOCOL_GET_PROTOCOL,
        )
    };
    if status.is_error() {
        return Err(status);
    }
    // SAFETY: the firmware's device path for `controller`.
    let parent_len = unsafe { path_len(parent.cast()) };
    let path = alloc(parent_len + 8 + 4)?.cast::<u8>();
    // SAFETY: `path` holds parent_len + 12 bytes; we copy the parent nodes and
    // append the 8-byte ACPI ADR node and the 4-byte end node.
    unsafe {
        ptr::copy_nonoverlapping(parent.cast::<u8>(), path, parent_len);
        let adr = path.add(parent_len);
        adr.write(device_path::TYPE_ACPI);
        adr.add(1).write(ACPI_ADR_DP);
        adr.add(2)
            .cast::<[u8; 2]>()
            .write_unaligned(8u16.to_le_bytes());
        adr.add(4)
            .cast::<[u8; 4]>()
            .write_unaligned(DISPLAY_ADR.to_le_bytes());
        let end = adr.add(8);
        end.write(device_path::TYPE_END);
        end.add(1).write(END_ENTIRE);
        end.add(2)
            .cast::<[u8; 2]>()
            .write_unaligned(4u16.to_le_bytes());
    }

    let private = match alloc(core::mem::size_of::<Private>()) {
        Ok(p) => p.cast::<Private>(),
        Err(status) => {
            free(path.cast());
            return Err(status);
        }
    };
    // SAFETY: `private` is a fresh allocation of the right size; `write`
    // initialises it without reading the old contents.
    unsafe {
        private.write(Private {
            gop: gop::Protocol {
                query_mode,
                set_mode,
                blt: blt_entry,
                mode: null_mut(),
            },
            mode: gop::Mode {
                max_mode: modes.len(),
                mode: 0,
                info: null_mut(),
                size_of_info: core::mem::size_of::<gop::ModeInformation>(),
                frame_buffer_base: fb_base,
                frame_buffer_size: 0,
            },
            info: gop::ModeInformation {
                version: 0,
                horizontal_resolution: 0,
                vertical_resolution: 0,
                pixel_format: gop::PIXEL_BLUE_GREEN_RED_RESERVED_8_BIT_PER_COLOR,
                pixel_information: gop::PixelBitmask {
                    red_mask: 0,
                    green_mask: 0,
                    blue_mask: 0,
                    reserved_mask: 0,
                },
                pixels_per_scan_line: 0,
            },
            svga,
            fb_base,
            modes,
            pci_io: pci,
            controller,
            child: null_mut(),
            device_path: path.cast(),
        });
        let p = &mut *private;
        p.mode.info = &raw mut p.info;
        p.gop.mode = &raw mut p.mode;
    }

    // SAFETY: `private` is initialised and owned by this device.
    let status = unsafe { set_mode(private.cast(), preferred) };
    if status.is_error() {
        free(private.cast());
        free(path.cast());
        return Err(status);
    }

    // SAFETY: installing protocols on a new child handle, then recording the
    // parent/child relationship through PciIo, as the UEFI driver model asks.
    unsafe {
        let p = &mut *private;
        let mut child: Handle = null_mut();
        let mut status = (bs().install_protocol_interface)(
            &mut child,
            guid(&device_path::PROTOCOL_GUID),
            efi::NATIVE_INTERFACE,
            p.device_path.cast(),
        );
        if !status.is_error() {
            status = (bs().install_protocol_interface)(
                &mut child,
                guid(&gop::PROTOCOL_GUID),
                efi::NATIVE_INTERFACE,
                (&raw mut p.gop).cast(),
            );
        }
        if status.is_error() {
            free(private.cast());
            free(path.cast());
            return Err(status);
        }
        p.child = child;
        let mut unused = null_mut();
        (bs().open_protocol)(
            controller,
            guid(&pci_io::PROTOCOL_GUID),
            &mut unused,
            this.driver_binding_handle,
            child,
            efi::OPEN_PROTOCOL_BY_CHILD_CONTROLLER,
        );
    }
    Ok(())
}

unsafe extern "efiapi" fn stop(
    this: *mut driver_binding::Protocol,
    controller: Handle,
    children: usize,
    child_handles: *mut Handle,
) -> Status {
    // SAFETY: firmware passes our own binding protocol.
    let this = unsafe { &*this };
    for i in 0..children {
        // SAFETY: firmware passes `children` valid handles.
        let child = unsafe { *child_handles.add(i) };
        let mut interface = null_mut();
        // SAFETY: boot service calls on our own child handle.
        unsafe {
            let status = (bs().open_protocol)(
                child,
                guid(&gop::PROTOCOL_GUID),
                &mut interface,
                this.driver_binding_handle,
                controller,
                efi::OPEN_PROTOCOL_GET_PROTOCOL,
            );
            if status.is_error() {
                return status;
            }
            let private = interface.cast::<Private>();
            close_pci(this, controller, child);
            (bs().uninstall_protocol_interface)(child, guid(&gop::PROTOCOL_GUID), interface);
            (bs().uninstall_protocol_interface)(
                child,
                guid(&device_path::PROTOCOL_GUID),
                (*private).device_path.cast(),
            );
            free((*private).device_path.cast());
            free(private.cast());
        }
    }
    if children == 0 {
        close_pci(this, controller, controller);
    }
    Status::SUCCESS
}

// --- Graphics Output Protocol ----------------------------------------------

/// # Safety
/// `this` must be the `gop` field of a live `Private`.
unsafe fn private<'a>(this: *mut gop::Protocol) -> &'a mut Private {
    // SAFETY: `gop` is the first field of the `repr(C)` `Private`.
    unsafe { &mut *this.cast::<Private>() }
}

fn mode_info(width: u32, height: u32, pitch: u32) -> gop::ModeInformation {
    gop::ModeInformation {
        version: 0,
        horizontal_resolution: width,
        vertical_resolution: height,
        pixel_format: gop::PIXEL_BLUE_GREEN_RED_RESERVED_8_BIT_PER_COLOR,
        pixel_information: gop::PixelBitmask {
            red_mask: 0,
            green_mask: 0,
            blue_mask: 0,
            reserved_mask: 0,
        },
        pixels_per_scan_line: pitch / 4,
    }
}

unsafe extern "efiapi" fn query_mode(
    this: *mut gop::Protocol,
    mode: u32,
    size: *mut usize,
    info: *mut *mut gop::ModeInformation,
) -> Status {
    if this.is_null() || size.is_null() || info.is_null() {
        return Status::INVALID_PARAMETER;
    }
    // SAFETY: firmware hands back the protocol we installed.
    let p = unsafe { private(this) };
    let Some((width, height)) = p.modes.get(mode) else {
        return Status::INVALID_PARAMETER;
    };
    let Ok(out) = alloc(core::mem::size_of::<gop::ModeInformation>()) else {
        return Status::OUT_OF_RESOURCES;
    };
    // SAFETY: `out` is a fresh allocation of the right size; `size` and
    // `info` were checked non-null.
    unsafe {
        out.cast::<gop::ModeInformation>()
            .write(mode_info(width, height, width * 4));
        *size = core::mem::size_of::<gop::ModeInformation>();
        *info = out.cast();
    }
    Status::SUCCESS
}

unsafe extern "efiapi" fn set_mode(this: *mut gop::Protocol, mode: u32) -> Status {
    if this.is_null() {
        return Status::INVALID_PARAMETER;
    }
    // SAFETY: firmware hands back the protocol we installed.
    let p = unsafe { private(this) };
    let Some((width, height)) = p.modes.get(mode) else {
        return Status::UNSUPPORTED;
    };
    let svga = p.svga;
    svga.write(reg::ENABLE, 0);
    svga.write(reg::WIDTH, width);
    svga.write(reg::HEIGHT, height);
    svga.write(reg::BITS_PER_PIXEL, 32);
    svga.write(reg::ENABLE, 1);
    let pitch = svga.read(reg::BYTES_PER_LINE);
    let offset = svga.read(reg::FB_OFFSET);
    if pitch < width * 4 {
        return Status::DEVICE_ERROR;
    }
    p.info = mode_info(width, height, pitch);
    p.mode.mode = mode;
    p.mode.frame_buffer_base = p.fb_base + u64::from(offset);
    p.mode.frame_buffer_size = pitch as usize * height as usize;

    let mut fb = Framebuffer::of(p);
    let black = [0u32];
    let clear = blt::Request {
        op: Op::VideoFill,
        src_x: 0,
        src_y: 0,
        dst_x: 0,
        dst_y: 0,
        width: width as usize,
        height: height as usize,
        delta: 0,
    };
    let _ = blt::blt(&mut fb, Some(&mut [black[0]]), &clear);
    Status::SUCCESS
}

/// The linear framebuffer of the current mode.
struct Framebuffer {
    base: *mut u32,
    width: usize,
    height: usize,
    stride: usize,
}

impl Framebuffer {
    fn of(p: &Private) -> Self {
        Self {
            base: p.mode.frame_buffer_base as *mut u32,
            width: p.info.horizontal_resolution as usize,
            height: p.info.vertical_resolution as usize,
            stride: p.info.pixels_per_scan_line as usize,
        }
    }
}

impl Surface for Framebuffer {
    fn width(&self) -> usize {
        self.width
    }
    fn height(&self) -> usize {
        self.height
    }
    fn read(&self, x: usize, y: usize) -> u32 {
        // SAFETY: `blt` keeps (x, y) inside the mode, which fits in VRAM.
        unsafe { self.base.add(y * self.stride + x).read_volatile() }
    }
    fn write(&mut self, x: usize, y: usize, pixel: u32) {
        // SAFETY: as in `read`.
        unsafe { self.base.add(y * self.stride + x).write_volatile(pixel) }
    }
}

unsafe extern "efiapi" fn blt_entry(
    this: *mut gop::Protocol,
    buffer: *mut gop::BltPixel,
    operation: gop::BltOperation,
    src_x: usize,
    src_y: usize,
    dst_x: usize,
    dst_y: usize,
    width: usize,
    height: usize,
    delta: usize,
) -> Status {
    if this.is_null() {
        return Status::INVALID_PARAMETER;
    }
    // SAFETY: firmware hands back the protocol we installed.
    let p = unsafe { private(this) };
    let op = match operation {
        gop::BLT_VIDEO_FILL => Op::VideoFill,
        gop::BLT_VIDEO_TO_BLT_BUFFER => Op::VideoToBuffer,
        gop::BLT_BUFFER_TO_VIDEO => Op::BufferToVideo,
        gop::BLT_VIDEO_TO_VIDEO => Op::VideoToVideo,
        _ => return Status::INVALID_PARAMETER,
    };
    let request = blt::Request {
        op,
        src_x,
        src_y,
        dst_x,
        dst_y,
        width,
        height,
        delta,
    };

    // The caller's buffer spans every row the operation touches: compute
    // that extent so the slice we build never exceeds what it promised.
    let stride = if delta == 0 { width } else { delta / 4 };
    let (bx, by) = match op {
        Op::VideoFill => (0, 0),
        Op::VideoToBuffer => (dst_x, dst_y),
        Op::BufferToVideo => (src_x, src_y),
        Op::VideoToVideo => (0, 0),
    };
    let len = match op {
        Op::VideoFill => Some(1),
        Op::VideoToVideo => Some(0),
        _ => by
            .checked_add(height)
            .and_then(|rows| rows.checked_sub(1))
            .and_then(|last| last.checked_mul(stride))
            .and_then(|start| start.checked_add(bx))
            .and_then(|start| start.checked_add(width)),
    };
    let Some(len) = len else {
        return Status::INVALID_PARAMETER;
    };
    let slice = if buffer.is_null() || len == 0 {
        None
    } else {
        // SAFETY: GOP callers guarantee `buffer` covers the rectangle they
        // describe with (x, y, width, height, delta); `len` is exactly that.
        Some(unsafe { core::slice::from_raw_parts_mut(buffer.cast::<u32>(), len) })
    };
    if op != Op::VideoToVideo && slice.is_none() {
        return Status::INVALID_PARAMETER;
    }
    let mut fb = Framebuffer::of(p);
    match blt::blt(&mut fb, slice, &request) {
        Ok(()) => Status::SUCCESS,
        Err(blt::Invalid) => Status::INVALID_PARAMETER,
    }
}

// --- Entry point ------------------------------------------------------------

/// Installs the driver binding protocol on our image handle.
pub fn entry(image: Handle, system_table: *mut efi::SystemTable) -> Status {
    // SAFETY: the firmware passes a valid system table to the entry point.
    let services = unsafe { (*system_table).boot_services };
    BOOT_SERVICES.store(services, Ordering::Relaxed);

    let Ok(binding) = alloc(core::mem::size_of::<driver_binding::Protocol>()) else {
        return Status::OUT_OF_RESOURCES;
    };
    let binding = binding.cast::<driver_binding::Protocol>();
    // SAFETY: fresh allocation of the right size, initialised before use.
    unsafe {
        binding.write(driver_binding::Protocol {
            supported,
            start,
            stop,
            version: 0x10,
            image_handle: image,
            driver_binding_handle: image,
        });
        let mut handle = image;
        (bs().install_protocol_interface)(
            &mut handle,
            guid(&driver_binding::PROTOCOL_GUID),
            efi::NATIVE_INTERFACE,
            binding.cast(),
        )
    }
}
