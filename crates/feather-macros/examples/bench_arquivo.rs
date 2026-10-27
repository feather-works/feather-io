//! Roda um arquivo JS no motor das macros: `cargo run --release --example bench_arquivo -- a.js`.
use std::rc::Rc;
struct Nada;
impl feather_macros::HospedeiroMacros for Nada {
    fn chamar(&self, op: &str, _: &serde_json::Value) -> Result<serde_json::Value, String> { Err(op.into()) }
}
fn main() {
    let codigo = std::fs::read_to_string(std::env::args().nth(1).expect("arquivo")).unwrap();
    let t = std::time::Instant::now();
    let r = feather_macros::executar(Rc::new(Nada), "", &codigo);
    println!("{:?} {:?} [{:.0} ms]", r.saida, r.erro, t.elapsed().as_secs_f64() * 1000.0);
}
