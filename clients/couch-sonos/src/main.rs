use couch_sonos::{Client, Playback, SourceId};
fn main() {
    if let Err(error) = run() {
        eprintln!("couch-sonos: {error}");
        std::process::exit(1);
    }
}
fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let usage="Usage: couch-sonos discover | IPv4 [status|play|pause|play-pause|stop|next|previous|volume 0..100|mute|unmute|sources|source tv|line-in|favorite:ID|playlist:ID|now]";
    if args.first().map(String::as_str) == Some("discover") && args.len() == 1 {
        println!("{}", serde_json::to_string(&couch_sonos::discover()?)?);
        return Ok(());
    }
    let address = args.first().ok_or(usage)?.parse()?;
    let action = args.get(1).map(String::as_str).unwrap_or("status");
    if args.len()
        > if action == "volume" || action == "source" {
            3
        } else {
            2
        }
    {
        return Err(usage.into());
    }
    let source = if action == "source" {
        Some(match args.get(2).ok_or(usage)?.as_str() {
            "tv" => SourceId::HomeTheater,
            "line-in" => SourceId::LineIn,
            other => match other.split_once(':') {
                Some(("favorite", id)) if !id.is_empty() => SourceId::Favorite(id.into()),
                Some(("playlist", id)) if !id.is_empty() => SourceId::Playlist(id.into()),
                _ => return Err(usage.into()),
            },
        })
    } else {
        None
    };
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
        "play-pause" => Some(Playback::PlayPause),
        // The Control API has no stop; this pauses the group.
        "stop" => Some(Playback::Stop),
        "next" => Some(Playback::Next),
        "previous" => Some(Playback::Previous),
        "status" | "volume" | "mute" | "unmute" | "sources" | "source" | "now" => None,
        _ => return Err(usage.into()),
    };
    // A configured key that cannot be a header is replaced by the placeholder,
    // which otherwise looks exactly like having configured nothing. Say so once,
    // on stderr so it cannot corrupt the JSON on stdout, and never print the value.
    let key = couch_sonos::api_key_choice();
    if key.rejected {
        eprintln!(
            "couch-sonos: the configured API key is not usable as a header and was ignored; \
             falling back to the built-in placeholder"
        );
    }
    let client = Client::connect_with_key(address, &key.key)?;
    if let Some(command) = command {
        client.playback(command)?;
    } else {
        match action {
            "volume" => client.set_volume(volume.unwrap())?,
            "mute" => client.set_muted(true)?,
            "unmute" => client.set_muted(false)?,
            "source" => client.select_source(&source.unwrap())?,
            "now" => {
                println!("{}", serde_json::to_string(&client.snapshot()?)?);
                return Ok(());
            }
            "sources" => {
                // One line per source: the argument `source` takes, then the row text.
                for s in client.sources()? {
                    let argument = match &s.id {
                        SourceId::HomeTheater => "tv".to_owned(),
                        SourceId::LineIn => "line-in".to_owned(),
                        SourceId::Favorite(id) => format!("favorite:{id}"),
                        SourceId::Playlist(id) => format!("playlist:{id}"),
                    };
                    println!("{argument}\t{}\t{}", s.name, s.detail);
                }
                return Ok(());
            }
            _ => {
                println!("{}", serde_json::to_string(&client.status()?)?);
                return Ok(());
            }
        }
    }
    println!("{{\"acknowledged\":true}}");
    Ok(())
}
