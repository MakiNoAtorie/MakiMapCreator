//! Edits on a worker (ADR-0014): stroke parts, undo and redo apply in order, off the UI thread.

use std::collections::{HashMap, VecDeque};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::time::{Duration, Instant};

use eframe::egui;
use mmc_core::{Command, MapId};

use crate::brush::stamp_discs;
use crate::canvas::{SharedProject, lock_project};
use crate::live::{Applied, Mark};

/// How often the worker checks for finished rebuilds of derived layers.
const JOBS_POLL: Duration = Duration::from_millis(100);
/// How long the worker waits for derived layers after an edit.
const JOBS_WAIT: Duration = Duration::from_secs(30);
/// Sum of squared stamp radii, in pixels, applied per project lock; the render fetches between.
const LOCK_STAMPS: f64 = 2.0 * 256.0 * 256.0;
/// Prefetch radius around the pen, in brush radii: about the next frame's stamps.
const PREFETCH_REACH: f64 = 2.0;

pub enum Op {
    /// Part `part` of stroke `stroke`; parts until `End` are one undo step.
    Part {
        map: MapId,
        stroke: u64,
        part: u32,
        cmd: Command,
        sent: Instant,
    },
    /// The pen lifted at `up`.
    End {
        map: MapId,
        stroke: u64,
        up: Instant,
    },
    Undo(MapId),
    Redo(MapId),
    /// Report when the derived layers being rebuilt are done.
    Watch,
    /// Load the tiles of `layer` within `radius` metres of `at` into the tile cache.
    Prefetch {
        map: MapId,
        layer: mmc_core::Layer,
        at: mmc_core::RootPoint,
        radius: f64,
    },
    /// Answers once every op sent before it is done.
    Sync(Sender<()>),
}

pub enum Done {
    Applied {
        map: MapId,
        stroke: u64,
        parts: Vec<u32>,
        result: mmc_core::Result<()>,
    },
    Ended {
        map: MapId,
        result: mmc_core::Result<()>,
    },
    /// Undo or redo; `Ok(false)` when there was nothing to do.
    Stepped {
        map: MapId,
        result: mmc_core::Result<bool>,
    },
    /// Derived layers were rebuilt, or the rebuild failed.
    Rebuilt(mmc_core::Result<()>),
}

impl Done {
    pub fn map(&self) -> Option<MapId> {
        match self {
            Done::Applied { map, .. } | Done::Ended { map, .. } | Done::Stepped { map, .. } => {
                Some(*map)
            }
            Done::Rebuilt(_) => None,
        }
    }
}

/// The edit worker; dropping it stops the worker after the queued ops.
pub struct Editor {
    ops: Sender<Op>,
    done: Receiver<Done>,
}

impl Editor {
    pub fn new(project: SharedProject, ctx: egui::Context, applied: Applied) -> Editor {
        let (ops, rx) = channel();
        let (tx, done) = channel();
        std::thread::spawn(move || {
            Worker {
                project,
                ops: rx,
                done: tx,
                ctx,
                applied,
                queue: VecDeque::new(),
                stats: HashMap::new(),
                watch: None,
            }
            .run()
        });
        Editor { ops, done }
    }

    pub fn send(&self, op: Op) {
        let _ = self.ops.send(op);
    }

    /// Blocks until the ops sent so far are done and drops their results; for closing a project.
    pub fn sync(&self) {
        let (tx, rx) = channel();
        self.send(Op::Sync(tx));
        let _ = rx.recv();
        self.done();
    }

    /// Results that arrived since the last call; never blocks.
    pub fn done(&self) -> Vec<Done> {
        self.done.try_iter().collect()
    }
}

/// Per-stroke timings, logged at pen up.
#[derive(Default)]
struct Stats {
    parts: u32,
    applies: u32,
    /// Stamps of the stroke in the project.
    stamps: usize,
    lock_wait: Duration,
    lock_wait_max: Duration,
    apply: Duration,
    /// Longest time from a part leaving the UI to its edit being in the project.
    latency_max: Duration,
    /// Largest stamp radius in metres, and the map's metres per pixel.
    radius: f64,
    mpp: f64,
    layer: Option<mmc_core::Layer>,
}

struct Worker {
    project: SharedProject,
    ops: Receiver<Op>,
    done: Sender<Done>,
    ctx: egui::Context,
    applied: Applied,
    queue: VecDeque<Op>,
    stats: HashMap<(MapId, u64), Stats>,
    watch: Option<Instant>,
}

impl Worker {
    fn run(mut self) {
        loop {
            if self.queue.is_empty() {
                let next = match self.watch {
                    Some(_) => match self.ops.recv_timeout(JOBS_POLL) {
                        Ok(op) => Some(op),
                        Err(RecvTimeoutError::Timeout) => None,
                        Err(RecvTimeoutError::Disconnected) => return,
                    },
                    None => match self.ops.recv() {
                        Ok(op) => Some(op),
                        Err(_) => return,
                    },
                };
                self.queue.extend(next);
            }
            self.queue.extend(self.ops.try_iter());
            match self.queue.pop_front() {
                Some(op) => self.handle(op),
                None => self.poll_jobs(),
            }
            self.ctx.request_repaint();
        }
    }

