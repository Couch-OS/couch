//! Full-OS capabilities are outside runtime slots and cannot be changed by updates.
use crate::{release::Manifest, Result};
use serde::Deserialize;
use std::{fs, io::Read, path::Path};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Marker {
    schema: u32,
    model: String,
    id: String,
}
pub(crate) fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 96
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}
pub(crate) fn installed(root: &Path) -> Result<String> {
    let path = root.join("os-baseline.json");
    let meta = fs::symlink_metadata(&path)
        .map_err(|_| "Full OS compatibility marker missing; install the current full OS first")?;
    if !meta.is_file() || meta.len() > 1024 {
        return Err("Invalid full OS compatibility marker; reinstall the full OS".into());
    }
    let mut bytes = Vec::new();
    fs::File::open(path)
        .and_then(|f| f.take(1025).read_to_end(&mut bytes))
        .map_err(|_| "Could not read full OS compatibility marker")?;
    if bytes.len() > 1024 {
        return Err("Oversized full OS compatibility marker".into());
    }
    let marker: Marker =
        serde_json::from_slice(&bytes).map_err(|_| "Invalid full OS compatibility marker")?;
    if marker.schema != 1 || marker.model != "sanytron-ha100" || !valid_id(&marker.id) {
        return Err("Unsupported full OS compatibility marker".into());
    }
    Ok(marker.id)
}
pub(crate) fn check(root: &Path, manifest: &Manifest) -> Result<()> {
    let Some(required) = &manifest.required_os_baseline else {
        // Old signed manifests retain their exact original serialization and policy.
        return Ok(());
    };
    if !valid_id(required) || installed(root)? != *required {
        return Err(
            "This update requires a different full OS baseline; run the full OS installer first"
                .into(),
        );
    }
    Ok(())
}
