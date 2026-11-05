//! Breadth-first search over choices.
//!
//! Each frontier state is a VM paused at a SELECT, plus the choices that led
//! there. States merge when they sit at the same command with the same call
//! stacks and the same story flags (#0-#1999, persistent #12000-#12999).
//! Minigame and system variables are left out of the key, so a merge can in
//! principle hide a path; every path reported is still a real sequence of
//! choices, and [`replay`] checks it.
//!
//! Minigame stubs draw random numbers from a generator seeded by the search
//! seed and the choice path, so a path replays the same way every time.
//! Each depth runs on all CPU cores.

use crate::sim::{Recorder, Rng, PERSISTENT};
use luca_vm::{Game, Stop, Vm};
use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::hash::{Hash, Hasher};

/// Instructions one segment (choice to choice) may run before we call it stuck.
const SEGMENT_BUDGET: usize = 2_000_000;

/// Paused VM stored compactly: nonzero variables only.
struct Node {
    scene: usize,
    pc: usize,
    vars: Vec<(u16, i32)>,
    gosub: Vec<(usize, usize)>,
    far: Vec<luca_vm::Frame>,
    pending: Option<u16>,
    path: Vec<u8>,
    options: usize,
}

fn pack(vm: &Vm, path: Vec<u8>, options: usize) -> Node {
    let vars = vm.vars.iter().enumerate().filter(|(_, v)| **v != 0).map(|(i, v)| (i as u16, *v)).collect();
    Node {
        scene: vm.scene,
        pc: vm.pc,
        vars,
        gosub: vm.gosub.clone(),
        far: vm.far.clone(),
        pending: vm.pending_select,
        path,
        options,
    }
}

fn unpack(n: &Node) -> Vm {
    let mut vm = Vm::new(n.scene);
    vm.pc = n.pc;
    for &(i, v) in &n.vars {
        vm.vars[i as usize] = v;
    }
    vm.gosub = n.gosub.clone();
    vm.far = n.far.clone();
    vm.pending_select = n.pending;
    vm
}

fn key(vm: &Vm) -> u64 {
    let mut h = DefaultHasher::new();
    (vm.scene, vm.pc).hash(&mut h);
    vm.gosub.hash(&mut h);
    for f in &vm.far {
        (f.scene, f.pc).hash(&mut h);
    }
    vm.vars[..2000].hash(&mut h);
    vm.vars[PERSISTENT].hash(&mut h);
    h.finish()
}

/// Seed for the segment that follows `path` (FNV-1a over the path).
fn segment_seed(seed: u64, path: &[u8]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64 ^ seed;
    for &b in path {
        h = (h ^ (b as u64 + 1)).wrapping_mul(0x100_0000_01b3);
    }
    h | 1
}

#[derive(Clone)]
pub struct Found {
    pub path: Vec<u8>,
    pub depth: usize,
}

impl Found {
    /// Keep the shortest path, ties broken by the path itself, so results
    /// don't depend on thread timing.
    fn better(&self, other: &Found) -> bool {
        (self.depth, &self.path) < (other.depth, &other.path)
    }
}

#[derive(Default)]
pub struct Report {
    pub trophies: BTreeMap<u16, Found>,
    pub endings: BTreeMap<String, Found>,
    pub scenes: BTreeMap<String, usize>,
    pub levels: Vec<usize>,
    /// Persistent flags left at any ending (max over endings): what a player
    /// who has seen every ending found so far would carry into a new game.
    pub persist: BTreeMap<u16, i32>,
}

fn keep<K: Ord>(map: &mut BTreeMap<K, Found>, k: K, f: Found) {
    match map.entry(k) {
        std::collections::btree_map::Entry::Vacant(e) => {
            e.insert(f);
        }
        std::collections::btree_map::Entry::Occupied(mut e) => {
            if f.better(e.get()) {
                e.insert(f);
            }
        }
    }
}

impl Report {
    fn merge(&mut self, o: Report) {
        for (k, f) in o.trophies {
            keep(&mut self.trophies, k, f);
        }
        for (k, f) in o.endings {
            keep(&mut self.endings, k, f);
        }
        for (k, n) in o.scenes {
            *self.scenes.entry(k).or_default() += n;
        }
        for (k, v) in o.persist {
            let e = self.persist.entry(k).or_insert(v);
            *e = (*e).max(v);
        }
    }
}

fn start_vm(game: &Game, start: &str, persist: &[(u16, i32)]) -> Vm {
    let mut vm = Vm::new(game.scene(start).expect("start scene"));
    for &(v, x) in persist {
        vm.set(v, x);
    }
    vm
}

