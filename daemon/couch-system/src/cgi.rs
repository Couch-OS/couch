use couch_system::{
    client,
    protocol::{Reply, Request},
};
use std::{collections::BTreeMap, io::Read};
fn decode(value: &str) -> Result<String, String> {
    let mut result = Vec::new();
    let mut bytes = value.bytes();
    while let Some(b) = bytes.next() {
        match b {
            b'+' => result.push(b' '),
            b'%' => {
                let a = bytes
                    .next()
                    .and_then(|b| (b as char).to_digit(16))
                    .ok_or("Invalid form encoding")?;
                let b = bytes
                    .next()
                    .and_then(|b| (b as char).to_digit(16))
                    .ok_or("Invalid form encoding")?;
                result.push((a * 16 + b) as u8);
            }
            _ => result.push(b),
        }
    }
    String::from_utf8(result).map_err(|_| "Invalid form text".into())
}
fn form(body: &str) -> Result<BTreeMap<String, String>, String> {
    let mut fields = BTreeMap::new();
    for pair in body.split('&') {
        let (key, value) = pair.split_once('=').ok_or("Invalid form field")?;
        if fields.insert(decode(key)?, decode(value)?).is_some() {
            return Err("Duplicate form field".into());
        }
    }
    Ok(fields)
}
pub fn run() -> Result<(), String> {
    let name = std::env::var("SCRIPT_NAME").unwrap_or_default();
    if name.ends_with("/scan") {
        println!("Content-Type: application/json\r\nCache-Control: no-store\r\n\r");
        // Cached before AP transition; live scans can disrupt the requesting phone.
        let networks = match client::call(Request::Scan) {
            Ok(Reply::Networks(Ok(networks))) => networks,
            _ => Vec::new(),
        };
        let value: Vec<_> = networks.iter().map(|n| serde_json::json!({"ssid":n.ssid,"signal":n.dbm,"secure":n.secured,"supported":n.supported})).collect();
        println!("{}", serde_json::json!(value));
        return Ok(());
    }
    println!("Content-Type: text/plain\r\nCache-Control: no-store\r\n\r");
    let result = (|| {
        if std::env::var("REQUEST_METHOD").as_deref() != Ok("POST") {
            return Err("POST required".into());
        }
        let size = std::env::var("CONTENT_LENGTH")
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            .filter(|n| *n <= 16384)
            .ok_or("Invalid request size")?;
        let mut bytes = vec![0; size];
        std::io::stdin()
            .read_exact(&mut bytes)
            .map_err(|_| "Incomplete request")?;
        let body = String::from_utf8(bytes).map_err(|_| "Invalid form text")?;
        let mut fields = form(&body)?;
        let request = match name.as_str() {
            "/cgi-bin/save" => Request::PortalJoin {
                ssid: fields.remove("ssid").ok_or("Network name required")?,
                password: fields.remove("psk").unwrap_or_default(),
            },
            "/cgi-bin/enroll" => Request::EnrollKey {
                key: fields.remove("key").ok_or("Public key required")?,
            },
            "/cgi-bin/setpw" => Request::SetPassword {
                password: fields.remove("pw").ok_or("Password required")?,
            },
            _ => return Err("Unknown operation".into()),
        };
        if !fields.is_empty() {
            return Err("Unexpected form field".into());
        }
        match client::call(request)? {
            Reply::Done(result) => result,
            _ => Err("Unexpected system reply".into()),
        }
    })();
    match result {
        Ok(()) if name.ends_with("/save")=>println!("OK Testing the connection. Saved only if the test passes; the hotspot returns on failure."),
        Ok(())=>println!("OK Access configuration saved."),
        Err(error)=>println!("ERR {error}"),
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn form_decoding_is_literal_and_rejects_ambiguity() {
        let fields = form("ssid=A%2B%26%22&psk=a%5Cb%20c").unwrap();
        assert_eq!(fields["ssid"], "A+&\"");
        assert_eq!(fields["psk"], "a\\b c");
        for body in ["ssid=a&ssid=b", "ssid=%G0", "ssid=%A", "ssid=%ff"] {
            assert!(form(body).is_err());
        }
    }
}
