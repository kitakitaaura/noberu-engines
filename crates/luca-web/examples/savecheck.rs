//! Save/load check against a real SCRIPT.PAK:
//! cargo run --release -p luca-web --example savecheck -- SCRIPT.PAK
fn run(p: &mut luca_web::Player, n: usize) -> Vec<String> {
    let mut out = Vec::new();
    for _ in 0..n {
        let ev = p.step();
        if ev.contains("\"t\":\"select\"") {
            p.choose(1);
        }
        out.push(ev);
    }
    out
}

fn main() {
    let path = std::env::args().nth(1).expect("SCRIPT.PAK");
    let Ok(mut p) = luca_web::Player::new(std::fs::read(path).unwrap(), 3.0) else { panic!("load") };
    for skip in [60usize, 400, 1200] {
        run(&mut p, skip);
        let at = p.location();
        let save = p.save("{\"page\":1}");
        let a = run(&mut p, 20);
        let page = p.load(&save).unwrap_or_else(|_| panic!("load failed"));
        let b = run(&mut p, 20);
        println!("{at}: save {} bytes, page {page}, replay {}", save.len(), if a == b { "identical" } else { "DIFFERS" });
        if a != b {
            for (x, y) in a.iter().zip(&b) {
                if x != y {
                    println!("  first diff:\n  {}\n  {}", &x[..x.len().min(200)], &y[..y.len().min(200)]);
                    break;
                }
            }
        }
    }
}
