//! Screen objects (`SVGA_CMD_DEFINE_SCREEN` / `DESTROY_SCREEN`) and the
//! rectangle arithmetic of blits between a guest image (the GMRFB) and a
//! screen. Pure bookkeeping: pixels live in the device model.

use crate::command::{Point, ScreenObject, SignedRect};

/// Why a screen definition was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScreenError {
    /// The id is not below the device's screen count.
    BadId(u32),
    /// Width or height is 0 or above the device limits.
    BadSize {
        /// Requested width.
        width: u32,
        /// Requested height.
        height: u32,
    },
}

/// The defined screens, indexed by id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Screens {
    slots: Vec<Option<ScreenObject>>,
    max_width: u32,
    max_height: u32,
}

impl Screens {
    /// Room for `count` screens of at most `max_width` x `max_height`.
    pub fn new(count: u32, max_width: u32, max_height: u32) -> Self {
        Self {
            slots: vec![None; count as usize],
            max_width,
            max_height,
        }
    }

    /// Defines or redefines a screen.
    pub fn define(&mut self, screen: ScreenObject) -> Result<(), ScreenError> {
        let (width, height) = (screen.width, screen.height);
        if !(1..=self.max_width).contains(&width) || !(1..=self.max_height).contains(&height) {
            return Err(ScreenError::BadSize { width, height });
        }
        let slot = self
            .slots
            .get_mut(screen.id as usize)
            .ok_or(ScreenError::BadId(screen.id))?;
        *slot = Some(screen);
        Ok(())
    }

    /// Destroys a screen; false if it did not exist.
    pub fn destroy(&mut self, id: u32) -> bool {
        self.slots
            .get_mut(id as usize)
            .and_then(Option::take)
            .is_some()
    }

    /// Screen `id`, if defined.
    pub fn get(&self, id: u32) -> Option<&ScreenObject> {
        self.slots.get(id as usize)?.as_ref()
    }

    /// The screen to show: the one flagged primary, else the lowest id.
    pub fn primary(&self) -> Option<&ScreenObject> {
        let defined = || self.slots.iter().flatten();
        defined()
            .find(|s| s.flags & svga_proto::screen_flag::IS_PRIMARY != 0)
            .or_else(|| defined().next())
    }

    /// Forgets every screen (mode set through the legacy registers, reset).
    pub fn clear(&mut self) {
        self.slots.iter_mut().for_each(|slot| *slot = None);
    }
}

/// A blit between a guest image and a screen, clipped to the screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Blit {
    /// Top-left pixel in the guest image.
    pub image_x: u32,
    /// Top-left pixel in the guest image.
    pub image_y: u32,
    /// Top-left pixel on the screen.
    pub screen_x: u32,
    /// Top-left pixel on the screen.
    pub screen_y: u32,
    /// Width in pixels (> 0).
    pub width: u32,
    /// Height in pixels (> 0).
    pub height: u32,
}

/// Clips a blit given as `SVGA_CMD_BLIT_*` arguments: `rect` is on the
/// screen, `origin` is the image pixel matching `rect`'s top-left corner.
/// Returns `None` when nothing remains, or when the image side would start
/// at a negative coordinate.
pub fn clip(
    origin: Point,
    rect: SignedRect,
    screen_width: u32,
    screen_height: u32,
) -> Option<Blit> {
    let (sw, sh) = (i64::from(screen_width), i64::from(screen_height));
    let left = i64::from(rect.left).max(0);
    let top = i64::from(rect.top).max(0);
    let right = i64::from(rect.right).min(sw);
    let bottom = i64::from(rect.bottom).min(sh);
    if right <= left || bottom <= top {
        return None;
    }
    // Shift the image origin by however much the rectangle was clipped.
    // All values come from i32/u32, so none of these i64 operations can
    // overflow; checked forms keep that explicit.
    let image_x = i64::from(origin.x).checked_add(left.checked_sub(i64::from(rect.left))?)?;
    let image_y = i64::from(origin.y).checked_add(top.checked_sub(i64::from(rect.top))?)?;
    Some(Blit {
        image_x: u32::try_from(image_x).ok()?,
        image_y: u32::try_from(image_y).ok()?,
        screen_x: u32::try_from(left).ok()?,
        screen_y: u32::try_from(top).ok()?,
        width: u32::try_from(right.checked_sub(left)?).ok()?,
        height: u32::try_from(bottom.checked_sub(top)?).ok()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(id: u32, flags: u32, width: u32, height: u32) -> ScreenObject {
        ScreenObject {
            struct_size: 44,
            id,
            flags,
            width,
            height,
            root: Point { x: 0, y: 0 },
            backing_store: None,
            clone_count: Some(0),
        }
    }

    #[test]
    fn define_destroy_primary() {
        let mut screens = Screens::new(4, 2560, 1600);
        assert_eq!(screens.primary(), None);
        screens.define(screen(2, 0, 800, 600)).unwrap();
        assert_eq!(screens.primary().map(|s| s.id), Some(2));
        screens
            .define(screen(3, svga_proto::screen_flag::IS_PRIMARY, 1024, 768))
            .unwrap();
        assert_eq!(screens.primary().map(|s| s.id), Some(3));
        assert!(screens.destroy(3));
        assert!(!screens.destroy(3));
        assert_eq!(
            screens.define(screen(4, 0, 800, 600)),
            Err(ScreenError::BadId(4))
        );
        assert_eq!(
            screens.define(screen(0, 0, 0, 600)),
            Err(ScreenError::BadSize {
                width: 0,
                height: 600
            })
        );
        assert!(screens.define(screen(0, 0, 4000, 600)).is_err());
    }

    #[test]
    fn clipping() {
        let rect = |left, top, right, bottom| SignedRect {
            left,
            top,
            right,
            bottom,
        };
        let at = |x, y| Point { x, y };
        assert_eq!(
            clip(at(10, 20), rect(0, 0, 100, 50), 800, 600),
            Some(Blit {
                image_x: 10,
                image_y: 20,
                screen_x: 0,
                screen_y: 0,
                width: 100,
                height: 50
            })
        );
        // Partly off the top-left: the image origin follows the clipping.
        assert_eq!(
            clip(at(0, 0), rect(-5, -3, 10, 10), 800, 600),
            Some(Blit {
                image_x: 5,
                image_y: 3,
                screen_x: 0,
                screen_y: 0,
                width: 10,
                height: 10
            })
        );
        // Partly off the bottom-right.
        assert_eq!(
            clip(at(0, 0), rect(790, 590, 900, 700), 800, 600).map(|b| (b.width, b.height)),
            Some((10, 10))
        );
        assert_eq!(
            clip(at(0, 0), rect(10, 10, 10, 20), 800, 600),
            None,
            "empty"
        );
        assert_eq!(
            clip(at(0, 0), rect(900, 0, 950, 10), 800, 600),
            None,
            "off screen"
        );
        assert_eq!(
            clip(at(-1, 0), rect(0, 0, 10, 10), 800, 600),
            None,
            "negative image origin"
        );
        assert!(clip(at(i32::MAX, 0), rect(i32::MIN, 0, i32::MAX, 10), 800, 600).is_some());
    }
}
