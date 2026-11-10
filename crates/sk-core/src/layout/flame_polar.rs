//! Polar flame graph (`polar.v1`; Python `layout/flame_polar.py`): the flame graph in polar
//! coordinates, one ring per level.
//!
//! The zoom root is the centre disc (`PolarGeometry.hole`); level k is the k-th ring around it.
//! Angles run in [`UNIT`] = 2^20 units per turn from 12 o'clock, clockwise on screen. Notation:
//! lu = dpr · font scale, g = max(1, round(2·dpr)), m = min_element · dpr (docs/visuals.md):
//!
//! - centre (W // 2, H // 2), radius R = min(W, H) // 2 − round(8·dpr);
//! - centre disc: hole = min(max(round(0.24·R), round(36·lu)), round(0.4·R));
//! - rings: n = min(detail, (R − hole) // round(14·lu)), pitch ring_w = (R − hole) // n; ring k is
//!   drawn from r0 = its inner edge + ⌈g/2⌉ to r1 = its outer edge − ⌊g/2⌋;
//! - angles a0 = round(a·2^20), a1 = round(b·2^20); the gap between sectors is drawn by frontends;
//! - lengths along a ring are arcs at its middle radius: a sector shorter than 2·dpr there is not
//!   drawn, a folder at least m long shows its children;
//! - labels: a sector thicker than it is long names itself along the radius, any other along the
//!   ring within the chord that stays round(2·dpr) inside its outer edge.
//!
//! Constants match the tokens `visual.flamePolar.*`.

use std::f64::consts::PI;

use sk_protocol::view::{ANGLE_UNIT, FrameGeometry, POLAR_LAYOUT, PolarGeometry, PolarKind};

use super::api::LayoutParams;
use super::partition::{Cell, PartitionEngine, Projection};
use super::rows::{label, label_box};
use super::treemap::pround;

/// CSS px between the outer ring and the canvas edge (selection ring, shadow).
pub const MARGIN: i64 = 8;
/// Centre disc radius as a share of the radius ...
pub const HOLE_RATIO: f64 = 0.24;
/// ... at least this many CSS px × font scale ...
pub const HOLE_MIN: i64 = 36;
/// ... and at most this share.
pub const HOLE_MAX_RATIO: f64 = 0.4;
/// CSS px × font scale: the thinnest ring.
pub const RING_MIN: i64 = 14;
/// CSS px between rings and between neighbouring sectors.
pub const GAP: i64 = 2;

pub const UNIT: i64 = ANGLE_UNIT;
const TURN: f64 = 2.0 * PI;

/// Rings of one viewport.
#[derive(Clone, Debug)]
pub struct FlamePolarProjection {
    pub levels: u32,
    pub cx: i64,
    pub cy: i64,
    pub hole: i64,
    pub ring_w: i64,
    pub gap: i64,
    /// per level (0: the centre disc): drawn radii
    pub r0: Vec<i64>,
    pub r1: Vec<i64>,
    pub mid: Vec<f64>,
    /// arc pixels per angle unit at the middle radius
    pub arc: Vec<f64>,
    pub min_draw: f64,
    pub min_expand: f64,
    pub label_unit: f64,
    /// tangential labels keep this far inside the outer edge
    pub pad: i64,
}

impl FlamePolarProjection {
    pub fn new(p: &LayoutParams) -> FlamePolarProjection {
        let d = p.dpr;
        let lu = d * p.font_scale;
        let (w, h) = p.canvas();
        let (w, h) = (w as i64, h as i64);
        let g = pround(GAP as f64 * d).max(1);
        let r = (w.min(h).div_euclid(2) - pround(MARGIN as f64 * d)).max(0);
        let hole = pround(HOLE_RATIO * r as f64)
            .max(pround(HOLE_MIN as f64 * lu))
            .min(pround(HOLE_MAX_RATIO * r as f64))
            .max(0);
        let ring_min = pround(RING_MIN as f64 * lu).max(1);
        let n = (p.detail as i64)
            .min((r - hole).div_euclid(ring_min))
            .max(0);
        let ring_w = if n > 0 { (r - hole).div_euclid(n) } else { 0 };
        let mut r0 = vec![0];
        let mut r1 = vec![hole];
        for k in 1..=n {
            r0.push(hole + (k - 1) * ring_w + (g + 1).div_euclid(2));
            r1.push(hole + k * ring_w - g.div_euclid(2));
        }
        let mid: Vec<f64> = r0
            .iter()
            .zip(&r1)
            .map(|(&a, &b)| (a + b) as f64 / 2.0)
            .collect();
        let arc = mid.iter().map(|&m| TURN * m / UNIT as f64).collect();
        FlamePolarProjection {
            levels: n as u32,
            cx: w.div_euclid(2),
            cy: h.div_euclid(2),
            hole,
            ring_w,
            gap: g,
            r0,
            r1,
            mid,
            arc,
            min_draw: p.min_draw * d,
            min_expand: p.min_element * d,
            label_unit: lu,
            pad: pround(2.0 * d),
        }
    }
}

impl Projection for FlamePolarProjection {
    fn levels(&self) -> u32 {
        self.levels
    }

    fn unit_px(&self, level: u32) -> f64 {
        TURN * self.mid[level as usize]
    }

    fn cell(&self, a: f64, b: f64, level: u32) -> Option<Cell> {
        let u = UNIT as f64;
        let (a0, a1) = (pround(a * u), pround(b * u));
        let l = level as usize;
        if (a1 - a0) as f64 * self.arc[l] < self.min_draw {
            return None;
        }
        Some([a0 as i32, a1 as i32, self.r0[l] as i32, self.r1[l] as i32])
    }

    fn expandable(&self, cell: &Cell, level: u32) -> bool {
        (cell[1] - cell[0]) as f64 * self.arc[level as usize] >= self.min_expand
    }

    fn label(&self, cell: &Cell, _kind: u8) -> u8 {
        let [a0, a1, r0, r1] = cell.map(|v| v as i64);
        let (lu, g) = (self.label_unit, self.gap as f64);
        let angle = (a1 - a0) as f64 * TURN / UNIT as f64;
        let thick = (r1 - r0) as f64;
        let mid = (r0 + r1) as f64 / 2.0;
        let along = angle * mid - g;
        if thick > along {
            // along the radius: one line, as long as the ring is thick
            let bits = label_box(thick / lu, (angle * r0 as f64 - g) / lu, true);
            return if bits & label::NAME != 0 {
                label::NAME | label::SHORT | label::RADIAL
            } else {
                0
            };
        }
        let outer = (r1 - self.pad) as f64;
        let chord = 2.0 * f64::max(0.0, outer * outer - mid * mid).sqrt();
        label_box(along.min(chord) / lu, thick / lu, false)
    }

    fn meta(&self) -> FrameGeometry {
        FrameGeometry::Polar(PolarGeometry {
            kind: PolarKind,
            cx: self.cx as i32,
            cy: self.cy as i32,
            radius: (self.hole + self.levels as i64 * self.ring_w) as i32,
            hole: self.hole as i32,
            ring_w: self.ring_w as i32,
            rings: self.levels as i32,
            gap: self.gap as i32,
            unit: UNIT,
        })
    }
}

fn make(p: &LayoutParams) -> Box<dyn Projection> {
    Box::new(FlamePolarProjection::new(p))
}

/// The engine of the `flame_polar` visualization.
pub fn flame_polar_engine() -> PartitionEngine {
    PartitionEngine::new(make, POLAR_LAYOUT, "flame_polar")
}
