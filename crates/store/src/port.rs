//! The store as core's persistence port.

use std::path::Path;
use std::sync::Arc;

use mmc_core::Layer;
use mmc_core::persist::*;

use crate::{FileReader, Reader, Store, TileConn};

type R<T> = mmc_core::Result<T>;

impl Storage for Store {
    fn write(&self, writes: &[Write]) -> R<()> {
        Ok(Store::write(self, writes)?)
    }

    fn saver(&self) -> Saver {
        let wc = self.working_copy.clone();
        Arc::new(move |file: &Path| Ok(crate::save_snapshot(&wc, file)?))
    }

    fn tile_opener(&self) -> TileOpener {
        let (wc, gate) = (self.working_copy.clone(), self.gate.clone());
        Arc::new(move || {
            Ok(Box::new(TileConn::open_gated(&wc, gate.clone())?) as Box<dyn TileStore>)
        })
    }

    fn text_index(&self) -> R<Box<dyn TextIndex>> {
        Ok(Box::new(self.reader()?))
    }

    fn close(self: Box<Self>) -> R<()> {
        Ok(Store::close(*self)?)
    }

    fn insert_map(&self, parent: Option<i64>, kind: &str, w: u32, h: u32, mpp: f64) -> R<i64> {
        Ok(Store::insert_map(self, parent, kind, w, h, mpp)?)
    }

    fn maps(&self) -> R<Vec<MapRecord>> {
        Ok(Store::maps(self)?)
    }

    fn seed(&self) -> R<u64> {
        Ok(Store::seed(self)? as u64)
    }

    fn tile(&self, key: &TileKey) -> R<Option<TileRecord>> {
        Ok(Store::tile(self, key)?)
    }

    fn edited_tiles(&self, map_id: i64, layer: Layer) -> R<Vec<(u32, u32)>> {
        Ok(Store::edited_tiles(self, map_id, layer.as_str())?)
    }

    fn entity_kinds(&self) -> R<Vec<EntityKindRecord>> {
        Ok(Store::entity_kinds(self)?)
    }

    fn field_defs(&self, kind: &str) -> R<Vec<FieldDefRecord>> {
        Ok(Store::field_defs(self, kind)?)
    }

    fn next_entity_id(&self) -> R<i64> {
        Ok(Store::next_entity_id(self)?)
    }

    fn entity(&self, id: i64) -> R<Option<EntityRecord>> {
        Ok(Store::entity(self, id)?)
    }

    fn entities(&self) -> R<Vec<EntityRecord>> {
        Ok(Store::entities(self)?)
    }

    fn relations(&self) -> R<Vec<RelationRecord>> {
        Ok(Store::relations(self)?)
    }

    fn page(&self, entity_id: i64) -> R<Option<String>> {
        Ok(Store::page(self, entity_id)?)
    }

    fn page_template(&self, kind: &str) -> R<Option<String>> {
        Ok(Store::page_template(self, kind)?)
    }

    fn backlinks(&self, entity_id: i64) -> R<Vec<i64>> {
        Ok(Store::backlinks(self, entity_id)?)
    }

    fn anchor(&self, entity_id: i64) -> R<Option<AnchorRecord>> {
        Ok(Store::anchor(self, entity_id)?)
    }

    fn anchors(&self) -> R<Vec<AnchorRecord>> {
        Ok(Store::anchors(self)?)
    }

    fn next_generation_id(&self) -> R<i64> {
        Ok(Store::next_generation_id(self)?)
    }

    fn generations(&self, map_id: i64) -> R<Vec<GenerationRecord>> {
        Ok(Store::generations(self, map_id)?)
    }

    fn objects(&self, map_id: i64) -> R<Vec<ObjectRecord>> {
        Ok(Store::objects(self, map_id)?)
    }

    fn next_object_id(&self) -> R<i64> {
        Ok(Store::next_object_id(self)?)
    }

    fn insert_child_map(&self, m: &NewChildMap) -> R<i64> {
        Ok(Store::insert_child_map(self, m)?)
    }

    fn child_maps(&self) -> R<Vec<ChildMapRecord>> {
        Ok(Store::child_maps(self)?)
    }

    fn parent_context(&self, map_id: i64, layer: &str) -> R<Option<ContextRecord>> {
        Ok(Store::parent_context(self, map_id, layer)?)
    }

    fn child_link(&self, map_id: i64) -> R<Option<ChildLink>> {
        Ok(Store::child_link(self, map_id)?)
    }

    fn out_of_date(&self, map_id: i64) -> R<bool> {
        Ok(Store::out_of_date(self, map_id)?)
    }

    fn child_state(&self, map_id: i64) -> R<Option<ChildState>> {
        Ok(Store::child_state(self, map_id)?)
    }

    fn add_asset(&self, pack_id: &str, asset_id: &str, data: &[u8]) -> R<()> {
        Ok(Store::add_asset(self, pack_id, asset_id, data)?)
    }

    fn asset(&self, pack_id: &str, asset_id: &str) -> R<Option<Vec<u8>>> {
        Ok(Store::asset(self, pack_id, asset_id)?)
    }

    fn asset_blob_count(&self) -> R<i64> {
        Ok(Store::asset_blob_count(self)?)
    }
}

impl TileStore for TileConn {
    fn tile(&self, key: &TileKey) -> R<Option<TileRecord>> {
        Ok(TileConn::tile(self, key)?)
    }

    fn layer_tiles_until(
        &self,
        map_id: i64,
        layer: Layer,
        stop: &dyn Fn() -> bool,
    ) -> R<Option<LayerTiles>> {
        Ok(TileConn::layer_tiles_until(
            self,
            map_id,
            layer.as_str(),
            stop,
        )?)
    }

    fn write_tiles(&self, writes: &[(TileKey, Option<TileRecord>)]) -> R<()> {
        Ok(TileConn::write_tiles(self, writes)?)
    }
}

impl TextIndex for Reader {
    fn search_text(&self, text: &str, limit: usize) -> R<Vec<TextHitRecord>> {
        Ok(Reader::search_text(self, text, limit)?)
    }
}

impl ProjectReader for FileReader {
    fn maps(&self) -> R<Vec<MapRecord>> {
        Ok(FileReader::maps(self)?)
    }

    fn tile(&self, key: &TileKey) -> R<Option<TileRecord>> {
        Ok(FileReader::tile(self, key)?)
    }
}
