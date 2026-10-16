//! Which tile pictures to code for a frame, and as what, in the single tiled sequence.

use super::TILE_COUNT;
use super::hevc::MAX_REF_DISTANCE;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PicKind {
    /// Starts a new sequence; only ever tile 0, followed by `Intra` for the other tiles.
    Idr,
    /// I slices coded as TRAIL_R, so the other tiles' references survive.
    Intra,
    /// P from the previous picture of the same tile.
    Inter,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TilePic {
    pub tile: usize,
    pub kind: PicKind,
}

/// Re-code a skipped tile before its reference falls out of short-term RPS range.
const KEEPALIVE: u64 = MAX_REF_DISTANCE as u64 / 2;

#[derive(Debug)]
pub struct Planner {
    refresh: bool,
    intra: u8,
    heartbeat: bool,
    /// Picture count at which each tile was last coded.
    last: [u64; TILE_COUNT],
    pics: u64,
    /// Backends that can't reference across skipped tiles code all tiles whenever any changed.
    per_tile_skip: bool,
}

impl Planner {
    pub fn new(per_tile_skip: bool) -> Self {
        Self {
            refresh: true,
            intra: 0,
            heartbeat: false,
            last: [0; TILE_COUNT],
            pics: 0,
            per_tile_skip,
        }
    }

    /// `None`: full refresh (IDR on tile 0, intra on the rest). `Some(t)`: intra-code tile `t`.
    pub fn request_keyframe(&mut self, tile: Option<usize>) {
        match tile {
            Some(t) if t < TILE_COUNT => self.intra |= 1 << t,
            _ => self.refresh = true,
        }
    }

    /// Makes the next plan code at least one tile (the stalest) even if nothing changed.
    pub fn request_heartbeat(&mut self) {
        self.heartbeat = true;
    }

    pub fn keyframe_pending(&self) -> bool {
        self.refresh || self.intra != 0
    }

    pub fn plan(&mut self, changed: u8) -> Vec<TilePic> {
        let heartbeat = std::mem::take(&mut self.heartbeat);
        let pics: Vec<TilePic> = if std::mem::take(&mut self.refresh) {
            self.intra = 0;
            self.pics = 0;
            (0..TILE_COUNT)
                .map(|tile| TilePic {
                    tile,
                    kind: if tile == 0 {
                        PicKind::Idr
                    } else {
                        PicKind::Intra
                    },
                })
                .collect()
        } else {
            let stale = (0..TILE_COUNT)
                .filter(|&t| self.pics - self.last[t] >= KEEPALIVE)
                .fold(0u8, |m, t| m | 1 << t);
            let intra = std::mem::take(&mut self.intra);
            let mut mask = changed | intra | stale;
            if mask == 0 && heartbeat {
                mask = 1 << (0..TILE_COUNT).min_by_key(|&t| self.last[t]).unwrap();
            }
            if !self.per_tile_skip && mask != 0 {
                mask = (1 << TILE_COUNT) - 1;
            }
            (0..TILE_COUNT)
                .filter(|t| mask & (1 << t) != 0)
                .map(|tile| TilePic {
                    tile,
                    kind: if intra & (1 << tile) != 0 {
                        PicKind::Intra
                    } else {
                        PicKind::Inter
                    },
                })
                .collect()
        };
        for p in &pics {
            self.pics += 1;
            self.last[p.tile] = self.pics;
        }
        pics
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use PicKind::*;

    fn kinds(p: &[TilePic]) -> Vec<(usize, PicKind)> {
        p.iter().map(|p| (p.tile, p.kind)).collect()
    }

    #[test]
    fn refresh_then_skip() {
        let mut p = Planner::new(true);
        assert_eq!(
            kinds(&p.plan(0)),
            [(0, Idr), (1, Intra), (2, Intra), (3, Intra)]
        );
        assert_eq!(kinds(&p.plan(0b0100)), [(2, Inter)]);
        assert!(p.plan(0).is_empty());
        p.request_keyframe(Some(1));
        assert_eq!(kinds(&p.plan(0b0001)), [(0, Inter), (1, Intra)]);
        p.request_keyframe(None);
        assert_eq!(p.plan(0).len(), 4);
    }

    #[test]
    fn all_tiles_without_per_tile_skip() {
        let mut p = Planner::new(false);
        p.plan(0);
        assert_eq!(
            kinds(&p.plan(0b0010)),
            [(0, Inter), (1, Inter), (2, Inter), (3, Inter)]
        );
        assert!(p.plan(0).is_empty());
    }

    #[test]
    fn heartbeat_codes_stalest_tile() {
        let mut p = Planner::new(true);
        p.plan(0);
        p.plan(0b0001);
        p.plan(0b1000);
        p.request_heartbeat();
        assert_eq!(kinds(&p.plan(0)), [(1, Inter)]);
        p.request_heartbeat();
        assert_eq!(kinds(&p.plan(0)), [(2, Inter)]);
        p.request_heartbeat();
        assert_eq!(kinds(&p.plan(0b0001)), [(0, Inter)]);
        assert!(p.plan(0).is_empty());
    }

    #[test]
    fn keepalive() {
        let mut p = Planner::new(true);
        p.plan(0);
        let mut coded3 = false;
        for _ in 0..KEEPALIVE {
            coded3 |= p.plan(0b0001).iter().any(|t| t.tile == 3);
        }
        assert!(coded3);
    }
}
