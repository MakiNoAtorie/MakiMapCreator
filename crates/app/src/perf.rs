//! Timing harnesses: pen strokes through the edit worker while a render worker runs; an 8192 px export while editing.
//! `cargo test -p mmc-app perf -- --ignored --nocapture`

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex, mpsc};
    use std::time::{Duration, Instant};

    use mmc_core::{Kinds, Layer, MapKindModule, Project, RootPoint, ToolAction};
    use mmc_render::{Renderer, ShaderPass, View};

    use crate::brush::{self, Direction, Options, Sample};
    use crate::canvas::lock_project;
    use crate::edit::{Done, Editor, Op};
    use crate::input::Phase;

    const FRAME: Duration = Duration::from_micros(16_667);

    /// (time s, phase, sample) at 200 Hz: three quick lines, a long one, five short ones.
    fn timeline() -> Vec<(f64, Phase, Sample)> {
        let mut out = vec![];
        let mut line = |t0: f64, dur: f64, a: (f64, f64), b: (f64, f64)| {
            let n = (dur / 0.005) as usize;
            for i in 0..=n {
                let f = i as f64 / n as f64;
                let phase = match i {
                    0 => Phase::Down,
                    i if i == n => Phase::Up,
                    _ => Phase::Move,
                };
                let at = RootPoint {
                    x: a.0 + (b.0 - a.0) * f,
                    y: a.1 + (b.1 - a.1) * f,
                };
                out.push((t0 + i as f64 * 0.005, phase, Sample { at, pressure: 0.5 }));
            }
        };
        for k in 0..3 {
            let y = 20000.0 + k as f64 * 8000.0;
            line(0.2 + k as f64 * 0.38, 0.3, (10000.0, y), (40000.0, y));
        }
        line(1.6, 2.0, (10000.0, 60000.0), (70000.0, 30000.0));
        for k in 0..5 {
            let x = 15000.0 + k as f64 * 9000.0;
            line(
                4.0 + k as f64 * 0.2,
                0.15,
                (x, 70000.0),
                (x + 4000.0, 76000.0),
            );
        }
        out
    }

    #[derive(Default)]
    struct Stroke {
        up: Option<Instant>,
        applied: Option<Instant>,
        /// Part to the epoch requested once it was applied.
        parts: HashMap<u32, Option<u64>>,
        rendered: Option<Instant>,
    }

    /// A new default region map, as File > New makes it.
    fn new_project(dir: &std::path::Path) -> Arc<Mutex<Option<Project>>> {
        let kinds = Arc::new(Kinds::new(vec![
            Box::new(mmc_kind_world::WorldKind),
            Box::new(mmc_kind_region::RegionKind),
        ]));
        let root = mmc_kind_region::RegionKind.default_settings();
        let store = mmc_store::Store::create(&dir.join("wc.sqlite")).unwrap();
        let p = Project::create(Box::new(store), kinds, root).unwrap();
        Arc::new(Mutex::new(Some(p)))
    }

    /// As `Canvas`'s worker without the GPU draw: (epoch, complete) per scene read.
    fn render_worker(
        project: Arc<Mutex<Option<Project>>>,
        requests: mpsc::Receiver<u64>,
        done: mpsc::Sender<(u64, bool)>,
    ) -> Duration {
        let mut max_wait = Duration::ZERO;
        let mut r = Renderer::warp().unwrap();
        let shader = mmc_kind_region::RegionKind.render_passes()[0].shader;
        r.register(Layer::HEIGHT, Box::new(ShaderPass::new(r.device(), shader)));
        let map = project.lock().unwrap().as_ref().unwrap().maps()[0].id;
        // The whole map on a 4K screen.
        let view = View {
            map,
            min: RootPoint { x: -5000.0, y: 0.0 },
            max: RootPoint {
                x: 86920.0,
                y: 81920.0,
            },
            width: 3840,
            height: 2160,
        };
        let mut pending: Option<u64> = None;
        loop {
            let next = match pending.take() {
                Some(e) => match requests.recv_timeout(Duration::from_millis(16)) {
                    Ok(n) => n,
                    Err(mpsc::RecvTimeoutError::Timeout) => e,
                    Err(_) => return max_wait,
                },
                None => match requests.recv() {
                    Ok(n) => n,
                    Err(_) => return max_wait,
                },
            };
            let epoch = requests.try_iter().last().unwrap_or(next);
            let fetched = {
                let (g, wait) = lock_project(&project, "render");
                max_wait = max_wait.max(wait);
                let p = g.as_ref().unwrap();
                r.fetch_cached(p, view).and_then(|f| f.with_summaries(p))
            };
            let complete = fetched.is_ok_and(|f| f.scene().is_complete());
            if !complete {
                pending = Some(epoch);
            }
            if done.send((epoch, complete)).is_err() {
                return max_wait;
            }
        }
    }

    /// Prints this crate's log lines, so the worker's per-stroke timings show.
    struct Print;
    impl log::Log for Print {
        fn enabled(&self, m: &log::Metadata) -> bool {
            m.target().starts_with("mmc_app::edit")
        }
        fn log(&self, r: &log::Record) {
            if self.enabled(r.metadata()) {
                eprintln!("{}", r.args());
            }
        }
        fn flush(&self) {}
    }

    #[test]
    #[ignore = "timing harness"]
    fn replay_strokes_through_the_edit_worker() {
        replay(240.0);
    }

    /// The largest brush: 512 px at 10 m/px.
    #[test]
    #[ignore = "timing harness"]
    fn replay_large_brush_strokes() {
        replay(5120.0);
    }

    /// Replays `timeline` with a brush of `size` metres radius.
    fn replay(size: f64) {
        let _ = log::set_logger(&Print);
        log::set_max_level(log::LevelFilter::Info);
        let dir = tempfile::tempdir().unwrap();
        let project = new_project(dir.path());
        let map = project.lock().unwrap().as_ref().unwrap().maps()[0].id;
        let (req_tx, req_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let wp = project.clone();
        let worker = std::thread::spawn(move || render_worker(wp, req_rx, done_tx));
        req_tx.send(0).unwrap();
        let editor = Editor::new(project.clone(), eframe::egui::Context::default());

        let events = timeline();
        let mut opts = Options::for_map(10.0);
        opts.size = size;
        let plan = |path: &[Sample]| {
            brush::plan(
                ToolAction::Brush,
                Layer::HEIGHT,
                Direction::Raise,
                &opts,
                10.0,
                path,
            )
        };
        let mut strokes: Vec<Stroke> = vec![];
        let mut live: Option<(Vec<Sample>, usize, u32)> = None;
        let (mut epoch, mut fresh) = (0u64, 0u64);
        let (mut frame_max, mut slow, mut frames) = (Duration::ZERO, 0, 0);
        let start = Instant::now();
        let mut next_event = 0;
        loop {
            let f0 = Instant::now();
            let t = start.elapsed().as_secs_f64();
            let send_part = |live: &mut Option<(Vec<Sample>, usize, u32)>,
                             strokes: &mut Vec<Stroke>,
                             id: usize| {
                let Some((path, sent, parts)) = live else {
                    return;
                };
                if let Some(cmd) = plan(path).and_then(|c| brush::tail(c, *sent)) {
                    *sent += brush::stamp_discs(&cmd).len();
                    *parts += 1;
                    strokes[id].parts.insert(*parts, None);
                    editor.send(Op::Part {
                        map,
                        stroke: id as u64,
                        part: *parts,
                        cmd,
                        sent: Instant::now(),
                    });
                }
            };
            while next_event < events.len() && events[next_event].0 <= t {
                let (_, phase, s) = events[next_event];
                next_event += 1;
                match phase {
                    Phase::Down => {
                        strokes.push(Stroke::default());
                        live = Some((vec![s], 0, 0));
                    }
                    Phase::Move => live.as_mut().unwrap().0.push(s),
                    Phase::Up => {
                        let id = strokes.len() - 1;
                        live.as_mut().unwrap().0.push(s);
                        send_part(&mut live, &mut strokes, id);
                        live = None;
                        let up = Instant::now();
                        strokes[id].up = Some(up);
                        editor.send(Op::End {
                            map,
                            stroke: id as u64,
                            up,
                        });
                    }
                    Phase::Hover => {}
                }
            }
            if live.is_some() {
                let id = strokes.len() - 1;
                send_part(&mut live, &mut strokes, id);
            }
            for d in editor.done() {
                match d {
                    Done::Applied {
                        stroke,
                        parts,
                        result,
                        ..
                    } => {
                        result.unwrap();
                        epoch += 1;
                        req_tx.send(epoch).unwrap();
                        for p in parts {
                            strokes[stroke as usize].parts.insert(p, Some(epoch));
                        }
                    }
                    Done::Ended { .. } => {
                        if let Some(s) = strokes
                            .iter_mut()
                            .find(|s| s.up.is_some() && s.applied.is_none())
                        {
                            s.applied = Some(Instant::now());
                        }
                    }
                    Done::Rebuilt(_) => {
                        epoch += 1;
                        req_tx.send(epoch).unwrap();
                    }
                    Done::Stepped { .. } => {}
                }
            }
            while let Ok((e, complete)) = done_rx.try_recv() {
                if complete {
                    fresh = fresh.max(e);
                }
            }
            for s in strokes
                .iter_mut()
                .filter(|s| s.applied.is_some() && s.rendered.is_none())
            {
                if s.parts.values().all(|e| e.is_some_and(|e| fresh >= e)) {
                    s.rendered = Some(Instant::now());
                }
            }
            let spent = f0.elapsed();
            frames += 1;
            frame_max = frame_max.max(spent);
            slow += (spent > Duration::from_millis(50)) as u32;
            let all = next_event == events.len() && strokes.iter().all(|s| s.rendered.is_some());
            if all || start.elapsed() > Duration::from_secs(60) {
                break;
            }
            if spent < FRAME {
                std::thread::sleep(FRAME - spent);
            }
        }
        drop(editor);
        drop(req_tx);
        let render_wait = worker.join().unwrap();
        let ms = |a: Option<Instant>, up: Option<Instant>| {
            a.zip(up)
                .map_or(-1.0, |(a, u)| (a - u).as_secs_f64() * 1000.0)
        };
        let mut applied: Vec<f64> = strokes.iter().map(|s| ms(s.applied, s.up)).collect();
        let mut rendered: Vec<f64> = strokes.iter().map(|s| ms(s.rendered, s.up)).collect();
        println!("applied {applied:.0?}\nrendered {rendered:.0?}");
        applied.sort_by(f64::total_cmp);
        rendered.sort_by(f64::total_cmp);
        println!(
            "up->applied median {:.0} max {:.0} ms; up->rendered median {:.0} max {:.0} ms; {frames} frames, max {:.1} ms, {slow} over 50 ms; render lock wait max {:.0} ms",
            applied[applied.len() / 2],
            applied[applied.len() - 1],
            rendered[rendered.len() / 2],
            rendered[rendered.len() - 1],
            frame_max.as_secs_f64() * 1000.0,
            render_wait.as_secs_f64() * 1000.0
        );
        project.lock().unwrap().take().unwrap().close().unwrap();
    }

    #[test]
    #[ignore = "timing harness"]
    fn export_8192_while_editing() {
        let dir = tempfile::tempdir().unwrap();
        let project = new_project(dir.path());
        let (map, settings) = {
            let g = project.lock().unwrap();
            let m = &g.as_ref().unwrap().maps()[0];
            (m.id, m.settings)
        };
        let editor = Editor::new(project.clone(), eframe::egui::Context::default());
        let mut opts = Options::for_map(10.0);
        opts.size = 2400.0;
        let stroke = |x: f64| {
            let at = |y| Sample {
                at: RootPoint { x, y },
                pressure: 0.8,
            };
            brush::plan(
                ToolAction::Brush,
                Layer::HEIGHT,
                Direction::Raise,
                &opts,
                10.0,
                &[at(5000.0), at(75000.0)],
            )
            .unwrap()
        };
        for (i, x) in [10000.0, 40000.0, 70000.0].into_iter().enumerate() {
            let sent = Instant::now();
            editor.send(Op::Part {
                map,
                stroke: i as u64,
                part: 1,
                cmd: stroke(x),
                sent,
            });
            editor.send(Op::End {
                map,
                stroke: i as u64,
                up: sent,
            });
        }
        let mut r = Renderer::warp().unwrap();
        let shader = mmc_kind_region::RegionKind.render_passes()[0].shader;
        r.register(Layer::HEIGHT, Box::new(ShaderPass::new(r.device(), shader)));
        editor.sync();
        let path = dir.path().join("export.png");
        let view = crate::export::full_view(map, &settings, 8192);
        let (tx, rx) = mpsc::channel();
        let (p2, path2) = (project.clone(), path.clone());
        let t = Instant::now();
        let export = std::thread::spawn(move || {
            let r = crate::export::export(&p2, &r, view, &path2);
            tx.send(()).unwrap();
            r
        });
        // A small stroke part every frame while the export runs: time to applied.
        let mut waits = vec![];
        let mut k = 10;
        while rx.try_recv().is_err() {
            let sent = Instant::now();
            let small = Options {
                size: 100.0,
                ..opts
            };
            let at = Sample {
                at: RootPoint {
                    x: 1000.0 + k as f64,
                    y: 1000.0,
                },
                pressure: 0.5,
            };
            let cmd = brush::plan(
                ToolAction::Brush,
                Layer::HEIGHT,
                Direction::Raise,
                &small,
                10.0,
                &[at],
            )
            .unwrap();
            editor.send(Op::Part {
                map,
                stroke: k,
                part: 1,
                cmd,
                sent,
            });
            editor.send(Op::End {
                map,
                stroke: k,
                up: sent,
            });
            loop {
                if editor
                    .done()
                    .iter()
                    .any(|d| matches!(d, Done::Ended { .. }))
                {
                    break;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            waits.push(sent.elapsed().as_secs_f64() * 1000.0);
            k += 1;
            std::thread::sleep(FRAME);
        }
        export.join().unwrap().unwrap();
        let secs = t.elapsed().as_secs_f64();
        // PNG IHDR: width and height, big-endian, at bytes 16..24.
        let head = std::fs::read(&path).unwrap();
        let be = |i: usize| u32::from_be_bytes(head[i..i + 4].try_into().unwrap());
        let (width, height) = (be(16), be(20));
        waits.sort_by(f64::total_cmp);
        println!(
            "export {}x{} in {secs:.1} s; {} edits meanwhile, applied median {:.0} max {:.0} ms",
            width,
            height,
            waits.len(),
            waits[waits.len() / 2],
            waits[waits.len() - 1]
        );
        assert_eq!((width, height), (8192, 8192));
        drop(editor);
        project.lock().unwrap().take().unwrap().close().unwrap();
    }
}
