//! Push-triggered state refresh with polling recovery. No networking under the
//! state mutex; commands have their own connection and never queue behind SSE.
use crate::{settings::Settings, tls, Error, Hue, Light, Result};
use std::{
    collections::HashMap,
    io::{BufRead, BufReader},
    path::PathBuf,
    sync::{Arc, Mutex, Weak},
    thread,
    time::{Duration, Instant},
};

// Hue may acknowledge a write before resource snapshots reflect it.
struct Pending {
    state: Light,
    brightness: Option<u8>,
    until: Instant,
}
#[derive(Default)]
struct Cache {
    lights: HashMap<String, Light>,
    updated: Option<Instant>,
    streaming: bool,
    generation: u64,
    commanding: bool,
    dirty: bool,
    pending: HashMap<String, Pending>,
}
impl Cache {
    fn apply_snapshot(&mut self, lights: Vec<Light>, now: Instant) {
        let mut lights: HashMap<_, _> = lights
            .into_iter()
            .map(|light| (light.entity_id.clone(), light))
            .collect();
        self.pending.retain(|_, pending| now < pending.until);
        for (id, pending) in &self.pending {
            if let Some(observed) = lights.get_mut(id) {
                // Unavailability and deletion remain authoritative. Only guard
                // old power/level observations during the bounded settling time.
                if observed.on.is_some()
                    && (observed.on != pending.state.on
                        || pending
                            .brightness
                            .is_some_and(|p| p > 0 && observed.brightness_percent != Some(p)))
                {
                    *observed = pending.state.clone();
                    self.dirty = true;
                }
            }
        }
        self.lights = lights;
        self.updated = Some(now);
    }
    fn fresh(&self) -> bool {
        self.updated
            .is_some_and(|t| t.elapsed() < Duration::from_secs(if self.streaming { 65 } else { 5 }))
    }
    fn invalidate(&mut self) {
        self.updated = None;
        self.generation += 1;
    }
}
struct Session {
    client: Arc<Hue>,
    cache: Mutex<Cache>,
    refresh_lock: Mutex<()>,
}
impl Session {
    fn refresh(&self) -> Result<()> {
        let _guard = match self.refresh_lock.try_lock() {
            Ok(g) => g,
            Err(_) => {
                self.cache.lock().unwrap().dirty = true;
                return Ok(());
            }
        };
        let generation = {
            let mut cache = self.cache.lock().unwrap();
            cache.dirty = false;
            cache.generation
        };
        let result = self.client.control_states();
        let mut cache = self.cache.lock().unwrap();
        // A response started before a command must not overwrite its target.
        if cache.generation != generation || cache.commanding || cache.dirty {
            cache.dirty = true;
            return Ok(());
        }
        match result {
            Ok(lights) => {
                cache.apply_snapshot(lights, Instant::now());
                Ok(())
            }
            Err(e) => {
                cache.invalidate();
                Err(e)
            }
        }
    }
    fn toggle(&self, id: &str) -> Result<Light> {
        self.command(id, None)
    }
    /// Colour temperature, written straight through. It is kept out of
    /// `command` on purpose: the settling guard there is about power and
    /// level, and a lamp's mirek is never guessed from a press - the screen
    /// carries its own pending target for the second the bridge takes.
    fn set_mirek(&self, id: &str, mirek: u16) -> Result<Light> {
        let cached = {
            let c = self.cache.lock().unwrap();
            c.fresh().then(|| c.lights.get(id).cloned()).flatten()
        };
        let mut state = match cached {
            Some(s) => s,
            None => self.client.control_state(id)?,
        };
        if state.on.is_none() {
            return Err(Error::Unavailable);
        }
        {
            let mut c = self.cache.lock().unwrap();
            c.generation += 1;
            c.commanding = true;
        }
        let result = self
            .client
            .command_for_state(&state, couch_ha::Command::Mirek(mirek));
        let mut c = self.cache.lock().unwrap();
        c.commanding = false;
        c.generation += 1;
        if let Err(e) = result {
            c.invalidate();
            c.dirty = true;
            return Err(e);
        }
        // Hue turns the lamp on to show a colour temperature, so say so.
        state.on = Some(true);
        state.mirek = Some(mirek);
        c.dirty = true;
        c.lights.insert(id.into(), state.clone());
        Ok(state)
    }
    fn command(&self, id: &str, brightness: Option<u8>) -> Result<Light> {
        let cached = {
            let c = self.cache.lock().unwrap();
            if c.fresh() {
                c.lights.get(id).cloned()
            } else {
                None
            }
        };
        let fast = cached.is_some();
        let mut state = match cached {
            Some(s) => s,
            None => self.client.control_state(id)?,
        };
        if let Some(scene) = id.strip_prefix("scene:") {
            self.client.recall_scene(scene)?;
            let mut cache = self.cache.lock().unwrap();
            cache.invalidate();
            cache.pending.clear();
            cache.dirty = true;
            return Ok(state);
        }
        let on = !state.on.ok_or(Error::Unavailable)?;
        if brightness.is_some_and(|p| p > 100 || !state.dimmable) {
            return Err(Error::Brightness);
        }
        {
            let mut c = self.cache.lock().unwrap();
            c.generation += 1;
            c.commanding = true;
        }
        let started = Instant::now();
        let result = if let Some(p) = brightness {
            self.client
                .command_for_state(&state, couch_ha::Command::Brightness(p))
        } else {
            self.client.set_power(id, on)
        };
        let mut c = self.cache.lock().unwrap();
        c.commanding = false;
        c.generation += 1;
        if let Err(e) = result {
            c.pending.remove(id);
            c.invalidate();
            c.dirty = true;
            return Err(e);
        }
        state.on = Some(brightness.map_or(on, |p| p > 0));
        // Power-off keeps Hue's saved dimming level for the next power-on.
        if let Some(percent) = brightness.filter(|p| *p > 0) {
            state.brightness_percent = Some(percent);
        }
        c.pending.insert(
            id.into(),
            Pending {
                state: state.clone(),
                brightness,
                until: Instant::now() + Duration::from_secs(2),
            },
        );
        c.dirty = true;
        c.lights.insert(id.into(), state.clone());
        // Do not extend the age of other lights based on this command.
        println!(
            "couch-hue: command acknowledged in {} ms (cached={fast})",
            started.elapsed().as_millis()
        );
        Ok(state)
    }
}

