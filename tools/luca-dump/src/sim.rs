//! Flag/jump simulator runs: random playouts over the compiled game.

pub use luca_lbee::{Rng, MAZE_STEP, MAZE_STUCK, PERSISTENT};
use luca_vm::{Game, Host, Insn, Stop, Vm};
use std::collections::{BTreeMap, BTreeSet};

pub fn load_game(pak_path: &str) -> Result<Game, Box<dyn std::error::Error>> {
    let pak = luca_pak::Pak::open(vfs::NativeFile::open(pak_path)?)?;
    let mut scripts = Vec::new();
    for e in pak.entries() {
        let name = e.name.clone().unwrap_or_default();
        if !luca_lbee::is_scene(&name) {
            continue;
        }
        scripts.push(luca_script::Script::parse(&name, &pak.read(e)?)?);
    }
    Ok(Game::compile(scripts).map_err(|e| e.0)?)
}

#[derive(Default)]
pub struct Recorder {
    pub trophies: Vec<u16>,
    pub engine_ops: BTreeMap<String, usize>,
    pub messages: usize,
    pub scenes: BTreeSet<usize>,
    pub rng: Option<Rng>,
    /// A minigame stub drew random results in this run.
    pub minigame: bool,
}

impl Host for Recorder {
    fn message(&mut self, _game: &Game, _vm: &Vm, s: usize, _pc: usize) -> bool {
        self.messages += 1;
        self.scenes.insert(s);
        false
    }
    fn trophy(&mut self, id: u16) {
        self.trophies.push(id);
    }
    fn engine(&mut self, game: &Game, vm: &mut Vm, s: usize, pc: usize) {
        let name = game.scenes[s].script.ops[pc].name();
        *self.engine_ops.entry(name.to_string()).or_default() += 1;
        let rng = &mut self.rng;
        let mut pick = |n: usize| match rng {
            Some(r) => r.below(n),
            None => 0,
        };
        if luca_lbee::autoplay(game, vm, s, pc, &mut pick) {
            self.minigame = true;
        }
    }
    fn random(&mut self, lo: i32, hi: i32) -> i32 {
        match &mut self.rng {
            Some(r) => lo + r.below((hi - lo + 1) as usize) as i32,
            None => lo,
        }
    }
}

/// Variables that conditions read but no script command ever writes:
/// the engine (minigames, system) must set them.
pub fn engine_vars(game: &Game) -> BTreeMap<u16, usize> {
    let mut written = BTreeSet::new();
    let mut read = BTreeMap::<u16, usize>::new();
    for sc in &game.scenes {
        for i in &sc.code {
            let mut r = Vec::new();
            match i {
                Insn::Assign { target, value } | Insn::Add { target, value } => {
                    if let luca_vm::Target::Var(v) = target {
                        written.insert(*v);
                    }
                    value.vars(&mut r);
                }
                Insn::Random { var, .. } | Insn::Select { var, .. } => {
                    written.insert(*var);
                }
                Insn::Arith { target: luca_vm::Target::Var(v), .. } => {
                    written.insert(*v);
                }
                Insn::FlagClr { from, to } => written.extend(*from..=*to),
                Insn::If { cond, .. } => cond.vars(&mut r),
                Insn::OnGoto { sel, .. } => sel.vars(&mut r),
                _ => {}
            }
            for v in r {
                *read.entry(v).or_default() += 1;
            }
        }
    }
    read.retain(|v, _| !written.contains(v));
    read
}

