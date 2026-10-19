//! The map kind interface (ADR-0001). Each kind is a crate that implements it; the caller
//! assembles the kinds it ships into `Kinds`.

use crate::{Command, Layer, MapId, MapKind, Project, Result};

/// An edit tool a kind offers; the app maps `id` to its strings and input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tool {
    pub id: &'static str,
    pub layer: Layer,
}

/// What a child map shows on its parent (ADR-0002).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Summary {
    /// RGBA.
    pub tint: [u8; 4],
}

pub trait MapKindModule: Send + Sync {
    fn kind(&self) -> MapKind;

    /// Layers maps of this kind use; `Layer::is_derived` tells authored from derived.
    fn layers(&self) -> &'static [Layer];

    /// Layers drawn, bottom first; the renderer has a draw pass per layer.
    fn render_layers(&self) -> &'static [Layer];

    fn tools(&self) -> &'static [Tool];

    /// Generator ids, in menu order.
    fn generators(&self) -> &'static [&'static str];

    /// Runs `generator` on `map` and returns its output as one command (ADR-0003).
    fn generate(
        &self,
        project: &Project,
        map: MapId,
        generator: &str,
        seed: u64,
    ) -> Result<Command>;

    /// Entity kind ids its generators create.
    fn entity_kinds(&self) -> &'static [&'static str];

    /// The kind of map made from an area of this one.
    fn child_kind(&self) -> Option<MapKind> {
        None
    }

    /// What `map`, a child of another map, shows on its parent.
    fn summary(&self, _project: &Project, _map: MapId) -> Result<Option<Summary>> {
        Ok(None)
    }
}

/// The kinds an app ships.
#[derive(Default)]
pub struct Kinds(Vec<Box<dyn MapKindModule>>);

impl Kinds {
    pub fn new(kinds: Vec<Box<dyn MapKindModule>>) -> Kinds {
        Kinds(kinds)
    }

    pub fn get(&self, kind: MapKind) -> Option<&dyn MapKindModule> {
        self.0.iter().find(|k| k.kind() == kind).map(|k| &**k)
    }
}
