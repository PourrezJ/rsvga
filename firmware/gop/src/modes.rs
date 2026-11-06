//! Video modes offered through GOP.

/// Resolutions offered when the device can display them, smallest first.
pub const CANDIDATES: [(u32, u32); 15] = [
    (640, 480),
    (800, 600),
    (1024, 768),
    (1280, 720),
    (1280, 800),
    (1280, 1024),
    (1366, 768),
    (1440, 900),
    (1600, 900),
    (1680, 1050),
    (1920, 1080),
    (1920, 1200),
    (2560, 1440),
    (2560, 1600),
    (3840, 2160),
];

/// Resolution set when the driver starts.
pub const PREFERRED: (u32, u32) = (1024, 768);

/// Bytes per pixel: modes are always 32-bit BGRX.
pub const BYTES_PER_PIXEL: u64 = 4;

/// The modes the device can show, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModeList {
    modes: [(u32, u32); CANDIDATES.len()],
    count: usize,
}

impl ModeList {
    /// Keeps the candidates within the device limits and VRAM. The pitch is
    /// estimated as `width * 4`, which is what the device reports for 32 bpp.
    pub fn new(max_width: u32, max_height: u32, vram_size: u32) -> Self {
        let mut list = Self {
            modes: [(0, 0); CANDIDATES.len()],
            count: 0,
        };
        for &(width, height) in &CANDIDATES {
            let bytes = u64::from(width) * BYTES_PER_PIXEL * u64::from(height);
            if width <= max_width && height <= max_height && bytes <= u64::from(vram_size) {
                list.modes[list.count] = (width, height);
                list.count += 1;
            }
        }
        list
    }

    /// Number of modes.
    pub fn len(&self) -> u32 {
        self.count as u32
    }

    /// Mode `index`, if it exists.
    pub fn get(&self, index: u32) -> Option<(u32, u32)> {
        self.modes[..self.count]
            .get(usize::try_from(index).ok()?)
            .copied()
    }

    /// Index of [`PREFERRED`], or of the largest mode below it.
    pub fn preferred(&self) -> Option<u32> {
        let fits = |&(w, h): &(u32, u32)| w <= PREFERRED.0 && h <= PREFERRED.1;
        let index = self.modes[..self.count]
            .iter()
            .rposition(fits)
            .or((self.count > 0).then_some(0))?;
        u32::try_from(index).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_and_vram_filter_the_modes() {
        let list = ModeList::new(3840, 2160, 64 << 20);
        assert_eq!(list.len(), 15);
        let small = ModeList::new(1920, 1200, 8 << 20);
        assert_eq!(
            small.get(small.len() - 1),
            Some((1920, 1080)),
            "1920x1200x4 > 8 MiB"
        );
        assert_eq!(ModeList::new(1024, 768, 64 << 20).len(), 3);
        assert_eq!(ModeList::new(320, 200, 64 << 20).len(), 0);
    }

    #[test]
    fn preferred_mode() {
        let list = ModeList::new(3840, 2160, 64 << 20);
        assert_eq!(list.get(list.preferred().unwrap()), Some(PREFERRED));
        let tiny = ModeList::new(800, 600, 64 << 20);
        assert_eq!(tiny.get(tiny.preferred().unwrap()), Some((800, 600)));
        assert_eq!(ModeList::new(0, 0, 0).preferred(), None);
        assert_eq!(list.get(99), None);
    }
}
