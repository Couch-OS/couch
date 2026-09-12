//! Blocking local Sonos UPnP client. Run on a worker thread, never the GUI thread.
//! Playback affects the selected coordinator's group; volume/mute address one player.
use serde::Serialize;
use std::{
    collections::BTreeSet,
    net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket},
    time::{Duration, Instant},
};

const LIMIT: u64 = 512 * 1024;
const AV: &str = "urn:schemas-upnp-org:service:AVTransport:1";
const RC: &str = "urn:schemas-upnp-org:service:RenderingControl:1";
const ZG: &str = "urn:schemas-upnp-org:service:ZoneGroupTopology:1";
const ST: &str = "urn:schemas-upnp-org:device:ZonePlayer:1";
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    Transport,
    Response,
    Unsupported,
    Http(u16),
    Soap(u16),
    Volume,
    Cancelled,
    Command,
    NotCoordinator { coordinator: String },
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transport => f.write_str("Cannot reach Sonos within the network timeout"),
            Self::Response => f.write_str("Invalid or oversized Sonos response"),
            Self::Unsupported => {
                f.write_str("Player does not advertise the required Sonos UPnP services")
            }
            Self::Http(code) => write!(f, "Sonos HTTP error {code}"),
            Self::Soap(code) => write!(f, "Sonos UPnP error {code}"),
            Self::Cancelled => f.write_str("Sonos command expired before dispatch"),
            Self::Command => f.write_str("Unsupported Sonos command"),
            Self::Volume => f.write_str("Volume must be between 0 and 100"),
            Self::NotCoordinator { coordinator } => write!(
                f,
                "Select group coordinator {coordinator} explicitly for playback"
            ),
        }
    }
}
impl std::error::Error for Error {}

#[derive(Debug, Clone, Serialize)]
pub struct Player {
    pub uuid: String,
    pub name: String,
    pub model: String,
}
#[derive(Debug, Clone, Serialize)]
pub struct Status {
    pub player: Player,
    pub coordinator: String,
    /// Transport state reported by this player; a group member may report a proxy state.
    pub transport: String,
    pub volume: u8,
    pub muted: bool,
}
#[derive(Debug, Clone, Copy)]
pub enum Playback {
    Play,
    Pause,
    Stop,
    Next,
    Previous,
}

