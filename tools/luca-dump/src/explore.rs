//! Breadth-first search over choices.
//!
//! Each frontier state is a VM paused at a SELECT, plus the choices that led
//! there. States merge when they sit at the same command with the same call
//! stacks and the same story flags (#0-#1999, persistent #12000-#12999).
//! Minigame and system variables are left out of the key, so a merge can in
//! principle hide a path; every path reported is still a real sequence of
//! choices that reaches its goal when replayed.

use crate::sim::{Recorder, Rng, PERSISTENT};
use luca_vm::{Game, Stop, Vm};
use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, HashSet};
use std::hash::{Hash, Hasher};

/// Paused VM stored compactly: nonzero variables only.
struct Node {
    scene: usize,
    pc: usize,
    vars: Vec<(u16, i32)>,
    gosub: Vec<(usize, usize)>,
    far: Vec<luca_vm::Frame>,
    pending: Option<u16>,
    path: Vec<u8>,
}

fn pack(vm: &Vm, path: Vec<u8>) -> Node {
    let vars = vm.vars.iter().enumerate().filter(|(_, v)| **v != 0).map(|(i, v)| (i as u16, *v)).collect();
    Node {
        scene: vm.scene,
        pc: vm.pc,
        vars,
        gosub: vm.gosub.clone(),
        far: vm.far.clone(),
        pending: vm.pending_select,
        path,
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

pub struct Found {
    pub path: Vec<u8>,
    pub depth: usize,
}

pub struct Report {
    pub trophies: BTreeMap<u16, Found>,
    pub endings: BTreeMap<String, Found>,
    pub scenes: BTreeMap<String, usize>,
    pub levels: Vec<usize>,
    /// Persistent flags left at any ending (max over endings): what a player
    /// who has seen every ending found so far would carry into a new game.
    pub persist: BTreeMap<u16, i32>,
}

/// Explore from `start` with persistent flags `persist` (index from #12000).
/// `cap` bounds the frontier per level (sampled down when exceeded).
pub fn bfs(game: &Game, start: &str, persist: &[(u16, i32)], cap: usize, seed: u64) -> Report {
    let mut vm = Vm::new(game.scene(start).expect("start scene"));
    for &(v, x) in persist {
        vm.set(v, x);
    }
    let mut rep = Report {
        trophies: BTreeMap::new(),
        endings: BTreeMap::new(),
        scenes: BTreeMap::new(),
        levels: Vec::new(),
        persist: BTreeMap::new(),
    };
    let mut rng = Rng(seed | 1);
    // Run the start up to the first choice.
    let mut frontier = Vec::new();
    advance(game, vm, Vec::new(), &mut rep, &mut frontier, &mut rng);
    let t0 = std::time::Instant::now();
    // Visited across all depths: some menus loop back to themselves.
    let mut seen = HashSet::new();
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
        let mut next = Vec::new();
        for (node, options, _) in frontier {
            let base = unpack(&node);
            for i in 0..options {
                let mut vm = base.clone();
                vm.choose(i);
                let mut path = node.path.clone();
                path.push(i as u8);
                let mut out = Vec::new();
                advance(game, vm, path, &mut rep, &mut out, &mut rng);
                for (n, opts, k) in out {
                    if seen.insert(k) {
                        next.push((n, opts, k));
                    }
                }
            }
        }
        frontier = next;
    }
    rep
}

/// Run until the next choice or the end; record what happened.
fn advance(
    game: &Game,
    mut vm: Vm,
    path: Vec<u8>,
    rep: &mut Report,
    out: &mut Vec<(Node, usize, u64)>,
    rng: &mut Rng,
) {
    let mut host = Recorder { rng: Some(Rng(rng.next() | 1)), ..Default::default() };
    let stop = vm.run(game, &mut host, 50_000_000);
    let depth = path.len();
    for t in host.trophies.drain(..) {
        rep.trophies.entry(t).or_insert_with(|| Found { path: path.clone(), depth });
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
            out.push((pack(&vm, path), options, k))
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
            rep.endings.entry(at).or_insert(Found { path, depth });
        }
    }
}
