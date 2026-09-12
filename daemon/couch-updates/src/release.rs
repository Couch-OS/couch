use crate::Result;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{io::Read, path::Path, time::Duration};
const API: &str = "https://api.github.com/repos/dangerouslaser/couch/releases?per_page=100";
const PREFIX: &str = "https://github.com/dangerouslaser/couch/releases/download/";
#[derive(Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    Stable,
    Alpha,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct File {
    pub path: String,
    pub size: u64,
    pub sha256: String,
    pub mode: u32,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema: u32,
    pub model: String,
    pub version: String,
    pub kind: String,
    pub installable: bool,
    pub notes: String,
    pub url: String,
    pub size: u64,
    pub sha256: String,
    pub files: Vec<File>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required_os_baseline: Option<String>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedManifest {
    pub signed: Manifest,
    pub signature: String,
}
pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn decode_hex(s: &str) -> Result<Vec<u8>> {
    if !s.len().is_multiple_of(2)
        || !s
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err("Invalid signature or digest encoding".into());
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|_| "Invalid hex".into()))
        .collect()
}
pub(crate) fn digest(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}
fn version(tag: &str) -> Option<semver::Version> {
    semver::Version::parse(tag.strip_prefix('v')?).ok()
}
pub(crate) fn fetch(url: &str, limit: u64) -> Result<Vec<u8>> {
    if url != API && !url.starts_with(PREFIX) {
        return Err("Unsupported update origin".into());
    }
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(90)))
        .build()
        .new_agent();
    let mut reply = agent
        .get(url)
        .header("User-Agent", "couch-updater")
        .call()
        .map_err(|_| "Update server unavailable; check connectivity or rate limits")?;
    let mut bytes = Vec::new();
    reply
        .body_mut()
        .as_reader()
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Could not read update response")?;
    if bytes.len() as u64 > limit {
        return Err("Update exceeds its size limit".into());
    }
    Ok(bytes)
}
fn verify(bytes: &[u8], key: &[u8], expected: &str) -> Result<Manifest> {
    let envelope: SignedManifest =
        serde_json::from_slice(bytes).map_err(|_| "Invalid signed update manifest")?;
    let key: [u8; 32] = key.try_into().map_err(|_| "Invalid update trust key")?;
    let key =
        ed25519_dalek::VerifyingKey::from_bytes(&key).map_err(|_| "Invalid update trust key")?;
    let signature = ed25519_dalek::Signature::from_slice(&decode_hex(&envelope.signature)?)
        .map_err(|_| "Invalid update signature")?;
    key.verify_strict(
        &serde_json::to_vec(&envelope.signed).map_err(|_| "Invalid manifest")?,
        &signature,
    )
    .map_err(|_| "Update publisher signature did not verify")?;
    let m = envelope.signed;
    if m.schema != 1
        || m.model != "sanytron-ha100"
        || m.version != expected
        || version(&m.version).is_none()
        || m.size == 0
        || m.size > 128 * 1024 * 1024
        || m.sha256.len() != 64
        || decode_hex(&m.sha256).is_err()
        || m.url
            != format!(
                "{PREFIX}{}/couch-{}-ha100-runtime.tar.gz",
                m.version, m.version
            )
    {
        return Err("Update does not match this remote or selected release".into());
    }
    if m.required_os_baseline
        .as_deref()
        .is_some_and(|id| !crate::baseline::valid_id(id))
    {
        return Err("Invalid required OS baseline".into());
    }
    Ok(m)
}
pub(crate) fn discover(
    channel: Channel,
    installed: &str,
    key_path: &Path,
) -> Result<Option<Manifest>> {
    let bytes = fetch(API, 4 * 1024 * 1024)?;
    let releases: Vec<serde_json::Value> =
        serde_json::from_slice(&bytes).map_err(|_| "Invalid release listing")?;
    let current = version(installed);
    let mut candidates = Vec::new();
    for release in releases {
        if release["draft"] != false {
            continue;
        }
        let Some(tag) = release["tag_name"].as_str() else {
            continue;
        };
        let Some(v) = version(tag) else { continue };
        if current.as_ref().is_some_and(|c| v <= *c) {
            continue;
        }
        if !v.pre.is_empty()
            && (channel == Channel::Stable || !v.pre.as_str().starts_with("alpha."))
        {
            continue;
        }
        if release["prerelease"].as_bool() != Some(!v.pre.is_empty()) {
            continue;
        }
        let name = format!("couch-{tag}-ha100-update.json");
        let Some(asset) = release["assets"]
            .as_array()
            .and_then(|a| a.iter().find(|a| a["name"] == name))
        else {
            continue;
        };
        let url = format!("{PREFIX}{tag}/{name}");
        if asset["browser_download_url"] != url {
            continue;
        }
        let size = asset["size"]
            .as_u64()
            .filter(|n| *n > 0 && *n <= 256 * 1024)
            .ok_or("Invalid update manifest size")?;
        let digest = asset["digest"]
            .as_str()
            .and_then(|s| s.strip_prefix("sha256:"))
            .ok_or("Update manifest has no asset digest")?
            .to_owned();
        candidates.push((v, tag.to_owned(), url, size, digest));
    }
    candidates.sort_by(|a, b| b.0.cmp(&a.0));
    let Some((_, tag, url, size, sha)) = candidates.first() else {
        return Ok(None);
    };
    let key = std::fs::read_to_string(key_path)
        .map_err(|_| "This build has no update signing key configured")?;
    let key = decode_hex(key.trim())?;
    let bytes = fetch(url, *size)?;
    if bytes.len() as u64 != *size || digest(&bytes) != *sha {
        return Err("Update manifest digest mismatch".into());
    }
    verify(&bytes, &key, tag).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::Signer;
    #[test]
    fn publisher_signature_binds_version_model_and_payload() {
        let key = ed25519_dalek::SigningKey::from_bytes(&[42; 32]);
        let m = Manifest {
            schema: 1,
            model: "sanytron-ha100".into(),
            version: "v1.2.3".into(),
            kind: "runtime".into(),
            installable: true,
            notes: String::new(),
            url: format!("{PREFIX}v1.2.3/couch-v1.2.3-ha100-runtime.tar.gz"),
            size: 123,
            sha256: "a".repeat(64),
            files: Vec::new(),
            required_os_baseline: None,
        };
        let old_bytes = serde_json::to_vec(&m).unwrap();
        assert!(!String::from_utf8_lossy(&old_bytes).contains("required_os_baseline"));
        let legacy: serde_json::Value = serde_json::from_slice(&old_bytes).unwrap();
        assert_eq!(legacy.as_object().unwrap().len(), 10);
        let signature = hex(&key.sign(&old_bytes).to_bytes());
        let mut signed = SignedManifest {
            signed: m,
            signature,
        };
        let bytes = serde_json::to_vec(&signed).unwrap();
        assert!(verify(&bytes, key.verifying_key().as_bytes(), "v1.2.3").is_ok());
        assert!(verify(&bytes, key.verifying_key().as_bytes(), "v1.2.4").is_err());
        assert!(verify(&bytes, &[1; 32], "v1.2.3").is_err());
        signed.signed.required_os_baseline = Some("baseline-a".into());
        assert!(verify(
            &serde_json::to_vec(&signed).unwrap(),
            key.verifying_key().as_bytes(),
            "v1.2.3"
        )
        .is_err());
        signed.signature = hex(&key
            .sign(&serde_json::to_vec(&signed.signed).unwrap())
            .to_bytes());
        assert!(verify(
            &serde_json::to_vec(&signed).unwrap(),
            key.verifying_key().as_bytes(),
            "v1.2.3"
        )
        .is_ok());
        signed.signed.required_os_baseline = Some("baseline-b".into());
        assert!(verify(
            &serde_json::to_vec(&signed).unwrap(),
            key.verifying_key().as_bytes(),
            "v1.2.3"
        )
        .is_err());
        signed.signed.sha256 = "b".repeat(64);
        assert!(verify(
            &serde_json::to_vec(&signed).unwrap(),
            key.verifying_key().as_bytes(),
            "v1.2.3"
        )
        .is_err());
    }
    #[test]
    fn versions_sort_numerically_and_stable_follows_alpha() {
        assert!(version("v1.10.0") > version("v1.9.9"));
        assert!(version("v1.2.0") > version("v1.2.0-alpha.9"));
        assert!(version("latest").is_none());
    }
}
