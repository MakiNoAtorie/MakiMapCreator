//! Search budgets on a 10 000-entity fixture with long pages (ADR-0014).

use std::collections::BTreeMap;

use mmc_fixtures::{LongPages, ManyEntities, Rng, word};

use crate::{RUNS, median, time_ms};

pub fn measure(out: &mut BTreeMap<String, f64>) {
    let dir = tempfile::tempdir().unwrap();
    let (mut p, map, ids) = ManyEntities::standard(5).build(&dir.path().join("search.sqlite"));
    let pages = LongPages {
        seed: 6,
        pages: 200,
        words_per_page: 3000,
    };
    pages.write(&mut p, map, &ids);

    // Keystrokes: 2 to 4 letters of existing names, and a misspelt longer one.
    let mut rng = Rng::new(7);
    let names: Vec<String> = (0..RUNS)
        .map(|_| {
            let id = ids[(rng.next_u64() % ids.len() as u64) as usize];
            p.entity(id).unwrap().unwrap().name
        })
        .collect();
    let lookup = names
        .iter()
        .enumerate()
        .map(|(i, n)| {
            let q: String = match i % 3 {
                0 => n.chars().take(2).collect(),
                1 => n.chars().take(4).collect(),
                _ => n
                    .chars()
                    .rev()
                    .skip(1)
                    .collect::<String>()
                    .chars()
                    .rev()
                    .collect(),
            };
            time_ms(|| p.find_names(&q, 10)).0
        })
        .collect();
    out.insert("name_lookup".into(), median(lookup));

    let mut rng = Rng::new(8);
    let full_text = (0..RUNS)
        .map(|_| {
            let q = word(&mut rng);
            time_ms(|| p.search_text(&q, 20).unwrap().wait().unwrap()).0
        })
        .collect();
    out.insert("full_text_first_results".into(), median(full_text));
}
