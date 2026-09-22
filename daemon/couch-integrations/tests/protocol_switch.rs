//! Protocol 3 is unreleased. Its only switch is couch-plugin's
//! `protocol-3-preview` feature, and Cargo unifies features across a build, so
//! one dependency (or dev-dependency) anywhere in this workspace that enabled
//! it would switch it on in the package manager that ships. This runs with
//! this workspace's own feature set and fails if that ever happens.

#[test]
fn the_package_manager_is_never_built_with_the_protocol_3_preview() {
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
    assert_eq!(
        manifest.validate(),
        Err(couch_plugin::Error::Incompatible),
        "a protocol 3 package must still be one that needs a newer Couch"
    );
}
