//! Search: an in-memory name index (prefix and fuzzy, no SQL) and full-text queries on a worker (ADR-0014).

use std::collections::BTreeMap;
use std::sync::mpsc::{Receiver, channel};

use crate::{EntityId, Error, Project, Result};

/// A name match. Higher `score` is better.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameHit {
    pub id: EntityId,
    pub name: String,
    pub score: u32,
}

/// A full-text match on an entity's name, text field or page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextHit {
    pub entity: EntityId,
    /// Page excerpt with matches in `[` `]`; `None` for a name or field match.
    pub snippet: Option<String>,
}

/// Lower-case names by entity id. Kept equal to the store by the entity write path.
#[derive(Default)]
pub(crate) struct NameIndex {
    names: BTreeMap<EntityId, (String, String)>,
}

impl NameIndex {
    pub(crate) fn set(&mut self, id: EntityId, name: &str) {
        self.names
            .insert(id, (name.to_owned(), name.to_lowercase()));
    }

    pub(crate) fn remove(&mut self, id: EntityId) {
        self.names.remove(&id);
    }

    fn find(&self, query: &str, limit: usize) -> Vec<NameHit> {
        let q = query.trim().to_lowercase();
        if q.is_empty() || limit == 0 {
            return vec![];
        }
        let qc: Vec<char> = q.chars().collect();
        let mut hits: Vec<NameHit> = self
            .names
            .iter()
            .filter_map(|(id, (name, lower))| {
                let score = score(&q, &qc, lower)?;
                Some(NameHit {
                    id: *id,
                    name: name.clone(),
                    score,
                })
            })
            .collect();
        hits.sort_by(|a, b| {
            b.score
                .cmp(&a.score)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
                .then(a.id.cmp(&b.id))
        });
        hits.truncate(limit);
        hits
    }
}

fn words(s: &str) -> impl Iterator<Item = &str> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
}

/// Tiers: name prefix, word prefix, substring, then a typo within a word prefix.
fn score(q: &str, qc: &[char], lower: &str) -> Option<u32> {
    let extra = lower.chars().count().saturating_sub(qc.len()) as u32;
    if lower.starts_with(q) {
        return Some(1000 - extra.min(99));
    }
    if let Some(i) = words(lower).position(|w| w.starts_with(q)) {
        return Some(800 - (i as u32).min(9) * 10 - extra.min(9));
    }
    if lower.contains(q) {
        return Some(600 - extra.min(99));
    }
    if qc.len() < 3 {
        return None;
    }
    let max = if qc.len() < 6 { 1 } else { 2 };
    words(lower)
        .map(|w| prefix_distance(qc, w))
        .min()
        .filter(|d| *d <= max)
        .map(|d| 400 - 100 * d as u32)
}

/// Fewest edits (insert, delete, substitute, swap) from `q` to any prefix of `word`.
fn prefix_distance(q: &[char], word: &str) -> usize {
    let w: Vec<char> = word.chars().collect();
    let n = w.len();
    // d[i][j]: q[..i] against w[..j]; only the last two rows plus one are kept.
    let (mut prev2, mut prev) = (vec![0usize; n + 1], (0..=n).collect::<Vec<_>>());
    for i in 1..=q.len() {
        let mut cur = vec![i; n + 1];
        for j in 1..=n {
            let cost = usize::from(q[i - 1] != w[j - 1]);
            cur[j] = (prev[j - 1] + cost).min(prev[j] + 1).min(cur[j - 1] + 1);
            if i > 1 && j > 1 && q[i - 1] == w[j - 2] && q[i - 2] == w[j - 1] {
                cur[j] = cur[j].min(prev2[j - 2] + 1);
            }
        }
        prev2 = std::mem::replace(&mut prev, cur);
    }
    prev.into_iter().min().unwrap_or(q.len())
}

/// A full-text query running on a worker thread.
pub struct TextSearch {
    rx: Receiver<Result<Vec<TextHit>>>,
}

impl TextSearch {
    /// The results once the worker is done; `None` while it runs, and after the results were taken.
    pub fn poll(&mut self) -> Option<Result<Vec<TextHit>>> {
        self.rx.try_recv().ok()
    }

    /// Blocks until the worker is done. Returns no hits if `poll` already took them.
    pub fn wait(self) -> Result<Vec<TextHit>> {
        self.rx.recv().unwrap_or(Ok(vec![]))
    }
}

impl Project {
    /// Entities whose name matches `query` by prefix, word prefix, substring or a small typo; best first.
    pub fn find_names(&self, query: &str, limit: usize) -> Vec<NameHit> {
        self.names.find(query, limit)
    }

    /// Starts a full-text search over names, text fields and pages (every word, the last as prefix).
    /// Sees the edits applied before this call. Results hold at most `limit` entities.
    pub fn search_text(&self, query: &str, limit: usize) -> Result<TextSearch> {
        let reader = self.store.reader()?;
        let query = query.to_owned();
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let r = reader
                .search_text(&query, limit)
                .map(|hits| {
                    hits.into_iter()
                        .map(|h| TextHit {
                            entity: EntityId(h.entity_id),
                            snippet: h.snippet,
                        })
                        .collect()
                })
                .map_err(Error::from);
            let _ = tx.send(r);
        });
        Ok(TextSearch { rx })
    }

    pub(crate) fn build_name_index(&self) -> Result<NameIndex> {
        let mut idx = NameIndex::default();
        for e in self.store.entities()? {
            idx.set(EntityId(e.id), &e.name);
        }
        Ok(idx)
    }
}
