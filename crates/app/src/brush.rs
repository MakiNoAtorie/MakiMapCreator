//! Brush logic without UI: options, pressure mapping, stroke resampling, shortcut routing.

use eframe::egui::{Key, Modifiers};
use mmc_core::{BrushStroke, Layer, RootPoint, StrokePoint};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    Raise,
    Lower,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Options {
    /// Radius in metres at full size.
    pub size: f64,
    /// Height added at the stroke centre for one pass at full pressure; always positive.
    pub strength: f32,
    /// Display only: core has a fixed profile today (`(1-d^2)^2`).
    pub falloff: f32,
    pub pressure_size: bool,
    pub pressure_strength: bool,
}

/// Smallest radius under pressure, as a fraction of `size`.
const MIN_SIZE_FRACTION: f64 = 0.1;
/// Stamp spacing as a fraction of the stamp radius.
const SPACING: f64 = 0.25;
/// Radius levels under pressure; one core command per level run.
const LEVELS: f64 = 8.0;
/// `[` and `]` multiply the size by this.
const SIZE_STEP: f64 = 1.25;
pub const MAX_STRENGTH: f32 = 200.0;

impl Options {
    /// Defaults for a map with `mpp` metres per pixel.
    pub fn for_map(mpp: f64) -> Options {
        Options {
            size: 24.0 * mpp,
            strength: 20.0,
            falloff: 0.5,
            pressure_size: false,
            pressure_strength: true,
        }
    }

    pub fn size_limits(mpp: f64) -> (f64, f64) {
        (mpp, 512.0 * mpp)
    }

    /// Radius for a pen `pressure`.
    pub fn radius_at(&self, pressure: f32) -> f64 {
        if self.pressure_size {
            self.size
                * (MIN_SIZE_FRACTION + (1.0 - MIN_SIZE_FRACTION) * pressure.clamp(0.0, 1.0) as f64)
        } else {
            self.size
        }
    }

    /// Pressure that core multiplies into the strength.
    pub fn pressure_for(&self, pressure: f32) -> f32 {
        if self.pressure_strength {
            pressure.clamp(0.0, 1.0)
        } else {
            1.0
        }
    }

    /// `[` is `dir` -1, `]` is +1.
    pub fn step_size(&mut self, dir: i32, mpp: f64) {
        let (lo, hi) = Self::size_limits(mpp);
        self.size = (self.size * SIZE_STEP.powi(dir)).clamp(lo, hi);
    }
}

/// One pen sample in root space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sample {
    pub at: RootPoint,
    pub pressure: f32,
}

/// Turns a pen path into core commands: stamps at even spacing along the path, grouped by radius.
/// One command when pressure does not change the size.
pub fn plan(tool: Tool, o: &Options, mpp: f64, path: &[Sample]) -> Vec<BrushStroke> {
    let sign = if tool == Tool::Raise { 1.0 } else { -1.0 };
    let strength = sign * o.strength * SPACING as f32;
    let mut out: Vec<BrushStroke> = Vec::new();
    let mut level = 0u32;
    for s in stamps(o, mpp, path) {
        let l = if o.pressure_size {
            ((o.radius_at(s.pressure) / o.size) * LEVELS)
                .ceil()
                .max(1.0) as u32
        } else {
            0
        };
        if out.is_empty() || l != level {
            level = l;
            let radius = if o.pressure_size {
                o.size * l as f64 / LEVELS
            } else {
                o.size
            };
            out.push(BrushStroke {
                layer: Layer::Height,
                points: vec![],
                radius,
                strength,
            });
        }
        out.last_mut().unwrap().points.push(StrokePoint {
            at: s.at,
            pressure: o.pressure_for(s.pressure),
        });
    }
    out
}

fn stamps(o: &Options, mpp: f64, path: &[Sample]) -> Vec<Sample> {
    let Some(first) = path.first() else {
        return vec![];
    };
    let gap = |pressure: f32| SPACING * o.radius_at(pressure).max(mpp);
    let mut out = vec![*first];
    let mut next = gap(first.pressure);
    let mut walked = 0.0;
    for w in path.windows(2) {
        let (a, b) = (w[0], w[1]);
        let len = ((b.at.x - a.at.x).powi(2) + (b.at.y - a.at.y).powi(2)).sqrt();
        while len > 0.0 && walked + len >= next {
            let t = (next - walked) / len;
            let pressure = a.pressure + (b.pressure - a.pressure) * t as f32;
            let at = RootPoint {
                x: a.at.x + (b.at.x - a.at.x) * t,
                y: a.at.y + (b.at.y - a.at.y) * t,
            };
            out.push(Sample { at, pressure });
            next += gap(pressure);
        }
        walked += len;
    }
    out
}

