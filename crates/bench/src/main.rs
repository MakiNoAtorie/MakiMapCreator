//! Benchmark gate for the ADR-0014 budgets. Run in release:
//! `cargo run -p mmc-bench --release -- [results.json]`; exit 1 if a budget is exceeded.

use std::collections::BTreeMap;
use std::time::Instant;

use mmc_core::Project;
use mmc_fixtures::{LargeMap, Rng};

mod search;

const BUDGETS: &str = include_str!("../budgets.toml");
const RUNS: usize = 7;

fn parse_budgets(text: &str) -> BTreeMap<String, f64> {
    text.lines()
        .map(|l| l.split('#').next().unwrap().trim())
        .filter(|l| !l.is_empty())
        .map(|l| {
            let (k, v) = l.split_once('=').expect("budget line: name = ms");
            (k.trim().to_string(), v.trim().parse().expect("budget ms"))
        })
        .collect()
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

/// Names of budgets that are exceeded or have no result.
fn failures(budgets: &BTreeMap<String, f64>, results: &BTreeMap<String, f64>) -> Vec<String> {
    budgets
        .iter()
        .filter(|(k, max)| results.get(*k).is_none_or(|ms| ms > max))
        .map(|(k, _)| k.clone())
        .collect()
}

fn time_ms<T>(f: impl FnOnce() -> T) -> (f64, T) {
    let t = Instant::now();
    let out = f();
    (t.elapsed().as_secs_f64() * 1000.0, out)
}

/// One measured block per budget: the median ms over `RUNS`.
fn measure() -> BTreeMap<String, f64> {
    let dir = tempfile::tempdir().unwrap();
    let fixture = LargeMap::region(1);
    let (mut project, map) = fixture.build(&dir.path().join("wc.sqlite"));
    let file = dir.path().join("big.mmc");

    let mut rng = Rng::new(99);
    let brush = (0..RUNS)
        .map(|_| {
            let cmd = fixture.stroke(&mut rng);
            time_ms(|| project.apply(map, cmd).unwrap()).0
        })
        .collect();
    let save = (0..RUNS)
        .map(|_| time_ms(|| project.save(&file).unwrap()).0)
        .collect();
    project.close().unwrap();

    let open = (0..RUNS)
        .map(|i| {
            let wc = dir.path().join(format!("o{i}"));
            let (ms, p) = time_ms(|| Project::open(&file, &wc).unwrap());
            p.close().unwrap();
            ms
        })
        .collect();

    BTreeMap::from([
        ("brush_stroke".into(), median(brush)),
        ("open_project".into(), median(open)),
        ("save".into(), median(save)),
    ])
}

fn json(budgets: &BTreeMap<String, f64>, results: &BTreeMap<String, f64>) -> String {
    let rows: Vec<String> = budgets
        .iter()
        .map(|(k, max)| {
            let ms = results.get(k).copied().unwrap_or(f64::NAN);
            let ok = ms <= *max;
            format!("  \"{k}\": {{\"median_ms\": {ms:.3}, \"budget_ms\": {max}, \"ok\": {ok}}}")
        })
        .collect();
    format!("{{\n{}\n}}\n", rows.join(",\n"))
}

fn main() {
    let budgets = parse_budgets(BUDGETS);
    let mut results = measure();
    search::measure(&mut results);
    let out = json(&budgets, &results);
    print!("{out}");
    if let Some(path) = std::env::args().nth(1) {
        std::fs::write(path, &out).unwrap();
    }
    let bad = failures(&budgets, &results);
    if !bad.is_empty() {
        eprintln!("budget exceeded: {}", bad.join(", "));
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_flags_over_budget_and_missing_results() {
        let b = parse_budgets("a = 10 # c\n\nb = 5\nc = 1\n");
        let r = BTreeMap::from([("a".to_string(), 10.0), ("b".to_string(), 5.1)]);
        assert_eq!(failures(&b, &r), ["b", "c"]);
    }

    #[test]
    fn shipped_budgets_parse() {
        assert!(parse_budgets(BUDGETS).contains_key("brush_stroke"));
    }

    #[test]
    fn median_picks_middle() {
        assert_eq!(median(vec![3.0, 1.0, 9.0]), 3.0);
    }
}