    fn send(&self, d: Done) {
        let _ = self.done.send(d);
    }

    fn handle(&mut self, op: Op) {
        match op {
            Op::Part {
                map,
                stroke,
                part,
                cmd,
                sent,
            } => {
                let (cmd, parts) = self.join_parts(map, stroke, part, cmd);
                let mpp = {
                    let guard = lock_project(&self.project, "edit").0;
                    let Some(p) = guard.as_ref() else { return };
                    p.maps()
                        .iter()
                        .find(|m| m.id == map)
                        .map_or(1.0, |m| m.settings.metres_per_pixel)
                };
                let discs = stamp_discs(&cmd);
                let last = discs.last().map(|d| d.0);
                let s = self.stats.entry((map, stroke)).or_default();
                s.parts += parts.len() as u32;
                s.mpp = mpp;
                s.radius = discs.iter().map(|d| d.1).fold(s.radius, f64::max);
                s.layer = layer(&cmd);
                let mut result = Ok(());
                // A few stamps per lock, so a large brush never holds the project for long.
                for chunk in split(cmd, mpp) {
                    let (mut guard, wait) = lock_project(&self.project, "edit");
                    let t = Instant::now();
                    let Some(p) = guard.as_mut() else { return };
                    let n = stamp_discs(&chunk).len();
                    result = p.apply_part(map, chunk);
                    let s = self.stats.entry((map, stroke)).or_default();
                    if result.is_ok() {
                        s.stamps += n;
                        let mut applied = self.applied.lock().unwrap_or_else(|e| e.into_inner());
                        let level = p.stroke_level(map);
                        let stamps = s.stamps;
                        applied.insert(
                            map,
                            Mark {
                                stroke,
                                stamps,
                                level,
                            },
                        );
                    }
                    drop(guard);
                    s.applies += 1;
                    s.apply += t.elapsed();
                    s.lock_wait += wait;
                    s.lock_wait_max = s.lock_wait_max.max(wait);
                    if result.is_err() {
                        break;
                    }
                }
                let s = self.stats.entry((map, stroke)).or_default();
                s.latency_max = s.latency_max.max(sent.elapsed());
                // The stroke goes on near its end: load what it will reach before it does.
                if let (Some(at), Some(l)) = (last, s.layer) {
                    let guard = lock_project(&self.project, "edit").0;
                    if let Some(p) = guard.as_ref() {
                        let _ = p.prefetch(map, l, at, s.radius * PREFETCH_REACH);
                    }
                }
                self.send(Done::Applied {
                    map,
                    stroke,
                    parts,
                    result,
                });
            }
            Op::End { map, stroke, up } => {
                let (mut guard, _) = lock_project(&self.project, "edit");
                let Some(p) = guard.as_mut() else { return };
                let (mut tiles, mut encode, mut write) = (0, Duration::ZERO, Duration::ZERO);
                let result = match p.take_stroke(map) {
                    // Encoding needs no project, so it runs without the lock.
                    Some(ended) => {
                        drop(guard);
                        tiles = ended.tiles();
                        let t = Instant::now();
                        let encoded = ended.encode();
                        encode = t.elapsed();
                        let mut guard = lock_project(&self.project, "edit").0;
                        let Some(p) = guard.as_mut() else { return };
                        let t = Instant::now();
                        let r = encoded.and_then(|e| p.commit_stroke(e));
                        write = t.elapsed();
                        r
                    }
                    None => p.end_stroke(map),
                };
                let s = self.stats.remove(&(map, stroke)).unwrap_or_default();
                let ms = |d: Duration| d.as_secs_f64() * 1000.0;
                log::info!(
                    "stroke {stroke}: {} radius {:.0} px ({:.0} m), {} parts in {} applies, {tiles} tiles, pen up to applied {:.0} ms (encode {:.0}, write {:.0}), part latency max {:.0} ms, lock wait {:.0} ms (max {:.0}), apply {:.0} ms",
                    s.layer.map_or("-", |l| l.as_str()),
                    s.radius / s.mpp.max(f64::MIN_POSITIVE),
                    s.radius,
                    s.parts,
                    s.applies,
                    ms(up.elapsed()),
                    ms(encode),
                    ms(write),
                    ms(s.latency_max),
                    ms(s.lock_wait),
                    ms(s.lock_wait_max),
                    ms(s.apply)
                );
                if let Err(e) = &result {
                    log::info!("stroke {stroke}: {e}");
                }
                self.watch = Some(Instant::now() + JOBS_WAIT);
                self.send(Done::Ended { map, result });
            }
            Op::Undo(map) | Op::Redo(map) => {
                let forward = matches!(op, Op::Redo(_));
                let (mut guard, wait) = lock_project(&self.project, "edit");
                let t = Instant::now();
                let Some(p) = guard.as_mut() else { return };
                let result = if forward { p.redo(map) } else { p.undo(map) };
                let tiles = p.step_tiles(map, !forward);
                drop(guard);
                let ms = |d: Duration| d.as_secs_f64() * 1000.0;
                log::info!(
                    "{} on map {map:?}: {result:?}, {tiles} tiles, lock wait {:.0} ms, under lock {:.0} ms",
                    if forward { "redo" } else { "undo" },
                    ms(wait),
                    ms(t.elapsed())
                );
                if matches!(result, Ok(true)) {
                    self.watch = Some(Instant::now() + JOBS_WAIT);
                }
                self.send(Done::Stepped { map, result });
            }
            Op::Watch => self.watch = Some(Instant::now() + JOBS_WAIT),
            Op::Prefetch {
                map,
                layer,
                at,
                radius,
            } => {
                // A newer prefetch replaces queued ones.
                if self.queue.iter().any(|o| matches!(o, Op::Prefetch { .. })) {
                    return;
                }
                let guard = lock_project(&self.project, "edit").0;
                if let Some(p) = guard.as_ref() {
                    let _ = p.prefetch(map, layer, at, radius);
                }
            }
            Op::Sync(tx) => {
                let _ = tx.send(());
            }
        }
    }

