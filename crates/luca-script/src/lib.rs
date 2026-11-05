//! LUCA System scripts (one file per scene in SCRIPT.PAK).
//!
//! A script is a run of records until end of file:
//!
//! ```text
//! u16 len   (bytes, including these 4 header bytes)
//! u8  opcode
//! u8  fixed_flag   0: none, 1: one u16, >=2: two u16 before the params
//! ..  len-4 bytes of parameters
//! ..  one pad byte when len is odd (records are 2-aligned)
//! ```
//!
//! Jump targets are byte offsets into the scene file. Text strings are
//! UTF-16LE, expressions and file names are Shift-JIS, both NUL-terminated.
//! Parameter layouts follow LuckSystem (MIT) and lbee-utils (GPLv3) for
//! Little Busters! English Edition; commands with no known layout keep raw bytes.

mod opcodes_lb_en;

use std::fmt;
use thiserror::Error;

pub use opcodes_lb_en::LB_EN;

#[derive(Error, Debug)]
pub enum ScriptError {
    #[error("{0}: record at {1:#x} runs past end of file")]
    Truncated(String, usize),
    #[error("{0}: bad record length {2} at {1:#x}")]
    BadLength(String, usize, usize),
}

/// One decoded command.
#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    /// A line of dialogue or narration.
    Message { voice: u16, jp: String, en: String, tail: Vec<u8> },
    /// Choice menu; options are `$d`-separated. The result goes to variable `var`.
    Select { var: u16, pre: [u16; 3], jp: String, en: String, post: Vec<u16> },
    /// IFY jumps when `expr` is true, IFN when it is false.
    If { jump_if_true: bool, expr: String, target: u32 },
    Goto { target: u32 },
    Gosub { arg: u16, target: u32, tail: Vec<u8> },
    Return { tail: Vec<u8> },
    /// Call into another scene; returns with FARRETURN.
    FarCall { index: u16, file: String, target: u32, tail: Vec<u8> },
    FarReturn,
    /// Go to another scene (start, or a position in it). No return.
    Jump { file: String, target: Option<u32> },
    End,
    /// `#var = expr`. Indexed writes (`var` top bit set, see [`indexed_base`])
    /// carry an `index` expression; the target is `#(base + index)`.
    Equ { var: u16, index: Option<String>, expr: String },
    /// `#var = value` (value missing: 0)
    Equn { var: u16, index: Option<String>, value: Option<u16> },
    /// `#var += expr`
    Add { var: u16, index: Option<String>, expr: String },
    /// `#var -= expr`, `#var |= expr`, ... When `var` has its top bit set the
    /// target is indexed: `exprs` is [index, value] and the variable is
    /// `(var & 0x3fff) + index` (inferred from use in the minigame scenes).
    Arith { op: ArithOp, var: u16, exprs: Vec<String> },
    /// Computed jump: `targets[expr]`.
    OnGoto { expr: String, targets: Vec<u32> },
    /// `#var = random(lo..=hi)`
    Random { var: u16, lo: String, hi: String },
    /// Clear variables `from..=to` (third value seen: 65535).
    FlagClr { from: u16, to: u16, rest: Vec<u16> },
    VarStrSet { id: u16, text: String },
    SayaVoiceText { voice: u16, jp: String, en: String },
    /// Command with no decoded layout yet.
    Raw,
}

/// Variable writes with the top bit set are indexed: the base variable is
/// the low 14 bits and an index expression follows (e.g. `0xDFFC` is the HP
/// array `#8188[...]` in battles).
pub fn indexed_base(var: u16) -> Option<u16> {
    (var & 0x8000 != 0).then_some(var & 0x3fff)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArithOp {
    Sub,
    Mul,
    Div,
    Mod,
    And,
    Or,
}

#[derive(Debug, Clone)]
pub struct Op {
    /// Byte offset of this record in the scene file (jump targets point here).
    pub pos: u32,
    pub opcode: u8,
    pub fixed: Vec<u16>,
    /// Parameter bytes after the fixed u16s.
    pub params: Vec<u8>,
    pub kind: Kind,
}

impl Op {
    pub fn name(&self) -> &'static str {
        LB_EN.get(self.opcode as usize).copied().unwrap_or("?")
    }

    /// Jump targets inside the same scene, if this command can branch.
    pub fn local_targets(&self) -> Vec<u32> {
        match &self.kind {
            Kind::If { target, .. } | Kind::Goto { target } | Kind::Gosub { target, .. } => vec![*target],
            Kind::OnGoto { targets, .. } => targets.clone(),
            _ => vec![],
        }
    }
}

pub struct Script {
    pub name: String,
    pub ops: Vec<Op>,
}

