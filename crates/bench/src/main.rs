//! Benchmark gate for the ADR-0014 budgets: `cargo run -p mmc-bench --release -- [results.json]`.

use std::collections::BTreeMap;
use std::time::Instant;

use mmc_core::{Command, Layer, TileId};
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
    // One stamp of the largest brush (512 px) into a stroke session: what the edit worker does per lock.
    let large = (0..RUNS)
        .map(|i| {
            let Command::Brush(mut b) = fixture.stroke(&mut rng) else {
                unreachable!()
            };
            b.points.truncate(1);
            b.points[0].radius = 5120.0;
            b.points[0].at.x = 6000.0 + i as f64 * 11000.0;
            let ms = time_ms(|| project.apply_part(map, Command::Brush(b)).unwrap()).0;
            project.end_stroke(map).unwrap();
            ms
        })
        .collect();
    // Save and open time the fixture as built; strokes during a save grow the file by machine speed.
    project.wait_for_jobs().unwrap();
    let save = (0..RUNS)
        .map(|_| {
            time_ms(|| {
                project.save(&file);
                project.wait_for_save().unwrap();
            })
            .0
        })
        .collect();
    // Editing pause: the median stroke during each save; a blocking save makes one stroke the whole save.
    let paused = dir.path().join("paused.mmc");
    let pause = (0..RUNS)
        .map(|_| {
            project.wait_for_jobs().unwrap();
            project.save(&paused);
            let mut strokes = vec![];
            loop {
                let cmd = fixture.stroke(&mut rng);
                strokes.push(time_ms(|| project.apply(map, cmd).unwrap()).0);
                if let Some(res) = project.poll_save() {
                    res.unwrap();
                    break median(strokes);
                }
            }
        })
        .collect();
    project.close().unwrap();

    let open = (0..RUNS)
        .map(|i| {
            let wc = dir.path().join(format!("o{i}"));
            let (ms, p) = time_ms(|| mmc_fixtures::open(&file, &wc).unwrap());
            p.close().unwrap();
            ms
        })
        .collect();

    let first_frame = (0..RUNS)
        .map(|_| time_ms(|| mmc_fixtures::read_overviews(&file, Layer::HEIGHT).unwrap()).0)
        .collect();

    // Pan a 1920x1080 view at full resolution to an area no tile of which is loaded.
    let p = mmc_fixtures::open(&file, &dir.path().join("pan")).unwrap();
    let top = p.top_level(map).unwrap();
    let id = |level, tx, ty| TileId {
        map,
        layer: Layer::HEIGHT,
        level,
        tx,
        ty,
    };
    p.cached_tile(id(top, 0, 0)).unwrap();
    p.wait_tiles();
    let (mut stand_in, mut full) = (Vec::new(), Vec::new());
    for run in 0..RUNS as u32 {
        let (x0, y0) = (run % 4 * 8, run / 4 * 5);
        let t = Instant::now();
        for ty in y0..y0 + 5 {
            for tx in x0..x0 + 8 {
                assert!(p.cached_tile(id(0, tx, ty)).unwrap().is_some());
            }
        }
        stand_in.push(t.elapsed().as_secs_f64() * 1000.0);
        p.wait_tiles();
        full.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    p.close().unwrap();

    // ADR-0014: full rebuild of derived layers at 4096² after a stroke wide enough to touch many tiles.
    let fixture = LargeMap {
        size: 4096,
        radius: 2000.0,
        ..LargeMap::region(2)
    };
    let (mut project, map) = fixture.build(&dir.path().join("rebuild.sqlite"));
    project.wait_for_jobs().unwrap();
    let rebuild = (0..RUNS)
        .map(|_| {
            // The rebuild starts when `apply` returns; time it alone.
            project.apply(map, fixture.stroke(&mut rng)).unwrap();
            time_ms(|| project.wait_for_jobs().unwrap()).0
        })
        .collect();
    project.close().unwrap();

    BTreeMap::from([
        ("full_rebuild".into(), median(rebuild)),
        ("brush_stroke".into(), median(brush)),
        ("large_brush_stamp".into(), median(large)),
        ("open_project".into(), median(open)),
        ("open_first_frame".into(), median(first_frame)),
        ("pan_stand_in".into(), median(stand_in)),
        ("pan_full_tiles".into(), median(full)),
        ("save".into(), median(save)),
        ("save_edit_pause".into(), median(pause)),
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
