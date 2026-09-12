use couch_coreelec::{couch_kodi::Kodi, Client, OsAction, SshConfig};
use std::{net::IpAddr, path::PathBuf};
fn main() {
    if let Err(e) = run() {
        eprintln!("couch-coreelec: {e}");
        std::process::exit(1);
    }
}
fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let usage="Usage: couch-coreelec discover | IP [status|play-pause|stop|next|previous|up|down|left|right|select|back|home|identity|service|restart-kodi|reboot|poweroff] [--confirm]";
    if args == ["discover"] {
        println!("{}", serde_json::to_string(&couch_coreelec::discover()?)?);
        return Ok(());
    }
    let ip: IpAddr = args.first().ok_or(usage)?.parse()?;
    let action = args.get(1).map(String::as_str).unwrap_or("status");
    let disruptive = matches!(action, "restart-kodi" | "reboot" | "poweroff");
    if args.len() > 2 && !(disruptive && args.len() == 3 && args[2] == "--confirm") {
        return Err(usage.into());
    }
    if disruptive && args.get(2).map(String::as_str) != Some("--confirm") {
        return Err(
            "this action interrupts playback or powers down the box; pass --confirm".into(),
        );
    }
    let port = std::env::var("COUCH_COREELEC_KODI_PORT")
        .unwrap_or("9090".into())
        .parse()?;
    let mut client = Client::new(Kodi::tcp(ip.to_string(), port));
    if matches!(action, "identity" | "service") || disruptive {
        let path = |key| -> Result<PathBuf, Box<dyn std::error::Error>> {
            Ok(std::env::var(key)
                .map_err(|_| format!("set {key} to an existing absolute path"))?
                .into())
        };
        client = client.with_ssh(SshConfig::new(
            ip,
            std::env::var("COUCH_COREELEC_SSH_PORT")
                .unwrap_or("22".into())
                .parse()?,
            std::env::var("COUCH_COREELEC_SSH_USER").unwrap_or("root".into()),
            path("COUCH_COREELEC_SSH_KEY")?,
            path("COUCH_COREELEC_KNOWN_HOSTS")?,
        )?);
    }
    match action {
        "status" => {
            client.kodi.ping()?;
            println!("{:?}", client.kodi.now_playing()?);
        }
        "play-pause" => {
            client.kodi.play_pause()?;
        }
        "stop" => {
            client.kodi.stop()?;
        }
        "next" => {
            client.kodi.next()?;
        }
        "previous" => {
            client.kodi.previous()?;
        }
        "up" => client.kodi.up()?,
        "down" => client.kodi.down()?,
        "left" => client.kodi.left()?,
        "right" => client.kodi.right()?,
        "select" => client.kodi.select()?,
        "back" => client.kodi.back()?,
        "home" => client.kodi.home()?,
        "identity" => println!("{}", serde_json::to_string(&client.identity()?)?),
        "service" => println!("{}", serde_json::to_string(&client.kodi_service()?)?),
        "restart-kodi" => client.action(OsAction::RestartKodi)?,
        "reboot" => client.action(OsAction::Reboot)?,
        "poweroff" => client.action(OsAction::PowerOff)?,
        _ => return Err(usage.into()),
    }
    Ok(())
}
