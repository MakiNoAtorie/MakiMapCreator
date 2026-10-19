//! The persistence port: core's records and the traits a storage backend implements.
//! Core never names a backend; the caller (app, tests, bench) passes one in.

use std::path::Path;
use std::sync::Arc;

use crate::{Layer, Result};

#[derive(Debug, Clone, PartialEq)]
pub struct MapRecord {
    pub id: i64,
    pub parent_id: Option<i64>,
    pub kind: String,
    pub width: u32,
    pub height: u32,
    pub metres_per_pixel: f64,
    /// Root-space position of pixel (0, 0)'s top-left corner, in metres.
    pub origin_x: f64,
    pub origin_y: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TileKey {
    pub map_id: i64,
    pub layer: Layer,
    pub level: u32,
    pub tx: u32,
    pub ty: u32,
}

/// One tile blob; core owns the format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TileRecord {
    pub data: Vec<u8>,
    pub edited: bool,
}

/// Level-0 tiles of one layer as ((tx, ty), record), ordered.
pub type LayerTiles = Vec<((u32, u32), TileRecord)>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityKindRecord {
    pub id: String,
    pub label: String,
    /// "political" or "natural".
    pub category: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldDefRecord {
    pub kind: String,
    pub key: String,
    pub label: String,
    pub field_type: String,
    /// JSON array of options for choice fields.
    pub choices: Option<String>,
    pub builtin: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityRecord {
    pub id: i64,
    pub kind: String,
    pub name: String,
    /// JSON object of field values.
    pub fields: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelationRecord {
    pub from_id: i64,
    pub to_id: i64,
    pub rel_type: String,
}

/// One page with the entity ids its links point to. `body: None` means no page row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageRecord {
    pub entity_id: i64,
    pub body: Option<String>,
    pub links: Vec<i64>,
}

/// `points` is `x,y;x,y` in root-space metres.
#[derive(Debug, Clone, PartialEq)]
pub struct AnchorRecord {
    pub entity_id: i64,
    pub points: String,
    pub feature_id: Option<i64>,
    pub unplaced: bool,
}

/// One run of a generator on a map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerationRecord {
    pub id: i64,
    pub map_id: i64,
    pub generator: String,
    /// The u64 seed, bit for bit.
    pub seed: i64,
    /// Parameter values, a JSON object; opaque to the backend.
    pub params: String,
    pub app_version: String,
}

/// A map object; `points` is opaque to the backend.
#[derive(Debug, Clone, PartialEq)]
pub struct ObjectRecord {
    pub id: i64,
    pub map_id: i64,
    pub layer: String,
    /// "point", "path" or "polygon".
    pub shape: String,
    pub points: Vec<u8>,
    pub entity_id: Option<i64>,
    pub label_dx: Option<f64>,
    pub label_dy: Option<f64>,
}

/// A rectangle of one parent layer: parent pixels `x0..x0+width`, `y0..y0+height`.
/// `origin_x`, `origin_y` and `metres_per_pixel` are the parent's transform.
#[derive(Debug, Clone, PartialEq)]
pub struct ContextRecord {
    pub layer: String,
    pub origin_x: f64,
    pub origin_y: f64,
    pub metres_per_pixel: f64,
    pub x0: u32,
    pub y0: u32,
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

/// A child map to insert; `outline` and context `data` are opaque to the backend.
#[derive(Debug, Clone, PartialEq)]
pub struct NewChildMap {
    pub parent_id: i64,
    pub kind: String,
    pub width: u32,
    pub height: u32,
    pub metres_per_pixel: f64,
    pub origin_x: f64,
    pub origin_y: f64,
    pub entity_id: i64,
    pub outline: Vec<u8>,
    pub context: Vec<ContextRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildMapRecord {
    pub map_id: i64,
    pub entity_id: i64,
    pub outline: Vec<u8>,
}

/// A child map's link to its parent: what it loses when it becomes a root map.
#[derive(Debug, Clone, PartialEq)]
pub struct ChildLink {
    pub parent_id: i64,
    pub entity_id: i64,
    pub outline: Vec<u8>,
    pub context: Vec<ContextRecord>,
}

/// What a re-sync replaces on a child map; `outline` and context `data` are opaque.
#[derive(Debug, Clone, PartialEq)]
pub struct ChildState {
    pub outline: Vec<u8>,
    pub context: Vec<ContextRecord>,
    pub out_of_date: bool,
}

/// A full-text hit. `snippet` comes from the page text and marks matches with `[` `]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextHitRecord {
    pub entity_id: i64,
    pub snippet: Option<String>,
}

/// One row change. `Storage::write` applies a batch in one transaction.
#[derive(Debug, Clone, PartialEq)]
pub enum Write {
    /// `None` deletes the tile.
    Tile(TileKey, Option<TileRecord>),
    /// `None` deletes the entity.
    Entity(i64, Option<EntityRecord>),
    /// `false` deletes the relation.
    Relation(RelationRecord, bool),
    /// Writes the page and its link index rows.
    Page(PageRecord),
    /// `None` deletes the anchor row.
    Anchor(i64, Option<AnchorRecord>),
    /// `false` removes the generation record.
    Generation(GenerationRecord, bool),
    /// `None` deletes the map object.
    Object(i64, Option<ObjectRecord>),
    /// `false` removes the user field.
    FieldDef(FieldDefRecord, bool),
    /// Flags a map deleted or not.
    MapDeleted(i64, bool),
    /// Writes or drops a map's parent link; `None` makes it a root map.
    ChildLink(i64, Option<ChildLink>),
    /// Replaces the outline, parent context and flag of a child map.
    ChildState(i64, ChildState),
    OutOfDate(i64, bool),
}

/// Writes a snapshot of the project to a file; runs on a worker.
pub type Saver = Arc<dyn Fn(&Path) -> Result<()> + Send + Sync>;
/// Opens another connection for a worker thread.
pub type TileOpener = Arc<dyn Fn() -> Result<Box<dyn TileStore>> + Send + Sync>;

/// A project's storage (seam to the backend). Reads see every committed write.
pub trait Storage: Send {
    /// Writes all rows in one transaction.
    fn write(&self, writes: &[Write]) -> Result<()>;
    /// Writes the project file without blocking writes on this storage.
    fn saver(&self) -> Saver;
    fn tile_opener(&self) -> TileOpener;
    /// A read connection for a full-text query on a worker.
    fn text_index(&self) -> Result<Box<dyn TextIndex>>;
    /// Closes the storage and deletes its working copy.
    fn close(self: Box<Self>) -> Result<()>;

    fn insert_map(
        &self,
        parent_id: Option<i64>,
        kind: &str,
        width: u32,
        height: u32,
        mpp: f64,
    ) -> Result<i64>;
    /// Live maps, ordered by id.
    fn maps(&self) -> Result<Vec<MapRecord>>;
    fn tile(&self, key: &TileKey) -> Result<Option<TileRecord>>;
    /// Level-0 tiles of a layer that the user edited by hand, ordered by (tx, ty).
    fn edited_tiles(&self, map_id: i64, layer: Layer) -> Result<Vec<(u32, u32)>>;

    fn entity_kinds(&self) -> Result<Vec<EntityKindRecord>>;
    /// Field definitions of one kind, ordered by key.
    fn field_defs(&self, kind: &str) -> Result<Vec<FieldDefRecord>>;
    /// The id the next new entity gets; ids of deleted entities are never reused.
    fn next_entity_id(&self) -> Result<i64>;
    fn entity(&self, id: i64) -> Result<Option<EntityRecord>>;
    /// All entities, ordered by id.
    fn entities(&self) -> Result<Vec<EntityRecord>>;
    /// All relations, ordered by (from, to, type).
    fn relations(&self) -> Result<Vec<RelationRecord>>;

    fn page(&self, entity_id: i64) -> Result<Option<String>>;
    fn page_template(&self, kind: &str) -> Result<Option<String>>;
    /// Existing entities whose page links to `entity_id`, ordered by id.
    fn backlinks(&self, entity_id: i64) -> Result<Vec<i64>>;

    fn anchor(&self, entity_id: i64) -> Result<Option<AnchorRecord>>;
    /// Anchor rows of existing entities, ordered by entity id.
    fn anchors(&self) -> Result<Vec<AnchorRecord>>;

    fn next_generation_id(&self) -> Result<i64>;
    /// Generations of a map, oldest first.
    fn generations(&self, map_id: i64) -> Result<Vec<GenerationRecord>>;
    /// Map objects of a map, ordered by id.
    fn objects(&self, map_id: i64) -> Result<Vec<ObjectRecord>>;
    /// The id the next new map object gets; ids are never reused.
    fn next_object_id(&self) -> Result<i64>;

    /// Inserts a child map with its outline and parent context in one transaction.
    fn insert_child_map(&self, m: &NewChildMap) -> Result<i64>;
    /// All child map rows, ordered by map id.
    fn child_maps(&self) -> Result<Vec<ChildMapRecord>>;
    fn parent_context(&self, map_id: i64, layer: &str) -> Result<Option<ContextRecord>>;
    /// The parent link of `map_id`; `None` for a root map.
    fn child_link(&self, map_id: i64) -> Result<Option<ChildLink>>;
    /// `false` for a map that is not a child map.
    fn out_of_date(&self, map_id: i64) -> Result<bool>;
    /// `None` for a map that is not a child map.
    fn child_state(&self, map_id: i64) -> Result<Option<ChildState>>;

    /// Stores `data` under (`pack_id`, `asset_id`); identical content is stored once.
    fn add_asset(&self, pack_id: &str, asset_id: &str, data: &[u8]) -> Result<()>;
    fn asset(&self, pack_id: &str, asset_id: &str) -> Result<Option<Vec<u8>>>;
    fn asset_blob_count(&self) -> Result<i64>;
}

/// Tile reads and writes on a worker's own connection.
pub trait TileStore: Send {
    fn tile(&self, key: &TileKey) -> Result<Option<TileRecord>>;
    /// Level-0 tiles of a layer in one snapshot; `None` as soon as `stop` returns true.
    fn layer_tiles_until(
        &self,
        map_id: i64,
        layer: Layer,
        stop: &dyn Fn() -> bool,
    ) -> Result<Option<LayerTiles>>;
    /// Writes all tiles in one transaction; `None` deletes the tile.
    fn write_tiles(&self, writes: &[(TileKey, Option<TileRecord>)]) -> Result<()>;
}

/// Full-text queries on a read connection.
pub trait TextIndex: Send {
    /// Existing entities whose name, text fields or page match every word of `text`.
    fn search_text(&self, text: &str, limit: usize) -> Result<Vec<TextHitRecord>>;
}

/// Reads a project file without opening it.
pub trait ProjectReader {
    fn maps(&self) -> Result<Vec<MapRecord>>;
    fn tile(&self, key: &TileKey) -> Result<Option<TileRecord>>;
}
