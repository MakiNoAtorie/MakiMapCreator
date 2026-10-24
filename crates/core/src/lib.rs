//! Headless core: the project model and its public API (seam 1). No UI, no SQL.

use std::fmt;

mod brush;
pub use brush::{
    BrushMode, DETAIL_STAGE, Detail, DetailNoise, DetailTables, OctaveTable, RIDGE_GAIN,
    RIDGE_HEIGHTS, detail_noise, detail_tables, stamp_weight,
};
mod cache;
mod command;
mod jobs;
pub use jobs::cpus;
mod layer;
mod migrate;
pub mod persist;
mod pipeline;
pub use pipeline::{Raster, Stage, StageInput, par_map};
mod pyramid;
pub mod rng;
mod save;
mod stroke;
mod writer;
pub use stroke::{EncodedStroke, EndedStroke};
mod undo;
pub use cache::TILE_CACHE_BYTES;
pub use command::{BrushStroke, Command, StrokePoint, brush_core, brush_weight};
pub use layer::{COAST_RELIEF, Layer, RootPoint, TILE_SIZE, WATER_LEVEL};
pub use pyramid::{Overview, TileId, read_overviews};
pub use undo::UNDO_LIMIT;
mod entities;
pub use entities::{
    AnchorEdit, Anchors, DEFAULT_ANCHOR_TOLERANCE, DerivedFeature, Document, Entity, EntityEdit,
    EntityId, EntityKind, FeatureId, FeatureKind, FieldDef, FieldType, Link, NameHit, PageEdit,
    Relation, TextHit, TextSearch, link_markup,
};
mod paint;
pub use paint::PaintStroke;
mod object;
pub use object::{MapObject, ObjectEdit, ObjectId, Shape};
mod generation;
pub use generation::{APP_VERSION, Generation, GenerationInfo, NewEntity};
mod kind;
pub use kind::{
    ChildDecl, GenRun, GeneratorDecl, Kinds, MapKindModule, Param, RenderPass, Summary, Tool,
    ToolAction,
};
mod child;
pub use child::{CHILD_MARGIN, MAX_CHILD_SIZE, ParentContext};
mod delete;
pub use delete::OwnMap;
mod resync;
pub use resync::{ChildSummary, RegenPlan, TileAt};

/// A map kind, named by its stored id. Each kind module defines its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MapKind(&'static str);

impl MapKind {
    pub const fn new(id: &'static str) -> MapKind {
        MapKind(id)
    }

    pub fn as_str(self) -> &'static str {
        self.0
    }
}

/// Size in pixels and scale of a map (ADR-0020).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MapSettings {
    pub kind: MapKind,
    pub width: u32,
    pub height: u32,
    pub metres_per_pixel: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MapId(i64);

#[derive(Debug, Clone, PartialEq)]
pub struct Map {
    pub id: MapId,
    pub parent: Option<MapId>,
    pub settings: MapSettings,
    /// Root-space position of pixel (0, 0)'s top-left corner (ADR-0008, ADR-0020).
    pub origin: RootPoint,
    /// The ground a child map owns, in root space (ADR-0019); `None` for a root map.
    pub outline: Option<Vec<RootPoint>>,
}

impl Map {
    fn from_record(r: persist::MapRecord, kinds: &Kinds) -> Result<Map> {
        Ok(Map {
            id: MapId(r.id),
            parent: r.parent_id.map(MapId),
            settings: MapSettings {
                kind: kinds.parse(&r.kind).ok_or(Error::UnknownMapKind(r.kind))?,
                width: r.width,
                height: r.height,
                metres_per_pixel: r.metres_per_pixel,
            },
            origin: RootPoint {
                x: r.origin_x,
                y: r.origin_y,
            },
            outline: None,
        })
    }
}

#[derive(Debug)]
pub enum Error {
    /// The file is not a project of this app.
    NotAProject,
    /// The file comes from a newer app.
    NewerVersion {
        found: i32,
        supported: i32,
    },
    /// The storage backend failed; the text is its message.
    Storage(String),
    /// Another running app holds the working copy.
    WorkingCopyInUse,
    UnknownMapKind(String),
    UnknownMap(MapId),
    CorruptTile,
    DerivedLayer(Layer),
    UnknownEntityKind(String),
    UnknownEntity(EntityId),
    EntityExists(EntityId),
    InvalidField {
        key: String,
        problem: FieldProblem,
    },
    CorruptEntity,
    InvalidAnchor(AnchorProblem),
    UnknownFeature(FeatureId),
    UnknownGenerator(String),
    /// Maps of this kind have no child maps.
    NoChildMaps(MapKind),
    /// The entity has no outline on the parent map.
    NoOutline(EntityId),
    /// Metres per pixel must be a positive number.
    InvalidScale,
    /// A child map would be wider or higher than `MAX_CHILD_SIZE`.
    ChildTooLarge,
    /// A root map has no parent to re-sync with.
    RootMap(MapId),
    /// The map has no generation to run again.
    NeverGenerated(MapId),
    /// A generator needs parent context the map does not have.
    NoParentContext(MapId),
    /// An outline needs at least three points.
    InvalidOutline,
    OutsideOutline,
    UnknownObject(ObjectId),
    NotAnObjectLayer(Layer),
    InvalidObject(ObjectId),
    /// Undo of a child map's creation while it has child maps of its own.
    MapHasChildren(MapId),
    OwnMapChoice(EntityId),
    StalePlan,
}

/// Why a field definition or value was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldProblem {
    /// The kind already has a field with this key.
    Duplicate,
    NoSuchField,
    /// An entity reference names no entity.
    NoSuchEntity,
    WrongType,
}

