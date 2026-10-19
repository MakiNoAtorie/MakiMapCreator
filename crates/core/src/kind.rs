//! The map kind interface (ADR-0001). Each kind is a crate that implements it; the caller
//! assembles the kinds it ships into `Kinds` and gives them to the project. A new kind needs
//! no edit in core or render.

use crate::{Command, Layer, MapId, MapKind, MapSettings, Project, Result, Stage};

/// What an edit tool does to its layer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ToolAction {
    /// Adds a soft stamp; the app's raise and lower give the sign.
    Brush,
    /// Sets the layer to `on` (raise) or `off` (lower) inside the stamp.
    Paint { on: f32, off: f32 },
}

/// An edit tool a kind offers; the app maps `id` to its strings and input.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tool {
    pub id: &'static str,
    pub layer: Layer,
    pub action: ToolAction,
}

/// A number a generator takes, with its default and range.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Param {
    pub id: &'static str,
    pub default: f64,
    pub min: f64,
    pub max: f64,
}

/// A generator a kind offers, with the parameters the Generate panel shows.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GeneratorDecl {
    pub id: &'static str,
    pub params: &'static [Param],
}

/// One generator run: which generator, its seed and its parameter values (ADR-0021).
#[derive(Debug, Clone, PartialEq)]
pub struct GenRun {
    pub generator: String,
    pub seed: u64,
    /// Values by parameter id; a missing one takes the declared default.
    pub params: Vec<(String, f64)>,
}

impl GenRun {
    pub fn new(generator: &str, seed: u64) -> GenRun {
        GenRun {
            generator: generator.into(),
            seed,
            params: vec![],
        }
    }

    /// The value of `p` in this run, clamped to its range.
    pub fn param(&self, p: &Param) -> f64 {
        self.params
            .iter()
            .find(|(id, _)| id == p.id)
            .map_or(p.default, |v| v.1)
            .clamp(p.min, p.max)
    }
}

/// A draw pass: a WGSL fragment shader over one layer (the renderer's bindings and `VIEW_WGSL`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RenderPass {
    pub layer: Layer,
    pub shader: &'static str,
}

/// How a kind makes child maps from an outline on one of its maps.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChildDecl {
    pub kind: MapKind,
    /// Default child scale: the parent's metres per pixel divided by this.
    pub scale_factor: f64,
    /// Parent layers the child keeps as parent context.
    pub context: &'static [Layer],
}

/// What a child map shows on its parent (ADR-0002).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Summary {
    /// RGBA.
    pub tint: [u8; 4],
}

pub trait MapKindModule: Send + Sync {
    fn kind(&self) -> MapKind;

    /// Size and scale of a new map of this kind.
    fn default_settings(&self) -> MapSettings;

    /// Raster layers; a layer a stage writes is derived, the others are authored.
    fn layers(&self) -> &'static [Layer];

    /// Stages that compute the derived layers (ADR-0018).
    fn stages(&self) -> &'static [Stage] {
        &[]
    }

    /// Draw passes, bottom first.
    fn render_passes(&self) -> &'static [RenderPass] {
        &[]
    }

    fn tools(&self) -> &'static [Tool] {
        &[]
    }

    /// Generators, in menu order.
    fn generators(&self) -> &'static [GeneratorDecl] {
        &[]
    }

    /// Runs `run` on `map` and returns its output as one command (ADR-0003).
    fn generate(&self, project: &Project, map: MapId, run: &GenRun) -> Result<Command> {
        let _ = (project, map);
        Err(crate::Error::UnknownGenerator(run.generator.clone()))
    }

    /// Entity kind ids its generators create.
    fn entity_kinds(&self) -> &'static [&'static str] {
        &[]
    }

    /// How child maps are made on maps of this kind; `None` when it has none.
    fn child(&self) -> Option<ChildDecl> {
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

    /// The kind stored as `id`.
    pub fn parse(&self, id: &str) -> Option<MapKind> {
        self.0.iter().map(|k| k.kind()).find(|k| k.as_str() == id)
    }

    pub fn all(&self) -> impl Iterator<Item = &dyn MapKindModule> {
        self.0.iter().map(|k| &**k)
    }
}
