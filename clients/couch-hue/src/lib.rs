//! Direct local Hue API v2 light control. Use from a worker thread.
pub mod live;
pub mod resources;
pub mod settings;
mod tls;
pub use couch_ha::{Command, Light};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
pub type Result<T> = std::result::Result<T, Error>;
#[derive(Debug, Clone, PartialEq)]
pub enum Error {
    Configuration,
    LinkButton,
    Authentication,
    Transport,
    Response,
    Unavailable,
    Brightness,
    ColourTemperature,
    Rejected,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self{
        Self::Configuration=>"Check the Hue bridge address and saved pairing",
        Self::LinkButton=>"Press the round link button on your Hue Bridge, then click Pair bridge again",
        Self::Authentication=>"Hue rejected the application key; pair the bridge again",
        Self::Transport=>"Cannot reach Hue over HTTPS, or its certificate changed; check the address or pair again",
        Self::Response=>"Hue returned an invalid or oversized response",
        Self::Unavailable=>"This Hue light is missing or unreachable",
        Self::Brightness=>"Brightness requires a dimmable light and a value from 0 to 100",
        Self::ColourTemperature=>"Colour temperature requires a tunable-white light and a value inside its range",
        Self::Rejected=>"Hue rejected the light command",
    })
    }
}
impl std::error::Error for Error {}
fn transport(e: ureq::Error) -> Error {
    match e {
        ureq::Error::StatusCode(401 | 403) => Error::Authentication,
        _ => Error::Transport,
    }
}
pub fn valid_id(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(i, b)| {
            if [8, 13, 18, 23].contains(&i) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        })
}
fn base(address: &str) -> Result<String> {
    let text = if address.contains("://") {
        address.to_string()
    } else {
        format!("https://{address}")
    };
    let u = url::Url::parse(&text).map_err(|_| Error::Configuration)?;
    if u.scheme() != "https"
        || u.host_str().is_none()
        || u.path() != "/"
        || !u.username().is_empty()
        || u.password().is_some()
        || u.query().is_some()
        || u.fragment().is_some()
    {
        return Err(Error::Configuration);
    }
    Ok(u.as_str().trim_end_matches('/').into())
}
fn response(mut r: ureq::http::Response<ureq::Body>) -> Result<Value> {
    if !r.status().is_success() {
        return Err(Error::Rejected);
    }
    r.body_mut()
        .with_config()
        .limit(4 * 1024 * 1024)
        .read_json()
        .map_err(|_| Error::Response)
}
fn data(v: Value) -> Result<Vec<Value>> {
    let errors = v["errors"].as_array().ok_or(Error::Response)?;
    if !errors.is_empty() {
        return Err(Error::Rejected);
    }
    v["data"].as_array().cloned().ok_or(Error::Response)
}
pub struct Hue {
    base: String,
    key: String,
    agent: ureq::Agent,
}
impl std::fmt::Debug for Hue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Hue").finish_non_exhaustive()
    }
}
impl Hue {
    pub fn new(address: &str, key: &str, certificate: &[u8]) -> Result<Self> {
        if key.is_empty()
            || !key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            || certificate.is_empty()
        {
            return Err(Error::Configuration);
        }
        Ok(Self {
            base: base(address)?,
            key: key.into(),
            agent: tls::agent(Arc::new(Mutex::new(certificate.to_vec()))),
        })
    }
    /// The first connection trusts the selected LAN bridge. Save its certificate
    /// only after physical-link-button pairing and successful light discovery.
    pub fn pair(address: &str) -> Result<settings::Settings> {
        let base = base(address)?;
        let pin = Arc::new(Mutex::new(Vec::new()));
        let agent = tls::agent(pin.clone());
        let v = response(
            agent
                .post(format!("{base}/api"))
                .send_json(json!({"devicetype":"couch#remote","generateclientkey":true}))
                .map_err(transport)?,
        )?;
        let rows = v.as_array().ok_or(Error::Response)?;
        for row in rows {
            if row["error"]["type"] == 101 {
                return Err(Error::LinkButton);
            }
            if row.get("error").is_some() {
                return Err(Error::Authentication);
            }
        }
        let key = rows
            .first()
            .and_then(|r| r["success"]["username"].as_str())
            .ok_or(Error::Response)?;
        let settings = settings::Settings {
            url: base,
            token: key.into(),
            certificate: pin.lock().map_err(|_| Error::Response)?.clone(),
        };
        settings.client()?.lights()?;
        Ok(settings)
    }
    pub fn lights(&self) -> Result<Vec<Light>> {
        let all = self.raw_resources()?;
        Self::parse_lights(&all)
    }
    fn raw_resources(&self) -> Result<Vec<Value>> {
        data(response(
            self.agent
                .get(format!("{}/clip/v2/resource", self.base))
                .header("hue-application-key", &self.key)
                .call()
                .map_err(transport)?,
        )?)
    }
    fn parse_lights(all: &[Value]) -> Result<Vec<Light>> {
        let mut lights = Vec::new();
        for v in all.iter().filter(|r| r["type"] == "light") {
            let id = v["id"]
                .as_str()
                .filter(|s| valid_id(s))
                .ok_or(Error::Response)?;
            let owner = v["owner"]["rid"].as_str();
            let connected = owner.is_some()
                && all.iter().any(|r| {
                    r["type"] == "zigbee_connectivity"
                        && r["owner"]["rid"].as_str() == owner
                        && r["status"] == "connected"
                });
            let on = if connected {
                v["on"]["on"].as_bool()
            } else {
                None
            };
            let dimmable = v["dimming"].is_object();
            // Hue keeps the saved dimming level while powered off.
            let brightness_percent = if on.is_some() {
                v["dimming"]["brightness"]
                    .as_f64()
                    .filter(|p| p.is_finite() && (0.0..=100.0).contains(p))
                    .map(|p| p.round() as u8)
            } else {
                None
            };
            // Hue's own colour-temperature schema, coolest mirek first. A lamp
            // in a colour mode reports `mirek_valid: false`, which is not a
            // reading: the screen shows no Kelvin rather than a stale one.
            let temperature = &v["color_temperature"];
            let schema = &temperature["mirek_schema"];
            let mirek_range = match (
                schema["mirek_minimum"].as_u64(),
                schema["mirek_maximum"].as_u64(),
            ) {
                (Some(cool), Some(warm)) if cool <= warm && warm <= u64::from(u16::MAX) => {
                    Some((cool as u16, warm as u16))
                }
                _ => None,
            };
            let mirek = mirek_range
                .filter(|_| on.is_some() && temperature["mirek_valid"].as_bool() != Some(false))
                .and_then(|(cool, warm)| {
                    temperature["mirek"]
                        .as_u64()
                        .filter(|m| (u64::from(cool)..=u64::from(warm)).contains(m))
                        .map(|m| m as u16)
                });
            lights.push(Light {
                entity_id: id.into(),
                name: v["metadata"]["name"].as_str().unwrap_or(id).into(),
                on,
                brightness_percent,
                dimmable,
                mirek,
                mirek_range,
            });
        }
        lights.sort_by(|a, b| a.name.cmp(&b.name).then(a.entity_id.cmp(&b.entity_id)));
        Ok(lights)
    }
    pub fn light(&self, id: &str) -> Result<Light> {
        if !valid_id(id) {
            return Err(Error::Configuration);
        }
        self.lights()?
            .into_iter()
            .find(|l| l.entity_id == id)
            .ok_or(Error::Unavailable)
    }
    pub fn command(&self, id: &str, command: Command) -> Result<()> {
        if matches!(command,Command::Brightness(p) if p>100) {
            return Err(Error::Brightness);
        }
        let state = self.control_state(id)?;
        self.command_for_state(&state, command)
    }
    /// Toggle from a fresh observation. Returns the bridge-acknowledged target,
    /// not a second observation of the bulb; callers should reconcile later.
    pub fn toggle(&self, id: &str) -> Result<Light> {
        let mut state = self.light(id)?;
        let on = !state.on.ok_or(Error::Unavailable)?;
        self.command_for_state(&state, if on { Command::On } else { Command::Off })?;
        state.on = Some(on);
        state.brightness_percent = if on { None } else { Some(0) };
        Ok(state)
    }
    fn command_for_state(&self, state: &Light, command: Command) -> Result<()> {
        let id = state.entity_id.as_str();
        if state.on.is_none() {
            return Err(Error::Unavailable);
        }
        let body = match command {
            Command::On => json!({"on":{"on":true}}),
            Command::Off | Command::Brightness(0) => json!({"on":{"on":false}}),
            Command::Brightness(p) => {
                if !state.dimmable {
                    return Err(Error::Brightness);
                }
                json!({"on":{"on":true},"dimming":{"brightness":p}})
            }
            Command::Mirek(m) => {
                if !state
                    .mirek_range
                    .is_some_and(|(cool, warm)| (cool..=warm).contains(&m))
                {
                    return Err(Error::ColourTemperature);
                }
                json!({"on":{"on":true},"color_temperature":{"mirek":m}})
            }
        };
        self.write_light(id, body)
    }
    /// Send an explicit power target without a preflight GET. The caller owns
    /// state freshness and reachability checks (see live::Live).
    pub fn set_power(&self, id: &str, on: bool) -> Result<()> {
        if !resources::valid_control(id) || id.starts_with("scene:") {
            return Err(Error::Configuration);
        }
        self.write_light(id, json!({"on":{"on":on}}))
    }
    fn write_light(&self, id: &str, body: Value) -> Result<()> {
        let (kind, id) = if let Some(id) = id.strip_prefix("room:") {
            ("grouped_light", id)
        } else {
            ("light", id)
        };
        self.write_resource(kind, id, body)
    }
    fn write_resource(&self, kind: &str, id: &str, body: Value) -> Result<()> {
        if !valid_id(id) {
            return Err(Error::Configuration);
        }
        let updated = data(response(
            self.agent
                .put(format!("{}/clip/v2/resource/{kind}/{id}", self.base))
                .header("hue-application-key", &self.key)
                .send_json(body)
                .map_err(transport)?,
        )?)?;
        if !updated.iter().any(|r| r["rid"] == id && r["rtype"] == kind) {
            return Err(Error::Response);
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    /// Hue's own colour-temperature schema, and the write that follows it.
    #[test]
    fn colour_temperature_comes_from_the_schema_and_is_written_as_mirek() {
        let id = "00000000-0000-0000-0000-000000000001";
        let lamp = |temperature: Value| {
            vec![
                json!({"type":"zigbee_connectivity","owner":{"rid":"device"},"status":"connected"}),
                json!({"type":"light","id":id,"owner":{"rid":"device"},"on":{"on":true},
                       "dimming":{"brightness":40.0},"metadata":{"name":"Desk lamp"},
                       "color_temperature":temperature}),
            ]
        };
        let schema = json!({"mirek":370,"mirek_valid":true,
            "mirek_schema":{"mirek_minimum":153,"mirek_maximum":500}});
        let tunable = Hue::parse_lights(&lamp(schema.clone())).unwrap();
        assert_eq!(tunable[0].mirek, Some(370));
        assert_eq!(tunable[0].mirek_range, Some((153, 500)));
        // A lamp showing a colour rather than a white says its mirek is not
        // valid. The range is still true; the reading is not, so there is none.
        let colour = Hue::parse_lights(&lamp(json!({"mirek":370,"mirek_valid":false,
            "mirek_schema":{"mirek_minimum":153,"mirek_maximum":500}})))
        .unwrap();
        assert_eq!(colour[0].mirek, None);
        assert_eq!(colour[0].mirek_range, Some((153, 500)));
        // No schema at all, and a mirek outside the one given: not tunable,
        // and not a reading.
        assert_eq!(
            Hue::parse_lights(&lamp(Value::Null)).unwrap()[0].mirek_range,
            None
        );
        let outside = Hue::parse_lights(&lamp(json!({"mirek":900,"mirek_valid":true,
            "mirek_schema":{"mirek_minimum":153,"mirek_maximum":500}})))
        .unwrap();
        assert_eq!(outside[0].mirek, None);

        // The write is a mirek at the light, and turns it on to show it.
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let address = server.server_addr();
        let worker = std::thread::spawn(move || {
            let mut request = server
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap()
                .unwrap();
            assert_eq!(request.url(), format!("/clip/v2/resource/light/{id}"));
            let mut text = String::new();
            std::io::Read::read_to_string(request.as_reader(), &mut text).unwrap();
            assert_eq!(
                serde_json::from_str::<Value>(&text).unwrap(),
                json!({"on":{"on":true},"color_temperature":{"mirek":250}})
            );
            request
                .respond(tiny_http::Response::from_string(
                    json!({"errors":[],"data":[{"rid":id,"rtype":"light"}]}).to_string(),
                ))
                .unwrap();
        });
        let client = Hue {
            base: format!("http://{address}"),
            key: "fixture".into(),
            agent: ureq::Agent::new_with_defaults(),
        };
        client
            .command_for_state(&tunable[0], Command::Mirek(250))
            .unwrap();
        worker.join().unwrap();
        // A value outside the light's range, and a light with no range, are
        // refused here rather than sent to the bridge.
        assert_eq!(
            client.command_for_state(&tunable[0], Command::Mirek(600)),
            Err(Error::ColourTemperature)
        );
        assert_eq!(
            client.command_for_state(
                &Hue::parse_lights(&lamp(Value::Null)).unwrap()[0],
                Command::Mirek(250)
            ),
            Err(Error::ColourTemperature)
        );
    }
    #[test]
    fn toggle_reads_once_and_only_returns_target_after_acknowledgement() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        for (connected, reject) in [(true, false), (false, false), (true, true)] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let id = "00000000-0000-0000-0000-000000000001";
            let server = std::thread::spawn(move || {
                for request in 0..if connected { 2 } else { 1 } {
                    let (mut socket, _) = listener.accept().unwrap();
                    socket
                        .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                        .unwrap();
                    let mut bytes = Vec::new();
                    let header_end = loop {
                        let mut b = [0];
                        socket.read_exact(&mut b).unwrap();
                        bytes.push(b[0]);
                        if bytes.ends_with(b"\r\n\r\n") {
                            break bytes.len();
                        }
                    };
                    let header = String::from_utf8(bytes).unwrap();
                    let length = header
                        .lines()
                        .find_map(|l| {
                            let (key, value) = l.split_once(':')?;
                            key.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    let mut body = vec![0; length];
                    socket.read_exact(&mut body).unwrap();
                    assert!(header_end > 0);
                    let value = if request == 0 {
                        assert!(header.starts_with("GET /clip/v2/resource "));
                        json!({"errors":[], "data":[
                            {"type":"light","id":id,"owner":{"rid":"device"},"on":{"on":false}},
                            {"type":"zigbee_connectivity","owner":{"rid":"device"},"status":if connected {"connected"} else {"disconnected"}}
                        ]})
                    } else {
                        assert!(header.starts_with(&format!("PUT /clip/v2/resource/light/{id} ")));
                        assert_eq!(serde_json::from_slice::<Value>(&body).unwrap(),json!({"on":{"on":true}}));
                        if reject { json!({"errors":[{}],"data":[]}) }
                        else { json!({"errors":[],"data":[{"rid":id,"rtype":"light"}]}) }
                    }.to_string();
                    write!(
                        socket,
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        value.len(),
                        value
                    )
                    .unwrap();
                }
            });
            let client = Hue {
                base: format!("http://{address}"),
                key: "fixture".into(),
                agent: ureq::Agent::new_with_defaults(),
            };
            let result = client.toggle(id);
            if !connected {
                assert_eq!(result.unwrap_err(), Error::Unavailable);
            } else if reject {
                assert_eq!(result.unwrap_err(), Error::Rejected);
            } else {
                let state = result.unwrap();
                assert_eq!(state.on, Some(true));
                assert_eq!(state.brightness_percent, None);
            }
            server.join().unwrap();
        }
    }
    #[test]
    fn rejects_unsafe_addresses_and_ids() {
        for a in [
            "http://192.168.1.1",
            "https://a/path",
            "https://user:secret@host",
            "https://host?x=1",
        ] {
            assert!(base(a).is_err())
        }
        assert_eq!(base("192.168.1.157").unwrap(), "https://192.168.1.157");
        assert!(!valid_id("../config"));
        assert!(valid_id("00000000-0000-0000-0000-000000000001"));
    }
    #[test]
    fn http_success_can_contain_hue_errors() {
        assert_eq!(
            data(json!({"errors":[{"description":"secret"}],"data":[]})),
            Err(Error::Rejected)
        );
        assert!(data(json!({"data":[]})).is_err());
    }
}
