//! Run the player natively and print its events: cargo run --release -p luca-web --example smoke -- SCRIPT.PAK [steps]
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let bytes = std::fs::read(&a[1]).expect("SCRIPT.PAK");
    let steps: usize = a.get(2).map_or(40, |s| s.parse().unwrap());
    let Ok(mut p) = luca_web::Player::new(bytes, 7.0) else { panic!("load failed") };
    for _ in 0..steps {
        let ev = p.step();
        println!("{} {}", p.location(), &ev.chars().take(300).collect::<String>());
        if ev.contains("\"t\":\"select\"") {
            p.choose(0);
        }
        if ev.contains("\"t\":\"end\"") {
            break;
        }
    }
}
