//! Saving and restoring a [`Vm`].
//!
//! Positions are stored as (scene name, byte offset of the command), not
//! instruction indexes, so a save survives changes to how we compile scripts.
//! Variables are stored sparse (only nonzero ones). Format, little-endian:
//!
//! ```text
//! "LUCAVM" u16 version
//! pos            the current command
//! u8 has_select, u16 select_var
//! u32 n, n x (u16 var, i32 value)
//! u32 n, n x pos                      GOSUB return points
//! u32 n, n x (pos, u32 m, m x (u16 local, i32 value))   FARCALL frames
//! pos = u16 name length, name (UTF-8), u32 byte offset (u32::MAX = past the end)
//! ```

use crate::{Frame, Game, Vm, LOCAL_VARS};

const MAGIC: &[u8; 6] = b"LUCAVM";
const VERSION: u16 = 1;
const END: u32 = u32::MAX;

#[derive(Debug)]
pub struct SaveError(pub String);

impl std::fmt::Display for SaveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

fn put_pos(out: &mut Vec<u8>, game: &Game, scene: usize, pc: usize) {
    let name = game.name(scene).as_bytes();
    out.extend_from_slice(&(name.len() as u16).to_le_bytes());
    out.extend_from_slice(name);
    let pos = game.scenes[scene].script.ops.get(pc).map_or(END, |o| o.pos);
    out.extend_from_slice(&pos.to_le_bytes());
}

struct Rd<'a> {
    b: &'a [u8],
    p: usize,
}

impl Rd<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8], SaveError> {
        let s = self.b.get(self.p..self.p + n).ok_or_else(|| SaveError("save is truncated".into()))?;
        self.p += n;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, SaveError> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, SaveError> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32, SaveError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn i32(&mut self) -> Result<i32, SaveError> {
        Ok(i32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn pos(&mut self, game: &Game) -> Result<(usize, usize), SaveError> {
        let n = self.u16()? as usize;
        let name = String::from_utf8_lossy(self.take(n)?).into_owned();
        let at = self.u32()?;
        let scene = game.scene(&name).ok_or_else(|| SaveError(format!("save names scene {name}, which this game lacks")))?;
        let pc = if at == END {
            game.scenes[scene].code.len() - 1
        } else {
            game.scenes[scene]
                .script
                .index_of(at)
                .ok_or_else(|| SaveError(format!("save points into {name} @{at}, not a command")))?
        };
        Ok((scene, pc))
    }
}

impl Vm {
    pub fn save(&self, game: &Game) -> Vec<u8> {
        let mut out = Vec::with_capacity(4096);
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&VERSION.to_le_bytes());
        put_pos(&mut out, game, self.scene, self.pc);
        out.push(self.pending_select.is_some() as u8);
        out.extend_from_slice(&self.pending_select.unwrap_or(0).to_le_bytes());
        let nz: Vec<(usize, i32)> = self.vars.iter().copied().enumerate().filter(|(_, v)| *v != 0).collect();
        out.extend_from_slice(&(nz.len() as u32).to_le_bytes());
        for (i, v) in nz {
            out.extend_from_slice(&(i as u16).to_le_bytes());
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&(self.gosub.len() as u32).to_le_bytes());
        for &(s, pc) in &self.gosub {
            put_pos(&mut out, game, s, pc);
        }
        out.extend_from_slice(&(self.far.len() as u32).to_le_bytes());
        for f in &self.far {
            put_pos(&mut out, game, f.scene, f.pc);
            let nz: Vec<(usize, i32)> = f.locals.iter().copied().enumerate().filter(|(_, v)| *v != 0).collect();
            out.extend_from_slice(&(nz.len() as u32).to_le_bytes());
            for (i, v) in nz {
                out.extend_from_slice(&(i as u16).to_le_bytes());
                out.extend_from_slice(&v.to_le_bytes());
            }
        }
        out
    }

    pub fn load(game: &Game, bytes: &[u8]) -> Result<Vm, SaveError> {
        let mut r = Rd { b: bytes, p: 0 };
        if r.take(6)? != MAGIC {
            return Err(SaveError("not a LUCA save".into()));
        }
        let version = r.u16()?;
        if version != VERSION {
            return Err(SaveError(format!("save version {version}, this build reads {VERSION}")));
        }
        let (scene, pc) = r.pos(game)?;
        let mut vm = Vm::new(scene);
        vm.pc = pc;
        let has_select = r.u8()? != 0;
        let var = r.u16()?;
        vm.pending_select = has_select.then_some(var);
        for _ in 0..r.u32()? {
            let i = r.u16()? as usize;
            vm.vars[i] = r.i32()?;
        }
        for _ in 0..r.u32()? {
            vm.gosub.push(r.pos(game)?);
        }
        for _ in 0..r.u32()? {
            let (scene, pc) = r.pos(game)?;
            let mut locals = vec![0i32; LOCAL_VARS.len()];
            for _ in 0..r.u32()? {
                let i = r.u16()? as usize;
                let v = r.i32()?;
                *locals.get_mut(i).ok_or_else(|| SaveError("bad local variable".into()))? = v;
            }
            vm.far.push(Frame { scene, pc, locals: locals.into() });
        }
        Ok(vm)
    }
}
