//! Diagnostics: with `RSVGA_PROFILE=1`, where the device's time goes,
//! logged every five seconds: time and count of each kind of work
//! ([`Cat`]), and how much of the wall time the command buffers took.
//! Spans nest (a draw's recording is inside its command buffer), so the
//! categories do not add up.

use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// A kind of work.
#[derive(Clone, Copy, Debug)]
pub enum Cat {
    /// A whole command buffer.
    Buffer,
    /// Guest-backed object commands.
    Gb,
    /// DX context commands.
    Dx,
    /// Legacy (Direct3D 9) commands on the state.
    D3d9State,
    /// Legacy commands on the GPU.
    D3d9Gpu,
    /// Direct3D 9 shader translations and their cache.
    Translate,
    /// Recording a draw (pipeline, descriptors, commands).
    Record,
    /// Submitting to the GPU and waiting for it.
    Flush,
    /// Device copies to the GPU.
    Upload,
    /// GPU copies back to the device.
    Download,
    /// The GPU itself executing submissions (timestamps; [`add`]).
    Gpu,
}

const CATS: usize = 11;
const NAMES: [&str; CATS] = [
    "buffers",
    "gb",
    "dx",
    "d3d9 state",
    "d3d9 gpu",
    "translate",
    "record",
    "flush",
    "upload",
    "download",
    "gpu busy",
];

static TIME: [AtomicU64; CATS] = [const { AtomicU64::new(0) }; CATS];
/// Occurrences by name (commands, causes of readbacks).
static EVENTS: Mutex<Vec<(&'static str, u64)>> = Mutex::new(Vec::new());

/// Counts one `name` event, if profiling.
pub fn count(name: &'static str) {
    if !enabled() {
        return;
    }
    let Ok(mut events) = EVENTS.lock() else {
        return;
    };
    match events.iter_mut().find(|(n, _)| *n == name) {
        Some((_, c)) => *c = c.saturating_add(1),
        None => events.push((name, 1)),
    }
}
static COUNT: [AtomicU64; CATS] = [const { AtomicU64::new(0) }; CATS];
static SINCE: Mutex<Option<Instant>> = Mutex::new(None);

fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RSVGA_PROFILE").is_some_and(|v| v != "0"))
}

/// Adds `time` taken by `count` pieces of `cat` work, if profiling.
pub fn add(cat: Cat, time: Duration, count: u64) {
    if !enabled() || count == 0 {
        return;
    }
    let i = cat as usize;
    let nanos = u64::try_from(time.as_nanos()).unwrap_or(u64::MAX);
    TIME[i].fetch_add(nanos, Ordering::Relaxed);
    COUNT[i].fetch_add(count, Ordering::Relaxed);
}

/// Time spent until dropped, if profiling.
pub struct Span(Option<(Cat, Instant)>);

/// Starts timing `cat`.
pub fn span(cat: Cat) -> Span {
    Span(enabled().then(|| (cat, Instant::now())))
}

impl Drop for Span {
    fn drop(&mut self) {
        if let Some((cat, start)) = self.0 {
            let nanos = u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX);
            let i = cat as usize;
            TIME[i].fetch_add(nanos, Ordering::Relaxed);
            COUNT[i].fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Logs and restarts the counters every five seconds.
pub fn report() {
    if !enabled() {
        return;
    }
    let Ok(mut since) = SINCE.lock() else { return };
    let start = *since.get_or_insert_with(Instant::now);
    let wall = start.elapsed();
    if wall < Duration::from_secs(5) {
        return;
    }
    *since = Some(Instant::now());
    let mut parts = Vec::new();
    for (i, name) in NAMES.iter().enumerate() {
        let t = TIME[i].swap(0, Ordering::Relaxed);
        let n = COUNT[i].swap(0, Ordering::Relaxed);
        if n > 0 {
            parts.push(format!("{name} {:.2} s ({n})", t as f64 / 1e9));
        }
    }
    crate::log!(
        "profile over {:.1} s: {}",
        wall.as_secs_f64(),
        parts.join(", ")
    );
    if let Ok(mut events) = EVENTS.lock()
        && !events.is_empty()
    {
        events.sort_by_key(|e| std::cmp::Reverse(e.1));
        let list: Vec<String> = events.iter().map(|(n, c)| format!("{n} {c}")).collect();
        crate::log!("profile events: {}", list.join(", "));
        events.clear();
    }
}