/// One credential-scoped session. Re-pairing drops the previous cache and pool.
/// The stream uses a separate HTTPS connection from commands and snapshots.
pub struct Live {
    path: PathBuf,
    session: Mutex<Option<(Vec<u8>, Arc<Session>)>>,
}
impl Live {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            session: Mutex::new(None),
        }
    }
    /// Drop all observations and reconnect on the next request (e.g. wake).
    pub fn reset(&self) {
        *self.session.lock().unwrap() = None;
    }
    fn session(&self) -> Result<Arc<Session>> {
        let bytes = match std::fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(_) => {
                self.reset();
                return Err(Error::Configuration);
            }
        };
        let mut slot = self.session.lock().unwrap();
        if let Some((old, session)) = &*slot {
            if *old == bytes {
                return Ok(session.clone());
            }
        }
        *slot = None;
        let settings: Settings =
            serde_json::from_slice(&bytes).map_err(|_| Error::Configuration)?;
        let session = Arc::new(Session {
            client: Arc::new(settings.client()?),
            cache: Mutex::new(Cache::default()),
            refresh_lock: Mutex::new(()),
        });
        let weak = Arc::downgrade(&session);
        thread::spawn(move || poll(weak));
        let weak = Arc::downgrade(&session);
        thread::spawn(move || stream(weak, settings));
        *slot = Some((bytes, session.clone()));
        Ok(session)
    }
    pub fn lights(&self) -> Result<Vec<Light>> {
        let session = self.session()?;
        // Startup and recovery reads happen on the GUI's existing worker.
        if !session.cache.lock().unwrap().fresh() {
            session.refresh()?;
        }
        let c = session.cache.lock().unwrap();
        if !c.fresh() {
            return Err(Error::Unavailable);
        }
        Ok(c.lights.values().cloned().collect())
    }
    pub fn brightness(&self, id: &str, percent: u8) -> Result<Light> {
        if percent > 100 || id.starts_with("scene:") {
            return Err(Error::Brightness);
        }
        self.session()?.command(id, Some(percent))
    }
    pub fn toggle(&self, id: &str) -> Result<Light> {
        self.session()?.toggle(id)
    }
    /// Colour temperature, in mirek. A room's grouped light and a scene have
    /// no range of their own, so neither takes one.
    pub fn mirek(&self, id: &str, mirek: u16) -> Result<Light> {
        if id.starts_with("scene:") || id.starts_with("room:") {
            return Err(Error::ColourTemperature);
        }
        self.session()?.set_mirek(id, mirek)
    }
}
fn poll(weak: Weak<Session>) {
    let mut last = Instant::now() - Duration::from_secs(60);
    loop {
        let Some(s) = weak.upgrade() else {
            return;
        };
        let interval = if s.cache.lock().unwrap().streaming {
            60
        } else {
            5
        };
        if last.elapsed() >= Duration::from_secs(interval) || s.cache.lock().unwrap().dirty {
            let _ = s.refresh();
            last = Instant::now();
        }
        drop(s);
        thread::sleep(Duration::from_secs(1));
    }
}
fn stream(weak: Weak<Session>, settings: Settings) {
    let agent = tls::stream_agent(Arc::new(Mutex::new(settings.certificate)));
    let url = format!(
        "{}/eventstream/clip/v2",
        crate::base(&settings.url).expect("validated Hue address")
    );
    let mut backoff = 1;
    while weak.strong_count() > 0 {
        let result = (|| -> Result<()> {
            let response = agent
                .get(&url)
                .header("hue-application-key", &settings.token)
                .header("Accept", "text/event-stream")
                .call()
                .map_err(crate::transport)?;
            if !response
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| v.starts_with("text/event-stream"))
            {
                return Err(Error::Response);
            }
            {
                let Some(s) = weak.upgrade() else {
                    return Ok(());
                };
                s.cache.lock().unwrap().streaming = true;
                // Refresh after subscribing so changes during reconnect are covered.
                s.refresh()?;
            }
            println!("couch-hue: event stream connected");
            let mut reader = BufReader::new(response.into_body().into_reader());
            loop {
                let changed = event(&mut reader)?;
                backoff = 1;
                let Some(s) = weak.upgrade() else {
                    return Ok(());
                };
                if changed {
                    let _ = s.refresh();
                }
            }
        })();
        let Some(s) = weak.upgrade() else {
            return;
        };
        {
            let mut c = s.cache.lock().unwrap();
            c.streaming = false;
            c.invalidate();
        }
        let _ = s.refresh();
        drop(s);
        if result.is_err() {
            println!("couch-hue: stream unavailable; polling, reconnect in {backoff}s");
        }
        thread::sleep(Duration::from_secs(backoff));
        backoff = (backoff * 2).min(30);
    }
}
// Parse SSE framing, including comments, CRLF and multi-line data. Bound both
// lines and events so a malformed bridge cannot grow memory without limit.
fn event(reader: &mut impl BufRead) -> Result<bool> {
    use std::io::Read;
    let mut data = String::new();
    loop {
        let mut line = String::new();
        let n = reader
            .take(65537)
            .read_line(&mut line)
            .map_err(|_| Error::Transport)?;
        if n == 0 {
            return Err(Error::Transport);
        }
        if n > 65536 {
            return Err(Error::Response);
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            if data.is_empty() {
                return Ok(false);
            }
            let v: serde_json::Value = serde_json::from_str(&data).map_err(|_| Error::Response)?;
            let events = v.as_array().ok_or(Error::Response)?;
            return Ok(events
                .iter()
                .any(|e| matches!(e["type"].as_str(), Some("update" | "add" | "delete"))));
        }
        if let Some(part) = line.strip_prefix("data:") {
            data.push_str(part.strip_prefix(' ').unwrap_or(part));
            data.push('\n');
            if data.len() > 131072 {
                return Err(Error::Response);
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn acknowledged_toggle_keeps_brightness_across_stale_snapshots() {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let address = server.server_addr();
        let id = "00000000-0000-0000-0000-000000000001";
        let remote = thread::spawn(move || {
            for on in [true, false, true] {
                let mut request = server
                    .recv_timeout(Duration::from_secs(2))
                    .unwrap()
                    .unwrap();
                assert_eq!(request.method(), &tiny_http::Method::Put);
                let mut body = String::new();
                request.as_reader().read_to_string(&mut body).unwrap();
                assert_eq!(
                    serde_json::from_str::<serde_json::Value>(&body).unwrap(),
                    serde_json::json!({"on":{"on":on}})
                );
                request
                    .respond(tiny_http::Response::from_string(
                        serde_json::json!({"errors":[],"data":[{"rid":id,"rtype":"light"}]})
                            .to_string(),
                    ))
                    .unwrap();
            }
        });
        let off = Light {
            entity_id: id.into(),
            name: "Test".into(),
            on: Some(false),
            brightness_percent: Some(56),
            dimmable: true,
            mirek: None,
            mirek_range: None,
        };
        let mut cache = Cache::default();
        cache.apply_snapshot(vec![off.clone()], Instant::now());
        let session = Session {
            client: Arc::new(Hue {
                base: format!("http://{address}"),
                key: "fixture".into(),
                agent: ureq::Agent::new_with_defaults(),
            }),
            cache: Mutex::new(cache),
            refresh_lock: Mutex::new(()),
        };
        let on = session.toggle(id).unwrap();
        assert_eq!((on.on, on.brightness_percent), (Some(true), Some(56)));
        {
            let mut cache = session.cache.lock().unwrap();
            // Old state can follow even a matching snapshot while Hue settles.
            for observed in [off.clone(), on.clone(), off.clone(), on.clone()] {
                cache.apply_snapshot(vec![observed], Instant::now());
                assert_eq!(cache.lights[id].on, Some(true));
                assert_eq!(cache.lights[id].brightness_percent, Some(56));
            }
        }
        assert_eq!(session.toggle(id).unwrap().on, Some(false));
        assert_eq!(session.toggle(id).unwrap().brightness_percent, Some(56));
        let mut cache = session.cache.lock().unwrap();
        let expired = cache.pending[id].until + Duration::from_millis(1);
        cache.apply_snapshot(vec![off], expired);
        assert_eq!(cache.lights[id].on, Some(false));
        assert!(cache.pending.is_empty());
        remote.join().unwrap();
    }
    #[test]
    fn settling_guard_preserves_unavailability_deletion_and_unrelated_changes() {
        let state = Light {
            entity_id: "room:test".into(),
            name: "Test".into(),
            on: Some(true),
            brightness_percent: Some(56),
            dimmable: true,
            mirek: None,
            mirek_range: None,
        };
        let mut cache = Cache::default();
        cache.pending.insert(
            state.entity_id.clone(),
            Pending {
                state: state.clone(),
                brightness: Some(56),
                until: Instant::now() + Duration::from_secs(2),
            },
        );
        let mut old = state.clone();
        old.brightness_percent = Some(20);
        let mut unrelated = old.clone();
        unrelated.entity_id = "other".into();
        cache.apply_snapshot(vec![old.clone(), unrelated], Instant::now());
        assert_eq!(cache.lights["room:test"].brightness_percent, Some(56));
        assert_eq!(cache.lights["other"].brightness_percent, Some(20));
        old.on = None;
        cache.apply_snapshot(vec![old], Instant::now());
        assert_eq!(cache.lights["room:test"].on, None);
        cache.apply_snapshot(vec![], Instant::now());
        assert!(cache.lights.is_empty());
    }
    #[test]
    fn dimming_uses_cached_state_and_sends_only_the_requested_level() {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let address = server.server_addr();
        let id = "00000000-0000-0000-0000-000000000001";
        let remote = thread::spawn(move || {
            for expected in [
                serde_json::json!({"on":{"on":true},"dimming":{"brightness":55}}),
                serde_json::json!({"on":{"on":false}}),
            ] {
                let mut request = server
                    .recv_timeout(Duration::from_secs(2))
                    .unwrap()
                    .unwrap();
                assert_eq!(request.method(), &tiny_http::Method::Put);
                assert_eq!(request.url(), format!("/clip/v2/resource/light/{id}"));
                let mut body = String::new();
                request.as_reader().read_to_string(&mut body).unwrap();
                assert_eq!(
                    serde_json::from_str::<serde_json::Value>(&body).unwrap(),
                    expected
                );
                request
                    .respond(tiny_http::Response::from_string(
                        serde_json::json!({"errors":[],"data":[{"rid":id,"rtype":"light"}]})
                            .to_string(),
                    ))
                    .unwrap();
            }
        });
        let mut cache = Cache::default();
        cache.updated = Some(Instant::now());
        cache.lights.insert(
            id.into(),
            Light {
                entity_id: id.into(),
                name: "Test".into(),
                on: Some(true),
                brightness_percent: Some(50),
                dimmable: true,
                mirek: None,
                mirek_range: None,
            },
        );
        let session = Session {
            client: Arc::new(Hue {
                base: format!("http://{address}"),
                key: "fixture".into(),
                agent: ureq::Agent::new_with_defaults(),
            }),
            cache: Mutex::new(cache),
            refresh_lock: Mutex::new(()),
        };
        assert!(matches!(
            session.command(id, Some(101)),
            Err(Error::Brightness)
        ));
        let state = session.command(id, Some(55)).unwrap();
        assert_eq!(state.brightness_percent, Some(55));
        assert_eq!(
            session.cache.lock().unwrap().lights[id].brightness_percent,
            Some(55)
        );
        assert_eq!(session.command(id, Some(0)).unwrap().on, Some(false));
        session
            .cache
            .lock()
            .unwrap()
            .lights
            .get_mut(id)
            .unwrap()
            .dimmable = false;
        assert!(matches!(
            session.command(id, Some(5)),
            Err(Error::Brightness)
        ));
        remote.join().unwrap();
    }
    #[test]
    fn grouped_dimming_uses_cached_state_and_grouped_light_endpoint() {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let address = server.server_addr();
        let uuid = "00000000-0000-0000-0000-000000000001";
        let id = "room:00000000-0000-0000-0000-000000000001";
        let remote = thread::spawn(move || {
            for expected in [
                serde_json::json!({"on":{"on":true},"dimming":{"brightness":55}}),
                serde_json::json!({"on":{"on":false}}),
            ] {
                let mut request = server
                    .recv_timeout(Duration::from_secs(2))
                    .unwrap()
                    .unwrap();
                assert_eq!(request.method(), &tiny_http::Method::Put);
                assert_eq!(
                    request.url(),
                    format!("/clip/v2/resource/grouped_light/{uuid}")
                );
                let mut body = String::new();
                request.as_reader().read_to_string(&mut body).unwrap();
                assert_eq!(
                    serde_json::from_str::<serde_json::Value>(&body).unwrap(),
                    expected
                );
                request.respond(tiny_http::Response::from_string(
                    serde_json::json!({"errors":[],"data":[{"rid":uuid,"rtype":"grouped_light"}]}).to_string()
                )).unwrap();
            }
        });
        let mut cache = Cache::default();
        cache.updated = Some(Instant::now());
        cache.lights.insert(
            id.into(),
            Light {
                entity_id: id.into(),
                name: "Test".into(),
                on: Some(true),
                brightness_percent: Some(50),
                dimmable: true,
                mirek: None,
                mirek_range: None,
            },
        );
        let session = Session {
            client: Arc::new(Hue {
                base: format!("http://{address}"),
                key: "fixture".into(),
                agent: ureq::Agent::new_with_defaults(),
            }),
            cache: Mutex::new(cache),
            refresh_lock: Mutex::new(()),
        };
        assert!(matches!(
            session.command(id, Some(101)),
            Err(Error::Brightness)
        ));
        let state = session.command(id, Some(55)).unwrap();
        assert_eq!(state.brightness_percent, Some(55));
        assert_eq!(
            session.cache.lock().unwrap().lights[id].brightness_percent,
            Some(55)
        );
        assert_eq!(session.command(id, Some(0)).unwrap().on, Some(false));
        session
            .cache
            .lock()
            .unwrap()
            .lights
            .get_mut(id)
            .unwrap()
            .dimmable = false;
        assert!(matches!(
            session.command(id, Some(5)),
            Err(Error::Brightness)
        ));
        remote.join().unwrap();
    }
    #[test]
    fn old_snapshot_cannot_undo_an_acknowledged_command() {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let address = server.server_addr();
        let id = "00000000-0000-0000-0000-000000000001";
        let (ready, wait) = std::sync::mpsc::channel();
        let (finish, release) = std::sync::mpsc::channel();
        let remote = thread::spawn(move || {
            let get = server
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .unwrap();
            assert_eq!(get.method(), &tiny_http::Method::Get);
            ready.send(()).unwrap();
            let put = server
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .unwrap();
            assert_eq!(put.method(), &tiny_http::Method::Put);
            put.respond(tiny_http::Response::from_string(format!(
                r#"{{"errors":[],"data":[{{"rid":"{id}","rtype":"light"}}]}}"#
            )))
            .unwrap();
            release.recv_timeout(Duration::from_secs(2)).unwrap();
            get.respond(tiny_http::Response::from_string(
                r#"{"errors":[],"data":[]}"#,
            ))
            .unwrap();
        });
        let mut cache = Cache::default();
        cache.updated = Some(Instant::now());
        cache.lights.insert(
            id.into(),
            Light {
                entity_id: id.into(),
                name: "Test".into(),
                on: Some(false),
                brightness_percent: Some(0),
                dimmable: false,
                mirek: None,
                mirek_range: None,
            },
        );
        let session = Arc::new(Session {
            client: Arc::new(Hue {
                base: format!("http://{address}"),
                key: "fixture".into(),
                agent: ureq::Agent::new_with_defaults(),
            }),
            cache: Mutex::new(cache),
            refresh_lock: Mutex::new(()),
        });
        let background = session.clone();
        let poll = thread::spawn(move || background.refresh().unwrap());
        wait.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(session.toggle(id).unwrap().on, Some(true));
        finish.send(()).unwrap();
        poll.join().unwrap();
        remote.join().unwrap();
        assert_eq!(session.cache.lock().unwrap().lights[id].on, Some(true));
    }
    #[test]
    fn sse_comments_multiline_deletes_and_limits() {
        assert!(!event(&mut &b": heartbeat\r\n\r\n"[..]).unwrap());
        assert!(event(&mut &b"id: 1\ndata: [\ndata: {\"type\":\"delete\"}]\n\n"[..]).unwrap());
        assert!(event(&mut &b"data: broken\n\n"[..]).is_err());
        assert!(event(&mut &vec![b'x'; 65537][..]).is_err());
        assert!(event(&mut &b""[..]).is_err());
    }
    #[test]
    fn disconnect_and_expiry_disable_fast_path() {
        let mut c = Cache::default();
        c.updated = Some(Instant::now() - Duration::from_secs(10));
        assert!(!c.fresh());
        c.streaming = true;
        assert!(c.fresh());
        c.invalidate();
        assert!(!c.fresh());
        c.updated = Some(Instant::now() - Duration::from_secs(66));
        assert!(!c.fresh());
    }
}
