//! UI-thread presentation only: no player IDs, stream indices or command state.
use crate::App;
use slint::{Image, SharedString};
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};
const TTL: Duration = Duration::from_secs(30);
const CAPACITY: usize = 3;
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Key {
    pub id: String,
    pub serial: u64,
    pub connection: String,
    pub host: String,
    pub port: u16,
}
#[derive(Clone)]
pub(crate) struct Presentation {
    title: SharedString,
    metadata: SharedString,
    elapsed: SharedString,
    remaining: SharedString,
    progress: f32,
    ready: bool,
    connected: bool,
    paused: bool,
    can_seek: bool,
    fanart: Image,
    logo: Image,
    has_art: bool,
    has_logo: bool,
    pub art_key: String,
}
impl Presentation {
    pub fn capture(app: &App, art_key: &str) -> Self {
        Self {
            title: app.get_player_title(),
            metadata: app.get_player_metadata(),
            elapsed: app.get_player_elapsed(),
            remaining: app.get_player_remaining(),
            progress: app.get_player_progress(),
            ready: app.get_player_ready(),
            connected: app.get_player_connected(),
            paused: app.get_player_paused(),
            can_seek: app.get_player_can_seek(),
            fanart: app.get_player_fanart(),
            logo: app.get_player_logo(),
            has_art: app.get_player_has_art(),
            has_logo: app.get_player_has_logo(),
            // A download still in flight is generation-bound and won't arrive
            // after reopen. Leave its key empty so the live refresh retries it.
            art_key: if app.get_player_has_art() || app.get_player_has_logo() {
                art_key.into()
            } else {
                String::new()
            },
        }
    }
    pub fn restore(&self, app: &App) {
        app.set_player_title(self.title.clone());
        app.set_player_metadata(self.metadata.clone());
        app.set_player_elapsed(self.elapsed.clone());
        app.set_player_remaining(self.remaining.clone());
        app.set_player_progress(self.progress);
        app.set_player_ready(self.ready);
        app.set_player_connected(self.connected);
        app.set_player_paused(self.paused);
        app.set_player_can_seek(self.can_seek);
        app.set_player_fanart(self.fanart.clone());
        app.set_player_logo(self.logo.clone());
        app.set_player_has_art(self.has_art);
        app.set_player_has_logo(self.has_logo);
    }
}
struct Entry<T> {
    key: Key,
    at: Instant,
    value: T,
}
pub(super) struct Cache<T = Presentation> {
    entries: VecDeque<Entry<T>>,
}
impl<T> Default for Cache<T> {
    fn default() -> Self {
        Self {
            entries: VecDeque::new(),
        }
    }
}
impl<T: Clone> Cache<T> {
    pub fn prune(&mut self, serial: u64, now: Instant) {
        self.entries
            .retain(|e| e.key.serial == serial && now.saturating_duration_since(e.at) < TTL);
    }
    pub fn remove(&mut self, key: &Key) {
        self.entries.retain(|e| &e.key != key);
    }
    pub fn insert(&mut self, key: Key, at: Instant, value: T, now: Instant) {
        self.prune(key.serial, now);
        self.remove(&key);
        if now.saturating_duration_since(at) >= TTL {
            return;
        }
        self.entries.push_back(Entry { key, at, value });
        while self.entries.len() > CAPACITY {
            self.entries.pop_front();
        }
    }
    pub fn get(&mut self, key: &Key, now: Instant) -> Option<(T, Instant)> {
        self.prune(key.serial, now);
        let index = self.entries.iter().position(|e| &e.key == key)?;
        let entry = self.entries.remove(index)?;
        let result = (entry.value.clone(), entry.at);
        self.entries.push_back(entry);
        Some(result)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn key(id: &str, serial: u64) -> Key {
        Key {
            id: id.into(),
            serial,
            connection: "kodi-1".into(),
            host: "192.0.2.1".into(),
            port: 9090,
        }
    }
    #[test]
    fn reuse_does_not_extend_authoritative_freshness() {
        let now = Instant::now();
        let k = key("device:a", 1);
        let mut c = Cache::default();
        c.insert(k.clone(), now, "playing", now);
        let (value, at) = c.get(&k, now + Duration::from_secs(20)).unwrap();
        c.insert(k.clone(), at, value, now + Duration::from_secs(25));
        assert!(c.get(&k, now + Duration::from_secs(30)).is_none());
    }
    #[test]
    fn exact_target_and_configuration_identity_are_required() {
        let now = Instant::now();
        let k = key("device:a", 1);
        let mut c = Cache::default();
        c.insert(k.clone(), now, "playing", now);
        for other in [
            key("activity:a", 1),
            Key {
                host: "192.0.2.2".into(),
                ..k.clone()
            },
            Key {
                connection: "kodi-2".into(),
                ..k.clone()
            },
            Key {
                port: 8080,
                ..k.clone()
            },
        ] {
            assert!(c.get(&other, now).is_none());
        }
        assert!(c.get(&key("device:a", 2), now).is_none());
        assert!(c.entries.is_empty());
    }
    #[test]
    fn cache_keeps_three_recent_views_and_removes_invalidated_state() {
        let now = Instant::now();
        let mut c = Cache::default();
        for id in ["a", "b", "c"] {
            c.insert(key(id, 1), now, id, now);
        }
        assert!(c.get(&key("a", 1), now).is_some());
        c.insert(key("d", 1), now, "d", now);
        assert!(c.get(&key("b", 1), now).is_none());
        assert_eq!(c.entries.len(), 3);
        c.remove(&key("a", 1));
        assert!(c.get(&key("a", 1), now).is_none());
    }
}
