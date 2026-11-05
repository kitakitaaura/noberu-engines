//! Browser build of the LUCA System player (Little Busters! English Edition).
//!
//! The page owns the files. It reads byte ranges itself (File.slice) and hands
//! bytes in; nothing here touches storage:
//!
//! - [`pak_index_len`] / [`pak_index`]: an archive's index, from its first bytes.
//! - [`decode_cz`]: one image to RGBA plus its placement data.
//! - [`Player`]: the script VM. [`Player::step`] runs until the page has to do
//!   something (show a line, offer a choice, wait for a transition) and
//!   returns everything that happened as a JSON array of events.
//!
//! Event list (`t` is the type):
//!
//! | t | fields | page does |
//! |---|---|---|
//! | `bg` | `id` | next frame's background (image id) |
//! | `sprite` | `id, x, y, mode` | put a sprite in the next frame, anchored at (x, y) |
//! | `frame` | | next frame starts with no sprites |
//! | `draw` | `frames, rule` | change to the next frame over `frames` (60 fps) |
//! | `bgm` / `bgm_stop` | `id` / `frames` | music (MUSIC.PAK id) |
//! | `se` | `id` | sound effect (SE.PAK id) |
//! | `close` | | hide the text box |
//! | `movie` | `name` | play files/movie/<name>.webm, then step |
//! | `trophy` | `id` | achievement |
//! | `text` | `name, text, jp_name, jp, voice` | **pause**: show the line, step on click |
//! | `select` | `options` | **pause**: call `choose(i)` |
//! | `wait` | | **pause**: step when the transition ends |
//! | `sleep` | `frames` | **pause**: step after `frames` |
//! | `end` | `at` | the script stopped |

use luca_script::{Kind, Script};
use luca_vm::{expr, Game, Host, Stop, Vm};
use std::collections::HashMap;
use wasm_bindgen::prelude::*;

fn err(e: impl std::fmt::Display) -> JsError {
    JsError::new(&e.to_string())
}

// --- archives ---------------------------------------------------------------

/// How many bytes from the start of an archive hold its index (from its first 36).
#[wasm_bindgen]
pub fn pak_index_len(head: &[u8]) -> Result<u32, JsError> {
    Ok(luca_pak::Index::index_len(head).map_err(err)? as u32)
}

/// An archive's index: where each entry is.
#[wasm_bindgen]
pub struct PakIndex {
    id_start: u32,
    offsets: Vec<f64>,
    lengths: Vec<u32>,
    names: Vec<Option<String>>,
    bases: Vec<u32>,
}

#[wasm_bindgen]
impl PakIndex {
    #[wasm_bindgen(getter)]
    pub fn id_start(&self) -> u32 {
        self.id_start
    }
    #[wasm_bindgen(getter)]
    pub fn count(&self) -> u32 {
        self.lengths.len() as u32
    }
    /// Byte offsets of every entry (index i = id - id_start).
    pub fn offsets(&self) -> Vec<f64> {
        self.offsets.clone()
    }
    pub fn lengths(&self) -> Vec<u32> {
        self.lengths.clone()
    }
    /// Base image per entry (CHARCG: the body a face is drawn on; 0 = none).
    /// Empty when the archive has no such table.
    pub fn bases(&self) -> Vec<u32> {
        self.bases.clone()
    }
    pub fn name(&self, i: u32) -> Option<String> {
        self.names.get(i as usize).cloned().flatten()
    }
}

/// Parse the index bytes of an archive `total` bytes long.
#[wasm_bindgen]
pub fn pak_index(index: &[u8], total: f64) -> Result<PakIndex, JsError> {
    let ix = luca_pak::Index::parse(index, total as u64).map_err(err)?;
    Ok(PakIndex {
        id_start: ix.header.id_start,
        offsets: ix.entries.iter().map(|e| e.offset as f64).collect(),
        lengths: ix.entries.iter().map(|e| e.length).collect(),
        names: ix.entries.iter().map(|e| e.name.clone()).collect(),
        bases: ix.base_ids().map_or_else(Vec::new, |v| v.into_iter().map(u32::from).collect()),
    })
}

