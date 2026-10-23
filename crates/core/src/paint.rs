//! Paint: sets a layer to one value inside a disc, for example land and sea.

use std::collections::BTreeSet;

use crate::command::EditTiles;
use crate::layer::{Layer, RootPoint};
use crate::{Map, Project, Result};

/// Sets `layer` to `value` within `radius` of each point.
#[derive(Debug, Clone, PartialEq)]
pub struct PaintStroke {
    pub layer: Layer,
    pub points: Vec<RootPoint>,
    /// Metres.
    pub radius: f64,
    pub value: f32,
}

impl Project {
    /// Paints the stroke into `tiles`, loading the ones it reaches first; gives the tiles it changed.
    pub(crate) fn paint_into(
        &self,
        map: &Map,
        s: &PaintStroke,
        tiles: &mut EditTiles,
        cached: bool,
    ) -> Result<BTreeSet<(u32, u32)>> {
        let mut touched = BTreeSet::new();
        let r = s.radius / map.settings.metres_per_pixel;
        for p in &s.points {
            let c = map.to_pixels(*p);
            self.disc(
                map,
                s.layer,
                c,
                r,
                (tiles, cached),
                &mut touched,
                |v, dist2| {
                    if dist2 >= r * r {
                        return false;
                    }
                    *v = s.value;
                    true
                },
            )?;
        }
        Ok(touched)
    }
}
