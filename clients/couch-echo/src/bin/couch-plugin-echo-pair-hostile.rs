//! A package that answers nonsense, written by hand rather than through the
//! SDK's `serve`, because `serve` cannot emit any of it. Built only with the
//! `protocol-3-preview` feature; see `couch_echo::v3`.
fn main() {
    let manifest = include_str!("../../tests/fixtures/plugin-pair-hostile-v3.json");
    if couch_echo::v3::serve_hostile(manifest).is_err() {
        std::process::exit(1);
    }
}
