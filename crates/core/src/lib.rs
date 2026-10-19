//! Headless core: the project model and its public API (seam 1). No UI, no SQL.

use std::fmt;
use std::path::Path;

use mmc_store::Store;

mod command;
mod jobs;
mod layer;
mod pipeline;
pub mod rng;
mod undo;
pub use command::{BrushStroke, Command, StrokePoint};
pub use layer::{Layer, RootPoint, TILE_SIZE};
pub use undo::UNDO_LIMIT;
mod entity;
pub use entity::{Entity, EntityEdit, EntityId, EntityKind, FieldDef, FieldType, Relation};
mod anchor;
pub use anchor::{
    AnchorEdit, Anchors, DEFAULT_ANCHOR_TOLERANCE, DerivedFeature, FeatureId, FeatureKind,
};
mod page;
pub use page::{Document, Link, PageEdit, link_markup};
mod search;
pub use search::{NameHit, TextHit, TextSearch};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapKind {
    World,
    Region,
    Settlement,
}

impl MapKind {
    fn as_str(self) -> &'static str {
        match self {
            MapKind::World => "world",
            MapKind::Region => "region",
            MapKind::Settlement => "settlement",
        }
    }

    fn parse(s: &str) -> Option<MapKind> {
        [MapKind::World, MapKind::Region, MapKind::Settlement]
            .into_iter()
            .find(|k| k.as_str() == s)
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

impl MapSettings {
    pub fn default_for(kind: MapKind) -> MapSettings {
        let (size, metres_per_pixel) = match kind {
            MapKind::World => (4096, 1000.0),
            MapKind::Region => (8192, 10.0),
            MapKind::Settlement => (4096, 0.5),
        };
        MapSettings {
            kind,
            width: size,
            height: size,
            metres_per_pixel,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MapId(i64);

#[derive(Debug, Clone, PartialEq)]
pub struct Map {
    pub id: MapId,
    pub parent: Option<MapId>,
    pub settings: MapSettings,
}

#[derive(Debug)]
pub enum Error {
    Store(mmc_store::Error),
    UnknownMapKind(String),
    UnknownMap(MapId),
    CorruptTile,
    DerivedLayer(Layer),
    UnknownEntityKind(String),
    UnknownEntity(EntityId),
    EntityExists(EntityId),
    InvalidField { key: String, reason: &'static str },
    CorruptEntity,
    InvalidAnchor(&'static str),
    UnknownFeature(FeatureId),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Store(e) => e.fmt(f),
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
        }
    }
}

impl std::error::Error for Error {}

impl From<mmc_store::Error> for Error {
    fn from(e: mmc_store::Error) -> Self {
        Error::Store(e)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// An open project. Edits go to a working copy; `save` writes the project file.
pub struct Project {
    store: Store,
    maps: Vec<Map>,
    history: std::collections::HashMap<MapId, undo::History>,
    names: search::NameIndex,
    pipeline: pipeline::Pipeline,
    anchors: anchor::State,
}

impl Project {
    /// Creates a project with one root map. `working_copy` is a file path in local app data.
    pub fn create(working_copy: &Path, root: MapSettings) -> Result<Project> {
        let store = Store::create(working_copy)?;
        let id = store.insert_map(
            None,
            root.kind.as_str(),
            root.width,
            root.height,
            root.metres_per_pixel,
        )?;
        Ok(Project {
            store,
            maps: vec![Map {
                id: MapId(id),
                parent: None,
                settings: root,
            }],
            history: Default::default(),
            names: Default::default(),
            pipeline: Default::default(),
            anchors: Default::default(),
        })
    }

    /// Opens the project `file` into a new working copy at `working_copy`.
    pub fn open(file: &Path, working_copy: &Path) -> Result<Project> {
        let store = Store::open(file, working_copy)?;
        let maps = store
            .maps()?
            .into_iter()
            .map(|r| {
                Ok(Map {
                    id: MapId(r.id),
                    parent: r.parent_id.map(MapId),
                    settings: MapSettings {
                        kind: MapKind::parse(&r.kind).ok_or(Error::UnknownMapKind(r.kind))?,
                        width: r.width,
                        height: r.height,
                        metres_per_pixel: r.metres_per_pixel,
                    },
                })
            })
            .collect::<Result<_>>()?;
        let mut p = Project {
            store,
            maps,
            history: Default::default(),
            names: Default::default(),
            pipeline: Default::default(),
            anchors: Default::default(),
        };
        p.names = p.build_name_index()?;
        Ok(p)
    }

    /// Adds a map with no parent and returns its id.
    pub fn add_root_map(&mut self, settings: MapSettings) -> Result<MapId> {
        let s = settings;
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
        });
        Ok(id)
    }

    pub fn save(&self, file: &Path) -> Result<()> {
        Ok(self.store.save(file)?)
    }

    /// Closes the project and deletes its working copy.
    pub fn close(mut self) -> Result<()> {
        self.pipeline.shutdown();
        Ok(self.store.close()?)
    }

    /// All maps in the tree, ordered by id.
    pub fn maps(&self) -> &[Map] {
        &self.maps
    }
}

impl Project {
    /// Embeds a copy of an asset; identical content is stored once (ADR-0015).
    pub fn add_asset(&self, pack_id: &str, asset_id: &str, data: &[u8]) -> Result<()> {
        Ok(self.store.add_asset(pack_id, asset_id, data)?)
    }

    pub fn asset(&self, pack_id: &str, asset_id: &str) -> Result<Option<Vec<u8>>> {
        Ok(self.store.asset(pack_id, asset_id)?)
    }

    pub fn asset_blob_count(&self) -> Result<i64> {
        Ok(self.store.asset_blob_count()?)
    }
}
