//! Brush logic without UI: options, pressure mapping, stroke resampling, shortcut routing.

use eframe::egui::{Key, Modifiers};
use mmc_core::{
    BrushMode, BrushStroke, Command, Detail, Layer, PaintStroke, RootPoint, StrokePoint,
    ToolAction, brush_weight,
};

/// Raise adds (brush), raises to the target or paints the tool's "on" value; lower the opposite.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Raise,
    Lower,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Options {
    /// Radius in metres at full size.
    pub size: f64,
    /// Height added at the stroke centre for one pass at full pressure; always positive.
    pub strength: f32,
    /// 0 hard edge, 1 soft (see `mmc_core::brush_weight`).
    pub falloff: f32,
    /// Root-space detail on soft stamps, 0..=1; 0 is the smooth falloff.
    pub detail: f32,
    /// Coarsest detail cell in metres.
    pub detail_scale: f64,
    pub pressure_size: bool,
    pub pressure_strength: bool,
}

/// Smallest radius under pressure, as a fraction of `size`.
const MIN_SIZE_FRACTION: f64 = 0.1;
/// Stamp spacing as a fraction of the stamp radius.
const SPACING: f64 = 0.25;
/// `[` and `]` multiply the size by this.
const SIZE_STEP: f64 = 1.25;
pub const MAX_STRENGTH: f32 = 200.0;

impl Options {
    /// Defaults for a map with `mpp` metres per pixel.
    pub fn for_map(mpp: f64) -> Options {
        Options {
            size: 24.0 * mpp,
            strength: 20.0,
            falloff: 1.0,
            detail: 0.5,
            detail_scale: 32.0 * mpp,
            pressure_size: false,
            pressure_strength: true,
        }
    }

    pub fn size_limits(mpp: f64) -> (f64, f64) {
        (mpp, 512.0 * mpp)
    }

