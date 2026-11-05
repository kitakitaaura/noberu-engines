//! Little Busters! English Edition specifics on top of the LUCA VM:
//! persistent variables, entry scenes, and autoplay for the minigames,
//! which live in the executable (battles, the baseball game, Saya's
//! labyrinth). Autoplay lets the simulator and the first browser player get
//! past them; real minigames come later.

use luca_vm::{Game, Insn, Vm};
use std::collections::BTreeMap;

/// New game.
pub const START: &str = "SEEN0513";
/// Refrain: no script jumps here; the executable starts it.
pub const REFRAIN: &str = "SEEN2513";

/// Whether a SCRIPT.PAK entry is a scene. The `_` entries are data tables and
/// SEEN8500/8501 are battle item tables.
pub fn is_scene(name: &str) -> bool {
    name.starts_with("SEEN") && name != "SEEN8500" && name != "SEEN8501"
}

/// Variables kept between playthroughs (cleared routes, unlocks). Inferred
/// from SEEN0528/SEEN2700 testing #12001..#12024 to gate later routes.
pub const PERSISTENT: std::ops::Range<usize> = 12000..13000;

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

/// Autoplay for an engine-side command. `pick(n)` returns 0..n (random in
/// the simulator). Returns true when a random minigame result was drawn.
pub fn autoplay(game: &Game, vm: &mut Vm, s: usize, pc: usize, pick: &mut dyn FnMut(usize) -> usize) -> bool {
    let op = &game.scenes[s].script.ops[pc];
    let name = op.name();
    let arg = op.params.get(..2).map_or(0, |b| u16::from_le_bytes([b[0], b[1]]));
    let mut pick = |n: usize| pick(n) as i32;
    match name {
        // Battles loop until a fighter's HP (#8188/#8189) reaches 0; the real
        // engine computes damage. Pick a winner.
        "BTFUNC" => {
            let loser = pick(2) as u16;
            vm.set(8188 + loser, 0);
            true
        }
        // Baseball game (SEEN8030): result of each step, values as the script tests them.
        "GMFUNC" => {
            match arg {
                1 => vm.set(6211, pick(4) - 1),
                3 => vm.set(6213, pick(9)),
                4 => vm.set(6215, pick(4)),
                _ => {}
            }
            true
        }
        // Saya's labyrinth (SEEN2804-2808): TASK 45 is one move; the engine
        // reports the tile in #10003/#10004. Deterministic: no dice needed.
        "TASK" if arg == 45 => {
            maze_step(game, vm, s, pc);
            false
        }
        // Saya's game-over dialog "REPLAY / GAME END": always replay.
        "TASK" if arg == 54 => {
            vm.set(6005, 0);
            false
        }
        _ => false,
    }
}

/// Spare variable (never used by the game) holding the labyrinth walk position.
pub const MAZE_STEP: u16 = 65535;
/// Spare variable counting all labyrinth moves this run (never reset).
const MAZE_MOVES: u16 = 65534;
/// Moves before the labyrinth stub gives up.
const MAZE_GIVE_UP: usize = 300;
/// MAZE_STEP value after giving up: the run's End is not a real ending.
pub const MAZE_STUCK: i32 = -1;

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
    let moves = vm.get(MAZE_MOVES) as usize + 1;
    vm.set(MAZE_MOVES, moves as i32);
    if moves > MAZE_GIVE_UP {
        // Unsolved puzzle: jump to the End appended after the scene and mark it.
        vm.pc = game.scenes[s].code.len() - 1;
        vm.set(MAZE_STEP, MAZE_STUCK);
        return;
    }
    // Every event tile three times (some unlock others), then the exit.
    // Days 3-5 (SEEN2806-2808) have puzzles this does not solve yet.
    let tile = if step < events.len() * 3 { Some(events[step % events.len()]) } else { exit };
    if let Some(&(x, y, is_exit)) = tile {
        vm.set(10003, x);
        vm.set(10004, y);
        vm.set(MAZE_STEP, if is_exit { 0 } else { step as i32 + 1 });
    }
}