/// Keyboard commands for painting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edit {
    Undo,
    Redo,
    SizeDown,
    SizeUp,
    Tool(Tool),
}

pub fn route(key: Key, m: Modifiers) -> Option<Edit> {
    if m.alt {
        return None;
    }
    match (key, m.command, m.shift) {
        (Key::Z, true, false) => Some(Edit::Undo),
        (Key::Z, true, true) => Some(Edit::Redo),
        (Key::OpenBracket, false, _) => Some(Edit::SizeDown),
        (Key::CloseBracket, false, _) => Some(Edit::SizeUp),
        (Key::R, false, false) => Some(Edit::Tool(Tool::Raise)),
        (Key::L, false, false) => Some(Edit::Tool(Tool::Lower)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(x: f64, pressure: f32) -> Sample {
        Sample {
            at: RootPoint { x, y: 0.0 },
            pressure,
        }
    }

    #[test]
    fn pressure_toggles_map_size_and_strength() {
        let mut o = Options::for_map(1.0);
        o.size = 100.0;
        o.pressure_size = false;
        o.pressure_strength = false;
        assert_eq!(o.radius_at(0.2), 100.0);
        assert_eq!(o.pressure_for(0.2), 1.0);
        o.pressure_size = true;
        o.pressure_strength = true;
        assert!((o.radius_at(1.0) - 100.0).abs() < 1e-9);
        assert!((o.radius_at(0.0) - 10.0).abs() < 1e-9);
        assert_eq!(o.pressure_for(0.2), 0.2);
    }

    #[test]
    fn bracket_steps_clamp_to_limits() {
        let mut o = Options::for_map(2.0);
        let s = o.size;
        o.step_size(1, 2.0);
        assert!((o.size - s * 1.25).abs() < 1e-9);
        o.step_size(-1, 2.0);
        assert!((o.size - s).abs() < 1e-9);
        for _ in 0..100 {
            o.step_size(1, 2.0);
        }
        assert_eq!(o.size, 1024.0);
        for _ in 0..100 {
            o.step_size(-1, 2.0);
        }
        assert_eq!(o.size, 2.0);
    }

    #[test]
    fn stamps_are_evenly_spaced_and_sign_follows_tool() {
        let mut o = Options::for_map(1.0);
        o.size = 40.0;
        let path = [p(0.0, 1.0), p(100.0, 1.0)];
        let raise = plan(Tool::Raise, &o, 1.0, &path);
        assert_eq!(raise.len(), 1);
        let pts = &raise[0].points;
        assert_eq!(pts.len(), 11); // spacing 10 m: 0, 10 .. 100
        assert!((pts[3].at.x - 30.0).abs() < 1e-9);
        assert!(raise[0].strength > 0.0);
        assert!(plan(Tool::Lower, &o, 1.0, &path)[0].strength < 0.0);
    }

    #[test]
    fn single_sample_is_one_stamp() {
        let o = Options::for_map(1.0);
        let s = plan(Tool::Raise, &o, 1.0, &[p(5.0, 0.5)]);
        assert_eq!(s[0].points.len(), 1);
        assert!(plan(Tool::Raise, &o, 1.0, &[]).is_empty());
    }

    #[test]
    fn size_pressure_splits_into_radius_runs() {
        let mut o = Options::for_map(1.0);
        o.size = 80.0;
        o.pressure_size = true;
        let path = [p(0.0, 0.1), p(300.0, 1.0)];
        let s = plan(Tool::Raise, &o, 1.0, &path);
        assert!(s.len() > 1);
        assert!(s.windows(2).all(|w| w[0].radius < w[1].radius));
        assert!((s.last().unwrap().radius - 80.0).abs() < 1e-9);
    }

    #[test]
    fn shortcuts_route() {
        let c = Modifiers::COMMAND;
        assert_eq!(route(Key::Z, c), Some(Edit::Undo));
        assert_eq!(route(Key::Z, c.plus(Modifiers::SHIFT)), Some(Edit::Redo));
        assert_eq!(route(Key::Z, Modifiers::NONE), None);
        assert_eq!(
            route(Key::OpenBracket, Modifiers::NONE),
            Some(Edit::SizeDown)
        );
        assert_eq!(
            route(Key::CloseBracket, Modifiers::NONE),
            Some(Edit::SizeUp)
        );
        assert_eq!(
            route(Key::R, Modifiers::NONE),
            Some(Edit::Tool(Tool::Raise))
        );
        assert_eq!(route(Key::R, c), None);
    }
}