    /// Detail scale range; core skips octaves under 8 px, so the smallest has one octave.
    pub fn detail_scale_limits(mpp: f64) -> (f64, f64) {
        (8.0 * mpp, 1024.0 * mpp)
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

    /// Fraction of the radius where the stamp has half its strength, for the cursor ring.
    pub fn half_radius(&self) -> f32 {
        let mut d = 0.5f64;
        // Bisection: the weight falls from 1 at the centre to 0 at the rim.
        let (mut lo, mut hi) = (0.0f64, 1.0f64);
        for _ in 0..30 {
            d = (lo + hi) / 2.0;
            if brush_weight(d * d, self.falloff) > 0.5 {
                lo = d;
            } else {
                hi = d;
            }
        }
        d as f32
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

/// One core command for a pen path: evenly spaced stamps with their own radius and pressure.
pub fn plan(
    action: ToolAction,
    layer: Layer,
    dir: Direction,
    o: &Options,
    mpp: f64,
    path: &[Sample],
) -> Option<Command> {
    let stamps = stamps(o, mpp, path);
    if stamps.is_empty() {
        return None;
    }
    let up = dir == Direction::Raise;
    let sign = if up { 1.0 } else { -1.0 };
    let brush = |strength: f32, target: Option<f32>, mode: BrushMode| {
        Command::Brush(BrushStroke {
            layer,
            points: stamps
                .iter()
                .map(|s| StrokePoint {
                    at: s.at,
                    pressure: o.pressure_for(s.pressure),
                    radius: o.radius_at(s.pressure),
                })
                .collect(),
            strength,
            falloff: o.falloff,
            target,
            mode,
            // Roughen's coarsest bumps are the brush size, so it gives bumps, not grain.
            detail: Detail {
                amount: o.detail,
                scale: if mode == BrushMode::Roughen {
                    o.size
                } else {
                    o.detail_scale
                },
            },
        })
    };
    // Smooth and flatten move a fraction of the way per stamp; full strength is all the way.
    let rate = o.strength / MAX_STRENGTH;
    Some(match action {
        ToolAction::Brush => brush(sign * o.strength * SPACING as f32, None, BrushMode::Add),
        ToolAction::Target { to, raise } => {
            brush(if raise { 1.0 } else { -1.0 }, Some(to), BrushMode::Add)
        }
        ToolAction::Smooth => brush(rate, None, BrushMode::Smooth),
        ToolAction::Flatten => brush(rate, None, BrushMode::Flatten),
        ToolAction::Roughen => brush(sign * o.strength * SPACING as f32, None, BrushMode::Roughen),
        ToolAction::Paint { on, off } => Command::Paint(PaintStroke {
            layer,
            points: stamps.iter().map(|s| s.at).collect(),
            radius: o.size,
            value: if up { on } else { off },
        }),
    })
}

/// Stamp centres and radii of a planned command, for the overlay.
pub fn stamp_discs(cmd: &Command) -> Vec<(RootPoint, f64)> {
    match cmd {
        Command::Brush(b) => b.points.iter().map(|p| (p.at, p.radius)).collect(),
        Command::Paint(p) => p.points.iter().map(|&at| (at, p.radius)).collect(),
        _ => vec![],
    }
}

/// The command with its stamps from `from` on; `None` when there are none.
/// Stamps of a path never change as samples are added, so parts sent this way add up to the whole stroke.
pub fn tail(cmd: Command, from: usize) -> Option<Command> {
    let cmd = match cmd {
        Command::Brush(mut b) => {
            b.points.drain(..from.min(b.points.len()));
            Command::Brush(b)
        }
        Command::Paint(mut p) => {
            p.points.drain(..from.min(p.points.len()));
            Command::Paint(p)
        }
        other => return Some(other),
    };
    (!stamp_discs(&cmd).is_empty()).then_some(cmd)
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
    Direction(Direction),
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
        (Key::R, false, false) => Some(Edit::Direction(Direction::Raise)),
        (Key::L, false, false) => Some(Edit::Direction(Direction::Lower)),
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

    fn brush(dir: Direction, o: &Options, path: &[Sample]) -> BrushStroke {
        match plan(ToolAction::Brush, Layer::HEIGHT, dir, o, 1.0, path) {
            Some(Command::Brush(b)) => b,
            other => panic!("{other:?}"),
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
    fn stamps_are_evenly_spaced_and_sign_follows_direction() {
        let mut o = Options::for_map(1.0);
        o.size = 40.0;
        let path = [p(0.0, 1.0), p(100.0, 1.0)];
        let raise = brush(Direction::Raise, &o, &path);
        let pts = &raise.points;
        assert_eq!(pts.len(), 11); // spacing 10 m: 0, 10 .. 100
        assert!((pts[3].at.x - 30.0).abs() < 1e-9);
        assert!(raise.strength > 0.0);
        assert_eq!(raise.falloff, o.falloff);
        assert!(brush(Direction::Lower, &o, &path).strength < 0.0);
    }

    #[test]
    fn parts_sent_while_drawing_add_up_to_the_whole_stroke() {
        let mut o = Options::for_map(1.0);
        o.size = 40.0;
        o.pressure_size = true;
        let path: Vec<Sample> = (0..40)
            .map(|i| p((i * i) as f64 * 0.7, (i % 7) as f32 / 7.0))
            .collect();
        let whole = brush(Direction::Raise, &o, &path).points;
        let (mut sent, mut joined) = (0, vec![]);
        for n in [1, 2, 9, 10, 25, 40] {
            let cmd = plan(
                ToolAction::Brush,
                Layer::HEIGHT,
                Direction::Raise,
                &o,
                1.0,
                &path[..n],
            );
            if let Some(Command::Brush(b)) = cmd.and_then(|c| tail(c, sent)) {
                sent += b.points.len();
                joined.extend(b.points);
            }
        }
        assert_eq!(joined, whole);
    }

    #[test]
    fn single_sample_is_one_stamp_and_no_sample_is_no_command() {
        let o = Options::for_map(1.0);
        assert_eq!(brush(Direction::Raise, &o, &[p(5.0, 0.5)]).points.len(), 1);
        let none = plan(
            ToolAction::Brush,
            Layer::HEIGHT,
            Direction::Raise,
            &o,
            1.0,
            &[],
        );
        assert!(none.is_none());
    }

    #[test]
    fn size_pressure_gives_each_stamp_its_radius_in_one_stroke() {
        let mut o = Options::for_map(1.0);
        o.size = 80.0;
        o.pressure_size = true;
        let s = brush(Direction::Raise, &o, &[p(0.0, 0.1), p(300.0, 1.0)]);
        let r: Vec<f64> = s.points.iter().map(|q| q.radius).collect();
        assert!(r.windows(2).all(|w| w[0] <= w[1]), "{r:?}");
        assert!(
            r[0] < 20.0 && r[r.len() - 1] > 70.0 && r[r.len() - 1] <= 80.0,
            "{r:?}"
        );
    }

    #[test]
    fn a_paint_tool_paints_on_or_off() {
        let o = Options::for_map(1.0);
        let lock = Layer::LOCK;
        let action = ToolAction::Paint { on: 1.0, off: 0.0 };
        let path = [p(0.0, 1.0)];
        for (dir, want) in [(Direction::Raise, 1.0), (Direction::Lower, 0.0)] {
            match plan(action, lock, dir, &o, 1.0, &path) {
                Some(Command::Paint(s)) => assert_eq!((s.layer, s.value), (lock, want)),
                other => panic!("{other:?}"),
            }
        }
    }

    #[test]
    fn a_target_tool_raises_to_or_lowers_to_with_the_falloff() {
        let mut o = Options::for_map(1.0);
        o.falloff = 0.3;
        let path = [p(0.0, 1.0)];
        // The tool decides the direction; the toggle does not.
        for (raise, dir, sign, want) in [
            (true, Direction::Lower, 1.0, 200.0),
            (false, Direction::Raise, -1.0, -200.0),
        ] {
            let action = ToolAction::Target { to: want, raise };
            match plan(action, Layer::HEIGHT, dir, &o, 1.0, &path) {
                Some(Command::Brush(b)) => assert_eq!(
                    (b.strength.signum(), b.target, b.falloff),
                    (sign, Some(want), 0.3)
                ),
                other => panic!("{other:?}"),
            }
        }
    }

    #[test]
    fn shaping_tools_plan_their_mode_with_the_detail_options() {
        let mut o = Options::for_map(10.0);
        o.detail = 0.7;
        o.strength = 50.0;
        let path = [p(0.0, 1.0)];
        o.size = 900.0;
        // Roughen's bumps follow the brush size.
        for (action, mode, strength, scale) in [
            (ToolAction::Smooth, BrushMode::Smooth, 0.25, 320.0),
            (ToolAction::Flatten, BrushMode::Flatten, 0.25, 320.0),
            (ToolAction::Roughen, BrushMode::Roughen, -12.5, 900.0),
            (ToolAction::Brush, BrushMode::Add, -12.5, 320.0),
        ] {
            match plan(action, Layer::HEIGHT, Direction::Lower, &o, 10.0, &path) {
                Some(Command::Brush(b)) => assert_eq!(
                    (b.mode, b.strength, b.detail.amount, b.detail.scale),
                    (mode, strength, 0.7, scale)
                ),
                other => panic!("{other:?}"),
            }
        }
    }

    #[test]
    fn half_radius_follows_falloff() {
        let mut o = Options::for_map(1.0);
        o.falloff = 1.0;
        assert!((o.half_radius() - 0.541).abs() < 0.01);
        o.falloff = 0.0;
        assert!(o.half_radius() > 0.99);
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
            Some(Edit::Direction(Direction::Raise))
        );
        assert_eq!(route(Key::R, c), None);
    }
}
