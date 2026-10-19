//! Entity pages: Markdown with `[[id|label]]` links, templates per kind and backlinks (ADR-0012).

use crate::persist::{PageRecord, Write};

use crate::{Entity, EntityId, Project, Result};

pub use mmc_page::Document;

/// Replaces the page text of `entity`. The page editor sends one per group of edits (ADR-0016).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageEdit {
    pub entity: EntityId,
    pub text: String,
}

/// An entity link in a page. The label is shown; the id resolves.
#[derive(Debug, Clone, PartialEq)]
pub struct Link {
    pub target: EntityId,
    pub label: String,
    /// `None` when the target was deleted: the link is missing.
    pub entity: Option<Entity>,
}

/// The page markup of a link to `target`.
pub fn link_markup(target: EntityId, label: &str) -> String {
    format!("[[{}|{label}]]", target.0)
}

/// Page rows before and after one page edit.
#[derive(Debug, Clone)]
pub(crate) struct PageChange {
    before: PageRecord,
    after: PageRecord,
}

impl Project {
    /// The page text of `id`; a page never written is its kind's template.
    pub fn page(&self, id: EntityId) -> Result<String> {
        let e = self.entity(id)?.ok_or(crate::Error::UnknownEntity(id))?;
        match self.store.page(id.0)? {
            Some(body) => Ok(body),
            None => Ok(self.store.page_template(&e.kind)?.unwrap_or_default()),
        }
    }

    /// Links in the page of `id`, in text order, resolved to their targets.
    pub fn links(&self, id: EntityId) -> Result<Vec<Link>> {
        mmc_page::entity_links(&self.page(id)?)
            .into_iter()
            .map(|l| {
                Ok(Link {
                    target: EntityId(l.id),
                    entity: self.entity(EntityId(l.id))?,
                    label: l.label,
                })
            })
            .collect()
    }

    /// Entities whose page links to `id`, ordered by id.
    pub fn backlinks(&self, id: EntityId) -> Result<Vec<EntityId>> {
        Ok(self
            .store
            .backlinks(id.0)?
            .into_iter()
            .map(EntityId)
            .collect())
    }

    pub(crate) fn page_change(&self, edit: &PageEdit) -> Result<PageChange> {
        let id = edit.entity;
        self.page(id)?;
        let record = |body: Option<String>| PageRecord {
            entity_id: id.0,
            links: body
                .as_deref()
                .map(|b| {
                    mmc_page::entity_links(b)
                        .into_iter()
                        .map(|l| l.id)
                        .collect()
                })
                .unwrap_or_default(),
            body,
        };
        Ok(PageChange {
            before: record(self.store.page(id.0)?),
            after: record(Some(edit.text.clone())),
        })
    }

    pub(crate) fn write_page_change(&self, c: &PageChange, forward: bool) -> Result<()> {
        let rec = if forward { &c.after } else { &c.before };
        self.store.write(&[Write::Page(rec.clone())])
    }
}
