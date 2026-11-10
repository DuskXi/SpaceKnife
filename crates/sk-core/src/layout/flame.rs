//! Flame graph (`flame.v1`; Python `layout/flame.py`): the partition layout as stacked bands, one
//! per level.
//!
//! The zoom root's bar is band 0 (`FlameGeometry.root`); level k is band k, growing away from the
//! root's side of the canvas: up from the bottom (the default) or down from the top (option
//! `origin`). Notation: lu = dpr · font scale, g = max(1, round(2·dpr)), W × H the canvas in
//! physical pixels (docs/visuals.md):
//!
//! - bands: row_min = round(20·lu), row_max = round(48·lu); n = min(detail, H // row_min − 1);
//!   row_h = min(H // (n + 1), row_max), a bar is row_h − g high;
//! - band k lies at y = k·row_h (root on top) or y = H − k·row_h − (row_h − g) (root at the bottom);
//! - x = round(a·(W + g)), w = round(b·(W + g)) − g − x;
//! - a bar narrower than round(2·dpr) is not drawn; a folder bar at least m wide shows its children;
//! - labels: `label_box(w / lu, h / lu)` without the large type.
//!
//! Constants match the tokens `visual.flame.*`.

use sk_protocol::view::{
    FLAME_LAYOUT, FlameGeometry, FlameKind, FlameOrigin, FrameGeometry, PixelRect,
};

use super::api::LayoutParams;
use super::partition::{Cell, PartitionEngine, Projection};
use super::rows::label_box;
use super::treemap::pround;

/// CSS px × font scale: the lowest band still holds a line of names.
pub const ROW_MIN: i64 = 20;
/// CSS px × font scale: shallow trees do not stretch bars over tall canvases.
pub const ROW_MAX: i64 = 48;
/// CSS px between bands and between neighbouring bars.
pub const GAP: i64 = 2;

/// Bands of one viewport.
#[derive(Clone, Debug)]
pub struct FlameProjection {
    pub origin: FlameOrigin,
    pub levels: u32,
    pub width: i64,
    pub gap: i64,
    pub row_h: i64,
    pub bar_h: i64,
    /// pixels of [0, 1): the width plus the gap of the rightmost bar
    pub span: f64,
    pub min_draw: i64,
    pub min_expand: f64,
    pub label_unit: f64,
    pub band: Vec<i64>,
}

impl FlameProjection {
    pub fn new(p: &LayoutParams) -> FlameProjection {
        let d = p.dpr;
        let lu = d * p.font_scale;
        let (w, h) = p.canvas();
        let (w, h) = (w as i64, h as i64);
        let g = pround(GAP as f64 * d).max(1);
        let row_min = pround(ROW_MIN as f64 * lu).max(1);
        let row_max = pround(ROW_MAX as f64 * lu).max(row_min);
        let n = (p.detail as i64).min(h.div_euclid(row_min) - 1).max(0);
        let row_h = (h.div_euclid(n + 1)).min(row_max);
        let origin = if p.opt_str("origin", "bottom") == "top" {
            FlameOrigin::Top
        } else {
            FlameOrigin::Bottom
        };
        let bar_h = (row_h - g).max(0);
        let band = (0..=n)
            .map(|k| match origin {
                FlameOrigin::Top => k * row_h,
                FlameOrigin::Bottom => h - k * row_h - bar_h,
            })
            .collect();
        FlameProjection {
            origin,
            levels: n as u32,
            width: w,
            gap: g,
            row_h,
            bar_h,
            span: (w + g) as f64,
            min_draw: pround(p.min_draw * d),
            min_expand: p.min_element * d,
            label_unit: lu,
            band,
        }
    }
}

impl Projection for FlameProjection {
    fn levels(&self) -> u32 {
        self.levels
    }

    fn unit_px(&self, _level: u32) -> f64 {
        self.span
    }

    fn cell(&self, a: f64, b: f64, level: u32) -> Option<Cell> {
        let x = pround(a * self.span);
        let w = pround(b * self.span) - self.gap - x;
        if w < self.min_draw {
            return None;
        }
        Some([
            x as i32,
            self.band[level as usize] as i32,
            w as i32,
            self.bar_h as i32,
        ])
    }

    fn expandable(&self, cell: &Cell, _level: u32) -> bool {
        cell[2] as f64 >= self.min_expand
    }

    fn label(&self, cell: &Cell, _kind: u8) -> u8 {
        let lu = self.label_unit;
        label_box(cell[2] as f64 / lu, cell[3] as f64 / lu, false)
    }

    fn meta(&self) -> FrameGeometry {
        FrameGeometry::Flame(FlameGeometry {
            kind: FlameKind,
            origin: self.origin,
            row_h: self.row_h as i32,
            rows: self.levels as i32,
            gap: self.gap as i32,
            root: PixelRect {
                x: 0,
                y: self.band[0] as i32,
                w: self.width as i32,
                h: self.bar_h as i32,
            },
        })
    }
}

fn make(p: &LayoutParams) -> Box<dyn Projection> {
    Box::new(FlameProjection::new(p))
}

/// The engine of the `flame` visualization.
pub fn flame_engine() -> PartitionEngine {
    PartitionEngine::new(make, FLAME_LAYOUT, "flame")
}