// --- images -----------------------------------------------------------------

/// A decoded image. Sprites are placed by their anchor: the script's (x, y)
/// is where the point (anchor_x, anchor_y) of the full `bounds` box goes,
/// and the pixels sit at (offset_x, offset_y) inside that box.
#[wasm_bindgen]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub offset_x: i32,
    pub offset_y: i32,
    pub anchor_x: i32,
    pub anchor_y: i32,
    rgba: Vec<u8>,
}

#[wasm_bindgen]
impl Image {
    /// RGBA pixels, row by row (moves them out: call once).
    pub fn take_rgba(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.rgba)
    }
}

#[wasm_bindgen]
pub fn decode_cz(bytes: &[u8]) -> Result<Image, JsError> {
    let cz = luca_cz::CzFile::decode(&mut std::io::Cursor::new(bytes)).map_err(err)?;
    let h = cz.header();
    let (width, height) = (h.width() as u32, h.height() as u32);
    let (mut offset_x, mut offset_y, mut anchor_x, mut anchor_y) = (0, 0, 0, 0);
    if let Some(x) = cz.extended_header() {
        offset_x = x.offset_x as i32;
        offset_y = x.offset_y as i32;
        anchor_x = x.unknown_width.unwrap_or(0) as i32;
        anchor_y = x.unknown_height.unwrap_or(0) as i32;
    }
    Ok(Image { width, height, offset_x, offset_y, anchor_x, anchor_y, rgba: cz.into_raw() })
}

// --- audio ------------------------------------------------------------------

/// Bytes to skip at the start of a MUSIC/SE/VOICE entry to get plain Ogg
/// ("OGGPAK\0", u32 sample rate, u32 length).
#[wasm_bindgen]
pub fn oggpak_skip(head: &[u8]) -> u32 {
    if head.starts_with(b"OGGPAK\0") { 15 } else { 0 }
}

// --- player -----------------------------------------------------------------

fn json_str(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

fn u16_at(b: &[u8], i: usize) -> Option<u16> {
    Some(u16::from_le_bytes([*b.get(i)?, *b.get(i + 1)?]))
}

fn u32_at(b: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_le_bytes([*b.get(i)?, *b.get(i + 1)?, *b.get(i + 2)?, *b.get(i + 3)?]))
}

struct WebHost {
    events: Vec<String>,
    /// `_VARSTR` table: index -> text.
    varstr_table: HashMap<u16, String>,
    /// String variables: slot -> (Japanese, English).
    varstr: HashMap<u16, (String, String)>,
    rng: luca_lbee::Rng,
}

impl WebHost {
    fn event(&mut self, body: String) {
        self.events.push(body);
    }

    /// Message text for display: `$(slot,lang)` string variables, `$v(expr)`
    /// values, other codes dropped. `lang` 0 = Japanese, 1 = English.
    fn render(&self, vm: &Vm, s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let b: Vec<char> = s.chars().collect();
        let mut i = 0;
        while i < b.len() {
            if b[i] != '$' || i + 1 >= b.len() {
                out.push(b[i]);
                i += 1;
                continue;
            }
            let c = b[i + 1];
            i += 2;
            match c {
                '(' => {
                    // $(slot,lang)
                    let end = b[i..].iter().position(|&c| c == ')').map_or(b.len(), |p| i + p);
                    let inner: String = b[i..end].iter().collect();
                    i = (end + 1).min(b.len());
                    let mut it = inner.split(',').map(|x| x.trim().parse::<u16>().unwrap_or(0));
                    let (slot, lang) = (it.next().unwrap_or(0), it.next().unwrap_or(1));
                    if let Some((jp, en)) = self.varstr.get(&slot) {
                        out.push_str(if lang == 0 { jp } else { en });
                    }
                }
                'v' if b.get(i) == Some(&'(') => {
                    // $v(expr) with nested parentheses
                    let mut depth = 0;
                    let mut end = i;
                    while end < b.len() {
                        match b[end] {
                            '(' => depth += 1,
                            ')' => {
                                depth -= 1;
                                if depth == 0 {
                                    break;
                                }
                            }
                            _ => {}
                        }
                        end += 1;
                    }
                    let inner: String = b[i + 1..end.min(b.len())].iter().collect();
                    i = (end + 1).min(b.len());
                    if let Ok(e) = expr::parse(&inner) {
                        out.push_str(&e.eval(&|v| vm.get(v)).to_string());
                    }
                }
                // $C[rrggbb] colour, $C[] end: drop
                'C' if b.get(i) == Some(&'[') => {
                    i = b[i..].iter().position(|&c| c == ']').map_or(b.len(), |p| i + p + 1);
                }
                // $I{n} indent: drop
                'I' if b.get(i) == Some(&'{') => {
                    i = b[i..].iter().position(|&c| c == '}').map_or(b.len(), |p| i + p + 1);
                }
                // $K85 keyword start / $K0 end, $S024: drop the code and its digits
                'K' | 'S' => {
                    while i < b.len() && b[i].is_ascii_digit() {
                        i += 1;
                    }
                }
                // $w wait, $d line split: drop
                'w' | 'd' => {}
                other => {
                    out.push('$');
                    out.push(other);
                }
            }
        }
        out
    }

