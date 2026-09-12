mod access;
mod cgi;
mod service;
fn main() {
    let result = match std::env::args().nth(1).as_deref() {
        Some("scan-json") => {
            use std::io::Read;
            let mut raw = String::new();
            match std::io::stdin().take(65536).read_to_string(&mut raw) {
                Ok(_) => {
                    let networks = couch_system::network::parse_scan(&raw);
                    let output:Vec<_>=networks.iter().map(|n|serde_json::json!({"ssid":n.ssid,"signal":n.dbm,"secure":n.secured,"supported":n.supported})).collect();
                    println!("{}", serde_json::json!(output));
                    Ok(())
                }
                Err(_) => Err("Could not read scan results".into()),
            }
        }
        Some("health") => match couch_system::client::call(couch_system::protocol::Request::Health)
        {
            Ok(couch_system::protocol::Reply::Ready) => Ok(()),
            _ => Err("System service is unavailable".into()),
        },
        Some("serve") => service::serve(),
        Some("cgi") => cgi::run(),
        Some("hotspot") => couch_system::client::action(couch_system::protocol::Request::Hotspot),
        Some("ssh-start") => couch_system::client::action(couch_system::protocol::Request::SshAuto),
        _ => Err("Usage: couch-system serve | cgi | hotspot | ssh-start".into()),
    };
    if let Err(error) = result {
        eprintln!("couch-system: {error}");
        std::process::exit(1);
    }
}
