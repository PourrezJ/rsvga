//! `rsvga`: a VMware SVGA II device for QEMU, served over vfio-user.
//!
//! ```text
//! rsvga --socket /run/user/1000/rsvga.sock [--snapshot frame.png] [--window]
//! qemu-system-x86_64 ... -device '{"driver":"vfio-user-pci",
//!     "socket":{"type":"unix","path":"/run/user/1000/rsvga.sock"}}'
//! ```

mod cursor;
mod damage;
mod device;
mod display;
mod guest;
mod irq;
mod pci;
#[cfg(any(feature = "window", test))]
mod qmp;
mod rom;
#[cfg(any(feature = "window", test))]
mod scale;
mod shm;
mod twod;
#[cfg(feature = "window")]
mod window;

use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use std::{fs, process, thread};

use device::{Handle, Options, Svga};
use display::{Frame, write_png};

static START: OnceLock<Instant> = OnceLock::new();

/// Logs to stderr with the time since start.
#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => {
        eprintln!("[{:8.3}] {}", $crate::uptime(), format_args!($($arg)*))
    };
}

/// Seconds since the server started.
pub fn uptime() -> f64 {
    START.get_or_init(Instant::now).elapsed().as_secs_f64()
}

struct Args {
    socket: PathBuf,
    rom: Option<PathBuf>,
    snapshot: Option<PathBuf>,
    window: bool,
    qmp: Option<PathBuf>,
    options: Options,
}

const USAGE: &str = "usage: rsvga --socket PATH [--rom DRIVER.efi] [--vram-mb N] [--fifo-kb N] \
                     [--snapshot FILE.png] [--window [--qmp QEMU.sock]]";

fn parse_args() -> Result<Args, String> {
    let mut socket = None;
    let mut rom = None;
    let mut snapshot = None;
    let mut window = false;
    let mut qmp = None;
    let mut options = Options {
        vram_size: 64 << 20,
        fifo_size: 2 << 20,
        max_width: 3840,
        max_height: 2160,
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "--socket" => socket = Some(PathBuf::from(value()?)),
            "--snapshot" => snapshot = Some(PathBuf::from(value()?)),
            "--rom" => rom = Some(PathBuf::from(value()?)),
            "--window" => window = true,
            "--qmp" => qmp = Some(PathBuf::from(value()?)),
            "--vram-mb" => options.vram_size = size(&value()?, 20)?,
            "--fifo-kb" => options.fifo_size = size(&value()?, 10)?,
            "--help" | "-h" => return Err(USAGE.into()),
            other => return Err(format!("unknown argument {other}\n{USAGE}")),
        }
    }
    let socket = socket.ok_or_else(|| USAGE.to_string())?;
    Ok(Args {
        socket,
        rom,
        snapshot,
        window,
        qmp,
        options,
    })
}

/// Reads a UEFI driver and packs it as the card's option ROM.
fn load_rom(path: &std::path::Path) -> Result<Vec<u8>, String> {
    let pe = fs::read(path).map_err(|error| error.to_string())?;
    rom::build(
        &pe,
        svga_proto::pci::VENDOR_ID_VMWARE,
        svga_proto::pci::DEVICE_ID_SVGA2,
        0x03_0000,
    )
    .map_err(|error| error.to_string())
}

/// Parses a power-of-two size given in units of `2^shift` bytes.
fn size(text: &str, shift: u32) -> Result<u32, String> {
    let units: u32 = text.parse().map_err(|_| format!("bad size {text}"))?;
    units
        .checked_shl(shift)
        .filter(|bytes| bytes.is_power_of_two() && units.leading_zeros() > shift)
        .ok_or_else(|| format!("size {text} must be a power of two that fits in 32 bits"))
}

fn main() {
    START.get_or_init(Instant::now);
    if let Some(level) = std::env::var("RSVGA_TRACE")
        .ok()
        .and_then(|v| v.parse().ok())
    {
        device::TRACE.store(level, std::sync::atomic::Ordering::Relaxed);
    }
    let args = parse_args().unwrap_or_else(|message| {
        eprintln!("{message}");
        process::exit(2);
    });
    if args.window && !cfg!(feature = "window") {
        eprintln!("this build has no window support (rebuild with --features window)");
        process::exit(2);
    }

    let rom = match &args.rom {
        None => Vec::new(),
        Some(path) => load_rom(path).unwrap_or_else(|error| {
            eprintln!("cannot use {} as option ROM: {error}", path.display());
            process::exit(2);
        }),
    };
    if !rom.is_empty() {
        log!("option ROM: {} bytes", rom.len());
    }
    let svga = Svga::new(&args.options, rom).unwrap_or_else(|error| {
        eprintln!("cannot allocate device memory: {error}");
        process::exit(1);
    });
    let _ = fs::remove_file(&args.socket);
    let listener = UnixListener::bind(&args.socket).unwrap_or_else(|error| {
        eprintln!("cannot listen on {}: {error}", args.socket.display());
        process::exit(1);
    });
    log!(
        "rsvga listening on {} (VRAM {} MiB, FIFO {} KiB)",
        args.socket.display(),
        args.options.vram_size >> 20,
        args.options.fifo_size >> 10
    );

    let served = svga.clone();
    thread::spawn(move || {
        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    log!("client connected");
                    served.set_client(true);
                    let mut handle = Handle(served.clone());
                    match vfio_user::serve(stream, &mut handle) {
                        Ok(()) => log!("client disconnected"),
                        Err(error) => log!("connection closed: {error}"),
                    }
                    served.set_client(false);
                    let _ = vfio_user::Device::reset(&mut handle);
                }
                Err(error) => log!("accept failed: {error}"),
            }
        }
    });

    #[cfg(feature = "window")]
    if args.window {
        window::run(svga, args.snapshot, args.qmp);
        return;
    }
    if args.qmp.is_some() {
        log!("--qmp is only used with --window: no input will be forwarded");
    }
    display_loop(&svga, args.snapshot.as_deref());
}

/// Headless mode: processes the FIFO and refreshes the snapshot at about
/// 60 Hz. With `--window`, [`window::run`] does the same from its event loop.
fn display_loop(svga: &Svga, snapshot: Option<&std::path::Path>) {
    let mut frame = Frame::default();
    let mut last_snapshot = Instant::now();
    let mut last_status = Instant::now();
    let tracing = std::env::var("RSVGA_TRACE").is_ok_and(|v| v != "0");
    loop {
        if tracing && last_status.elapsed() >= Duration::from_secs(5) {
            last_status = Instant::now();
            log!("{}", svga.fifo_status());
        }
        let started = Instant::now();
        {
            let mut state = svga.lock();
            svga.process_fifo(&mut state);
        }
        let mode = svga.capture(&mut frame);
        if let (Some(path), Some(_)) = (snapshot, mode) {
            if last_snapshot.elapsed() >= Duration::from_secs(1) {
                last_snapshot = Instant::now();
                if let Err(error) = save(path, &frame) {
                    log!("cannot write {}: {error}", path.display());
                }
            }
        }
        thread::sleep(Duration::from_millis(16).saturating_sub(started.elapsed()));
    }
}

/// Writes the PNG next to its destination, then renames it into place, so a
/// reader never sees a half-written file.
pub(crate) fn save(path: &std::path::Path, frame: &Frame) -> std::io::Result<()> {
    let partial = path.with_extension("png.partial");
    let mut file = std::io::BufWriter::new(fs::File::create(&partial)?);
    write_png(&mut file, frame)?;
    std::io::Write::flush(&mut file)?;
    drop(file);
    fs::rename(partial, path)
}
