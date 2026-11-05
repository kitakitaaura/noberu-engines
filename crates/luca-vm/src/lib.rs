//! LUCA System script VM.
//!
//! Scripts are compiled once into [`Insn`]s (jump offsets become instruction
//! indexes, expressions are parsed). [`Vm`] holds only mutable state, so
//! searches can clone it cheaply-ish (65,536 variables).
//!
//! The VM runs control flow and variables itself. Everything else (text,
//! images, sound, minigames) goes to a [`Host`]. A headless host gives the
//! flag/jump simulator; the browser player is another host.

pub mod expr;

use expr::{index_var, Expr};
use luca_script::{ArithOp, Kind, Script};
use std::collections::HashMap;

pub const NUM_VARS: usize = 1 << 16;
/// Variables local to a FARCALL frame: saved on FARCALL, restored on
/// FARRETURN. Inferred: battle loops count in #20001 and call scenes that
/// use #20001 for their own RANDOM, and only terminate if the caller's value
/// survives the call.
pub const LOCAL_VARS: std::ops::Range<usize> = 20000..21000;
/// SELECT stores the chosen option (0-based) here in Little Busters! EE.
pub const VAR_SELECT_DEFAULT: u16 = 6001;

#[derive(Debug, Clone)]
pub enum Target {
    Var(u16),
    Indexed { base: u16, index: Expr },
}

#[derive(Debug, Clone)]
pub enum Insn {
    /// Visual/audio/system command with no effect on flow.
    Nop,
    Message,
    Select { var: u16, options: usize },
    If { jump_if_true: bool, cond: Expr, to: usize },
    Goto(usize),
    Gosub(usize),
    Return,
    FarCall { scene: usize, to: usize },
    FarReturn,
    Jump { scene: usize, to: usize },
    OnGoto { sel: Expr, to: Vec<usize> },
    End,
    Assign { target: Target, value: Expr },
    Add { target: Target, value: Expr },
    Arith { op: ArithOp, target: Target, value: Expr },
    Random { var: u16, lo: Expr, hi: Expr },
    FlagClr { from: u16, to: u16 },
    Trophy(u16),
    /// Minigame or engine-side command the host must handle (TASK, BATTLE,
    /// GMFUNC, PTFUNC, LBFUNC, BTFUNC, PT, GM, SET, ...).
    Engine,
}

pub struct Scene {
    pub script: Script,
    pub code: Vec<Insn>,
}

/// All scenes of a game, compiled.
pub struct Game {
    pub scenes: Vec<Scene>,
    pub by_name: HashMap<String, usize>,
}

/// Commands passed to the host as [`Insn::Engine`].
const ENGINE_OPS: &[&str] = &[
    "TASK", "BATTLE", "BTFUNC", "GMFUNC", "GM", "PTFUNC", "PT", "LBFUNC", "SET", "VARSTR", "EQUV", "SETCGFLAG",
    "SETBGMFLAG", "ARFLAGSET", "BT_ACCESSORY_SELECT", "DEL_CALLSTACK", "UNDO_CLEAR", "MOVIE", "EX", "KOEP",
];

#[derive(Debug)]
pub struct CompileError(pub String);

impl Game {
    /// Compile scenes. Entries that are not scripts should be left out by the caller.
    pub fn compile(scripts: Vec<Script>) -> Result<Self, CompileError> {
        let by_name: HashMap<String, usize> =
            scripts.iter().enumerate().map(|(i, s)| (s.name.to_ascii_uppercase(), i)).collect();
        let mut scenes = Vec::with_capacity(scripts.len());
        for script in &scripts {
            let mut code = Vec::with_capacity(script.ops.len());
            for op in &script.ops {
                code.push(compile_op(script, op, &by_name, &scripts).map_err(|e| {
                    CompileError(format!("{} @{} {}: {e}", script.name, op.pos, op.name()))
                })?);
            }
            code.push(Insn::End); // running off the end stops
            scenes.push(code);
        }
        let scenes = scripts.into_iter().zip(scenes).map(|(script, code)| Scene { script, code }).collect();
        Ok(Self { scenes, by_name })
    }

    pub fn scene(&self, name: &str) -> Option<usize> {
        self.by_name.get(&name.to_ascii_uppercase()).copied()
    }

