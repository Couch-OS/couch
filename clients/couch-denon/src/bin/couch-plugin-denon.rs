//! Standalone integration adapter. stdout is reserved for framed protocol.
fn main() {
    let manifest = serde_json::from_str(include_str!("../../plugin.json"))
        .expect("embedded integration manifest");
    if couch_plugin::serve::<couch_denon::Client>(manifest).is_err() {
        std::process::exit(1);
    }
}
