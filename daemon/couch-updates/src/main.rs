use std::{
    io::{Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::Path,
};
fn run() -> Result<(), String> {
    let args: Vec<_> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("keygen") && args.len() == 3 {
        let mut seed = [0; 32];
        std::fs::File::open("/dev/urandom")
            .and_then(|mut f| f.read_exact(&mut seed))
            .map_err(|_| "Could not generate signing seed")?;
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&args[2])
            .map_err(|_| "Signing seed must be a new private file")?;
        let encoded: String = seed.iter().map(|b| format!("{b:02x}")).collect();
        file.write_all(encoded.as_bytes())
            .and_then(|_| file.sync_all())
            .map_err(|_| "Could not save signing seed")?;
        let key = ed25519_dalek::SigningKey::from_bytes(&seed);
        println!(
            "{}",
            key.verifying_key()
                .as_bytes()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        );
        seed.fill(0);
        return Ok(());
    }
    let boot = args.get(1).map(String::as_str) == Some("boot");
    if args.len() != if boot { 7 } else { 5 } {
        return Err("Usage: couch-updates keygen PRIVATE_SEED | couch-updates CLEAN_RUNTIME VERSION PRIVATE_SEED NEW_OUTPUT | couch-updates boot BOOT_PAYLOAD CLEAN_RUNTIME VERSION PRIVATE_SEED NEW_OUTPUT".into());
    }
    let seed_path = if boot { &args[5] } else { &args[3] };
    let encoded =
        std::fs::read_to_string(seed_path).map_err(|_| "Could not read private signing seed")?;
    let encoded = encoded.trim();
    if encoded.len() != 64 || !encoded.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("Signing seed must contain 32 hexadecimal bytes".into());
    }
    let mut seed = [0; 32];
    for (i, byte) in seed.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&encoded[2 * i..2 * i + 2], 16)
            .map_err(|_| "Invalid signing seed")?;
    }
    let result = if boot {
        couch_updates::bundle_boot(
            Path::new(&args[2]),
            Path::new(&args[3]),
            &args[4],
            &seed,
            Path::new(&args[6]),
        )
    } else {
        couch_updates::bundle(Path::new(&args[1]), &args[2], &seed, Path::new(&args[4]))
    };
    seed.fill(0);
    println!("Public update key: {}", result?);
    Ok(())
}
fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