/// Explore from `start` with persistent flags `persist`.
/// `cap` bounds the frontier per level (sampled down when exceeded).
pub fn bfs(game: &Game, start: &str, persist: &[(u16, i32)], cap: usize, seed: u64) -> Report {
    let mut rep = Report::default();
    let mut rng = Rng(seed | 1);
    let mut frontier = Vec::new();
    advance(game, start_vm(game, start, persist), Vec::new(), seed, &mut rep, &mut frontier);
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
    let t0 = std::time::Instant::now();
    // Visited across all depths: some menus loop back to themselves.
    let mut seen: HashSet<u64> = frontier.iter().map(|(_, k)| *k).collect();
    while !frontier.is_empty() {
        if frontier.len() > cap {
            // keep a random sample (Fisher-Yates prefix)
            for i in 0..cap {
                let j = i + rng.below(frontier.len() - i);
                frontier.swap(i, j);
            }
            frontier.truncate(cap);
        }
        rep.levels.push(frontier.len());
        if rep.levels.len() % 10 == 0 {
            eprintln!(
                "depth {:>4}  frontier {:>6}  trophies {:?}  {:?}",
                rep.levels.len(),
                frontier.len(),
                rep.trophies.keys().collect::<Vec<_>>(),
                t0.elapsed()
            );
        }
        let chunk = frontier.len().div_ceil(threads);
        let results: Vec<(Report, Vec<(Node, u64)>)> = std::thread::scope(|sc| {
            let handles: Vec<_> = frontier
                .chunks(chunk)
                .map(|part| {
                    sc.spawn(move || {
                        let mut local = Report::default();
                        let mut out = Vec::new();
                        for (node, _) in part {
                            let base = unpack(node);
                            for i in 0..node.options {
                                let mut vm = base.clone();
                                vm.choose(i);
                                let mut path = node.path.clone();
                                path.push(i as u8);
                                advance(game, vm, path, seed, &mut local, &mut out);
                            }
                        }
                        (local, out)
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().expect("search thread")).collect()
        });
        let mut next = Vec::new();
        for (local, out) in results {
            rep.merge(local);
            for (n, k) in out {
                if seen.insert(k) {
                    next.push((n, k));
                }
            }
        }
        frontier = next;
    }
    rep
}

/// Run until the next choice or the end; record what happened.
fn advance(game: &Game, mut vm: Vm, path: Vec<u8>, seed: u64, rep: &mut Report, out: &mut Vec<(Node, u64)>) {
    let mut host = Recorder { rng: Some(Rng(segment_seed(seed, &path))), ..Default::default() };
    let stop = vm.run(game, &mut host, SEGMENT_BUDGET);
    let depth = path.len();
    for t in host.trophies.drain(..) {
        keep(&mut rep.trophies, t, Found { path: path.clone(), depth });
    }
    for s in std::mem::take(&mut host.scenes) {
        *rep.scenes.entry(game.name(s).to_string()).or_default() += 1;
    }
    match stop {
        Stop::Select { options, .. } => {
            let k = key(&vm);
            if std::env::var_os("EXPLORE_TRACE").is_some() {
                let op = &game.scenes[vm.scene].script.ops[vm.pc];
                eprintln!("select after {:?} at {} @{}: {} key {k:x}", path, game.name(vm.scene), op.pos, op);
            }
            out.push((pack(&vm, path, options), k))
        }
        Stop::End | Stop::Limit => {
            if stop == Stop::End {
                for v in PERSISTENT {
                    let x = vm.vars[v];
                    if x != 0 {
                        let e = rep.persist.entry(v as u16).or_insert(x);
                        *e = (*e).max(x);
                    }
                }
            }
            let at = format!("{:?} {} @{}", stop, game.name(vm.scene), vm.pc);
            keep(&mut rep.endings, at, Found { path, depth });
        }
    }
}

/// What a replayed path did.
pub struct Replay {
    pub trophies: Vec<u16>,
    pub stop: Stop,
    pub at: String,
    pub scenes: BTreeSet<String>,
    /// Choices left unused when the run ended early (path doesn't fit).
    pub unused: usize,
}

/// Play one path from a fresh start with the same per-segment random
/// numbers the search used.
pub fn replay(game: &Game, start: &str, persist: &[(u16, i32)], seed: u64, path: &[u8]) -> Replay {
    let mut vm = start_vm(game, start, persist);
    let mut trophies = Vec::new();
    let mut scenes = BTreeSet::new();
    let mut i = 0;
    loop {
        let mut host = Recorder { rng: Some(Rng(segment_seed(seed, &path[..i]))), ..Default::default() };
        let stop = vm.run(game, &mut host, SEGMENT_BUDGET);
        trophies.extend(host.trophies);
        scenes.extend(host.scenes.iter().map(|s| game.name(*s).to_string()));
        match stop {
            Stop::Select { options, .. } if i < path.len() && (path[i] as usize) < options => {
                vm.choose(path[i] as usize);
                i += 1;
            }
            stop => {
                let at = format!("{} @{}", game.name(vm.scene), game.scenes[vm.scene].script.ops.get(vm.pc).map_or(0, |o| o.pos));
                return Replay { trophies, stop, at, scenes, unused: path.len() - i };
            }
        }
    }
}
