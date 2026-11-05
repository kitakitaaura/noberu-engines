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

#[derive(Default, Clone, Debug, PartialEq)]
struct Seg {
    text: String,
    /// "rrggbb", or empty for the default colour
    color: String,
    keyword: bool,
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
    /// values; other codes dropped. `lang` 0 = Japanese, 1 = English.
    fn render(&self, vm: &Vm, s: &str) -> String {
        self.render_rich(vm, s).into_iter().map(|g| g.text).collect()
    }

    /// Like `render`, as pieces with their style: `$C[rrggbb]` colour (until
    /// `$C[]`) and `$K<n>` glossary keywords (until `$K0`).
    fn render_rich(&self, vm: &Vm, s: &str) -> Vec<Seg> {
        let mut segs: Vec<Seg> = vec![Seg::default()];
        let push = |segs: &mut Vec<Seg>, t: &str| segs.last_mut().unwrap().text.push_str(t);
        let restyle = |segs: &mut Vec<Seg>, color: Option<String>, keyword: Option<bool>| {
            let last = segs.last().unwrap();
            let next = Seg {
                text: String::new(),
                color: color.unwrap_or_else(|| last.color.clone()),
                keyword: keyword.unwrap_or(last.keyword),
            };
            if last.text.is_empty() {
                *segs.last_mut().unwrap() = next;
            } else {
                segs.push(next);
            }
        };
        let b: Vec<char> = s.chars().collect();
        let mut i = 0;
        while i < b.len() {
            if b[i] != '$' || i + 1 >= b.len() {
                segs.last_mut().unwrap().text.push(b[i]);
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
                        push(&mut segs, if lang == 0 { jp } else { en });
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
                        push(&mut segs, &e.eval(&|v| vm.get(v)).to_string());
                    }
                }
                'C' if b.get(i) == Some(&'[') => {
                    let end = b[i..].iter().position(|&c| c == ']').map_or(b.len(), |p| i + p);
                    let color: String = b[i + 1..end].iter().collect();
                    i = (end + 1).min(b.len());
                    let ok = color.len() == 6 && color.chars().all(|c| c.is_ascii_hexdigit());
                    restyle(&mut segs, Some(if ok { color } else { String::new() }), None);
                }
                // $I{n} indent: drop
                'I' if b.get(i) == Some(&'{') => {
                    i = b[i..].iter().position(|&c| c == '}').map_or(b.len(), |p| i + p + 1);
                }
                'K' => {
                    let st = i;
                    while i < b.len() && b[i].is_ascii_digit() {
                        i += 1;
                    }
                    let n: String = b[st..i].iter().collect();
                    restyle(&mut segs, None, Some(n != "0" && !n.is_empty()));
                }
                // $S024: drop the code and its digits
                'S' => {
                    while i < b.len() && b[i].is_ascii_digit() {
                        i += 1;
                    }
                }
                // $w wait, $d line split: drop
                'w' | 'd' => {}
                other => {
                    push(&mut segs, "$");
                    segs.last_mut().unwrap().text.push(other);
                }
            }
        }
        segs.retain(|g| !g.text.is_empty());
        segs
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
        // Split the speaker off before rendering: a name can itself be a code.
        let (name_raw, body_raw) = Self::speaker(en);
        let name = self.render(vm, &name_raw);
        let rich = self.render_rich(vm, &body_raw);
        let text: String = rich.iter().map(|g| g.text.as_str()).collect();
        let (jp_name, jp_text) = Self::speaker(&self.render(vm, jp));
        if text.trim().is_empty() && name.is_empty() {
            return false;
        }
        let mut e = String::from("{\"t\":\"text\",\"name\":");
        json_str(&name, &mut e);
        e.push_str(",\"text\":");
        json_str(&text, &mut e);
        e.push_str(",\"rich\":[");
        for (i, g) in rich.iter().enumerate() {
            if i > 0 {
                e.push(',');
            }
            e.push('[');
            json_str(&g.text, &mut e);
            e.push(',');
            json_str(&g.color, &mut e);
            e.push_str(if g.keyword { ",1]" } else { ",0]" });
        }
        e.push(']');
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
            "SHAKELIST" | "SG_QUAKE" | "FULLQUAKE_ZOOM" | "SHAKE" => {
                self.event("{\"t\":\"shake\"}".into());
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

fn bad_save() -> JsError {
    err("not a LUCA save, or a damaged one")
}

struct SaveReader<'a> {
    b: &'a [u8],
    p: usize,
}

