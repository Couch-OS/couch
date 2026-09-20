//! The protocol 3 bridge fixture as a package executable: one connection with
//! eighty children. Built only with the `protocol-3-preview` feature; see
//! `couch_echo::v3`.
fn main() {
    let manifest = serde_json::from_str(include_str!("../../tests/fixtures/plugin-bridge-v3.json"))
        .expect("embedded integration manifest");
    if couch_plugin::serve::<couch_echo::v3::EchoBridge>(manifest).is_err() {
        std::process::exit(1);
    }
}