    /// Split "`Name@text" into (name, text).
    fn speaker(s: &str) -> (String, String) {
        if let Some(rest) = s.strip_prefix('`') {
            if let Some((name, text)) = rest.split_once('@') {
                return (name.to_string(), text.to_string());
            }
        }
        (String::new(), s.to_string())
    }
}

impl Host for WebHost {
    fn message(&mut self, game: &Game, vm: &Vm, scene: usize, pc: usize) -> bool {
        let op = &game.scenes[scene].script.ops[pc];
        let (voice, jp, en) = match &op.kind {
            Kind::Message { voice, jp, en, .. } => (*voice, jp, en),
            Kind::SayaVoiceText { voice, jp, en } => (*voice, jp, en),
            Kind::VarStrSet { id, text } => {
                self.varstr.insert(*id, (text.clone(), text.clone()));
                return false;
            }
            _ => return false,
        };
        let (name, text) = Self::speaker(&self.render(vm, en));
        let (jp_name, jp_text) = Self::speaker(&self.render(vm, jp));
        if text.trim().is_empty() && name.is_empty() {
            return false;
        }
        let mut e = String::from("{\"t\":\"text\",\"name\":");
        json_str(&name, &mut e);
        e.push_str(",\"text\":");
        json_str(&text, &mut e);
        e.push_str(",\"jp_name\":");
        json_str(&jp_name, &mut e);
        e.push_str(",\"jp\":");
        json_str(&jp_text, &mut e);
        e.push_str(&format!(",\"voice\":{voice}}}"));
        self.event(e);
        true
    }

