//! Headless core: the project model and its public API (seam 1). No UI, no SQL.

use std::fmt;

mod cache;
mod command;
mod jobs;
mod layer;
pub mod persist;
mod pipeline;
pub use pipeline::{Raster, Stage, StageInput, par_map};
mod pyramid;
pub mod rng;
mod save;
mod undo;
pub use cache::TILE_CACHE_BYTES;
pub use command::{BrushStroke, Command, StrokePoint};
pub use layer::{Layer, RootPoint, TILE_SIZE};
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
        reason: &'static str,
    },
    CorruptEntity,
    InvalidAnchor(&'static str),
    UnknownFeature(FeatureId),
    UnknownGenerator(String),
    InvalidChild(&'static str),
    OutsideOutline,
    UnknownObject(ObjectId),
    NotAnObjectLayer(Layer),
    InvalidObject(ObjectId),
    /// Undo of a child map's creation while it has child maps of its own.
    MapHasChildren(MapId),
    OwnMapChoice(EntityId),
    StalePlan,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NotAProject => write!(f, "the file is not a MakiMapCreator project"),
            Error::NewerVersion { found, supported } => write!(
                f,
                "the file was saved by a newer version of the app                  (file version {found}, this app supports up to {supported})"
            ),
            Error::Storage(e) => write!(f, "storage error: {e}"),
            Error::WorkingCopyInUse => write!(f, "another window has this work open"),
            Error::UnknownMapKind(k) => write!(f, "the file has an unknown map kind: {k}"),
            Error::UnknownMap(id) => write!(f, "the project has no map with id {}", id.0),
            Error::CorruptTile => write!(f, "the file has a damaged map tile"),
            Error::DerivedLayer(l) => {
                write!(
                    f,
                    "the {} layer is computed and can not be edited",
                    l.as_str()
                )
            }
            Error::UnknownEntityKind(k) => write!(f, "the project has no entity kind {k}"),
            Error::UnknownEntity(id) => write!(f, "the project has no entity with id {}", id.0),
            Error::EntityExists(id) => write!(f, "an entity with id {} already exists", id.0),
            Error::InvalidField { key, reason } => write!(f, "field {key}: {reason}"),
            Error::CorruptEntity => write!(f, "the file has a damaged entity or field definition"),
            Error::InvalidAnchor(why) => write!(f, "anchor: {why}"),
            Error::UnknownFeature(id) => write!(f, "there is no derived feature with id {}", id.0),
            Error::UnknownGenerator(g) => write!(f, "this map kind has no generator {g}"),
            Error::InvalidChild(why) => write!(f, "child map: {why}"),
            Error::OutsideOutline => write!(f, "the edit is outside the area this map owns"),
            Error::UnknownObject(id) => write!(f, "the map has no object with id {}", id.0),
            Error::NotAnObjectLayer(l) => write!(f, "{} is not an object layer", l.as_str()),
            Error::InvalidObject(id) => write!(f, "object {} has a wrong id or shape", id.0),
            Error::MapHasChildren(id) => {
                write!(f, "map {} has child maps; delete them first", id.0)
            }
            Error::OwnMapChoice(id) => write!(
                f,
                "entity {} has its own map: choose to delete the map or keep it as a root map",
                id.0
            ),
            Error::StalePlan => write!(f, "the map changed after the preview; preview again"),
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
}

impl Project {
    /// Creates a project with one root map on an empty `store`. `kinds` are the map kinds
    /// the app ships; maps of other kinds can not be made or opened.
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
        };
        p.names = p.build_name_index()?;
        p.load_child_outlines()?;
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
        self.wait_for_save()?;
        self.pipeline.shutdown();
        // Tile workers hold read connections to the working copy; stop them first.
        drop(self.cache);
        self.store.close()
    }

    /// All maps in the tree, ordered by id.
    pub fn maps(&self) -> &[Map] {
        &self.maps
    }

    /// The project seed (ADR-0021). Generator runs default to it, so neighbouring child maps
    /// match at their shared edges.
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
