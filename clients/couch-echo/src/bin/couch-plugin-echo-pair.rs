//! The pairing fixture as a package executable. Built only with the
//! `protocol-3-preview` feature; see `couch_echo::v3`.
fn main() {
    let manifest = serde_json::from_str(include_str!("../../tests/fixtures/plugin-pair-v3.json"))
        .expect("embedded integration manifest");
    if couch_plugin::serve::<couch_echo::v3::EchoPairTv>(manifest).is_err() {
        std::process::exit(1);
    }
}
