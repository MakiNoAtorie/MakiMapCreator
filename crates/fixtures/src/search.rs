//! Fixtures for search: many entities (#10) and long pages (#11).

use std::path::Path;

use mmc_core::{Command, Entity, EntityEdit, EntityId, MapId, PageEdit, Project};

use crate::{LargeMap, Rng};

const SYLLABLES: [&str; 40] = [
    "ar", "bel", "cor", "dun", "el", "fen", "gar", "hal", "ir", "jor", "kal", "lor", "mor", "nel",
    "or", "pel", "qui", "ras", "sil", "tor", "ul", "ver", "wyn", "xan", "yor", "zan", "bra", "cre",
    "dro", "eth", "fal", "gri", "hol", "ish", "kem", "lin", "mar", "nor", "ost", "pra",
];

/// Entity kinds the generator cycles through (seeded by the migration).
const KINDS: [&str; 6] = [
    "settlement",
    "region",
    "river",
    "landmark",
    "forest",
    "nation",
];

/// One of 1600 two-syllable words; page text and names share them, so queries find hits.
pub fn word(rng: &mut Rng) -> String {
    let mut pick = || SYLLABLES[(rng.next_u64() % 40) as usize];
    format!("{}{}", pick(), pick())
}

fn capitalised(w: &str) -> String {
    let mut c = w.chars();
    c.next()
        .map_or(String::new(), |f| f.to_uppercase().chain(c).collect())
}

/// Parameters of a many-entities fixture: names and one text field.
#[derive(Debug, Clone, Copy)]
pub struct ManyEntities {
    pub seed: u64,
    pub count: usize,
}

impl ManyEntities {
    /// The ADR-0014 size: 10 000 entities.
    pub fn standard(seed: u64) -> ManyEntities {
        ManyEntities {
            seed,
            count: 10_000,
        }
    }

    /// The next entity name from `rng`: one or two capitalised words.
    pub fn name(rng: &mut Rng) -> String {
        let first = capitalised(&word(rng));
        if rng.unit() < 0.5 {
            format!("{first} {}", capitalised(&word(rng)))
        } else {
            first
        }
    }

    /// Creates a project with a region map and `count` entities.
    pub fn build(&self, working_copy: &Path) -> (Project, MapId, Vec<EntityId>) {
        let mut p = Project::create(working_copy, LargeMap::region(self.seed).settings()).unwrap();
        let map = p.maps()[0].id;
        let mut rng = Rng::new(self.seed);
        let mut ids = Vec::with_capacity(self.count);
        for i in 0..self.count {
            let kind = KINDS[i % KINDS.len()];
            let mut fields = serde_json::Map::new();
            if kind == "nation" {
                fields.insert("colour".into(), word(&mut rng).into());
            }
            let id = p.next_entity_id().unwrap();
            let e = Entity {
                id,
                kind: kind.into(),
                name: Self::name(&mut rng),
                fields,
            };
            p.apply(map, Command::Entity(EntityEdit::Create(e)))
                .unwrap();
            ids.push(id);
        }
        (p, map, ids)
    }
}

/// Parameters of a long-pages fixture.
#[derive(Debug, Clone, Copy)]
pub struct LongPages {
    pub seed: u64,
    /// Pages written, one per entity from the start of the list.
    pub pages: usize,
    pub words_per_page: usize,
}

impl LongPages {
    /// The next page text from `rng`: headings and paragraphs of generated words.
    pub fn text(&self, rng: &mut Rng) -> String {
        let mut out = String::new();
        let mut left = self.words_per_page;
        while left > 0 {
            out.push_str("## ");
            out.push_str(&capitalised(&word(rng)));
            out.push_str("\n\n");
            let n = left.min(120);
            let words: Vec<String> = (0..n).map(|_| word(rng)).collect();
            out.push_str(&words.join(" "));
            out.push_str("\n\n");
            left -= n;
        }
        out
    }

    /// Writes a page for each of the first `pages` entities in `ids`.
    pub fn write(&self, p: &mut Project, map: MapId, ids: &[EntityId]) {
        let mut rng = Rng::new(self.seed);
        for &entity in ids.iter().take(self.pages) {
            let text = self.text(&mut rng);
            p.apply(map, Command::Page(PageEdit { entity, text }))
                .unwrap();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_gives_same_entities_and_pages() {
        let dir = tempfile::tempdir().unwrap();
        let snapshot = |name: &str, seed| {
            let (mut p, m, ids) = ManyEntities { seed, count: 12 }.build(&dir.path().join(name));
            let pages = LongPages {
                seed,
                pages: 2,
                words_per_page: 300,
            };
            pages.write(&mut p, m, &ids);
            let names: Vec<_> = p.entities().unwrap().into_iter().map(|e| e.name).collect();
            (names, p.page(ids[1]).unwrap())
        };
        let a = snapshot("a", 1);
        assert_eq!(a.0.len(), 12);
        assert!(a.1.split_whitespace().count() >= 300);
        assert_eq!(a, snapshot("b", 1));
        assert_ne!(a, snapshot("c", 2));
    }
}