    /// Joins the queued parts of the same stroke that follow into one command.
    fn join_parts(
        &mut self,
        map: MapId,
        stroke: u64,
        part: u32,
        mut cmd: Command,
    ) -> (Command, Vec<u32>) {
        let mut parts = vec![part];
        while let Some(Op::Part {
            map: m,
            stroke: s,
            part: n,
            cmd: next,
            ..
        }) = self.queue.front()
        {
            if (*m, *s) != (map, stroke) || !joinable(&cmd, next) {
                break;
            }
            parts.push(*n);
            let Some(Op::Part { cmd: next, .. }) = self.queue.pop_front() else {
                unreachable!()
            };
            match (&mut cmd, next) {
                (Command::Brush(a), Command::Brush(b)) => a.points.extend(b.points),
                (Command::Paint(a), Command::Paint(b)) => a.points.extend(b.points),
                _ => unreachable!(),
            }
        }
        (cmd, parts)
    }

    fn poll_jobs(&mut self) {
        let Some(until) = self.watch else { return };
        let (mut guard, _) = lock_project(&self.project, "edit");
        let Some(p) = guard.as_mut() else {
            self.watch = None;
            return;
        };
        let r = p.poll_jobs();
        drop(guard);
        match r {
            Ok(false) if Instant::now() < until => {}
            Ok(false) => self.watch = None,
            Ok(true) => {
                self.watch = None;
                self.send(Done::Rebuilt(Ok(())));
            }
            Err(e) => {
                self.watch = None;
                self.send(Done::Rebuilt(Err(e)));
            }
        }
    }
}

/// The command's stamps in runs whose squared pixel radii add up to at most `LOCK_STAMPS`; one stamp at least.
fn split(cmd: Command, mpp: f64) -> Vec<Command> {
    let runs = |radii: &[f64]| {
        let mut out = vec![];
        let (mut start, mut sum) = (0, 0.0);
        for (i, r) in radii.iter().enumerate() {
            let a = (r / mpp).powi(2);
            if i > start && sum + a > LOCK_STAMPS {
                out.push(start..i);
                (start, sum) = (i, 0.0);
            }
            sum += a;
        }
        out.push(start..radii.len());
        out
    };
    match cmd {
        Command::Brush(b) => {
            let radii: Vec<f64> = b.points.iter().map(|p| p.radius).collect();
            runs(&radii)
                .into_iter()
                .map(|r| {
                    Command::Brush(mmc_core::BrushStroke {
                        points: b.points[r].to_vec(),
                        ..b.clone()
                    })
                })
                .collect()
        }
        Command::Paint(p) => runs(&vec![p.radius; p.points.len()])
            .into_iter()
            .map(|r| {
                Command::Paint(mmc_core::PaintStroke {
                    points: p.points[r].to_vec(),
                    ..p.clone()
                })
            })
            .collect(),
        other => vec![other],
    }
}

fn layer(cmd: &Command) -> Option<mmc_core::Layer> {
    match cmd {
        Command::Brush(b) => Some(b.layer),
        Command::Paint(p) => Some(p.layer),
        _ => None,
    }
}

/// Two parts of a stroke make one command when only their points differ.
fn joinable(a: &Command, b: &Command) -> bool {
    match (a, b) {
        (Command::Brush(a), Command::Brush(b)) => {
            (a.layer, a.strength, a.falloff, a.target, a.mode, a.detail)
                == (b.layer, b.strength, b.falloff, b.target, b.mode, b.detail)
        }
        (Command::Paint(a), Command::Paint(b)) => {
            (a.layer, a.radius, a.value) == (b.layer, b.radius, b.value)
        }
        _ => false,
    }
}
