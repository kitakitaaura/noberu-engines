//! luca-dump: inspect LUCA System game files.
//!
//!   luca-dump summary <dir>                 one line per PAK
//!   luca-dump list <file.PAK>               every entry
//!   luca-dump cat <file.PAK> <name|#id>     raw bytes to stdout
//!   luca-dump png <file.PAK> <name|#id> <out.png>
//!   luca-dump cz-census <file.PAK> [limit]  decode CZ entries, report failures
//!   luca-dump cz-hash <file.PAK>            hash of every decoded CZ (golden checks)
//!   luca-dump scripts <SCRIPT.PAK> <outdir> decompile every scene; check jump targets
//!   luca-dump sim <SCRIPT.PAK> [runs] [seed]  random playouts from SEEN0513
//!   luca-dump engine-vars <SCRIPT.PAK>      variables only the engine sets
//!   luca-dump explore <SCRIPT.PAK> [cap] [seed] [#var=value,...]  breadth-first search over choices
//!   luca-dump campaign <SCRIPT.PAK> [cap] [seed]  repeat the search, carrying persistent flags between rounds
//!   luca-dump replay <SCRIPT.PAK> <start> <seed> <choices> [#var=value,...]  play one path, show what it reached

mod explore;
mod sim;

use luca_pak::{Entry, Pak};
use std::io::Write;
use std::path::Path;
use std::time::Instant;
use vfs::NativeFile;

type Res<T> = Result<T, Box<dyn std::error::Error>>;

fn open(path: &str) -> Res<Pak<NativeFile>> {
    Ok(Pak::open(NativeFile::open(path)?)?)
}

fn pick<'a>(pak: &'a Pak<NativeFile>, key: &str) -> Res<&'a Entry> {
    let e = match key.strip_prefix('#') {
        Some(id) => pak.by_id(id.parse()?),
        None => pak.find(key),
    };
    e.ok_or_else(|| format!("no entry {key}").into())
}

fn magic(b: &[u8]) -> String {
    b.iter().take(4).map(|&c| if c.is_ascii_graphic() { c as char } else { '.' }).collect()
}

fn summary(dir: &str) -> Res<()> {
    let mut paths: Vec<_> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("pak")))
        .collect();
    paths.sort();
    for p in paths {
        let t = Instant::now();
        match Pak::open(NativeFile::open(&p)?) {
            Ok(pak) => {
                let h = &pak.header;
                let first = pak.entries().first();
                let m = match first {
                    Some(e) if e.length >= 4 => magic(&pak.read(e)?[..4]),
                    _ => "-".into(),
                };
                println!(
                    "{:<12} {:>6} entries  id {:>6}  block {:>5}  flags {:#06x}  names {}  first {:?} [{}]  index {:?}",
                    p.file_name().unwrap().to_string_lossy(),
                    h.entry_count,
                    h.id_start,
                    h.block_size,
                    h.flags.0,
                    h.flags.has_names(),
                    first.and_then(|e| e.name.clone()).unwrap_or_default(),
                    m,
                    t.elapsed()
                );
            }
            Err(e) => println!("{:<12} ERROR {e}", p.file_name().unwrap().to_string_lossy()),
        }
    }
    Ok(())
}

fn list(path: &str) -> Res<()> {
    let pak = open(path)?;
    let mut out = std::io::stdout().lock();
    for e in pak.entries() {
        let head = pak.read(&Entry { length: e.length.min(4), ..e.clone() })?;
        writeln!(
            out,
            "{:>6} #{:<6} {:<32} {:>10} @{:<11} {}",
            e.index,
            e.id,
            e.name.as_deref().unwrap_or("-"),
            e.length,
            e.offset,
            magic(&head)
        )?;
    }
    Ok(())
}