impl Script {
    pub fn parse(name: &str, data: &[u8]) -> Result<Self, ScriptError> {
        let mut ops = Vec::new();
        let mut pos = 0usize;
        while pos + 4 <= data.len() {
            let len = u16::from_le_bytes([data[pos], data[pos + 1]]) as usize;
            let opcode = data[pos + 2];
            let flag = data[pos + 3];
            if len < 4 {
                return Err(ScriptError::BadLength(name.into(), pos, len));
            }
            if pos + len > data.len() {
                return Err(ScriptError::Truncated(name.into(), pos));
            }
            let raw = &data[pos + 4..pos + len];
            let nfixed = match flag {
                0 => 0,
                1 => 1,
                _ => 2,
            };
            let nfixed = nfixed.min(raw.len() / 2);
            let fixed = (0..nfixed).map(|i| u16::from_le_bytes([raw[2 * i], raw[2 * i + 1]])).collect();
            let params = raw[2 * nfixed..].to_vec();
            let name_str = LB_EN.get(opcode as usize).copied().unwrap_or("?");
            let kind = decode(name_str, &params).unwrap_or(Kind::Raw);
            ops.push(Op { pos: pos as u32, opcode, fixed, params, kind });
            pos += (len + 1) & !1;
        }
        Ok(Self { name: name.into(), ops })
    }

    /// Index of the command at byte offset `pos`.
    pub fn index_of(&self, pos: u32) -> Option<usize> {
        self.ops.binary_search_by_key(&pos, |o| o.pos).ok()
    }
}

struct Rd<'a> {
    b: &'a [u8],
    p: usize,
}

impl Rd<'_> {
    fn u16(&mut self) -> Option<u16> {
        let v = self.b.get(self.p..self.p + 2)?;
        self.p += 2;
        Some(u16::from_le_bytes([v[0], v[1]]))
    }
    fn u32(&mut self) -> Option<u32> {
        let v = self.b.get(self.p..self.p + 4)?;
        self.p += 4;
        Some(u32::from_le_bytes([v[0], v[1], v[2], v[3]]))
    }
    fn more(&self) -> bool {
        self.p < self.b.len()
    }
    fn rest(&mut self) -> Vec<u8> {
        let r = self.b[self.p.min(self.b.len())..].to_vec();
        self.p = self.b.len();
        r
    }
    fn rest_u16(&mut self) -> Vec<u16> {
        let mut v = Vec::new();
        while let Some(x) = self.u16() {
            v.push(x);
        }
        v
    }
    /// NUL-terminated UTF-16LE.
    fn wstr(&mut self) -> Option<String> {
        let mut units = Vec::new();
        loop {
            let u = self.u16()?;
            if u == 0 {
                break;
            }
            units.push(u);
        }
        Some(String::from_utf16_lossy(&units))
    }
    /// NUL-terminated Shift-JIS.
    fn sjis(&mut self) -> Option<String> {
        let rest = self.b.get(self.p..)?;
        let n = rest.iter().position(|&c| c == 0)?;
        self.p += n + 1;
        Some(decode_sjis(&rest[..n]))
    }
}

pub fn decode_sjis(b: &[u8]) -> String {
    if b.is_ascii() {
        return String::from_utf8_lossy(b).into_owned();
    }
    encoding_rs::SHIFT_JIS.decode_without_bom_handling(b).0.into_owned()
}

fn decode(name: &str, params: &[u8]) -> Option<Kind> {
    let mut r = Rd { b: params, p: 0 };
    let k = match name {
        "MESSAGE" => Kind::Message { voice: r.u16()?, jp: r.wstr()?, en: r.wstr()?, tail: r.rest() },
        "SELECT" => Kind::Select {
            var: r.u16()?,
            pre: [r.u16()?, r.u16()?, r.u16()?],
            jp: r.wstr()?,
            en: r.wstr()?,
            post: r.rest_u16(),
        },
        "IFY" | "IFN" => Kind::If { jump_if_true: name == "IFY", expr: r.sjis()?, target: r.u32()? },
        "GOTO" => Kind::Goto { target: r.u32()? },
        "GOSUB" => Kind::Gosub { arg: r.u16()?, target: r.u32()?, tail: r.rest() },
        "RETURN" => Kind::Return { tail: r.rest() },
        "FARCALL" => Kind::FarCall { index: r.u16()?, file: r.sjis()?, target: r.u32()?, tail: r.rest() },
        "FARRETURN" => Kind::FarReturn,
        "JUMP" => {
            let file = r.sjis()?;
            let target = if r.more() { Some(r.u32()?) } else { None };
            Kind::Jump { file, target }
        }
        "END" => Kind::End,
        "EQU" | "ADD" => {
            let var = r.u16()?;
            let index = if indexed_base(var).is_some() { Some(r.sjis()?) } else { None };
            let expr = r.sjis()?;
            if name == "EQU" { Kind::Equ { var, index, expr } } else { Kind::Add { var, index, expr } }
        }
        "EQUN" => {
            let var = r.u16()?;
            let index = if indexed_base(var).is_some() { Some(r.sjis()?) } else { None };
            Kind::Equn { var, index, value: if r.more() { Some(r.u16()?) } else { None } }
        }
        "SUB" | "MUL" | "DIV" | "MOD" | "AND" | "OR" => {
            let op = match name {
                "SUB" => ArithOp::Sub,
                "MUL" => ArithOp::Mul,
                "DIV" => ArithOp::Div,
                "MOD" => ArithOp::Mod,
                "AND" => ArithOp::And,
                _ => ArithOp::Or,
            };
            let var = r.u16()?;
            let n = if indexed_base(var).is_some() { 2 } else { 1 };
            let exprs = (0..n).map(|_| r.sjis()).collect::<Option<Vec<_>>>()?;
            if r.more() {
                return None;
            }
            Kind::Arith { op, var, exprs }
        }
        "ONGOTO" => {
            let expr = r.sjis()?;
            let mut targets = Vec::new();
            while r.more() {
                targets.push(r.u32()?);
            }
            Kind::OnGoto { expr, targets }
        }
        "RANDOM" => Kind::Random { var: r.u16()?, lo: r.sjis()?, hi: r.sjis()? },
        "FLAGCLR" => Kind::FlagClr { from: r.u16()?, to: r.u16()?, rest: r.rest_u16() },
        "VARSTR_SET" => Kind::VarStrSet { id: r.u16()?, text: r.wstr()? },
        "SAYAVOICETEXT" => Kind::SayaVoiceText { voice: r.u16()?, jp: r.wstr()?, en: r.wstr()? },
        _ => return None,
    };
    Some(k)
}