    pub fn name(&self, scene: usize) -> &str {
        &self.scenes[scene].script.name
    }
}

fn compile_op(
    script: &Script,
    op: &luca_script::Op,
    by_name: &HashMap<String, usize>,
    scripts: &[Script],
) -> Result<Insn, String> {
    let local = |pos: u32| script.index_of(pos).ok_or_else(|| format!("jump to @{pos} is not a command"));
    let p = |s: &str| expr::parse(s);
    let target = |var: u16, index: Option<&str>| -> Result<Target, String> {
        Ok(match (luca_script::indexed_base(var), index) {
            (Some(base), Some(i)) => Target::Indexed { base, index: expr::parse(i)? },
            _ => Target::Var(var),
        })
    };
    let far = |file: &str, pos: u32| -> Result<(usize, usize), String> {
        let scene = *by_name.get(&file.to_ascii_uppercase()).ok_or_else(|| format!("no scene {file}"))?;
        let to = scripts[scene].index_of(pos).ok_or_else(|| format!("{file} @{pos} is not a command"))?;
        Ok((scene, to))
    };
    Ok(match &op.kind {
        Kind::Message { .. } | Kind::SayaVoiceText { .. } | Kind::VarStrSet { .. } => Insn::Message,
        Kind::Select { var, en, .. } => Insn::Select { var: *var, options: en.split("$d").count() },
        Kind::If { jump_if_true, expr, target } => {
            Insn::If { jump_if_true: *jump_if_true, cond: p(expr)?, to: local(*target)? }
        }
        Kind::Goto { target } => Insn::Goto(local(*target)?),
        Kind::Gosub { target, .. } => Insn::Gosub(local(*target)?),
        Kind::Return { .. } => Insn::Return,
        Kind::FarCall { file, target, .. } => {
            let (scene, to) = far(file, *target)?;
            Insn::FarCall { scene, to }
        }
        Kind::FarReturn => Insn::FarReturn,
        Kind::Jump { file, target } => {
            let (scene, to) = far(file, target.unwrap_or(0))?;
            Insn::Jump { scene, to }
        }
        Kind::OnGoto { expr, targets } => {
            Insn::OnGoto { sel: p(expr)?, to: targets.iter().map(|t| local(*t)).collect::<Result<_, _>>()? }
        }
        Kind::End => Insn::End,
        Kind::Equ { var, index, expr } => Insn::Assign { target: target(*var, index.as_deref())?, value: p(expr)? },
        Kind::Equn { var, index, value } => Insn::Assign {
            target: target(*var, index.as_deref())?,
            value: Expr::Num(value.unwrap_or(0) as i32),
        },
        Kind::Add { var, index, expr } => Insn::Add { target: target(*var, index.as_deref())?, value: p(expr)? },
        Kind::Arith { op, var, exprs } => {
            let (index, value) = match exprs.as_slice() {
                [i, v] => (Some(i.as_str()), v),
                [v] => (None, v),
                _ => return Err("bad arithmetic layout".into()),
            };
            Insn::Arith { op: *op, target: target(*var, index)?, value: p(value)? }
        }
        Kind::Random { var, lo, hi } => Insn::Random { var: *var, lo: p(lo)?, hi: p(hi)? },
        Kind::FlagClr { from, to, .. } => Insn::FlagClr { from: *from, to: *to },
        Kind::Raw => match op.name() {
            "TROPHY" => Insn::Trophy(u16::from_le_bytes([
                *op.params.first().unwrap_or(&0),
                *op.params.get(1).unwrap_or(&0),
            ])),
            n if ENGINE_OPS.contains(&n) => Insn::Engine,
            _ => Insn::Nop,
        },
    })
}

