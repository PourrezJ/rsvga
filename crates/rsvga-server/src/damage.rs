//! Which part of the framebuffer changed since the display last looked.
//!
//! The guest reports changes with `SVGA_CMD_UPDATE` (and implies them with
//! copies); the display copies only that area instead of the whole screen.
//! A single bounding rectangle is kept: cheap, and the usual updates of a
//! desktop are local.

/// Accumulated damage, as `[x0, x1) x [y0, y1)` in framebuffer pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Damage {
    area: Option<(u32, u32, u32, u32)>,
}

impl Damage {
    /// Adds a rectangle (saturating, so no guest value can overflow).
    pub fn add(&mut self, x: u32, y: u32, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        let (x1, y1) = (x.saturating_add(width), y.saturating_add(height));
        self.area = Some(match self.area {
            None => (x, y, x1, y1),
            Some((ax0, ay0, ax1, ay1)) => (ax0.min(x), ay0.min(y), ax1.max(x1), ay1.max(y1)),
        });
    }

    /// Marks everything as changed.
    pub fn all(&mut self) {
        self.area = Some((0, 0, u32::MAX, u32::MAX));
    }

    /// Takes the accumulated area, clipped to a `width` x `height` screen.
    pub fn take(&mut self, width: u32, height: u32) -> Option<(u32, u32, u32, u32)> {
        let (x0, y0, x1, y1) = self.area.take()?;
        let (x1, y1) = (x1.min(width), y1.min(height));
        (x0 < x1 && y0 < y1).then_some((x0, y0, x1, y1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn union_clip_and_take() {
        let mut damage = Damage::default();
        assert_eq!(damage.take(100, 100), None);
        damage.add(10, 10, 5, 5);
        damage.add(50, 2, 10, 1);
        assert_eq!(damage.take(100, 100), Some((10, 2, 60, 15)));
        assert_eq!(damage.take(100, 100), None, "taken");
        damage.add(90, 90, u32::MAX, u32::MAX);
        assert_eq!(damage.take(100, 100), Some((90, 90, 100, 100)));
        damage.add(200, 200, 5, 5);
        assert_eq!(damage.take(100, 100), None, "off screen");
        damage.add(1, 1, 0, 9);
        assert_eq!(damage.take(100, 100), None, "empty rectangle");
        damage.all();
        assert_eq!(damage.take(64, 48), Some((0, 0, 64, 48)));
    }
}
