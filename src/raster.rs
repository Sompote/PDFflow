//! Editable picture pixels: straight-alpha RGBA in copy-on-write tiles, so a brush stroke (and
//! the undo step it leaves) only copies the tiles it touched.

use std::sync::Arc;

pub const TILE: u32 = 128;
/// Largest picture side kept for editing; bigger pictures are resampled when editing starts.
pub const MAX_EDIT_SIDE: u32 = 2048;

#[derive(Clone)]
pub struct Raster {
    pub w: u32,
    pub h: u32,
    tiles: Vec<Arc<Vec<u8>>>,
}

impl Raster {
    fn tiles_x(w: u32) -> u32 {
        w.div_ceil(TILE)
    }

    pub fn new(w: u32, h: u32) -> Self {
        let n = (Self::tiles_x(w) * h.div_ceil(TILE)) as usize;
        let blank = Arc::new(vec![0u8; (TILE * TILE * 4) as usize]);
        Raster { w, h, tiles: vec![blank; n] }
    }

    pub fn from_rgba(w: u32, h: u32, data: &[u8]) -> Self {
        let mut r = Raster::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                *r.px_mut(x, y) = [data[i], data[i + 1], data[i + 2], data[i + 3]];
            }
        }
        r
    }

    #[inline]
    fn locate(&self, x: u32, y: u32) -> (usize, usize) {
        let t = (y / TILE) * Self::tiles_x(self.w) + x / TILE;
        let o = ((y % TILE) * TILE + x % TILE) * 4;
        (t as usize, o as usize)
    }

    #[inline]
    pub fn get(&self, x: u32, y: u32) -> [u8; 4] {
        let (t, o) = self.locate(x.min(self.w - 1), y.min(self.h - 1));
        let d = &self.tiles[t];
        [d[o], d[o + 1], d[o + 2], d[o + 3]]
    }

    /// Clamped lookup with signed coordinates (edge pixels repeat).
    #[inline]
    pub fn get_clamped(&self, x: i32, y: i32) -> [u8; 4] {
        self.get(x.clamp(0, self.w as i32 - 1) as u32, y.clamp(0, self.h as i32 - 1) as u32)
    }

    #[inline]
    pub fn px_mut(&mut self, x: u32, y: u32) -> &mut [u8; 4] {
        let (t, o) = self.locate(x, y);
        let d = Arc::make_mut(&mut self.tiles[t]);
        (&mut d[o..o + 4]).try_into().expect("4 bytes")
    }

    /// RGBA bytes of a sub-rectangle, row by row.
    pub fn region(&self, x0: u32, y0: u32, w: u32, h: u32) -> Vec<u8> {
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        for y in y0..y0 + h {
            for x in x0..x0 + w {
                out.extend_from_slice(&self.get(x, y));
            }
        }
        out
    }

    pub fn to_rgba(&self) -> Vec<u8> {
        self.region(0, 0, self.w, self.h)
    }

    pub fn crop(&self, x0: u32, y0: u32, w: u32, h: u32) -> Raster {
        Raster::from_rgba(w, h, &self.region(x0, y0, w, h))
    }

    /// Number of tiles two rasters share (for tests of the copy-on-write behaviour).
    #[cfg(test)]
    pub fn shared_tiles(&self, other: &Raster) -> usize {
        self.tiles.iter().zip(&other.tiles).filter(|(a, b)| Arc::ptr_eq(a, b)).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_on_write_copies_only_touched_tiles() {
        let a = Raster::new(500, 300);
        let mut b = a.clone();
        *b.px_mut(10, 10) = [1, 2, 3, 4];
        assert_eq!(a.get(10, 10), [0, 0, 0, 0]);
        assert_eq!(b.get(10, 10), [1, 2, 3, 4]);
        let total = 4 * 3;
        assert_eq!(a.shared_tiles(&b), total - 1);
    }

    #[test]
    fn round_trips_and_crops() {
        let data: Vec<u8> = (0..(300 * 200)).flat_map(|i| [(i % 251) as u8, (i % 7) as u8, 9, 255]).collect();
        let r = Raster::from_rgba(300, 200, &data);
        assert_eq!(r.to_rgba(), data);
        let c = r.crop(130, 140, 20, 10);
        assert_eq!(c.get(0, 0), r.get(130, 140));
        assert_eq!(c.get(19, 9), r.get(149, 149));
    }
}