/// What the VM needs from whoever runs it.
pub trait Host {
    /// A line of text is shown (scene, instruction index).
    fn message(&mut self, _scene: usize, _pc: usize) {}
    fn trophy(&mut self, _id: u16) {}
    /// Engine-side command (minigame etc.). May change variables.
    fn engine(&mut self, _game: &Game, _vm: &mut Vm, _scene: usize, _pc: usize) {}
    /// Random integer in lo..=hi.
    fn random(&mut self, lo: i32, _hi: i32) -> i32 {
        lo
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Stop {
    /// Waiting for a choice: call [`Vm::choose`].
    Select { var: u16, options: usize },
    /// END reached, or FARRETURN with nothing to return to.
    End,
    /// Step budget used up (likely a loop waiting on the engine).
    Limit,
}

#[derive(Clone)]
pub struct Vm {
    pub vars: Vec<i32>,
    pub scene: usize,
    pub pc: usize,
    pub gosub: Vec<(usize, usize)>,
    pub far: Vec<Frame>,
    /// Variable the pending SELECT writes; set while stopped at a choice.
    pub pending_select: Option<u16>,
}

#[derive(Clone)]
pub struct Frame {
    pub scene: usize,
    pub pc: usize,
    pub locals: Box<[i32]>,
}

impl Vm {
    pub fn new(scene: usize) -> Self {
        Self { vars: vec![0; NUM_VARS], scene, pc: 0, gosub: Vec::new(), far: Vec::new(), pending_select: None }
    }

    pub fn get(&self, v: u16) -> i32 {
        self.vars[v as usize]
    }

    pub fn set(&mut self, v: u16, x: i32) {
        self.vars[v as usize] = x;
    }

    fn eval(&self, e: &Expr) -> i32 {
        e.eval(&|v| self.vars[v as usize])
    }

    fn target(&self, t: &Target) -> u16 {
        match t {
            Target::Var(v) => *v,
            Target::Indexed { base, index } => index_var(*base, self.eval(index)),
        }
    }

    /// Answer the pending SELECT with option `i` (0-based) and continue.
    pub fn choose(&mut self, i: usize) {
        if let Some(var) = self.pending_select.take() {
            self.set(var, i as i32);
            self.pc += 1;
        }
    }

    /// Run until a choice, the end, or `budget` instructions.
    pub fn run(&mut self, game: &Game, host: &mut dyn Host, budget: usize) -> Stop {
        for _ in 0..budget {
            let code = &game.scenes[self.scene].code;
            let insn = &code[self.pc];
            let mut next = self.pc + 1;
            match insn {
                Insn::Nop => {}
                Insn::Message => host.message(self.scene, self.pc),
                Insn::Select { var, options } => {
                    self.pending_select = Some(*var);
                    return Stop::Select { var: *var, options: *options };
                }
                Insn::If { jump_if_true, cond, to } => {
                    if (self.eval(cond) != 0) == *jump_if_true {
                        next = *to;
                    }
                }
                Insn::Goto(to) => next = *to,
                Insn::Gosub(to) => {
                    self.gosub.push((self.scene, self.pc + 1));
                    next = *to;
                }
                Insn::Return => match self.gosub.pop() {
                    Some((s, pc)) => {
                        self.scene = s;
                        next = pc;
                    }
                    None => return Stop::End,
                },
                Insn::FarCall { scene, to } => {
                    let locals = self.vars[LOCAL_VARS].into();
                    self.far.push(Frame { scene: self.scene, pc: self.pc + 1, locals });
                    self.scene = *scene;
                    self.pc = *to;
                    continue;
                }
                Insn::FarReturn => match self.far.pop() {
                    Some(f) => {
                        self.scene = f.scene;
                        self.pc = f.pc;
                        self.vars[LOCAL_VARS].copy_from_slice(&f.locals);
                        continue;
                    }
                    None => return Stop::End,
                },
                Insn::Jump { scene, to } => {
                    self.scene = *scene;
                    self.pc = *to;
                    continue;
                }
                Insn::OnGoto { sel, to } => {
                    let i = self.eval(sel);
                    if let Some(t) = usize::try_from(i).ok().and_then(|i| to.get(i)) {
                        next = *t;
                    }
                }
                Insn::End => return Stop::End,
                Insn::Assign { target, value } => {
                    let (v, x) = (self.target(target), self.eval(value));
                    self.set(v, x);
                }
                Insn::Add { target, value } => {
                    let v = self.target(target);
                    let x = self.get(v).wrapping_add(self.eval(value));
                    self.set(v, x);
                }
                Insn::Arith { op, target, value } => {
                    let v = self.target(target);
                    let (a, b) = (self.get(v), self.eval(value));
                    let x = match op {
                        ArithOp::Sub => a.wrapping_sub(b),
                        ArithOp::Mul => a.wrapping_mul(b),
                        ArithOp::Div => a.checked_div(b).unwrap_or(0),
                        ArithOp::Mod => a.checked_rem(b).unwrap_or(0),
                        ArithOp::And => a & b,
                        ArithOp::Or => a | b,
                    };
                    self.set(v, x);
                }
                Insn::Random { var, lo, hi } => {
                    let (lo, hi) = (self.eval(lo), self.eval(hi));
                    let x = host.random(lo, hi.max(lo));
                    self.set(*var, x);
                }
                Insn::FlagClr { from, to } => {
                    for v in *from..=(*to).max(*from) {
                        self.set(v, 0);
                    }
                }
                Insn::Trophy(id) => host.trophy(*id),
                Insn::Engine => {
                    let (s, pc) = (self.scene, self.pc);
                    host.engine(game, self, s, pc);
                }
            }
            self.pc = next;
        }
        Stop::Limit
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use luca_script::LB_EN;

    fn rec(name: &str, body: &[u8]) -> Vec<u8> {
        let op = LB_EN.iter().position(|&n| n == name).unwrap() as u8;
        let len = 4 + body.len();
        let mut v = (len as u16).to_le_bytes().to_vec();
        v.push(op);
        v.push(0);
        v.extend_from_slice(body);
        if len % 2 == 1 {
            v.push(0);
        }
        v
    }

    fn equn(var: u16, val: u16) -> Vec<u8> {
        rec("EQUN", &[var.to_le_bytes(), val.to_le_bytes()].concat())
    }

    fn game(scenes: Vec<(&str, Vec<Vec<u8>>)>) -> Game {
        let scripts = scenes
            .into_iter()
            .map(|(n, recs)| Script::parse(n, &recs.concat()).unwrap())
            .collect();
        Game::compile(scripts).unwrap()
    }

    struct Quiet;
    impl Host for Quiet {}

    #[test]
    fn farcall_keeps_caller_locals() {
        let mut call = vec![0, 0];
        call.extend_from_slice(b"SUB\0");
        call.extend_from_slice(&0u32.to_le_bytes());
        let g = game(vec![
            ("MAIN", vec![equn(20001, 7), rec("FARCALL", &call), equn(1, 1), rec("END", &[])]),
            ("SUB", vec![equn(20001, 99), equn(2, 5), rec("FARRETURN", &[])]),
        ]);
        let mut vm = Vm::new(0);
        assert_eq!(vm.run(&g, &mut Quiet, 100), Stop::End);
        assert_eq!(vm.get(20001), 7, "local restored after FARRETURN");
        assert_eq!(vm.get(2), 5, "globals written by the callee stay");
        assert_eq!(vm.get(1), 1);
    }

    #[test]
    fn select_then_branch() {
        let mut sel = Vec::new();
        for x in [6001u16, 0, 0, 0] {
            sel.extend_from_slice(&x.to_le_bytes());
        }
        for s in ["a$db", "A$dB"] {
            for u in s.encode_utf16().chain([0]) {
                sel.extend_from_slice(&u.to_le_bytes());
            }
        }
        // SELECT is 4+8+10+10 = 32 bytes at 0; IFN at 32 is 4+11+4 = 19 (+1 pad);
        // EQUN at 52, END at 60, EQUN at 64, END at 72.
        let mut ifn = b"(#6001==0)\0".to_vec();
        ifn.extend_from_slice(&64u32.to_le_bytes());
        let g = game(vec![(
            "MAIN",
            vec![rec("SELECT", &sel), rec("IFN", &ifn), equn(1, 10), rec("END", &[]), equn(1, 20), rec("END", &[])],
        )]);
        for (pick, want) in [(0, 10), (1, 20)] {
            let mut vm = Vm::new(0);
            assert_eq!(vm.run(&g, &mut Quiet, 100), Stop::Select { var: 6001, options: 2 });
            // A paused VM copied field by field (as the search stores it) still takes the choice.
            let mut copy = Vm::new(vm.scene);
            copy.vars = vm.vars.clone();
            copy.pc = vm.pc;
            copy.pending_select = vm.pending_select;
            copy.choose(pick);
            assert_eq!(copy.run(&g, &mut Quiet, 100), Stop::End);
            assert_eq!(copy.get(1), want);
        }
    }
}
