//! Explicit, interactive physical-device probe; never auto-discovers or controls playback.
use couch_appletv::metadata::{Client, Credentials, Pairing, Settings};
use std::{
    fs::OpenOptions,
    io::{Read, Write},
    path::Path,
    time::Duration,
};
fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 5 || !matches!(args[1].as_str(), "pair" | "watch") {
        return Err(
            "Usage: now_playing pair|watch ADDRESS AIRPLAY_PORT PRIVATE_CREDENTIAL_FILE".into(),
        );
    }
    let settings = Settings::new(args[2].parse()?, args[3].parse()?)?;
    let path = Path::new(&args[4]);
    if args[1] == "pair" {
        // Reserve before displaying a PIN; never overwrite an existing pairing.
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(path)?;
        let result = (|| -> Result<(), Box<dyn std::error::Error>> {
            let pairing = Pairing::begin(&settings)?;
            print!("PIN shown on Apple TV: ");
            std::io::stdout().flush()?;
            let mut pin = String::new();
            std::io::stdin().read_line(&mut pin)?;
            let credentials = pairing.finish(pin.trim())?;
            file.write_all(&serde_json::to_vec(&credentials)?)?;
            file.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            drop(file);
            let _ = std::fs::remove_file(path);
        }
        result?;
        println!("AirPlay pairing saved. Use watch to observe metadata.");
    } else {
        let mut bytes = vec![];
        std::fs::File::open(path)?
            .take(16_385)
            .read_to_end(&mut bytes)?;
        if bytes.len() > 16_384 {
            return Err("Credential file is too large".into());
        }
        let credentials: Credentials = serde_json::from_slice(&bytes)?;
        let mut client = Client::connect(&settings, &credentials)?;
        println!("Connected; waiting for metadata. Ctrl-C exits.");
        loop {
            if let Some(metadata) = client.poll()? {
                println!("{}", serde_json::to_string(&metadata)?);
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    Ok(())
}