/// Why anchors were refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnchorProblem {
    /// Only rivers and lakes have anchors.
    NotAnchored,
    /// River anchors on a lake or the reverse.
    WrongShape,
    /// The derived feature has no geometry.
    NoGeometry,
}

/// English text for logs; the app shows its own strings for each variant (ADR-0017).
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NewerVersion { found, supported } => {
                write!(f, "file version {found} is newer than {supported}")
            }
            Error::Storage(e) => write!(f, "storage error: {e}"),
            Error::InvalidField { key, problem } => write!(f, "field {key}: {problem:?}"),
            e => write!(f, "{e:?}"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

/// An open project. Edits go to a working copy; `save` writes the project file.
pub struct Project {
    store: Box<dyn persist::Storage>,
    kinds: std::sync::Arc<Kinds>,
    maps: Vec<Map>,
    history: std::collections::HashMap<MapId, undo::History>,
    names: entities::NameIndex,
    pipeline: pipeline::Pipeline,
    anchors: entities::AnchorState,
    saves: save::Saves,
    cache: std::sync::OnceLock<cache::TileCache>,
    resync: resync::State,
    strokes: stroke::Strokes,
    writer: std::sync::OnceLock<writer::Writer>,
}

impl Project {
    /// Creates a project with one root map; maps of kinds not in `kinds` can not be made or opened.
    pub fn create(
        store: Box<dyn persist::Storage>,
        kinds: std::sync::Arc<Kinds>,
        root: MapSettings,
    ) -> Result<Project> {
        if kinds.get(root.kind).is_none() {
            return Err(Error::UnknownMapKind(root.kind.as_str().into()));
        }
        let id = store.insert_map(
            None,
            root.kind.as_str(),
            root.width,
            root.height,
            root.metres_per_pixel,
        )?;
        Ok(Project {
            store,
            kinds,
            maps: vec![Map {
                id: MapId(id),
                parent: None,
                settings: root,
                origin: RootPoint { x: 0.0, y: 0.0 },
                outline: None,
            }],
            history: Default::default(),
            names: Default::default(),
            pipeline: Default::default(),
            anchors: Default::default(),
            saves: Default::default(),
            cache: Default::default(),
            resync: Default::default(),
            strokes: Default::default(),
            writer: Default::default(),
        })
    }

    /// A project on an opened or restored `store`.
    pub fn load(store: Box<dyn persist::Storage>, kinds: std::sync::Arc<Kinds>) -> Result<Project> {
        let maps = store
            .maps()?
            .into_iter()
            .map(|r| Map::from_record(r, &kinds))
            .collect::<Result<_>>()?;
        let mut p = Project {
            store,
            kinds,
            maps,
            history: Default::default(),
            names: Default::default(),
            pipeline: Default::default(),
            anchors: Default::default(),
            saves: Default::default(),
            cache: Default::default(),
            resync: Default::default(),
            strokes: Default::default(),
            writer: Default::default(),
        };
        p.names = p.build_name_index()?;
        p.load_child_outlines()?;
        p.migrate_land()?;
        Ok(p)
    }

    /// Adds a map with no parent and returns its id.
    pub fn add_root_map(&mut self, settings: MapSettings) -> Result<MapId> {
        let s = settings;
        if self.kinds.get(s.kind).is_none() {
            return Err(Error::UnknownMapKind(s.kind.as_str().into()));
        }
        let id = MapId(self.store.insert_map(
            None,
            s.kind.as_str(),
            s.width,
            s.height,
            s.metres_per_pixel,
        )?);
        self.maps.push(Map {
            id,
            parent: None,
            settings,
            origin: RootPoint { x: 0.0, y: 0.0 },
            outline: None,
        });
        Ok(id)
    }

    /// Waits for a running save, then closes the project and deletes its working copy.
    pub fn close(mut self) -> Result<()> {
        self.finish_strokes()?;
        self.flush_writes()?;
        self.wait_for_save()?;
        self.pipeline.shutdown();
        // Tile workers and the edit writer hold connections to the working copy; stop them first.
        drop(self.cache);
        drop(self.writer);
        self.store.close()
    }

    /// All maps in the tree, ordered by id.
    pub fn maps(&self) -> &[Map] {
        &self.maps
    }

    /// The project seed (ADR-0021); generator runs default to it, so neighbours match.
    pub fn seed(&self) -> Result<u64> {
        self.store.seed()
    }

    /// The map kinds this project was opened with.
    pub fn kinds(&self) -> &Kinds {
        &self.kinds
    }

    /// The kind module of `map`.
    pub(crate) fn kind_of(&self, map: MapId) -> Result<&dyn MapKindModule> {
        let kind = self.map(map)?.settings.kind;
        self.kinds
            .get(kind)
            .ok_or_else(|| Error::UnknownMapKind(kind.as_str().into()))
    }

    /// Whether a stage of `map`'s kind writes `layer` (ADR-0018).
    pub fn is_derived(&self, map: MapId, layer: Layer) -> Result<bool> {
        Ok(pipeline::writes(self.kind_of(map)?.stages(), layer))
    }
}

impl Project {
    /// Embeds a copy of an asset; identical content is stored once (ADR-0015).
    pub fn add_asset(&self, pack_id: &str, asset_id: &str, data: &[u8]) -> Result<()> {
        self.store.add_asset(pack_id, asset_id, data)
    }

    pub fn asset(&self, pack_id: &str, asset_id: &str) -> Result<Option<Vec<u8>>> {
        self.store.asset(pack_id, asset_id)
    }

    pub fn asset_blob_count(&self) -> Result<i64> {
        self.store.asset_blob_count()
    }
}
