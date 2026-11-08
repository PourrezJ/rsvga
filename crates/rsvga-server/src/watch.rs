//! Diagnostics: with `RSVGA_PROFILE=1`, a DX context that ran many
//! commands and then none for two seconds is logged once with its last
//! commands, and the device with its queues and interrupts
//! ([`crate::device::Svga::watch`]): what a program that stopped
//! drawing last asked for, and whether the device still owes it something.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

/// Commands kept per context.
const RECENT: usize = 48;
/// Commands since the last report that make silence worth a report.
const BUSY: u64 = 2000;
/// Silence that counts as a stall.
const QUIET: Duration = Duration::from_secs(2);

#[derive(Debug)]
struct Context {
    last: Instant,
    /// Command ids, oldest first.
    recent: VecDeque<u32>,
    since_report: u64,
}

/// Recent commands by DX context.
#[derive(Debug, Default)]
pub struct Watch {
    contexts: HashMap<u32, Context>,
}

impl Watch {
    /// Notes command `id` of DX context `cid`.
    pub fn command(&mut self, cid: u32, id: u32) {
        let c = self.contexts.entry(cid).or_insert_with(|| Context {
            last: Instant::now(),
            recent: VecDeque::with_capacity(RECENT),
            since_report: 0,
        });
        c.last = Instant::now();
        if c.recent.len() >= RECENT {
            c.recent.pop_front();
        }
        c.recent.push_back(id);
        c.since_report = c.since_report.saturating_add(1);
    }

    /// Contexts busy, then quiet for [`QUIET`]: their id and last commands
    /// (each context once per stall).
    pub fn stalled(&mut self) -> Vec<(u32, Vec<u32>)> {
        let mut out = Vec::new();
        for (&cid, c) in &mut self.contexts {
            if c.since_report >= BUSY && c.last.elapsed() >= QUIET {
                c.since_report = 0;
                out.push((cid, c.recent.iter().copied().collect()));
            }
        }
        out
    }
}

/// Command ids as names, repeats folded (`DX_DRAW x12`).
pub fn names(ids: &[u32]) -> String {
    let mut parts: Vec<(String, usize)> = Vec::new();
    for &id in ids {
        let name = svga_proto::cmd3d_names::name(id).map_or_else(|| id.to_string(), str::to_string);
        match parts.last_mut() {
            Some((last, n)) if *last == name => *n = n.saturating_add(1),
            _ => parts.push((name, 1)),
        }
    }
    parts
        .into_iter()
        .map(|(name, n)| if n > 1 { format!("{name} x{n}") } else { name })
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeats_are_folded() {
        let draw = svga_proto::cmd3d::DX_DRAW;
        let set = svga_proto::cmd3d::DX_SET_SHADER;
        assert_eq!(
            names(&[set, draw, draw, draw, set]),
            "DX_SET_SHADER, DX_DRAW x3, DX_SET_SHADER"
        );
    }

    #[test]
    fn a_quiet_context_is_reported_once() {
        let mut w = Watch::default();
        for _ in 0..BUSY {
            w.command(3, svga_proto::cmd3d::DX_DRAW);
        }
        assert!(w.stalled().is_empty(), "not quiet yet");
        if let Some(c) = w.contexts.get_mut(&3) {
            c.last = Instant::now().checked_sub(QUIET).unwrap_or(c.last);
        }
        let stalled = w.stalled();
        assert_eq!(stalled.len(), 1);
        assert_eq!(stalled[0].0, 3);
        assert_eq!(stalled[0].1.len(), RECENT);
        assert!(w.stalled().is_empty(), "reported once");
    }
}
