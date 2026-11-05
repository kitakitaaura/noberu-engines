//! Flag/jump simulator runs: random playouts over the compiled game.

use luca_vm::{Game, Host, Insn, Stop, Vm};
use std::collections::{BTreeMap, BTreeSet};

pub fn load_game(pak_path: &str) -> Result<Game, Box<dyn std::error::Error>> {
    let pak = luca_pak::Pak::open(vfs::NativeFile::open(pak_path)?)?;
    let mut scripts = Vec::new();
    for e in pak.entries() {
        let name = e.name.clone().unwrap_or_default();
        // Data tables (`_VARNUM`...) and the two battle item tables are not scripts.
        if !name.starts_with("SEEN") || name == "SEEN8500" || name == "SEEN8501" {
            continue;
        }
        scripts.push(luca_script::Script::parse(&name, &pak.read(e)?)?);
    }
    Ok(Game::compile(scripts).map_err(|e| e.0)?)
}

/// xorshift64*, enough for playouts.
pub struct Rng(pub u64);
impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
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
    fn message(&mut self, s: usize, _pc: usize) {
        self.messages += 1;
        self.scenes.insert(s);
    }
    fn trophy(&mut self, id: u16) {
        self.trophies.push(id);
    }
    fn engine(&mut self, game: &Game, vm: &mut Vm, s: usize, pc: usize) {
        let name = game.scenes[s].script.ops[pc].name();
        *self.engine_ops.entry(name.to_string()).or_default() += 1;
        // Minigame stubs. Battles loop until a fighter's HP (#8188/#8189)
        // reaches 0; the real engine computes damage. Pick a winner at random.
        let minigame = &mut self.minigame;
        let mut pick = |n: usize| match &mut self.rng {
            Some(r) => r.below(n) as i32,
            None => 0,
        };
        let op = &game.scenes[s].script.ops[pc];
        let arg = op.params.get(..2).map_or(0, |b| u16::from_le_bytes([b[0], b[1]]));
        match name {
            "BTFUNC" => {
                *minigame = true;
                let loser = pick(2) as u16;
                vm.set(8188 + loser, 0);
            }
            // Baseball game (SEEN8030): result of each step, values as the script tests them.
            "GMFUNC" => {
                *minigame = true;
                match arg {
                    1 => vm.set(6211, pick(4) - 1),
                    3 => vm.set(6213, pick(9)),
                    4 => vm.set(6215, pick(4)),
                    _ => {}
                }
            }
            // Saya's labyrinth (SEEN2804-2808): TASK 45 is one move; the
            // engine reports the tile in #10003/#10004.
            "TASK" if arg == 45 => {
                *minigame = true;
                maze_step(game, vm, s, pc)
            }
            _ => {}
        }
    }
    fn random(&mut self, lo: i32, hi: i32) -> i32 {
        match &mut self.rng {
            Some(r) => lo + r.below((hi - lo + 1) as usize) as i32,
            None => lo,
        }
    }
}

/// Spare variable (never used by the game) holding the labyrinth walk position.
const MAZE_STEP: u16 = 65535;

/// Tiles the labyrinth loop after `pc` tests, as (x, y, is_exit). A tile is
/// the exit when its block never jumps back to the move.
fn maze_tiles(game: &Game, s: usize, pc: usize) -> Vec<(i32, i32, bool)> {
    let script = &game.scenes[s].script;
    let code = &game.scenes[s].code;
    let mut tiles = Vec::new();
    for i in pc + 1..script.ops.len() {
        let op = &script.ops[i];
        if op.name() == "TASK" && op.params.starts_with(&[45, 0]) {
            break;
        }
        let luca_script::Kind::If { expr, .. } = &op.kind else { continue };
        let Insn::If { to, .. } = &code[i] else { continue };
        let loops_back = (i..*to).any(|j| matches!(code[j], Insn::Goto(t) if t <= pc));
        // Every "#10003==X)&&(#10004==Y" pair in the condition (some test several tiles).
        for part in expr.split("#10003==").skip(1) {
            let Some((x, rest)) = part.split_once(")&&(#10004==") else { continue };
            let y: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            let (Ok(x), Ok(y)) = (x.parse::<i32>(), y.parse::<i32>()) else { continue };
            if !tiles.iter().any(|t: &(i32, i32, bool)| t.0 == x && t.1 == y) {
                tiles.push((x, y, !loops_back));
            }
        }
    }
    tiles
}

type Tiles = std::sync::Arc<Vec<(i32, i32, bool)>>;

/// Tiles per move command, computed once (scanning the scene each move made
/// stuck labyrinth runs very slow).
fn maze_tiles_cached(game: &Game, s: usize, pc: usize) -> Tiles {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<BTreeMap<(usize, usize), Tiles>>> = std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(t) = cache.lock().unwrap().get(&(s, pc)) {
        return t.clone();
    }
    let t: Tiles = std::sync::Arc::new(maze_tiles(game, s, pc));
    cache.lock().unwrap().insert((s, pc), t.clone());
    t
}

fn maze_step(game: &Game, vm: &mut Vm, s: usize, pc: usize) {
    let tiles = maze_tiles_cached(game, s, pc);
    let events: Vec<_> = tiles.iter().filter(|t| !t.2).collect();
    let exit = tiles.iter().find(|t| t.2);
    let step = vm.get(MAZE_STEP) as usize;
    // Every event tile three times (some unlock others), then the exit.
    // Days 3-5 (SEEN2806-2808) have puzzles this does not solve yet.
    let tile = if step < events.len() * 3 { Some(events[step % events.len()]) } else { exit };
    if let Some(&(x, y, is_exit)) = tile {
        vm.set(10003, x);
        vm.set(10004, y);
        vm.set(MAZE_STEP, if is_exit { 0 } else { step as i32 + 1 });
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

/// Variables kept between playthroughs (cleared routes, unlocks). Inferred
/// from SEEN0528/SEEN2700 testing #12001..#12024 to gate later routes.
pub const PERSISTENT: std::ops::Range<usize> = 12000..13000;

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
