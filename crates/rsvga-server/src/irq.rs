//! The card's legacy PCI interrupt (INTx), signalled to QEMU through an
//! eventfd, with the semantics of Linux vfio-pci INTx:
//!
//! - the line is asserted while `pending & mask != 0`;
//! - signalling it masks it automatically ("automasked");
//! - an unmask (`SET_IRQS` with `ACTION_UNMASK`, or the resample eventfd that
//!   KVM signals on guest EOI) re-signals at once if the line is still
//!   asserted.
//!
//! The guest reads pending flags from the IRQ status port and acknowledges
//! them by writing them back.

use std::os::fd::OwnedFd;

/// State of the INTx line.
#[derive(Default)]
pub struct Intx {
    pending: u32,
    masked: bool,
    trigger: Option<OwnedFd>,
    signals: u64,
}

impl Intx {
    /// Pending flags (`SVGA_IRQFLAG_*`), as read from the IRQ status port.
    pub fn pending(&self) -> u32 {
        self.pending
    }

    /// Number of times the line was signalled (for tests and logs).
    pub fn signals(&self) -> u64 {
        self.signals
    }

    /// Sets (or removes) the eventfd QEMU listens on.
    pub fn set_trigger(&mut self, fd: Option<OwnedFd>, mask: u32) {
        self.trigger = fd;
        self.update(mask);
    }

    /// Whether an eventfd is connected.
    pub fn connected(&self) -> bool {
        self.trigger.is_some()
    }

    /// Adds pending flags and signals if they are enabled by `mask`.
    pub fn raise(&mut self, flags: u32, mask: u32) {
        self.pending |= flags;
        self.update(mask);
    }

    /// Acknowledges flags (a write to the IRQ status port).
    pub fn ack(&mut self, flags: u32, mask: u32) {
        self.pending &= !flags;
        self.update(mask);
    }

    /// Masks the line.
    pub fn mask(&mut self) {
        self.masked = true;
    }

    /// Unmasks the line; re-signals if it is still asserted.
    pub fn unmask(&mut self, mask: u32) {
        self.masked = false;
        self.update(mask);
    }

    /// Back to power-on state, keeping the eventfd.
    pub fn reset(&mut self) {
        self.pending = 0;
        self.masked = false;
    }

    /// Signals the eventfd if the line is asserted and not masked.
    pub fn update(&mut self, mask: u32) {
        if self.masked || self.pending & mask == 0 {
            return;
        }
        if let Some(fd) = &self.trigger {
            let _ = rustix::io::write(fd, &1u64.to_ne_bytes());
            self.signals = self.signals.saturating_add(1);
            self.masked = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustix::event::{EventfdFlags, eventfd};

    fn fired(fd: &OwnedFd) -> bool {
        let mut buf = [0u8; 8];
        rustix::io::read(fd, &mut buf).is_ok_and(|n| n == 8)
    }

    #[test]
    fn level_triggered_with_automask() {
        let fd = eventfd(0, EventfdFlags::CLOEXEC | EventfdFlags::NONBLOCK).unwrap();
        let mut line = Intx::default();
        line.set_trigger(Some(fd.try_clone().unwrap()), 0x1);
        line.raise(0x2, 0x1);
        assert!(!fired(&fd), "flag not enabled by the mask");
        line.raise(0x1, 0x1);
        assert!(fired(&fd));
        line.raise(0x1, 0x1);
        assert!(!fired(&fd), "automasked until EOI");
        line.unmask(0x1);
        assert!(fired(&fd), "still asserted after unmask: signalled again");
        line.ack(0x1, 0x1);
        line.unmask(0x1);
        assert!(!fired(&fd), "acknowledged: the line dropped");
        assert_eq!(line.pending(), 0x2);
        assert_eq!(line.signals(), 2);
    }

    #[test]
    fn nothing_happens_without_an_eventfd() {
        let mut line = Intx::default();
        line.raise(0x1, 0x1);
        assert_eq!((line.pending(), line.signals()), (0x1, 0));
        assert!(!line.connected());
    }
}
