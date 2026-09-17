use crate::Result;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{io::Read, path::Path, time::Duration};
/// The Couch repository by its permanent GitHub ID, which survives the move from
/// dangerouslaser to the Couch-OS organization. An owner-named URL would stop
/// listing releases for remotes still running this updater after that move.
const API: &str = "https://api.github.com/repositories/1363054496/releases?per_page=100";
/// Download locations of Couch releases under either repository owner. GitHub
/// lists assets under the current owner, and releases signed before the move
/// name the previous one.
pub(crate) const PREFIXES: [&str; 2] = [
    "https://github.com/dangerouslaser/couch/releases/download/",
    "https://github.com/Couch-OS/couch/releases/download/",
];
/// Where new releases are published and what their signed manifests name.
/// Updaters older than PREFIXES accept only this owner, so it changes to
/// Couch-OS only after the repository has moved.
pub(crate) const PREFIX: &str = PREFIXES[0];

/// The accepted asset URL for `name` in release `tag`, if `url` is one.
fn release_url(url: &str, tag: &str, name: &str) -> bool {
    let path = format!("{tag}/{name}");
    PREFIXES
        .iter()
        .any(|prefix| url.strip_prefix(prefix) == Some(path.as_str()))
}
#[derive(Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    Stable,
    Alpha,
    /// Builds from the `dev` branch: the alpha stream plus every tag carrying
    /// a trailing `dev` identifier (`v0.1.0-alpha.20260914.7.dev`). For the
    /// development remote, not for anyone else.
    Dev,
}
/// Whether a release version belongs on a channel. Stable takes only finished
/// versions. Alpha takes those and `alpha.` prereleases, but not dev builds.
/// Dev takes everything Alpha does plus the dev builds, and because a dev tag
/// keeps the `alpha.<date>.<n>` core, all of them sort in one order.
pub(crate) fn accepts(channel: Channel, v: &semver::Version) -> bool {
    let pre = v.pre.as_str();
    if pre.is_empty() {
        return true;
    }
    if !pre.starts_with("alpha.") {
        return false;
    }
    let dev = pre.split('.').any(|part| part == "dev");
    match channel {
        Channel::Stable => false,
        Channel::Alpha => !dev,
        Channel::Dev => true,
    }
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
pub(crate) fn version(tag: &str) -> Option<semver::Version> {
    semver::Version::parse(tag.strip_prefix('v')?).ok()
}
pub(crate) fn fetch(url: &str, limit: u64) -> Result<Vec<u8>> {
    if url != API && !PREFIXES.iter().any(|prefix| url.starts_with(prefix)) {
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
pub(crate) fn verify(bytes: &[u8], key: &[u8], expected: &str) -> Result<Manifest> {
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
        || !matches!(m.kind.as_str(), "runtime" | "boot")
        || !release_url(
            &m.url,
            &m.version,
            &format!("couch-{}-ha100-{}.tar.gz", m.version, m.kind),
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
/// A newer runtime and its matching boot payload, or the installed release's
/// boot payload when completing an update made by an older updater.
pub(crate) struct Offers {
    pub runtime: Option<Manifest>,
    pub boot: Option<Manifest>,
}
/// A signed manifest asset on a release, as the listing describes it.
struct Listed {
    tag: String,
    url: String,
    size: u64,
    digest: String,
}
fn listed(release: &serde_json::Value, tag: &str, name: &str) -> Result<Option<Listed>> {
    let Some(asset) = release["assets"]
        .as_array()
        .and_then(|a| a.iter().find(|a| a["name"] == name))
    else {
        return Ok(None);
    };
    let Some(url) = asset["browser_download_url"]
        .as_str()
        .filter(|url| release_url(url, tag, name))
    else {
        return Ok(None);
    };
    let url = url.to_owned();
    let size = asset["size"]
        .as_u64()
        .filter(|n| *n > 0 && *n <= 256 * 1024)
        .ok_or("Invalid update manifest size")?;
    // A release GitHub has attached no digest to is skipped, not an error: the
    // caller loops over every release, so failing here hid every older, signed
    // release behind one `phase: "error"`. The digest itself stays mandatory.
    let Some(digest) = asset["digest"]
        .as_str()
        .and_then(|s| s.strip_prefix("sha256:"))
    else {
        eprintln!("couch-updates: {tag}: {name} has no asset digest, skipping the release");
        return Ok(None);
    };
    let digest = digest.to_owned();
    Ok(Some(Listed {
        tag: tag.to_owned(),
        url,
        size,
        digest,
    }))
}
fn fetch_manifest(item: &Listed, key: &[u8]) -> Result<Manifest> {
    let bytes = fetch(&item.url, item.size)?;
    if bytes.len() as u64 != item.size || digest(&bytes) != item.digest {
        return Err("Update manifest digest mismatch".into());
    }
    verify(&bytes, key, &item.tag)
}
pub(crate) fn discover(channel: Channel, installed: &str, key_path: &Path) -> Result<Offers> {
    let bytes = fetch(API, 4 * 1024 * 1024)?;
    let releases: Vec<serde_json::Value> =
        serde_json::from_slice(&bytes).map_err(|_| "Invalid release listing")?;
    let (runtime, boot) = select_listed(releases, channel, installed)?;
    if runtime.is_none() && boot.is_none() {
        return Ok(Offers {
            runtime: None,
            boot: None,
        });
    }
    let key = std::fs::read_to_string(key_path)
        .map_err(|_| "This build has no update signing key configured")?;
    let key = decode_hex(key.trim())?;
    let runtime = runtime
        .map(|item| fetch_manifest(&item, &key))
        .transpose()?;
    let boot = boot.map(|item| fetch_manifest(&item, &key)).transpose()?;
    if runtime.as_ref().is_some_and(|m| m.kind != "runtime")
        || boot.as_ref().is_some_and(|m| m.kind != "boot")
    {
        return Err("Release manifest has the wrong payload type".into());
    }
    Ok(Offers { runtime, boot })
}

fn select_listed(
    releases: Vec<serde_json::Value>,
    channel: Channel,
    installed: &str,
) -> Result<(Option<Listed>, Option<Listed>)> {
    let current = version(installed);
    let mut candidates = Vec::new();
    let mut boot = None;
    for release in releases {
        if release["draft"] != false {
            continue;
        }
        let Some(tag) = release["tag_name"].as_str() else {
            continue;
        };
        let Some(v) = version(tag) else { continue };
        if release["prerelease"].as_bool() != Some(!v.pre.is_empty()) {
            continue;
        }
        if tag == installed {
            boot = Some(release.clone());
            continue;
        }
        if current.as_ref().is_some_and(|c| v <= *c) {
            continue;
        }
        if !accepts(channel, &v) {
            continue;
        }
        if let Some(item) = listed(&release, tag, &format!("couch-{tag}-ha100-update.json"))? {
            candidates.push((v, item, release.clone()));
        }
    }
    candidates.sort_by(|a, b| b.0.cmp(&a.0));
    let runtime = match candidates.into_iter().next() {
        Some((_, item, selected)) => {
            boot = Some(selected);
            Some(item)
        }
        None => None,
    };
    let boot = boot
        .map(|selected| -> Result<Option<Listed>> {
            let tag = selected["tag_name"].as_str().ok_or("Invalid release tag")?;
            let name = format!("couch-{tag}-ha100-boot.json");
            let has_boot = selected["assets"]
                .as_array()
                .is_some_and(|assets| assets.iter().any(|asset| asset["name"] == name));
            let paired = listed(&selected, tag, &name)?;
            if has_boot && paired.is_none() {
                return Err("The release's boot manifest has no usable asset digest".into());
            }
            Ok(paired)
        })
        .transpose()?
        .flatten();
    Ok((runtime, boot))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::Signer;
    fn listing(tag: &str, boot: bool) -> serde_json::Value {
        let mut assets = Vec::new();
        for kind in if boot {
            vec!["update", "boot"]
        } else {
            vec!["update"]
        } {
            let name = format!("couch-{tag}-ha100-{kind}.json");
            assets.push(serde_json::json!({"name":name,
                "browser_download_url":format!("{PREFIX}{tag}/{name}"),
                "size":512,"digest":format!("sha256:{}", "a".repeat(64))}));
        }
        serde_json::json!({"tag_name":tag,"draft":false,"prerelease":false,"assets":assets})
    }
    #[test]
    fn discovery_pairs_the_newest_runtime_with_its_own_boot_manifest() {
        let releases = vec![listing("v1.2.3", true), listing("v1.2.4", true)];
        let (runtime, boot) = select_listed(releases, Channel::Stable, "v1.2.3").unwrap();
        assert_eq!(runtime.unwrap().tag, "v1.2.4");
        assert_eq!(boot.unwrap().tag, "v1.2.4");
        let (runtime, boot) =
            select_listed(vec![listing("v1.2.3", true)], Channel::Stable, "v1.2.3").unwrap();
        assert!(runtime.is_none());
        assert_eq!(boot.unwrap().tag, "v1.2.3");
    }
    #[test]
    fn discovery_does_not_attach_an_older_boot_payload_to_a_runtime_only_release() {
        let releases = vec![listing("v1.2.3", true), listing("v1.2.4", false)];
        let (runtime, boot) = select_listed(releases, Channel::Stable, "v1.2.3").unwrap();
        assert_eq!(runtime.unwrap().tag, "v1.2.4");
        assert!(boot.is_none());
    }
    #[test]
    fn an_unverifiable_boot_companion_blocks_only_the_selected_release() {
        let mut broken = listing("v1.2.4", true);
        broken["assets"][1]["digest"] = serde_json::Value::Null;
        assert!(select_listed(vec![broken.clone()], Channel::Stable, "v1.2.3").is_err());
        let (runtime, boot) = select_listed(
            vec![broken, listing("v1.2.5", true)],
            Channel::Stable,
            "v1.2.3",
        )
        .unwrap();
        assert_eq!(runtime.unwrap().tag, "v1.2.5");
        assert_eq!(boot.unwrap().tag, "v1.2.5");
    }
    #[test]
    fn a_release_without_an_asset_digest_is_skipped_not_an_error() {
        let tag = "v0.1.0-alpha.20260914.9";
        let name = format!("couch-{tag}-ha100-update.json");
        let mut release = serde_json::json!({
            "assets": [{
                "name": name,
                "browser_download_url": format!("{PREFIX}{tag}/{name}"),
                "size": 512,
                "digest": format!("sha256:{}", "a".repeat(64)),
            }]
        });
        let found = listed(&release, tag, &name).unwrap().unwrap();
        assert_eq!(found.digest, "a".repeat(64));
        release["assets"][0]["digest"] = serde_json::Value::Null;
        assert!(listed(&release, tag, &name).unwrap().is_none());
        // A size that cannot be right is still the whole check's problem: it
        // means the listing itself is not what this code was written against.
        release["assets"][0]["digest"] = serde_json::json!(format!("sha256:{}", "a".repeat(64)));
        release["assets"][0]["size"] = serde_json::json!(0);
        assert!(listed(&release, tag, &name).is_err());
    }

    #[test]
    fn releases_are_found_under_either_repository_owner_and_no_other() {
        assert!(API.starts_with("https://api.github.com/repositories/"));
        let tag = "v1.2.4";
        let name = format!("couch-{tag}-ha100-update.json");
        for prefix in PREFIXES {
            let release = serde_json::json!({"assets": [{"name": name,
                "browser_download_url": format!("{prefix}{tag}/{name}"),
                "size": 512, "digest": format!("sha256:{}", "a".repeat(64))}]});
            let found = listed(&release, tag, &name).unwrap().unwrap();
            assert_eq!(found.url, format!("{prefix}{tag}/{name}"));
        }
        for url in [
            format!("https://github.com/other/couch/releases/download/{tag}/{name}"),
            format!("https://github.com/couch-os/couch/releases/download/{tag}/{name}"),
            format!("https://github.com/Couch-OS/couch-installer/releases/download/{tag}/{name}"),
            format!("{PREFIX}v1.2.5/{name}"),
            format!("{PREFIX}{tag}/{name}?download=1"),
        ] {
            let release = serde_json::json!({"assets": [{"name": name,
                "browser_download_url": url,
                "size": 512, "digest": format!("sha256:{}", "a".repeat(64))}]});
            assert!(listed(&release, tag, &name).unwrap().is_none(), "{url}");
        }
        assert!(fetch(
            &format!("https://github.com/other/couch/releases/download/{tag}/{name}"),
            1
        )
        .is_err_and(|e| e.to_string().contains("Unsupported update origin")));
    }
    #[test]
    fn signed_manifests_may_name_either_owner_but_not_another() {
        let key = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let sealed = |url: String| {
            let m = Manifest {
                schema: 1,
                model: "sanytron-ha100".into(),
                version: "v1.2.3".into(),
                kind: "runtime".into(),
                installable: true,
                notes: String::new(),
                url,
                size: 123,
                sha256: "a".repeat(64),
                files: Vec::new(),
                required_os_baseline: None,
            };
            let signature = hex(&key.sign(&serde_json::to_vec(&m).unwrap()).to_bytes());
            serde_json::to_vec(&SignedManifest {
                signed: m,
                signature,
            })
            .unwrap()
        };
        let key_bytes = key.verifying_key();
        for prefix in PREFIXES {
            let bytes = sealed(format!("{prefix}v1.2.3/couch-v1.2.3-ha100-runtime.tar.gz"));
            assert!(verify(&bytes, key_bytes.as_bytes(), "v1.2.3").is_ok());
        }
        let bytes = sealed("https://github.com/other/couch/releases/download/v1.2.3/couch-v1.2.3-ha100-runtime.tar.gz".into());
        assert!(verify(&bytes, key_bytes.as_bytes(), "v1.2.3").is_err());
    }
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
        // A dev build sits just above the alpha it was cut after, and below
        // the next alpha, so a remote on the dev channel follows promotions.
        let alpha = version("v0.1.0-alpha.20260913.122").unwrap();
        let dev = version("v0.1.0-alpha.20260913.122.dev").unwrap();
        let next = version("v0.1.0-alpha.20260914.130").unwrap();
        assert!(alpha < dev && dev < next);
    }
    #[test]
    fn channels_take_what_they_should() {
        let stable = version("v0.2.0").unwrap();
        let alpha = version("v0.1.0-alpha.20260913.122").unwrap();
        let dev = version("v0.1.0-alpha.20260913.122.dev").unwrap();
        let other = version("v0.1.0-beta.1").unwrap();
        assert!(!accepts(
            Channel::Alpha,
            &version("v0.1.0-alpha.20260913.122.dev.2").unwrap()
        ));
        for (channel, takes) in [
            (Channel::Stable, [true, false, false, false]),
            (Channel::Alpha, [true, true, false, false]),
            (Channel::Dev, [true, true, true, false]),
        ] {
            assert_eq!(
                [&stable, &alpha, &dev, &other].map(|v| accepts(channel, v)),
                takes
            );
        }
        assert_eq!(serde_json::to_string(&Channel::Dev).unwrap(), "\"dev\"");
    }
}
