use rust_multi_file::{Config, greet};

fn main() {
    let cfg = Config::new("app".into(), 8080);
    println!("{}", greet(&cfg.name));
}
