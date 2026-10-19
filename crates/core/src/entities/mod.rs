//! Entities, relations, fields, pages, search and anchors; uses core types and the port, never maps.

mod anchor;
mod entity;
mod page;
mod search;

pub(crate) use anchor::{AnchorChange, State as AnchorState};
pub use anchor::{
    AnchorEdit, Anchors, DEFAULT_ANCHOR_TOLERANCE, DerivedFeature, FeatureId, FeatureKind,
};
pub(crate) use entity::EntityChanges;
pub use entity::{Entity, EntityEdit, EntityId, EntityKind, FieldDef, FieldType, Relation};
pub(crate) use page::PageChange;
pub use page::{Document, Link, PageEdit, link_markup};
pub(crate) use search::NameIndex;
pub use search::{NameHit, TextHit, TextSearch};
