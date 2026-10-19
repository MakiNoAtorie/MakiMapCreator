//! Summary tints: each child map's summary drawn inside its outline on the parent (ADR-0002).

use mmc_core::{Kinds, Project, RootPoint};

use crate::{Image, Result, Scene};

impl Scene {
    /// Adds the summaries of the view map's child maps, drawn over its layers.
    pub fn with_summaries(mut self, project: &Project, kinds: &Kinds) -> Result<Scene> {
        let map = self.view.map;
        let origin = project
            .maps()
            .iter()
            .find(|m| m.id == map)
            .ok_or(mmc_core::Error::UnknownMap(map))?
            .origin;
        // Outlines in map metres, the space of `View`.
        self.summaries = project.child_summaries(kinds, map)?;
        for q in self.summaries.iter_mut().flat_map(|s| s.outline.iter_mut()) {
            (q.x, q.y) = (q.x - origin.x, q.y - origin.y);
        }
        Ok(self)
    }
}

/// Blends each summary tint over image pixels whose map pixel centre is inside its outline.
/// It uses absolute image pixels, so a tiled render equals a single render.
// ponytail: CPU, pixels × outlines; move to a shader pass if many children slow the frame.
pub(crate) fn tint(scene: &Scene, img: &mut Image) {
    if scene.summaries.is_empty() {
        return;
    }
    let (v, m) = (scene.view, scene.metres_per_pixel);
    let centre = |min: f64, max: f64, n: u32, p: u32| {
        let px = (min / m + (p as f64 + 0.5) * (max - min) / m / n as f64).floor();
        (px + 0.5) * m
    };
    for y in 0..v.height {
        let cy = centre(v.min.y, v.max.y, v.height, y);
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