    fn show(&mut self, game: &Game, _vm: &Vm, scene: usize, pc: usize) -> bool {
        let op = &game.scenes[scene].script.ops[pc];
        let p = &op.params;
        match op.name() {
            "IMAGELOAD" => {
                let (Some(mode), Some(id)) = (u16_at(p, 0), u16_at(p, 2)) else { return false };
                if mode & 0xff == 0 {
                    self.event(format!("{{\"t\":\"bg\",\"id\":{id},\"mode\":{mode}}}"));
                } else {
                    let x = u16_at(p, 6).unwrap_or(640) as i16;
                    let y = u16_at(p, 8).unwrap_or(720) as i16;
                    self.event(format!("{{\"t\":\"sprite\",\"id\":{id},\"x\":{x},\"y\":{y},\"mode\":{mode}}}"));
                }
                false
            }
            "HAIKEI_SET" => {
                self.event("{\"t\":\"frame\"}".into());
                false
            }
            "DRAW" => {
                let frames = u16_at(p, 0).unwrap_or(0);
                let rule = u16_at(p, 2).unwrap_or(0);
                self.event(format!("{{\"t\":\"draw\",\"frames\":{frames},\"rule\":{rule}}}"));
                false
            }
            "WAIT" => {
                self.event("{\"t\":\"wait\"}".into());
                true
            }
            "WAIT_COUNT" => {
                let frames = u16_at(p, 0).unwrap_or(0);
                self.event(format!("{{\"t\":\"sleep\",\"frames\":{frames}}}"));
                true
            }
            "BGM" => {
                match u32_at(p, 0) {
                    Some(id) if id != 0 => self.event(format!("{{\"t\":\"bgm\",\"id\":{id}}}")),
                    _ => {
                        let frames = u16_at(p, 4).unwrap_or(0);
                        self.event(format!("{{\"t\":\"bgm_stop\",\"frames\":{frames}}}"));
                    }
                }
                false
            }
            "SE" => {
                if let Some(id) = u32_at(p, 1) {
                    self.event(format!("{{\"t\":\"se\",\"id\":{id}}}"));
                }
                false
            }
            "CLOSE_WINDOW" => {
                self.event("{\"t\":\"close\"}".into());
                false
            }
            "MOVIE" => {
                let name: String = p.iter().take_while(|&&c| c != 0).map(|&c| c as char).collect();
                let mut e = String::from("{\"t\":\"movie\",\"name\":");
                json_str(&name, &mut e);
                e.push('}');
                self.event(e);
                true
            }
            _ => false,
        }
    }

    fn trophy(&mut self, id: u16) {
        self.event(format!("{{\"t\":\"trophy\",\"id\":{id}}}"));
    }

    fn engine(&mut self, game: &Game, vm: &mut Vm, scene: usize, pc: usize) {
        let op = &game.scenes[scene].script.ops[pc];
        if op.name() == "VARSTR" {
            // VARSTR (slot, jp, en): copy two _VARSTR table entries into a slot
            let p = &op.params;
            if let (Some(slot), Some(jp), Some(en)) = (u16_at(p, 0), u16_at(p, 2), u16_at(p, 4)) {
                let get = |i| self.varstr_table.get(&i).cloned().unwrap_or_default();
                self.varstr.insert(slot, (get(jp), get(en)));
            }
            return;
        }
        // Minigames are auto-played for now (battles, baseball, labyrinth).
        let rng = &mut self.rng;
        luca_lbee::autoplay(game, vm, scene, pc, &mut |n| rng.below(n));
    }

    fn random(&mut self, lo: i32, hi: i32) -> i32 {
        lo + self.rng.below((hi - lo + 1).max(1) as usize) as i32
    }
}

/// The script VM for one game.
#[wasm_bindgen]
pub struct Player {
    game: Game,
    vm: Vm,
    host: WebHost,
    pending_select: Option<Vec<String>>,
    ended: bool,
}

#[wasm_bindgen]
impl Player {
    /// Compile the game from the whole SCRIPT.PAK.
    #[wasm_bindgen(constructor)]
    pub fn new(script_pak: Vec<u8>, seed: f64) -> Result<Player, JsError> {
        let pak = luca_pak::Pak::open(script_pak).map_err(err)?;
        let mut scripts = Vec::new();
        let mut varstr_table = HashMap::new();
        for e in pak.entries() {
            let name = e.name.clone().unwrap_or_default();
            let bytes = pak.read(e).map_err(err)?;
            if name == "_VARSTR" {
                for op in Script::parse(&name, &bytes).map_err(err)?.ops {
                    if let Kind::VarStrSet { id, text } = op.kind {
                        varstr_table.insert(id, text);
                    }
                }
            }
            if luca_lbee::is_scene(&name) {
                scripts.push(Script::parse(&name, &bytes).map_err(err)?);
            }
        }
        let game = Game::compile(scripts).map_err(|e| err(e.0))?;
        let start = game.scene(luca_lbee::START).ok_or_else(|| err("no start scene"))?;
        let host = WebHost {
            events: Vec::new(),
            varstr_table,
            varstr: HashMap::new(),
            rng: luca_lbee::Rng((seed as u64) | 1),
        };
        Ok(Player { vm: Vm::new(start), game, host, pending_select: None, ended: false })
    }

