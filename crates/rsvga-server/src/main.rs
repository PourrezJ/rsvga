//! `rsvga`: a VMware SVGA II device for QEMU, served over vfio-user.
//!
//! ```text
//! rsvga --socket /run/user/1000/rsvga.sock [--snapshot frame.png] [--window]
//! qemu-system-x86_64 ... -device '{"driver":"vfio-user-pci",
//!     "socket":{"type":"unix","path":"/run/user/1000/rsvga.sock"}}'
//! ```

mod device;
mod display;
mod pci;
mod shm;

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
    snapshot: Option<PathBuf>,
    window: bool,
    options: Options,
}

const USAGE: &str =
    "usage: rsvga --socket PATH [--vram-mb N] [--fifo-kb N] [--snapshot FILE.png] [--window]";

fn parse_args() -> Result<Args, String> {
    let mut socket = None;
    let mut snapshot = None;
    let mut window = false;
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
            "--window" => window = true,
            "--vram-mb" => options.vram_size = size(&value()?, 20)?,
            "--fifo-kb" => options.fifo_size = size(&value()?, 10)?,
            "--help" | "-h" => return Err(USAGE.into()),
            other => return Err(format!("unknown argument {other}\n{USAGE}")),
        }
    }
    let socket = socket.ok_or_else(|| USAGE.to_string())?;
    Ok(Args {
        socket,
        snapshot,
        window,
        options,
    })
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
    if std::env::var_os("RSVGA_TRACE").is_some_and(|v| v == "1") {
        device::TRACE.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    let args = parse_args().unwrap_or_else(|message| {
        eprintln!("{message}");
        process::exit(2);
    });
    if args.window && !cfg!(feature = "window") {
        eprintln!("this build has no window support (rebuild with --features window)");
        process::exit(2);
    }

    let svga = Svga::new(&args.options).unwrap_or_else(|error| {
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
                    let mut handle = Handle(served.clone());
                    match vfio_user::serve(stream, &mut handle) {
                        Ok(()) => log!("client disconnected"),
                        Err(error) => log!("connection closed: {error}"),
                    }
                    let _ = vfio_user::Device::reset(&mut handle);
                }
                Err(error) => log!("accept failed: {error}"),
            }
        }
    });

    display_loop(&svga, args.snapshot.as_deref(), args.window);
}

/// Processes the FIFO and refreshes the outputs at about 60 Hz.
fn display_loop(svga: &Svga, snapshot: Option<&std::path::Path>, window: bool) {
    let mut frame = Frame::default();
    let mut last_snapshot = Instant::now();
    #[cfg(feature = "window")]
    let mut win = window.then(window::Window::open);
    #[cfg(not(feature = "window"))]
    let _ = window;

    loop {
        let started = Instant::now();
        {
            let mut state = svga.lock();
            svga.process_fifo(&mut state);
        }
        let mode = svga.capture(&mut frame);

        if let Some(path) = snapshot {
            if mode.is_some() && last_snapshot.elapsed() >= Duration::from_secs(1) {
                last_snapshot = Instant::now();
                if let Err(error) = save(path, &frame) {
                    log!("cannot write {}: {error}", path.display());
                }
            }
        }
        #[cfg(feature = "window")]
        if let Some(win) = win.as_mut() {
            if !win.show(mode.map(|_| &frame)) {
                log!("window closed");
                process::exit(0);
            }
        }
        thread::sleep(Duration::from_millis(16).saturating_sub(started.elapsed()));
    }
}

/// Writes the PNG next to its destination, then renames it into place, so a
/// reader never sees a half-written file.
fn save(path: &std::path::Path, frame: &Frame) -> std::io::Result<()> {
    let partial = path.with_extension("png.partial");
    let mut file = std::io::BufWriter::new(fs::File::create(&partial)?);
    write_png(&mut file, frame)?;
    std::io::Write::flush(&mut file)?;
    drop(file);
    fs::rename(partial, path)
}

#[cfg(feature = "window")]
mod window {
    use minifb::{ScaleMode, WindowOptions};

    use crate::display::Frame;

    pub struct Window(minifb::Window);

    impl Window {
        pub fn open() -> Self {
            let options = WindowOptions {
                resize: true,
                scale_mode: ScaleMode::AspectRatioStretch,
                ..WindowOptions::default()
            };
            let window = minifb::Window::new("rsvga", 1024, 768, options).unwrap_or_else(|error| {
                eprintln!("cannot open a window: {error}");
                std::process::exit(1);
            });
            Self(window)
        }

        /// Shows `frame` (or a black screen); false once the user closed the window.
        pub fn show(&mut self, frame: Option<&Frame>) -> bool {
            if !self.0.is_open() {
                return false;
            }
            let black = [0u32; 64 * 48];
            let result = match frame {
                Some(f) => self.0.update_with_buffer(&f.pixels, f.width, f.height),
                None => self.0.update_with_buffer(&black, 64, 48),
            };
            result.is_ok()
        }
    }
}
