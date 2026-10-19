//! The one pen input event type, and the pure logic that turns raw tablet data into it.

/// One pen sample. Every tablet API is converted to this before the rest of the app sees it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PenEvent {
    /// Window client area, physical pixels.
    pub pos: [f32; 2],
    /// `0..=1`.
    pub pressure: f32,
    pub eraser: bool,
    /// Degrees from vertical along screen x (right) and y (down), as Windows Ink reports it.
    pub tilt: [f32; 2],
    /// Barrel (side) button held.
    pub barrel: bool,
    pub phase: Phase,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Hover,
    Down,
    Move,
    Up,
}

/// Derives the phase from successive contact states; both APIs report contact as pressure.
#[derive(Default)]
pub struct PhaseTracker {
    contact: bool,
}

impl PhaseTracker {
    pub fn next(&mut self, contact: bool) -> Phase {
        let was = std::mem::replace(&mut self.contact, contact);
        match (was, contact) {
            (false, false) => Phase::Hover,
            (false, true) => Phase::Down,
            (true, true) => Phase::Move,
            (true, false) => Phase::Up,
        }
    }
}

/// Tablet API that delivers pen events. Only one is active at a time: while a WinTab
/// context is open the Wacom driver sends no Ink pen messages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PenApi {
    WinTab,
    Ink,
}

/// The WinTab device in use, or why WinTab is not used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WintabState {
    Device {
        name: String,
        pressure: (u32, u32),
    },
    NotSelected,
    NoDriver,
    /// The driver lacks a WinTab function.
    BadDriver,
    NoService,
    NoContext,
}

pub fn active_api(setting: PenApi, wintab_present: bool) -> PenApi {
    if setting == PenApi::WinTab && wintab_present {
        PenApi::WinTab
    } else {
        PenApi::Ink
    }
}

/// Wacom cursor index: 0 puck, 1 pen tip, 2 eraser, repeating per device.
pub fn wintab_is_eraser(cursor: u32) -> bool {
    cursor % 3 == 2
}

/// Drops the mouse press that a pen tap also generates (WinTab: the driver's mouse
/// emulation; Ink: winit's touch-to-pointer emulation). A press counts as the pen's when
/// a pen event arrived within `window` seconds before it, which covers the hover samples
/// that precede every tap.
pub struct DuplicatePressFilter {
    window: f64,
    last_pen: f64,
}

impl DuplicatePressFilter {
    pub fn new(window: f64) -> Self {
        Self {
            window,
            last_pen: f64::NEG_INFINITY,
        }
    }

    pub fn pen_seen(&mut self, time: f64) {
        self.last_pen = time;
    }

    pub fn accept_mouse_press(&self, time: f64) -> bool {
        time - self.last_pen > self.window
    }
}

/// Maps a raw driver pressure in `min..=max` to `0..=1`.
pub fn normalise_pressure(raw: u32, min: u32, max: u32) -> f32 {
    if max <= min {
        return 0.0;
    }
    (raw.clamp(min, max) - min) as f32 / (max - min) as f32
}

/// WinTab orientation (tenths of a degree) to tilt in degrees from vertical, along x and y.
/// Azimuth is clockwise from screen up; altitude is the angle above the tablet.
pub fn tilt_from_orientation(azimuth: i32, altitude: i32) -> [f32; 2] {
    let az = (azimuth as f32 / 10.0).to_radians();
    // Negative altitude means the pen is inverted (eraser end); tilt uses its magnitude.
    let alt = (altitude.unsigned_abs() as f32 / 10.0).to_radians();
    let (x, y, z) = (alt.cos() * az.sin(), -alt.cos() * az.cos(), alt.sin());
    [x.atan2(z).to_degrees(), y.atan2(z).to_degrees()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pressure_maps_driver_range_to_unit() {
        assert_eq!(normalise_pressure(0, 0, 32767), 0.0);
        assert_eq!(normalise_pressure(32767, 0, 32767), 1.0);
        assert_eq!(normalise_pressure(512, 0, 1024), 0.5);
        assert_eq!(normalise_pressure(40000, 0, 32767), 1.0);
        assert_eq!(normalise_pressure(5, 0, 0), 0.0);
    }

    fn near(a: [f32; 2], b: [f32; 2]) -> bool {
        (a[0] - b[0]).abs() < 0.01 && (a[1] - b[1]).abs() < 0.01
    }

    #[test]
    fn tilt_matches_windows_ink_axes() {
        assert!(near(tilt_from_orientation(1234, 900), [0.0, 0.0]));
        assert!(near(tilt_from_orientation(900, 450), [45.0, 0.0]));
        assert!(near(tilt_from_orientation(0, 450), [0.0, -45.0]));
        assert!(near(tilt_from_orientation(1800, -600), [0.0, 30.0]));
    }

    #[test]
    fn phase_follows_contact() {
        let mut t = PhaseTracker::default();
        let got: Vec<Phase> = [false, true, true, false, false]
            .into_iter()
            .map(|c| t.next(c))
            .collect();
        use Phase::*;
        assert_eq!(got, [Hover, Down, Move, Up, Hover]);
    }

    #[test]
    fn mouse_press_near_pen_activity_is_dropped() {
        let mut f = DuplicatePressFilter::new(1.0);
        assert!(f.accept_mouse_press(5.0));
        f.pen_seen(10.0);
        assert!(!f.accept_mouse_press(10.0));
        assert!(!f.accept_mouse_press(10.9));
        assert!(f.accept_mouse_press(11.5));
    }

    #[test]
    fn wintab_used_only_when_selected_and_present() {
        assert_eq!(active_api(PenApi::WinTab, true), PenApi::WinTab);
        assert_eq!(active_api(PenApi::WinTab, false), PenApi::Ink);
        assert_eq!(active_api(PenApi::Ink, true), PenApi::Ink);
    }

    #[test]
    fn wacom_cursor_index_marks_eraser() {
        assert!(!wintab_is_eraser(1));
        assert!(wintab_is_eraser(2));
        assert!(wintab_is_eraser(5));
        assert!(!wintab_is_eraser(4));
    }
}