pub struct Client {
    base: String,
    agent: ureq::Agent,
    player: Player,
    av_path: String,
    rc_path: String,
    zg_path: String,
}
fn xml(text: &str) -> Result<roxmltree::Document<'_>> {
    // roxmltree rejects DTDs by default; do not enable entity expansion.
    roxmltree::Document::parse(text).map_err(|_| Error::Response)
}
fn field(doc: &roxmltree::Document<'_>, name: &str) -> Result<String> {
    let mut nodes = doc
        .descendants()
        .filter(|n| n.is_element() && n.tag_name().name() == name);
    let value = nodes.next().ok_or(Error::Response)?.text().unwrap_or("");
    if nodes.next().is_some() {
        return Err(Error::Response);
    }
    Ok(value.to_owned())
}
fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
fn envelope(service: &str, action: &str, args: &[(&str, String)]) -> String {
    let fields: String = args
        .iter()
        .map(|(key, value)| format!("<{key}>{}</{key}>", escape(value)))
        .collect();
    format!(
        r#"<?xml version="1.0"?><s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/"><s:Body><u:{action} xmlns:u="{service}">{fields}</u:{action}></s:Body></s:Envelope>"#
    )
}
fn response(mut response: ureq::http::Response<ureq::Body>) -> Result<String> {
    let status = response.status().as_u16();
    let body = response
        .body_mut()
        .with_config()
        .limit(LIMIT)
        .read_to_string()
        .map_err(|_| Error::Response)?;
    let doc = xml(&body)?;
    if doc
        .descendants()
        .any(|n| n.has_tag_name(("http://schemas.xmlsoap.org/soap/envelope/", "Fault")))
    {
        return Err(Error::Soap(
            field(&doc, "errorCode")?
                .parse()
                .map_err(|_| Error::Response)?,
        ));
    }
    if !(200..300).contains(&status) {
        return Err(Error::Http(status));
    }
    Ok(body)
}
fn service_path(doc: &roxmltree::Document<'_>, service: &str) -> Result<String> {
    let node = doc
        .descendants()
        .find(|n| {
            n.has_tag_name("service")
                && n.children()
                    .any(|c| c.has_tag_name("serviceType") && c.text() == Some(service))
        })
        .ok_or(Error::Unsupported)?;
    let path = node
        .children()
        .find(|n| n.has_tag_name("controlURL"))
        .and_then(|n| n.text())
        .ok_or(Error::Response)?;
    // Accept only same-origin, plain absolute paths; never follow a device-supplied host.
    if !path.starts_with('/')
        || path.starts_with("//")
        || !path
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/-_".contains(&b))
    {
        return Err(Error::Response);
    }
    Ok(path.to_owned())
}
impl Client {
    pub fn connect(address: Ipv4Addr) -> Result<Self> {
        Self::connect_at(SocketAddrV4::new(address, 1400))
    }
    fn connect_at(address: SocketAddrV4) -> Result<Self> {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .proxy(None)
            .max_redirects(0)
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(5)))
            .build()
            .into();
        let base = format!("http://{address}");
        let body = response(
            agent
                .get(format!("{base}/xml/device_description.xml"))
                .call()
                .map_err(|_| Error::Transport)?,
        )?;
        let doc = xml(&body)?;
        // Root device identity only: descriptions can contain embedded renderer devices.
        let device = doc
            .root_element()
            .children()
            .find(|n| n.has_tag_name("device"))
            .ok_or(Error::Response)?;
        let get = |tag| {
            device
                .children()
                .find(|n| n.has_tag_name(tag))
                .and_then(|n| n.text())
                .map(str::to_owned)
                .ok_or(Error::Response)
        };
        if get("deviceType")? != ST || !get("manufacturer")?.starts_with("Sonos") {
            return Err(Error::Unsupported);
        }
        let udn = get("UDN")?;
        let uuid = udn
            .strip_prefix("uuid:")
            .filter(|s| !s.is_empty())
            .ok_or(Error::Response)?
            .to_owned();
        Ok(Self {
            base,
            agent,
            player: Player {
                uuid,
                name: get("friendlyName")?,
                model: get("modelName")?,
            },
            av_path: service_path(&doc, AV)?,
            rc_path: service_path(&doc, RC)?,
            zg_path: service_path(&doc, ZG)?,
        })
    }
    pub fn player(&self) -> &Player {
        &self.player
    }
    fn call(
        &self,
        service: &str,
        path: &str,
        action: &str,
        args: &[(&str, String)],
    ) -> Result<String> {
        let body = response(
            self.agent
                .post(format!("{}{path}", self.base))
                .header("Content-Type", "text/xml; charset=\"utf-8\"")
                .header("SOAPAction", format!("\"{service}#{action}\""))
                .send(envelope(service, action, args))
                .map_err(|_| Error::Transport)?,
        )?;
        let doc = xml(&body)?;
        if !doc
            .descendants()
            .any(|n| n.has_tag_name((service, format!("{action}Response").as_str())))
        {
            return Err(Error::Response);
        }
        Ok(body)
    }
    pub fn coordinator(&self) -> Result<String> {
        let body = self.call(ZG, &self.zg_path, "GetZoneGroupState", &[])?;
        let state = field(&xml(&body)?, "ZoneGroupState")?;
        coordinator(&state, &self.player.uuid)
    }
    pub fn status(&self) -> Result<Status> {
        let coordinator = self.coordinator()?;
        let body = self.call(
            AV,
            &self.av_path,
            "GetTransportInfo",
            &[("InstanceID", "0".into())],
        )?;
        let transport = field(&xml(&body)?, "CurrentTransportState")?;
        Ok(Status {
            player: self.player.clone(),
            coordinator,
            transport,
            volume: self.volume()?,
            muted: self.muted()?,
        })
    }
    pub fn playback(&self, command: Playback) -> Result<()> {
        self.playback_if_current(command, &|| true)
    }
    fn playback_if_current(&self, command: Playback, current: &dyn Fn() -> bool) -> Result<()> {
        let coordinator = self.coordinator()?;
        if coordinator != self.player.uuid {
            return Err(Error::NotCoordinator { coordinator });
        }
        let action = match command {
            Playback::Play => "Play",
            Playback::Pause => "Pause",
            Playback::Stop => "Stop",
            Playback::Next => "Next",
            Playback::Previous => "Previous",
        };
        let mut args = vec![("InstanceID", "0".into())];
        if matches!(command, Playback::Play) {
            args.push(("Speed", "1".into()));
        }
        if !current() {
            return Err(Error::Cancelled);
        }
        self.call(AV, &self.av_path, action, &args).map(|_| ())
    }
    /// Execute a closed button vocabulary, checking freshness after preparatory reads.
    /// A sent command cannot be recalled. This method never retries a write.
    pub fn command_if_current(&self, command: &str, current: &dyn Fn() -> bool) -> Result<()> {
        if !current() {
            return Err(Error::Cancelled);
        }
        let playback = match command {
            "play" => Some(Playback::Play),
            "pause" => Some(Playback::Pause),
            "stop" => Some(Playback::Stop),
            "next" => Some(Playback::Next),
            "previous" => Some(Playback::Previous),
            "play-pause" => {
                let body = self.call(
                    AV,
                    &self.av_path,
                    "GetTransportInfo",
                    &[("InstanceID", "0".into())],
                )?;
                Some(
                    if field(&xml(&body)?, "CurrentTransportState")? == "PLAYING" {
                        Playback::Pause
                    } else {
                        Playback::Play
                    },
                )
            }
            _ => None,
        };
        if let Some(command) = playback {
            return self.playback_if_current(command, current);
        }
        match command {
            "volume-up" | "volume-down" => {
                let volume = self.volume()?;
                if !current() {
                    return Err(Error::Cancelled);
                }
                self.set_volume(if command == "volume-up" {
                    volume.saturating_add(1).min(100)
                } else {
                    volume.saturating_sub(1)
                })
            }
            "mute" | "mute-on" | "mute-off" => {
                let muted = if command == "mute" {
                    !self.muted()?
                } else {
                    command == "mute-on"
                };
                if !current() {
                    return Err(Error::Cancelled);
                }
                self.set_muted(muted)
            }
            _ => Err(Error::Command),
        }
    }
    pub fn command(&self, command: &str) -> Result<()> {
        self.command_if_current(command, &|| true)
    }
    pub fn volume(&self) -> Result<u8> {
        let body = self.call(RC, &self.rc_path, "GetVolume", &channel())?;
        let volume: u8 = field(&xml(&body)?, "CurrentVolume")?
            .parse()
            .map_err(|_| Error::Response)?;
        if volume > 100 {
            return Err(Error::Response);
        }
        Ok(volume)
    }
    pub fn muted(&self) -> Result<bool> {
        let body = self.call(RC, &self.rc_path, "GetMute", &channel())?;
        match field(&xml(&body)?, "CurrentMute")?.as_str() {
            "1" | "true" => Ok(true),
            "0" | "false" => Ok(false),
            _ => Err(Error::Response),
        }
    }
    pub fn set_volume(&self, volume: u8) -> Result<()> {
        if volume > 100 {
            return Err(Error::Volume);
        }
        let mut args = channel();
        args.push(("DesiredVolume", volume.to_string()));
        self.call(RC, &self.rc_path, "SetVolume", &args).map(|_| ())
    }
    pub fn set_muted(&self, muted: bool) -> Result<()> {
        let mut args = channel();
        args.push(("DesiredMute", if muted { "1" } else { "0" }.into()));
        self.call(RC, &self.rc_path, "SetMute", &args).map(|_| ())
    }
}
fn channel() -> Vec<(&'static str, String)> {
    vec![("InstanceID", "0".into()), ("Channel", "Master".into())]
}
fn coordinator(state: &str, uuid: &str) -> Result<String> {
    let doc = xml(state)?;
    let mut groups = doc.descendants().filter(|n| {
        n.has_tag_name("ZoneGroup") && n.descendants().any(|m| m.attribute("UUID") == Some(uuid))
    });
    let coordinator = groups
        .next()
        .and_then(|n| n.attribute("Coordinator"))
        .filter(|s| !s.is_empty())
        .ok_or(Error::Response)?;
    if groups.next().is_some() {
        return Err(Error::Response);
    }
    Ok(coordinator.into())
}
/// Discover IPv4 responders in at most three seconds. Results are untrusted candidates;
/// connect verifies the device description. No LOCATION URL is fetched automatically.
pub fn discover() -> Result<Vec<Ipv4Addr>> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).map_err(|_| Error::Transport)?;
    socket
        .set_write_timeout(Some(Duration::from_secs(1)))
        .map_err(|_| Error::Transport)?;
    socket.send_to(format!("M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: \"ssdp:discover\"\r\nMX: 2\r\nST: {ST}\r\n\r\n").as_bytes(),("239.255.255.250",1900)).map_err(|_| Error::Transport)?;
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut found = BTreeSet::new();
    let mut buffer = [0; 8192];
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        if left.is_zero() {
            break;
        }
        socket
            .set_read_timeout(Some(left))
            .map_err(|_| Error::Transport)?;
        match socket.recv_from(&mut buffer) {
            Ok((len, SocketAddr::V4(peer))) if ssdp(&buffer[..len]) => {
                found.insert(*peer.ip());
                if found.len() >= 256 {
                    break;
                }
            }
            Ok(_) => {}
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ) =>
            {
                break
            }
            Err(_) => return Err(Error::Transport),
        }
    }
    Ok(found.into_iter().collect())
}
fn ssdp(bytes: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return false;
    };
    let mut lines = text.lines();
    lines
        .next()
        .is_some_and(|line| line.trim() == "HTTP/1.1 200 OK")
        && lines.any(|line| {
            line.split_once(':')
                .is_some_and(|(key, value)| key.eq_ignore_ascii_case("ST") && value.trim() == ST)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn soap(service: &str, action: &str, inner: &str) -> String {
        format!(
            r#"<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"><s:Body><u:{action}Response xmlns:u="{service}">{inner}</u:{action}Response></s:Body></s:Envelope>"#
        )
    }
    fn description() -> String {
        let services: String = [
            (AV, "/MediaRenderer/AVTransport/Control"),
            (RC, "/MediaRenderer/RenderingControl/Control"),
            (ZG, "/ZoneGroupTopology/Control"),
        ]
        .into_iter()
        .map(|(s, p)| {
            format!("<service><serviceType>{s}</serviceType><controlURL>{p}</controlURL></service>")
        })
        .collect();
        format!("<root xmlns=\"urn:schemas-upnp-org:device-1-0\"><device><deviceType>{ST}</deviceType><manufacturer>Sonos, Inc.</manufacturer><UDN>uuid:RINCON_TEST</UDN><friendlyName>Living room &amp; kitchen</friendlyName><modelName>Fixture</modelName><serviceList>{services}</serviceList></device></root>")
    }
    type RecordedRequest = (String, String, String);
    fn server(
        replies: Vec<(u16, String)>,
    ) -> (SocketAddrV4, std::thread::JoinHandle<Vec<RecordedRequest>>) {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let SocketAddr::V4(address) = server.server_addr().to_ip().unwrap() else {
            panic!()
        };
        let thread = std::thread::spawn(move || {
            let mut requests = vec![];
            for (status, body) in replies {
                let mut request = server
                    .recv_timeout(Duration::from_secs(3))
                    .unwrap()
                    .expect("missing request");
                let mut data = String::new();
                request.as_reader().read_to_string(&mut data).unwrap();
                let action = request
                    .headers()
                    .iter()
                    .find(|h| h.field.equiv("SOAPAction"))
                    .map(|h| h.value.as_str().to_owned())
                    .unwrap_or_default();
                requests.push((request.url().to_owned(), action, data));
                request
                    .respond(tiny_http::Response::from_string(body).with_status_code(status))
                    .unwrap();
            }
            // Check rejection paths have not emitted a later mutation.
            assert!(server
                .recv_timeout(Duration::from_millis(100))
                .unwrap()
                .is_none());
            requests
        });
        (address, thread)
    }
    #[test]
    fn discovery_filters_and_never_trusts_location() {
        assert!(ssdp(
            format!("HTTP/1.1 200 OK\r\nst: {ST}\r\nLOCATION: http://evil.example/\r\n").as_bytes()
        ));
        assert!(!ssdp(b"HTTP/1.1 200 OK\r\nST: upnp:rootdevice\r\n"));
        assert!(!ssdp(
            format!("NOTIFY * HTTP/1.1\r\nST: {ST}\r\n").as_bytes()
        ));
    }
    #[test]
    fn xml_escaping_and_entity_rejection() {
        let body = envelope(AV, "Play", &[("Value", "<&\"'>".into())]);
        assert_eq!(field(&xml(&body).unwrap(), "Value").unwrap(), "<&\"'>");
        assert!(xml(
            "<!DOCTYPE root [<!ENTITY leak SYSTEM 'file:///etc/passwd'>]><root>&leak;</root>"
        )
        .is_err());
        assert_eq!(
            field(&xml("<root><v>1</v><v>2</v></root>").unwrap(), "v"),
            Err(Error::Response)
        );
    }
    #[test]
    fn rejects_service_url_redirects() {
        for path in [
            "//other/Control",
            "http://other/Control",
            "/x?y=z",
            "/../Control",
        ] {
            let body = description().replace("/MediaRenderer/AVTransport/Control", path);
            assert_eq!(service_path(&xml(&body).unwrap(), AV), Err(Error::Response));
        }
    }
    #[test]
    fn group_member_playback_is_not_forwarded() {
        let topology=escape("<ZoneGroups><ZoneGroup Coordinator=\"RINCON_OTHER\"><ZoneGroupMember UUID=\"RINCON_OTHER\"/><ZoneGroupMember UUID=\"RINCON_TEST\"/></ZoneGroup></ZoneGroups>");
        let (address, thread) = server(vec![
            (200, description()),
            (
                200,
                soap(
                    ZG,
                    "GetZoneGroupState",
                    &format!("<ZoneGroupState>{topology}</ZoneGroupState>"),
                ),
            ),
        ]);
        let client = Client::connect_at(address).unwrap();
        assert_eq!(
            client.playback(Playback::Pause),
            Err(Error::NotCoordinator {
                coordinator: "RINCON_OTHER".into()
            })
        );
        assert_eq!(thread.join().unwrap().len(), 2);
    }
    #[test]
    fn volume_has_master_channel_and_validates_before_network() {
        let (address, thread) =
            server(vec![(200, description()), (200, soap(RC, "SetVolume", ""))]);
        let client = Client::connect_at(address).unwrap();
        assert_eq!(client.player().name, "Living room & kitchen");
        assert_eq!(client.set_volume(101), Err(Error::Volume));
        client.set_volume(25).unwrap();
        let requests = thread.join().unwrap();
        assert_eq!(requests[1].0, "/MediaRenderer/RenderingControl/Control");
        assert_eq!(requests[1].1, format!("\"{RC}#SetVolume\""));
        let doc = xml(&requests[1].2).unwrap();
        assert_eq!(field(&doc, "Channel").unwrap(), "Master");
        assert_eq!(field(&doc, "DesiredVolume").unwrap(), "25");
    }
    #[test]
    fn upnp_fault_is_typed_and_does_not_retry() {
        let fault = r#"<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"><s:Body><s:Fault><detail><UPnPError><errorCode>701</errorCode></UPnPError></detail></s:Fault></s:Body></s:Envelope>"#;
        let (address, thread) = server(vec![(200, description()), (500, fault.into())]);
        let client = Client::connect_at(address).unwrap();
        assert_eq!(client.set_muted(true), Err(Error::Soap(701)));
        assert_eq!(thread.join().unwrap().len(), 2);
    }
    #[test]
    fn oversized_description_is_rejected() {
        let (address, thread) = server(vec![(
            200,
            format!("<root>{}</root>", "x".repeat(LIMIT as usize)),
        )]);
        assert!(matches!(Client::connect_at(address), Err(Error::Response)));
        thread.join().unwrap();
    }
    #[test]
    fn coordinator_play_and_status_use_expected_services() {
        let topology = escape("<ZoneGroups><ZoneGroup Coordinator=\"RINCON_TEST\"><ZoneGroupMember UUID=\"RINCON_TEST\"/></ZoneGroup></ZoneGroups>");
        let state = soap(
            ZG,
            "GetZoneGroupState",
            &format!("<ZoneGroupState>{topology}</ZoneGroupState>"),
        );
        let (address, thread) = server(vec![
            (200, description()),
            (200, state.clone()),
            (200, soap(AV, "Play", "")),
            (200, state),
            (
                200,
                soap(
                    AV,
                    "GetTransportInfo",
                    "<CurrentTransportState>PLAYING</CurrentTransportState>",
                ),
            ),
            (
                200,
                soap(RC, "GetVolume", "<CurrentVolume>17</CurrentVolume>"),
            ),
            (200, soap(RC, "GetMute", "<CurrentMute>0</CurrentMute>")),
        ]);
        let client = Client::connect_at(address).unwrap();
        client.playback(Playback::Play).unwrap();
        let status = client.status().unwrap();
        assert_eq!(status.transport, "PLAYING");
        assert_eq!(status.volume, 17);
        assert!(!status.muted);
        let requests = thread.join().unwrap();
        let play = xml(&requests[2].2).unwrap();
        assert_eq!(field(&play, "Speed").unwrap(), "1");
        assert_eq!(field(&play, "InstanceID").unwrap(), "0");
    }
    #[test]
    fn expired_volume_command_stops_after_read_without_writing() {
        let (address, thread) = server(vec![
            (200, description()),
            (
                200,
                soap(RC, "GetVolume", "<CurrentVolume>10</CurrentVolume>"),
            ),
        ]);
        let client = Client::connect_at(address).unwrap();
        let checks = std::cell::Cell::new(0);
        assert_eq!(
            client.command_if_current("volume-up", &|| {
                let n = checks.get();
                checks.set(n + 1);
                n == 0
            }),
            Err(Error::Cancelled)
        );
        assert_eq!(thread.join().unwrap().len(), 2);
    }
    #[test]
    fn missing_or_ambiguous_group_fails_closed() {
        assert_eq!(coordinator("<ZoneGroups/>", "X"), Err(Error::Response));
        assert_eq!(coordinator("<ZoneGroups><ZoneGroup Coordinator=\"A\"><ZoneGroupMember UUID=\"X\"/></ZoneGroup><ZoneGroup Coordinator=\"B\"><ZoneGroupMember UUID=\"X\"/></ZoneGroup></ZoneGroups>","X"),Err(Error::Response));
    }
}
