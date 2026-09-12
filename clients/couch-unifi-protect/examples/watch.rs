//! Operator-run ten-second decode check. Takes only a private config path.
use couch_unifi_protect::{
    player::{Player, Status},
    settings::Settings,
};
use std::{
    io::Read,
    path::PathBuf,
    time::{Duration, Instant},
};
use zeroize::Zeroizing;
fn run() -> Result<(), &'static str> {
    let mut args = std::env::args_os().skip(1);
    let path = PathBuf::from(args.next().ok_or("Provide a private config file")?);
    if args.next().is_some() {
        return Err("Provide only the config file path");
    }
    let metadata = std::fs::symlink_metadata(&path).map_err(|_| "Cannot read private config")?;
    if !metadata.file_type().is_file() || metadata.len() > 98304 {
        return Err("Invalid private config");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err("Private config must have mode 0600");
        }
    }
    let mut bytes = Zeroizing::new(Vec::new());
    std::fs::File::open(path)
        .map_err(|_| "Cannot read config")?
        .take(98305)
        .read_to_end(&mut bytes)
        .map_err(|_| "Cannot read config")?;
    let mut value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| "Invalid config")?;
    let id = value
        .as_object_mut()
        .and_then(|v| v.remove("camera_id"))
        .and_then(|v| v.as_str().map(str::to_owned))
        .ok_or("Select camera_id in private config")?;
    let settings: Settings = serde_json::from_value(value).map_err(|_| "Invalid settings")?;
    settings
        .client()
        .map_err(|_| "Invalid settings or certificate trust")?;
    let player = Player::start(settings, id);
    let until = Instant::now() + Duration::from_secs(10);
    let mut frames = 0;
    while Instant::now() < until {
        if player.take_frame().is_some() {
            frames += 1;
        }
        if player.status() == Status::Unavailable {
            return Err(
                "Live decode unavailable; check media trust, codec and decoder installation",
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    drop(player);
    if frames == 0 {
        return Err("No decoded frames arrived within ten seconds");
    }
    println!("Decoded {frames} bounded frames; view closed.");
    Ok(())
}
fn main() {
    if let Err(message) = run() {
        eprintln!("{message}");
        std::process::exit(1);
    }
}
