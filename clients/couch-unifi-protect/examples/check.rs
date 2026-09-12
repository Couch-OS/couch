//! Explicit operator-run read-only check. Credentials never appear in arguments.
use couch_unifi_protect::{ApiKey, Client, Quality};
use serde::Deserialize;
use std::{fs, path::PathBuf, time::Duration};
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    origin: String,
    api_key: String,
    private_ca_pem: Option<PathBuf>,
    certificate_sha256: Option<String>,
    stream_host: Option<String>,
    camera_id: Option<String>,
}
fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().collect();
    if args.len() != 2 {
        return Err("Usage: check PRIVATE_CONFIG.json".into());
    }
    let path = PathBuf::from(&args[1]);
    let meta = fs::symlink_metadata(&path)?;
    if !meta.is_file() || meta.len() > 16384 {
        return Err("Expected bounded regular private config".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o077 != 0 {
            return Err("Config must have mode 0600".into());
        }
    }
    let bytes = zeroize::Zeroizing::new(fs::read(path)?);
    let config: Config =
        serde_json::from_slice(&bytes).map_err(|_| "Invalid private configuration")?;
    let ca = config
        .private_ca_pem
        .map(|p| -> Result<Vec<u8>, std::io::Error> {
            if fs::metadata(&p)?.len() > 65536 {
                return Err(std::io::Error::other("CA certificate exceeds limit"));
            }
            fs::read(p)
        })
        .transpose()?;
    if config.certificate_sha256.is_some() && ca.is_some() {
        return Err("Choose private CA or explicit certificate pin, not both".into());
    }
    let key = ApiKey::new(config.api_key)?;
    let mut client = match config.certificate_sha256 {
        Some(pin) => Client::new_pinned(&config.origin, key, &pin, Duration::from_secs(10))?,
        None => Client::new(&config.origin, key, ca.as_deref(), Duration::from_secs(10))?,
    };
    if let Some(host) = config.stream_host {
        client = client.with_stream_host(&host)?;
    }
    println!("Protect version: {}", client.application_version()?);
    println!("Visible cameras: {}", client.cameras()?.len());
    if let Some(id) = config.camera_id {
        let view = client.live_view(&id, Quality::Low, Duration::from_secs(30))?;
        println!(
            "Low-quality RTSPS descriptor validated (URL withheld; no video connection opened)."
        );
        view.close();
    }
    Ok(())
}
fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