impl fmt::Display for Op {
    /// One line per command, for dumps and diffs. Jump targets print as byte offsets.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name())?;
        match &self.kind {
            Kind::Message { voice, jp, en, .. } => write!(f, " ({voice}, {jp:?}, {en:?})"),
            Kind::Select { var, jp, en, .. } => write!(f, " ({var}, {jp:?}, {en:?})"),
            Kind::If { expr, target, .. } => write!(f, " ({expr:?}, @{target})"),
            Kind::Goto { target } => write!(f, " (@{target})"),
            Kind::Gosub { arg, target, .. } => write!(f, " ({arg}, @{target})"),
            Kind::FarCall { index, file, target, .. } => write!(f, " ({index}, {file:?}, @{target})"),
            Kind::Jump { file, target: Some(t) } => write!(f, " ({file:?}, @{t})"),
            Kind::Jump { file, target: None } => write!(f, " ({file:?})"),
            Kind::Equ { var, index: None, expr } | Kind::Add { var, index: None, expr } => {
                write!(f, " ({var}, {expr:?})")
            }
            Kind::Equ { var, index: Some(i), expr } | Kind::Add { var, index: Some(i), expr } => {
                write!(f, " ({var}, [{i:?}], {expr:?})")
            }
            Kind::Equn { var, index, value } => {
                write!(f, " ({var}")?;
                if let Some(i) = index {
                    write!(f, ", [{i:?}]")?;
                }
                if let Some(v) = value {
                    write!(f, ", {v}")?;
                }
                write!(f, ")")
            }
            Kind::Random { var, lo, hi } => write!(f, " ({var}, {lo:?}, {hi:?})"),
            Kind::Arith { var, exprs, .. } => write!(f, " ({var}, {exprs:?})"),
            Kind::OnGoto { expr, targets } => {
                write!(f, " ({expr:?}")?;
                for t in targets {
                    write!(f, ", @{t}")?;
                }
                write!(f, ")")
            }
            Kind::VarStrSet { id, text } => write!(f, " ({id}, {text:?})"),
            Kind::SayaVoiceText { voice, jp, en } => write!(f, " ({voice}, {jp:?}, {en:?})"),
            _ => {
                write!(f, " [")?;
                for (i, x) in self.fixed.iter().enumerate() {
                    write!(f, "{}{x}", if i > 0 { " " } else { "" })?;
                }
                write!(f, "|")?;
                for b in &self.params {
                    write!(f, "{b:02x}")?;
                }
                write!(f, "]")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(op: u8, flag: u8, body: &[u8]) -> Vec<u8> {
        let len = 4 + body.len();
        let mut v = (len as u16).to_le_bytes().to_vec();
        v.push(op);
        v.push(flag);
        v.extend_from_slice(body);
        if len % 2 == 1 {
            v.push(0);
        }
        v
    }

    #[test]
    fn parses_goto_and_padding() {
        let goto = LB_EN.iter().position(|&n| n == "GOTO").unwrap() as u8;
        let ifn = LB_EN.iter().position(|&n| n == "IFN").unwrap() as u8;
        let mut data = rec(ifn, 0, b"#1==0\0\x0c\x00\x00\x00"); // 4+10 = 14
        data.extend(rec(goto, 0, &[0, 0, 0, 0]));
        let s = Script::parse("t", &data).unwrap();
        assert_eq!(s.ops.len(), 2);
        assert_eq!(s.ops[0].kind, Kind::If { jump_if_true: false, expr: "#1==0".into(), target: 12 });
        assert_eq!(s.ops[1].pos, 14);
        assert_eq!(s.ops[1].kind, Kind::Goto { target: 0 });
    }
}