fn to_png(rgba: &[u8], w: u32, h: u32, out: &Path) -> Res<()> {
    let f = std::io::BufWriter::new(std::fs::File::create(out)?);
    let mut enc = png::Encoder::new(f, w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()?.write_image_data(rgba)?;
    Ok(())
}

fn decode(bytes: &[u8]) -> Res<luca_cz::CzFile> {
    Ok(luca_cz::CzFile::decode(&mut std::io::Cursor::new(bytes))?)
}

fn png_cmd(path: &str, key: &str, out: &str) -> Res<()> {
    let pak = open(path)?;
    let e = pick(&pak, key)?;
    let cz = decode(&pak.read(e)?)?;
    let h = cz.header();
    to_png(cz.as_raw(), h.width() as u32, h.height() as u32, Path::new(out))?;
    println!("{:?} {}x{} depth {} -> {out}", h.version(), h.width(), h.height(), h.depth());
    Ok(())
}

fn cz_census(path: &str, limit: usize) -> Res<()> {
    let pak = open(path)?;
    let (mut ok, mut bad, mut skipped) = (0, 0, 0);
    let mut kinds = std::collections::BTreeMap::<String, usize>::new();
    let t = Instant::now();
    for e in pak.entries().iter().take(limit) {
        let bytes = pak.read(e)?;
        if !bytes.starts_with(b"CZ") {
            skipped += 1;
            *kinds.entry(format!("other {}", magic(&bytes))).or_default() += 1;
            continue;
        }
        // CZ decoders can panic on unsupported depths; record instead of dying.
        match std::panic::catch_unwind(|| decode(&bytes).map_err(|e| e.to_string())) {
            Ok(Ok(cz)) => {
                ok += 1;
                let h = cz.header();
                *kinds.entry(format!("{:?} depth {}", h.version(), h.depth())).or_default() += 1;
            }
            Ok(Err(err)) => {
                bad += 1;
                eprintln!("FAIL {:?} {}: {err}", e.name, magic(&bytes));
            }
            Err(_) => {
                bad += 1;
                eprintln!("PANIC {:?} {}", e.name, magic(&bytes));
            }
        }
    }
    println!("ok {ok}  failed {bad}  not CZ {skipped}  in {:?}", t.elapsed());
    for (k, n) in kinds {
        println!("  {n:>6}  {k}");
    }
    Ok(())
}

fn cz_hash(path: &str) -> Res<()> {
    use std::hash::{DefaultHasher, Hasher};
    let pak = open(path)?;
    for e in pak.entries() {
        let bytes = pak.read(e)?;
        if !bytes.starts_with(b"CZ") {
            continue;
        }
        let cz = decode(&bytes)?;
        let mut h = DefaultHasher::new();
        h.write(cz.as_raw());
        println!("{} {:016x}", e.index, h.finish());
    }
    Ok(())
}

fn scripts(path: &str, outdir: &str) -> Res<()> {
    let pak = open(path)?;
    std::fs::create_dir_all(outdir)?;
    let names: std::collections::HashSet<String> =
        pak.entries().iter().filter_map(|e| e.name.clone()).map(|n| n.to_ascii_uppercase()).collect();
    let (mut total, mut raw, mut bad_jumps, mut bad_files) = (0usize, 0usize, 0usize, 0usize);
    let mut raw_by_name = std::collections::BTreeMap::<&str, usize>::new();
    for e in pak.entries() {
        let name = e.name.clone().unwrap_or_else(|| format!("#{}", e.id));
        let s = match luca_script::Script::parse(&name, &pak.read(e)?) {
            Ok(s) => s,
            Err(err) => {
                eprintln!("PARSE {err}");
                continue;
            }
        };
        let mut out = std::io::BufWriter::new(std::fs::File::create(Path::new(outdir).join(format!("{name}.txt")))?);
        for op in &s.ops {
            total += 1;
            if op.kind == luca_script::Kind::Raw {
                raw += 1;
                *raw_by_name.entry(op.name()).or_default() += 1;
            }
            for t in op.local_targets() {
                if s.index_of(t).is_none() {
                    bad_jumps += 1;
                    eprintln!("{name} @{}: {} -> @{t} is not a command start", op.pos, op.name());
                }
            }
            if let luca_script::Kind::FarCall { file, .. } | luca_script::Kind::Jump { file, .. } = &op.kind {
                if !names.contains(&file.to_ascii_uppercase()) {
                    bad_files += 1;
                    eprintln!("{name} @{}: {} -> missing scene {file:?}", op.pos, op.name());
                }
            }
            writeln!(out, "{}: {op}", op.pos)?;
        }
    }
    println!("{total} commands, {raw} without a decoded layout, {bad_jumps} bad local jumps, {bad_files} missing scenes");
    for (n, c) in raw_by_name.iter().filter(|(_, c)| **c >= 50) {
        println!("  raw {c:>7}  {n}");
    }
    Ok(())
}

fn main() -> Res<()> {
    let a: Vec<String> = std::env::args().collect();
    let arg = |i: usize| a.get(i).map(String::as_str).ok_or("missing argument");
    match a.get(1).map(String::as_str) {
        Some("summary") => summary(arg(2)?),
        Some("list") => list(arg(2)?),
        Some("cat") => {
            let pak = open(arg(2)?)?;
            let e = pick(&pak, arg(3)?)?;
            std::io::stdout().write_all(&pak.read(e)?)?;
            Ok(())
        }
        Some("png") => png_cmd(arg(2)?, arg(3)?, arg(4)?),
        Some("cz-hash") => cz_hash(arg(2)?),
        Some("scripts") => scripts(arg(2)?, arg(3)?),
        Some("sim") => {
            let game = sim::load_game(arg(2)?)?;
            let runs = a.get(3).map_or(Ok(100), |s| s.parse())?;
            let seed = a.get(4).map_or(Ok(1), |s| s.parse())?;
            sim::playouts(&game, "SEEN0513", runs, seed);
            Ok(())
        }
        Some("explore") => {
            let game = sim::load_game(arg(2)?)?;
            let cap = a.get(3).map_or(Ok(20_000), |s| s.parse())?;
            let seed = a.get(4).map_or(Ok(1), |s| s.parse())?;
            let mut persist = Vec::new();
            for kv in a.get(5).map(String::as_str).unwrap_or("").split(',').filter(|s| !s.is_empty()) {
                let (k, v) = kv.trim_start_matches('#').split_once('=').ok_or("use #var=value")?;
                persist.push((k.parse()?, v.parse()?));
            }
            let t = Instant::now();
            let rep = explore::bfs(&game, "SEEN0513", &persist, cap, seed);
            println!("levels (frontier per choice depth): {:?}", rep.levels);
            println!("time {:?}", t.elapsed());
            println!("trophies:");
            for (t, f) in &rep.trophies {
                let p: String = f.path.iter().map(|c| char::from(b'0' + c)).collect();
                println!("  {t:>3}  after {:>3} choices  {p}", f.depth);
            }
            println!("endings:");
            for (e, f) in &rep.endings {
                println!("  {e}  (after {} choices)", f.depth);
            }
            let missing: Vec<_> = game
                .scenes
                .iter()
                .map(|s| s.script.name.clone())
                .filter(|n| !rep.scenes.contains_key(n))
                .collect();
            println!("scenes with text never shown: {} {:?}", missing.len(), missing);
            Ok(())
        }
        Some("campaign") => {
            use std::collections::{BTreeMap, BTreeSet};
            let game = sim::load_game(arg(2)?)?;
            let cap = a.get(3).map_or(Ok(20_000), |s| s.parse())?;
            let seed: u64 = a.get(4).map_or(Ok(1), |s| s.parse())?;
            // How a trophy was first reached: enough to replay it.
            struct Hit {
                round: usize,
                start: &'static str,
                seed: u64,
                flags: Vec<(u16, i32)>,
                path: Vec<u8>,
            }
            let mut persist: BTreeMap<u16, i32> = Default::default();
            let mut trophies: BTreeMap<u16, Hit> = Default::default();
            let mut endings: BTreeMap<String, usize> = Default::default();
            let mut scenes: BTreeSet<String> = Default::default();
            for round in 1..=8 {
                let t = Instant::now();
                let flags: Vec<(u16, i32)> = persist.iter().map(|(k, v)| (*k, *v)).collect();
                let before = (trophies.len(), endings.len(), persist.len());
                // New game, and Refrain (SEEN2513: no script jumps there; the
                // executable starts it once the routes are cleared).
                for start in ["SEEN0513", "SEEN2513"] {
                    let rseed = seed + round as u64;
                    let rep = explore::bfs(&game, start, &flags, cap, rseed);
                    for (t, f) in rep.trophies {
                        trophies.entry(t).or_insert(Hit { round, start, seed: rseed, flags: flags.clone(), path: f.path });
                    }
                    for e in rep.endings.keys() {
                        endings.entry(e.clone()).or_insert(round);
                    }
                    scenes.extend(rep.scenes.keys().cloned());
                    for (k, v) in rep.persist {
                        let e = persist.entry(k).or_insert(v);
                        *e = (*e).max(v);
                    }
                }
                println!(
                    "round {round}: {} trophies, {} endings, {} persistent flags set, {:?}",
                    trophies.len(),
                    endings.len(),
                    persist.len(),
                    t.elapsed()
                );
                if (trophies.len(), endings.len(), persist.len()) == before {
                    break;
                }
            }
            println!("trophies (round, start, choices; replayed from scratch to check):");
            for (t, h) in &trophies {
                let r = explore::replay(&game, h.start, &h.flags, h.seed, &h.path);
                let ok = if r.trophies.contains(t) { "ok" } else { "REPLAY FAILED" };
                let p: String = h.path.iter().map(|c| char::from(b'0' + c)).collect();
                println!("  {t:>3}  round {}  {}  {:>3} choices  {ok}  {p}", h.round, h.start, h.path.len());
            }
            println!("endings (first round reached):");
            for (e, r) in &endings {
                println!("  {e}  round {r}");
            }
            let missing: Vec<_> =
                game.scenes.iter().map(|s| s.script.name.clone()).filter(|n| !scenes.contains(n)).collect();
            println!("scenes with text never shown: {} {:?}", missing.len(), missing);
            Ok(())
        }
        Some("replay") => {
            let game = sim::load_game(arg(2)?)?;
            let start = arg(3)?;
            let seed: u64 = arg(4)?.parse()?;
            let path: Vec<u8> = arg(5)?.bytes().map(|c| c - b'0').collect();
            let mut flags = Vec::new();
            for kv in a.get(6).map(String::as_str).unwrap_or("").split(',').filter(|s| !s.is_empty()) {
                let (k, v) = kv.trim_start_matches('#').split_once('=').ok_or("use #var=value")?;
                flags.push((k.parse()?, v.parse()?));
            }
            let r = explore::replay(&game, start, &flags, seed, &path);
            println!("stopped: {:?} at {}", r.stop, r.at);
            println!("trophies: {:?}", r.trophies);
            println!("choices unused: {}", r.unused);
            println!("scenes with text: {:?}", r.scenes);
            Ok(())
        }
        Some("engine-vars") => {
            let game = sim::load_game(arg(2)?)?;
            for (v, n) in sim::engine_vars(&game) {
                println!("#{v}\tread {n}x");
            }
            Ok(())
        }
        Some("cz-census") => cz_census(arg(2)?, a.get(3).map_or(Ok(usize::MAX), |s| s.parse())?),
        _ => {
            eprintln!("usage: luca-dump summary|list|cat|png|cz-census ...");
            std::process::exit(2);
        }
    }
}
