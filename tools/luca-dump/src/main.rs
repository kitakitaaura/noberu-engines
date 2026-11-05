//! luca-dump: inspect LUCA System game files.
//!
//!   luca-dump summary <dir>                 one line per PAK
//!   luca-dump list <file.PAK>               every entry
//!   luca-dump cat <file.PAK> <name|#id>     raw bytes to stdout
//!   luca-dump png <file.PAK> <name|#id> <out.png>
//!   luca-dump cz-census <file.PAK> [limit]  decode CZ entries, report failures
//!   luca-dump cz-hash <file.PAK>            hash of every decoded CZ (golden checks)

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
        Some("cz-census") => cz_census(arg(2)?, a.get(3).map_or(Ok(usize::MAX), |s| s.parse())?),
        _ => {
            eprintln!("usage: luca-dump summary|list|cat|png|cz-census ...");
            std::process::exit(2);
        }
    }
}
