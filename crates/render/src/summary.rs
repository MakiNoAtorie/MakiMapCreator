//! Summary tints: each child map's summary drawn inside its outline on the parent (ADR-0002).

use mmc_core::{ChildSummary, MapId, Project, RootPoint};

use crate::{Image, Result, Scene};

impl Scene {
    /// Adds the summaries of the view map's child maps, drawn over its layers.
    pub fn with_summaries(mut self, project: &Project) -> Result<Scene> {
        self.summaries = summaries(project, self.view.map)?;
        Ok(self)
    }
}

/// The summaries of `map`'s child maps, outlines in map metres (the space of `View`).
pub(crate) fn summaries(project: &Project, map: MapId) -> Result<Vec<ChildSummary>> {
    let origin = project
        .maps()
        .iter()
        .find(|m| m.id == map)
        .ok_or(mmc_core::Error::UnknownMap(map))?
        .origin;
    let mut out = project.child_summaries(map)?;
    for q in out.iter_mut().flat_map(|s| s.outline.iter_mut()) {
        (q.x, q.y) = (q.x - origin.x, q.y - origin.y);
    }
    Ok(out)
}

/// Blends summary tints by map pixel centre, so a tiled render equals a single render; `img` holds view rows from `row0`.
// ponytail: CPU, pixels × outlines; move to a shader pass if many children slow the frame.
pub(crate) fn tint(scene: &Scene, img: &mut Image, row0: u32) {
    if scene.summaries.is_empty() {
        return;
    }
    let (v, m) = (scene.view, scene.metres_per_pixel);
    let centre = |min: f64, max: f64, n: u32, p: u32| {
        let px = (min / m + (p as f64 + 0.5) * (max - min) / m / n as f64).floor();
        (px + 0.5) * m
    };
    for y in 0..img.height {
        let cy = centre(v.min.y, v.max.y, v.height, row0 + y);
        for x in 0..v.width {
            let i = ((y * v.width + x) * 4) as usize;
            // Transparent pixels are outside the map.
            if img.rgba[i + 3] == 0 {
                continue;
            }
            let at = RootPoint {
                x: centre(v.min.x, v.max.x, v.width, x),
                y: cy,
            };
            let Some(s) = scene.summaries.iter().find(|s| s.contains(at)) else {
                continue;
            };
            let [r, g, b, a] = s.summary.tint.map(u32::from);
            for (c, t) in img.rgba[i..i + 3].iter_mut().zip([r, g, b]) {
                *c = ((*c as u32 * (255 - a) + t * a + 127) / 255) as u8;
            }
        }
    }
}