pub fn playouts(game: &Game, start: &str, runs: usize, seed: u64) {
    let start = game.scene(start).expect("start scene");
    let mut rng = Rng(seed | 1);
    let mut trophies = BTreeMap::<u16, usize>::new();
    let mut stops = BTreeMap::<String, usize>::new();
    let mut engine_ops = BTreeMap::<String, usize>::new();
    let mut scenes_seen = BTreeSet::new();
    let mut diagnosed = BTreeSet::new();
    let mut saved = vec![0i32; PERSISTENT.len()];
    let mut player_runs = 0;
    for _ in 0..runs {
        // A "player" plays up to 15 times in a row, keeping persistent flags.
        if player_runs == 15 {
            saved.fill(0);
            player_runs = 0;
        }
        player_runs += 1;
        let mut vm = Vm::new(start);
        vm.vars[PERSISTENT].copy_from_slice(&saved);
        let mut host = Recorder { rng: Some(Rng(rng.next() | 1)), ..Default::default() };
        let mut selects = 0;
        let stop = loop {
            scenes_seen.insert(vm.scene);
            match vm.run(game, &mut host, 50_000_000) {
                Stop::Select { options, .. } => {
                    selects += 1;
                    vm.choose(rng.below(options));
                }
                s => break s,
            }
        };
        if stop == Stop::Limit && diagnosed.insert(game.name(vm.scene).to_string()) && diagnosed.len() <= 4 {
            let mut seen = BTreeSet::new();
            for _ in 0..400 {
                seen.insert((vm.scene, vm.pc));
                if vm.run(game, &mut host, 1) != Stop::Limit {
                    break;
                }
            }
            println!("-- loop in {}:", game.name(vm.scene));
            for (sc, pc) in seen {
                let op = &game.scenes[sc].script.ops[pc];
                let s = format!("{op}");
                println!("   {} {:>6}: {}", game.name(sc), op.pos, s.chars().take(110).collect::<String>());
            }
        }
        saved.copy_from_slice(&vm.vars[PERSISTENT]);
        let at = format!("{:?} in {} @{}", stop, game.name(vm.scene), vm.pc);
        *stops.entry(at).or_default() += 1;
        for t in host.trophies {
            *trophies.entry(t).or_default() += 1;
        }
        for (k, v) in host.engine_ops {
            *engine_ops.entry(k).or_default() += v;
        }
        let _ = selects;
    }
    println!("{runs} random playouts from {}", game.name(start));
    println!("trophies (id: runs): {:?}", trophies);
    println!("scenes entered: {} of {}", scenes_seen.len(), game.scenes.len());
    println!("stopped at:");
    for (k, v) in &stops {
        println!("  {v:>6}  {k}");
    }
    println!("engine commands met: {engine_ops:?}");
}

/// Run one scene directly (choices: always the first option) and show where
/// it stops; when stuck, print the commands of the loop.
pub fn probe(game: &Game, scene: &str, flags: &[(u16, i32)], seed: u64) {
    let mut vm = Vm::new(game.scene(scene).expect("scene"));
    for &(v, x) in flags {
        vm.set(v, x);
    }
    let mut host = Recorder { rng: Some(Rng(seed | 1)), ..Default::default() };
    let t = std::time::Instant::now();
    let mut choices = 0;
    let stop = loop {
        match vm.run(game, &mut host, 20_000_000) {
            Stop::Select { .. } => {
                choices += 1;
                vm.choose(0);
            }
            s => break s,
        }
    };
    println!("{stop:?} in {} @{} after {choices} choices, {:?}", game.name(vm.scene), vm.pc, t.elapsed());
    println!("engine commands: {:?}", host.engine_ops);
    println!("maze step #{MAZE_STEP} = {}, tile ({}, {})", vm.get(MAZE_STEP), vm.get(10003), vm.get(10004));
    if stop == Stop::Limit {
        let mut seen = BTreeSet::new();
        for _ in 0..2000 {
            seen.insert((vm.scene, vm.pc));
            if vm.run(game, &mut host, 1) != Stop::Limit {
                break;
            }
        }
        for (sc, pc) in seen {
            let op = &game.scenes[sc].script.ops[pc];
            println!("   {} {:>6}: {}", game.name(sc), op.pos, format!("{op}").chars().take(120).collect::<String>());
        }
    }
}
