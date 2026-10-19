//! Map view transform: screen points (egui) to root space metres and back.

use eframe::egui::{Pos2, Rect, Vec2};
use mmc_core::RootPoint;

/// Zoom limits in metres per screen point.
const MIN_SCALE: f64 = 0.01;
const MAX_SCALE: f64 = 1.0e6;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MapView {
    /// Root-space point at the centre of the canvas.
    pub centre: RootPoint,
    pub metres_per_point: f64,
}

impl MapView {
    /// A view that shows all of a `width` x `height` metre map inside `rect`.
    pub fn fit(width: f64, height: f64, rect: Rect) -> MapView {
        let s = (width / rect.width() as f64).max(height / rect.height() as f64);
        MapView {
            centre: RootPoint {
                x: width / 2.0,
                y: height / 2.0,
            },
            metres_per_point: s.clamp(MIN_SCALE, MAX_SCALE),
        }
    }

    pub fn root_at(&self, rect: Rect, p: Pos2) -> RootPoint {
        let d = p - rect.center();
        RootPoint {
            x: self.centre.x + d.x as f64 * self.metres_per_point,
            y: self.centre.y + d.y as f64 * self.metres_per_point,
        }
    }

    pub fn screen_at(&self, rect: Rect, r: RootPoint) -> Pos2 {
        let s = self.metres_per_point;
        rect.center()
            + Vec2::new(
                ((r.x - self.centre.x) / s) as f32,
                ((r.y - self.centre.y) / s) as f32,
            )
    }

    /// Moves the map with a pointer drag of `delta` points.
    pub fn pan(&mut self, delta: Vec2) {
        self.centre.x -= delta.x as f64 * self.metres_per_point;
        self.centre.y -= delta.y as f64 * self.metres_per_point;
    }

    /// Zooms by `factor` (above 1 zooms in) and keeps the root point under `p` fixed.
    pub fn zoom_at(&mut self, rect: Rect, p: Pos2, factor: f64) {
        let fixed = self.root_at(rect, p);
        self.metres_per_point = (self.metres_per_point / factor).clamp(MIN_SCALE, MAX_SCALE);
        let moved = self.root_at(rect, p);
        self.centre.x += fixed.x - moved.x;
        self.centre.y += fixed.y - moved.y;
    }

    /// Root-space corners (min, max) of `rect`.
    pub fn visible(&self, rect: Rect) -> (RootPoint, RootPoint) {
        (self.root_at(rect, rect.min), self.root_at(rect, rect.max))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect() -> Rect {
        Rect::from_min_size(Pos2::new(100.0, 50.0), Vec2::new(800.0, 400.0))
    }

    fn near(a: RootPoint, b: RootPoint) -> bool {
        (a.x - b.x).abs() < 1e-6 && (a.y - b.y).abs() < 1e-6
    }

    #[test]
    fn fit_shows_whole_map_centred() {
        let v = MapView::fit(8000.0, 8000.0, rect());
        assert_eq!(v.metres_per_point, 20.0);
        let (min, max) = v.visible(rect());
        assert!(near(min, RootPoint { x: -4000.0, y: 0.0 }));
        assert!(near(
            max,
            RootPoint {
                x: 12000.0,
                y: 8000.0
            }
        ));
    }

    #[test]
    fn screen_and_root_round_trip() {
        let v = MapView {
            centre: RootPoint { x: 500.0, y: -20.0 },
            metres_per_point: 2.5,
        };
        let p = Pos2::new(130.0, 444.0);
        let r = v.root_at(rect(), p);
        assert!(near(
            r,
            RootPoint {
                x: -425.0,
                y: 465.0
            }
        ));
        assert!((v.screen_at(rect(), r) - p).length() < 1e-3);
    }

    #[test]
    fn pan_moves_map_with_pointer() {
        let mut v = MapView::fit(1000.0, 1000.0, rect());
        let p = Pos2::new(300.0, 200.0);
        let under = v.root_at(rect(), p);
        v.pan(Vec2::new(40.0, -10.0));
        assert!(near(v.root_at(rect(), p + Vec2::new(40.0, -10.0)), under));
    }

    #[test]
    fn zoom_keeps_point_under_cursor() {
        let mut v = MapView::fit(1000.0, 1000.0, rect());
        let p = Pos2::new(222.0, 333.0);
        let under = v.root_at(rect(), p);
        v.zoom_at(rect(), p, 2.0);
        assert_eq!(v.metres_per_point, 1.25);
        assert!(near(v.root_at(rect(), p), under));
    }

    #[test]
    fn zoom_is_clamped() {
        let mut v = MapView::fit(1000.0, 1000.0, rect());
        v.zoom_at(rect(), rect().center(), 1.0e9);
        assert_eq!(v.metres_per_point, MIN_SCALE);
    }
}
