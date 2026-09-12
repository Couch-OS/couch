use couch_sonos::{Client, Playback};
fn main() {
    if let Err(error) = run() {
        eprintln!("couch-sonos: {error}");
        std::process::exit(1);
    }
}
fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let usage="Usage: couch-sonos discover | IPv4 [status|play|pause|stop|next|previous|volume 0..100|mute|unmute]";
    if args.first().map(String::as_str) == Some("discover") && args.len() == 1 {
        println!("{}", serde_json::to_string(&couch_sonos::discover()?)?);
        return Ok(());
    }
    let address = args.first().ok_or(usage)?.parse()?;
    let action = args.get(1).map(String::as_str).unwrap_or("status");
    if args.len() > if action == "volume" { 3 } else { 2 } {
        return Err(usage.into());
    }
    let volume = if action == "volume" {
        let v: u8 = args.get(2).ok_or(usage)?.parse()?;
        if v > 100 {
            return Err(usage.into());
        }
        Some(v)
    } else {
        None
    };
    let command = match action {
        "play" => Some(Playback::Play),
        "pause" => Some(Playback::Pause),
        "stop" => Some(Playback::Stop),
        "next" => Some(Playback::Next),
        "previous" => Some(Playback::Previous),
        "status" | "volume" | "mute" | "unmute" => None,
        _ => return Err(usage.into()),
    };
    let client = Client::connect(address)?;
    if let Some(command) = command {
        client.playback(command)?;
    } else {
        match action {
            "volume" => client.set_volume(volume.unwrap())?,
            "mute" => client.set_muted(true)?,
            "unmute" => client.set_muted(false)?,
            _ => {
                println!("{}", serde_json::to_string(&client.status()?)?);
                return Ok(());
            }
        }
    }
    println!("{{\"acknowledged\":true}}");
    Ok(())
}