    /// Restart at a scene (by name, e.g. "SEEN0513"), keeping variables.
    pub fn start(&mut self, scene: &str) -> Result<(), JsError> {
        let s = self.game.scene(scene).ok_or_else(|| err(format!("no scene {scene}")))?;
        self.vm.scene = s;
        self.vm.pc = 0;
        self.vm.gosub.clear();
        self.vm.far.clear();
        self.pending_select = None;
        self.ended = false;
        Ok(())
    }

    /// Run until the page has to act. Returns a JSON array of events.
    pub fn step(&mut self) -> String {
        self.host.events.clear();
        if self.ended {
            self.host.events.push("{\"t\":\"end\",\"at\":\"\"}".into());
        } else if let Some(options) = &self.pending_select {
            // still waiting for choose()
            let mut e = String::from("{\"t\":\"select\",\"options\":[");
            for (i, o) in options.iter().enumerate() {
                if i > 0 {
                    e.push(',');
                }
                json_str(o, &mut e);
            }
            e.push_str("]}");
            self.host.events.push(e);
        } else {
            match self.vm.run(&self.game, &mut self.host, 5_000_000) {
                Stop::Pause => {}
                Stop::Select { .. } => {
                    let op = &self.game.scenes[self.vm.scene].script.ops[self.vm.pc];
                    let options: Vec<String> = match &op.kind {
                        Kind::Select { en, .. } => {
                            let text = self.host.render(&self.vm, &en.replace("$d", "\u{1}"));
                            text.split('\u{1}').map(str::to_string).collect()
                        }
                        _ => vec![],
                    };
                    self.pending_select = Some(options);
                    return self.step_select_event();
                }
                stop @ (Stop::End | Stop::Limit) => {
                    self.ended = true;
                    let at = format!(
                        "{:?} in {} @{}",
                        stop,
                        self.game.name(self.vm.scene),
                        self.game.scenes[self.vm.scene].script.ops.get(self.vm.pc).map_or(0, |o| o.pos)
                    );
                    let mut e = String::from("{\"t\":\"end\",\"at\":");
                    json_str(&at, &mut e);
                    e.push('}');
                    self.host.events.push(e);
                }
            }
        }
        format!("[{}]", self.host.events.join(","))
    }

    fn step_select_event(&mut self) -> String {
        let options = self.pending_select.clone().unwrap_or_default();
        let mut e = String::from("{\"t\":\"select\",\"options\":[");
        for (i, o) in options.iter().enumerate() {
            if i > 0 {
                e.push(',');
            }
            json_str(o, &mut e);
        }
        e.push_str("]}");
        self.host.events.push(e);
        format!("[{}]", self.host.events.join(","))
    }

    /// Answer the pending choice (0-based).
    pub fn choose(&mut self, i: u32) {
        if self.pending_select.take().is_some() {
            self.vm.choose(i as usize);
        }
    }

    /// Where the script is, for debugging: "SEEN0514 @1234".
    pub fn location(&self) -> String {
        let pos = self.game.scenes[self.vm.scene].script.ops.get(self.vm.pc).map_or(0, |o| o.pos);
        format!("{} @{}", self.game.name(self.vm.scene), pos)
    }

    pub fn get_var(&self, v: u16) -> i32 {
        self.vm.get(v)
    }

    pub fn set_var(&mut self, v: u16, x: i32) {
        self.vm.set(v, x);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speaker_and_codes() {
        let host = WebHost {
            events: vec![],
            varstr_table: HashMap::new(),
            varstr: [(1, ("朱鷺戸".to_string(), "Tokido".to_string()))].into_iter().collect(),
            rng: luca_lbee::Rng(1),
        };
        let mut vm = Vm::new(0);
        vm.set(6240, 4);
        let r = host.render(&vm, "`$(1,1)@❝No, $K85pork$K0, $C[60ff40]pitch no.$v(#6240+1)$C[]❞");
        assert_eq!(WebHost::speaker(&r), ("Tokido".into(), "❝No, pork, pitch no.5❞".into()));
    }
}
