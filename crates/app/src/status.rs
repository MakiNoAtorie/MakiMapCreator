//! Status bar texts.

use mmc_core::{MapKind, MapSettings, RootPoint};

use crate::i18n::{tr, tr_args};

/// Whole metres, `-0` shown as `0`.
fn metres(v: f64) -> String {
    format!("{:.0}", v.round() + 0.0)
}

pub fn cursor(p: Option<RootPoint>) -> String {
    match p {
        Some(p) => tr_args("status-cursor", &[("x", &metres(p.x)), ("y", &metres(p.y))]),
        None => tr("status-outside"),
    }
}

/// Elevation in metres; empty outside the map.
pub fn elevation(h: Option<f32>) -> String {
    h.map(|h| tr_args("status-elevation", &[("h", &metres(h as f64))]))
        .unwrap_or_default()
}

pub fn map(s: &MapSettings) -> String {
    tr_args(
        "status-map",
        &[
            ("w", &s.width.to_string()),
            ("h", &s.height.to_string()),
            ("mpp", &s.metres_per_pixel.to_string()),
        ],
    )
}

pub fn map_kind(k: MapKind) -> String {
    tr(match k {
        MapKind::World => "map-kind-world",
        MapKind::Region => "map-kind-region",
        MapKind::Settlement => "map-kind-settlement",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_shows_whole_metres() {
        let p = RootPoint { x: 1234.6, y: -0.2 };
        assert_eq!(cursor(Some(p)), "x 1235 m   y 0 m");
        assert_eq!(cursor(None), "Cursor outside map");
    }

    #[test]
    fn elevation_rounds_and_is_empty_outside() {
        assert_eq!(elevation(Some(-35.5)), "-36 m");
        assert_eq!(elevation(None), "");
    }

    #[test]
    fn map_shows_size_and_scale() {
        let s = MapSettings::default_for(MapKind::Region);
        assert_eq!(map(&s), "8192 × 8192 px · 1 px = 10 m");
        let s = MapSettings::default_for(MapKind::Settlement);
        assert_eq!(map(&s), "4096 × 4096 px · 1 px = 0.5 m");
    }
}
