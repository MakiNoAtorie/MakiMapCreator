//! Edits on a worker (ADR-0014): stroke parts, undo and redo apply in order, off the UI thread.

use std::collections::{HashMap, VecDeque};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::time::{Duration, Instant};

use eframe::egui;
use mmc_core::{Command, MapId};

use crate::canvas::{SharedProject, lock_project};

/// How often the worker checks for finished rebuilds of derived layers.
const JOBS_POLL: Duration = Duration::from_millis(100);
/// How long the worker waits for derived layers after an edit.
const JOBS_WAIT: Duration = Duration::from_secs(30);

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
            Done::Applied { map, .. } | Done::Ended { map } | Done::Stepped { map, .. } => {
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
    pub fn new(project: SharedProject, ctx: egui::Context) -> Editor {
        let (ops, rx) = channel();
        let (tx, done) = channel();
        std::thread::spawn(move || {
            Worker {
                project,
                ops: rx,
                done: tx,
                ctx,
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
    lock_wait: Duration,
    lock_wait_max: Duration,
    apply: Duration,
    /// Longest time from a part leaving the UI to its edit being in the project.
    latency_max: Duration,
}

struct Worker {
    project: SharedProject,
    ops: Receiver<Op>,
    done: Sender<Done>,
    ctx: egui::Context,
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
                let (mut guard, wait) = lock_project(&self.project, "edit");
                let t = Instant::now();
                let result = match guard.as_mut() {
                    Some(p) => p.apply_part(map, cmd),
                    None => return,
                };
                drop(guard);
                let s = self.stats.entry((map, stroke)).or_default();
                s.parts += parts.len() as u32;
                s.applies += 1;
                s.apply += t.elapsed();
                s.lock_wait += wait;
                s.lock_wait_max = s.lock_wait_max.max(wait);
                s.latency_max = s.latency_max.max(sent.elapsed());
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
                p.end_stroke(map);
                drop(guard);
                let s = self.stats.remove(&(map, stroke)).unwrap_or_default();
                let ms = |d: Duration| d.as_secs_f64() * 1000.0;
                log::info!(
                    "stroke {stroke}: {} parts in {} applies, pen up to applied {:.0} ms, part latency max {:.0} ms, lock wait {:.0} ms (max {:.0}), apply {:.0} ms",
                    s.parts,
                    s.applies,
                    ms(up.elapsed()),
                    ms(s.latency_max),
                    ms(s.lock_wait),
                    ms(s.lock_wait_max),
                    ms(s.apply)
                );
                self.watch = Some(Instant::now() + JOBS_WAIT);
                self.send(Done::Ended { map });
            }
            Op::Undo(map) | Op::Redo(map) => {
                let forward = matches!(op, Op::Redo(_));
                let (mut guard, _) = lock_project(&self.project, "edit");
                let Some(p) = guard.as_mut() else { return };
                let result = if forward { p.redo(map) } else { p.undo(map) };
                drop(guard);
                log::info!(
                    "{} on map {map:?}: {result:?}",
                    if forward { "redo" } else { "undo" }
                );
                if matches!(result, Ok(true)) {
                    self.watch = Some(Instant::now() + JOBS_WAIT);
                }
                self.send(Done::Stepped { map, result });
            }
            Op::Watch => self.watch = Some(Instant::now() + JOBS_WAIT),
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

/// Two parts of a stroke make one command when only their points differ.
fn joinable(a: &Command, b: &Command) -> bool {
    match (a, b) {
        (Command::Brush(a), Command::Brush(b)) => {
            (a.layer, a.strength, a.falloff) == (b.layer, b.strength, b.falloff)
        }
        (Command::Paint(a), Command::Paint(b)) => {
            (a.layer, a.radius, a.value) == (b.layer, b.radius, b.value)
        }
        _ => false,
    }
}