impl<'a> SaveReader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], JsError> {
        let s = self.b.get(self.p..self.p + n).ok_or_else(bad_save)?;
        self.p += n;
        Ok(s)
    }
    fn u16(&mut self) -> Result<u16, JsError> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32, JsError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn text(&mut self) -> Result<String, JsError> {
        let n = self.u32()? as usize;
        Ok(String::from_utf8_lossy(self.take(n)?).into_owned())
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
                    self.pending_select = Some(self.select_options());
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

    /// Options of the SELECT the VM is stopped at.
    fn select_options(&self) -> Vec<String> {
        let op = &self.game.scenes[self.vm.scene].script.ops[self.vm.pc];
        match &op.kind {
            Kind::Select { en, .. } => {
                let text = self.host.render(&self.vm, &en.replace("$d", "\u{1}"));
                text.split('\u{1}').map(str::to_string).collect()
            }
            _ => vec![],
        }
    }

    /// Save the game at the current pause. `page` is the page's own state
    /// (what is on screen, music, the line being shown), returned by `load`.
    pub fn save(&self, page: &str) -> Vec<u8> {
        let mut out = b"LUCASAVE".to_vec();
        let vm = self.vm.save(&self.game);
        out.extend_from_slice(&(vm.len() as u32).to_le_bytes());
        out.extend_from_slice(&vm);
        let put = |out: &mut Vec<u8>, s: &str| {
            out.extend_from_slice(&(s.len() as u32).to_le_bytes());
            out.extend_from_slice(s.as_bytes());
        };
        let mut slots: Vec<_> = self.host.varstr.iter().collect();
        slots.sort_by_key(|(k, _)| **k);
        out.extend_from_slice(&(slots.len() as u32).to_le_bytes());
        for (slot, (jp, en)) in slots {
            out.extend_from_slice(&slot.to_le_bytes());
            put(&mut out, jp);
            put(&mut out, en);
        }
        put(&mut out, page);
        out
    }

    /// Restore a save made by `save`; returns the page state given then.
    /// Persistent flags (#12000-#12999) keep their current values: they belong
    /// to the player, not to the save.
    pub fn load(&mut self, bytes: &[u8]) -> Result<String, JsError> {
        let rest = bytes.strip_prefix(b"LUCASAVE").ok_or_else(bad_save)?;
        let mut r = SaveReader { b: rest, p: 0 };
        let n = r.u32()? as usize;
        let mut vm = Vm::load(&self.game, r.take(n)?).map_err(err)?;
        let mut varstr = HashMap::new();
        for _ in 0..r.u32()? {
            let slot = r.u16()?;
            let jp = r.text()?;
            let en = r.text()?;
            varstr.insert(slot, (jp, en));
        }
        let page = r.text()?;
        let persistent = luca_lbee::PERSISTENT;
        vm.vars[persistent.clone()].copy_from_slice(&self.vm.vars[persistent]);
        self.vm = vm;
        self.host.varstr = varstr;
        self.ended = false;
        self.pending_select = self.vm.pending_select.map(|_| self.select_options());
        Ok(page)
    }

    /// English text of a string variable (slot 0 holds the in-game date).
    pub fn varstr(&self, slot: u16) -> String {
        self.host.varstr.get(&slot).map(|v| v.1.clone()).unwrap_or_default()
    }

    /// Persistent flags #12000-#12999 (cleared routes, unlocks), to keep
    /// between sessions.
    pub fn globals(&self) -> Vec<i32> {
        self.vm.vars[luca_lbee::PERSISTENT].to_vec()
    }

    pub fn set_globals(&mut self, g: Vec<i32>) {
        let r = luca_lbee::PERSISTENT;
        if g.len() == r.len() {
            self.vm.vars[r].copy_from_slice(&g);
        }
    }

    /// Start over at the beginning (`refrain`: at Refrain), keeping the
    /// persistent flags and clearing everything else.
    pub fn new_game(&mut self, refrain: bool) -> Result<(), JsError> {
        let name = if refrain { luca_lbee::REFRAIN } else { luca_lbee::START };
        let scene = self.game.scene(name).ok_or_else(|| err(format!("no scene {name}")))?;
        let globals = self.globals();
        self.vm = Vm::new(scene);
        self.set_globals(globals);
        self.host.varstr.clear();
        self.pending_select = None;
        self.ended = false;
        Ok(())
    }

    /// Refrain opens once Rin's route has ended (#12002, set at the end of
    /// SEEN2001), which itself needs the five other heroines.
    pub fn refrain_unlocked(&self) -> bool {
        self.vm.get(12002) != 0
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
        let segs = host.render_rich(&vm, "a $K85pork$K0 b $C[60ff40]c$C[] d");
        let flat: Vec<(&str, &str, bool)> = segs.iter().map(|g| (g.text.as_str(), g.color.as_str(), g.keyword)).collect();
        assert_eq!(flat, vec![("a ", "", false), ("pork", "", true), (" b ", "", false), ("c", "60ff40", false), (" d", "", false)]);
    }
}
