//! The package manager and protocol crate must agree on the released host
//! contract without relying on an optional Cargo feature.

#[test]
fn the_package_manager_accepts_the_released_protocol_3_contract() {
    assert_eq!(
        couch_plugin::accepted_protocol_version(),
        couch_plugin::PROTOCOL_VERSION
    );
    assert_eq!(
        couch_integrations::PROTOCOL_VERSION,
        couch_plugin::PROTOCOL_VERSION
    );
    let manifest: couch_plugin::Manifest = serde_json::from_str(
        r#"{"protocol_version":3,"min_core_protocol_version":3,"id":"echo","label":"Echo",
            "version":"1","executable":"bin/e","capabilities":[],"settings":[]}"#,
    )
    .unwrap();
    assert_eq!(manifest.validate(), Ok(()));
}
