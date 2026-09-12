use super::{proto as pb, Error, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    time::{SystemTime, UNIX_EPOCH},
};
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaybackState {
    #[default]
    Unknown,
    Playing,
    Paused,
    Stopped,
    Interrupted,
    Seeking,
}
/// Missing values mean the app did not supply them. Artwork URLs are untrusted
/// metadata; this client never fetches URLs or launches a media receiver.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct NowPlaying {
    pub app_id: Option<String>,
    pub app_name: Option<String>,
    pub player_id: Option<String>,
    pub item_id: Option<String>,
    pub title: Option<String>,
    pub subtitle: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub series: Option<String>,
    pub season: Option<i32>,
    pub episode: Option<i32>,
    pub genre: Option<String>,
    pub state: PlaybackState,
    pub duration: Option<f64>,
    pub position: Option<f64>,
    pub playback_rate: Option<f32>,
    pub is_live: Option<bool>,
    pub artwork_url: Option<String>,
    pub artwork_id: Option<String>,
    pub artwork_available: Option<bool>,
    pub artwork_mime: Option<String>,
    /// Position sample's timestamp in Unix seconds, converted from Apple's 2001 epoch.
    pub position_timestamp: Option<f64>,
}
impl NowPlaying {
    /// Advance only known playing state; never invent duration for live content.
    pub fn position_at(&self, now: SystemTime) -> Option<f64> {
        let mut position = self.position?;
        if self.state == PlaybackState::Playing {
            if let (Some(stamp), Ok(now)) =
                (self.position_timestamp, now.duration_since(UNIX_EPOCH))
            {
                let delta = (now.as_secs_f64() - stamp).max(0.0);
                position += delta * f64::from(self.playback_rate.unwrap_or(1.0).max(0.0));
            }
        }
        Some(
            self.duration
                .map(|d| position.min(d))
                .unwrap_or(position)
                .max(0.0),
        )
    }
}
#[derive(Default)]
struct Player {
    state: Option<i32>,
    items: Vec<pb::Item>,
    location: i32,
}
#[derive(Default)]
struct Client {
    name: Option<String>,
    active: Option<String>,
    players: BTreeMap<String, Player>,
}
#[derive(Default)]
pub(super) struct State {
    active: Option<String>,
    clients: BTreeMap<String, Client>,
}
fn text(value: &Option<String>) -> Option<String> {
    value
        .as_ref()
        .filter(|v| {
            !v.is_empty()
                && v.len() <= 4096
                && !v
                    .chars()
                    .any(|c| c.is_control() && !matches!(c, '\n' | '\t'))
        })
        .cloned()
}
fn number(value: Option<f64>) -> Option<f64> {
    value.filter(|n| n.is_finite() && *n >= 0.0)
}
fn path(path: Option<&pb::PlayerPath>) -> (String, String) {
    (
        path.and_then(|p| p.client.as_ref())
            .and_then(|c| c.bundle.clone())
            .unwrap_or_default(),
        path.and_then(|p| p.player.as_ref())
            .and_then(|p| p.id.clone())
            .unwrap_or_default(),
    )
}
impl State {
    fn client(&mut self, info: Option<&pb::ClientInfo>) -> Result<&mut Client> {
        let id = info
            .and_then(|i| i.bundle.as_ref())
            .cloned()
            .unwrap_or_default();
        if id.len() > 512 || (!self.clients.contains_key(&id) && self.clients.len() >= 32) {
            return Err(Error::Protocol);
        }
        let client = self.clients.entry(id).or_default();
        if let Some(name) = info.and_then(|i| text(&i.name)) {
            client.name = Some(name);
        }
        Ok(client)
    }
    fn player(&mut self, value: Option<&pb::PlayerPath>) -> Result<&mut Player> {
        let (_, id) = path(value);
        let client = self.client(value.and_then(|p| p.client.as_ref()))?;
        if id.len() > 512 || (!client.players.contains_key(&id) && client.players.len() >= 16) {
            return Err(Error::Protocol);
        }
        Ok(client.players.entry(id).or_default())
    }
    pub fn apply(&mut self, msg: &pb::Envelope) -> Result<()> {
        match msg.kind {
            Some(4) => {
                let value = msg.state.as_ref().ok_or(Error::Protocol)?;
                if value.queue.as_ref().is_some_and(|q| q.items.len() > 128) {
                    return Err(Error::Protocol);
                }
                let player = self.player(value.path.as_ref())?;
                if let Some(state) = value.playback_state {
                    player.state = Some(state);
                }
                if let Some(queue) = &value.queue {
                    player.items = queue.items.clone();
                    player.location = queue.location.unwrap_or(0);
                }
            }
            Some(46) => {
                let info = msg
                    .active_client
                    .as_ref()
                    .ok_or(Error::Protocol)?
                    .client
                    .as_ref();
                self.client(info)?;
                self.active = info
                    .and_then(|i| i.bundle.clone())
                    .filter(|s| !s.is_empty());
            }
            Some(47) => {
                let value = msg
                    .active_player
                    .as_ref()
                    .ok_or(Error::Protocol)?
                    .path
                    .as_ref();
                let (_, id) = path(value);
                self.player(value)?;
                self.client(value.and_then(|p| p.client.as_ref()))?.active = Some(id);
            }
            Some(53) => {
                let info = msg
                    .remove_client
                    .as_ref()
                    .ok_or(Error::Protocol)?
                    .client
                    .as_ref();
                let id = info
                    .and_then(|i| i.bundle.as_ref())
                    .cloned()
                    .unwrap_or_default();
                self.clients.remove(&id);
                if self.active.as_ref() == Some(&id) {
                    self.active = None;
                }
            }
            Some(54) => {
                let value = msg
                    .remove_player
                    .as_ref()
                    .ok_or(Error::Protocol)?
                    .path
                    .as_ref();
                let (client, id) = path(value);
                if let Some(c) = self.clients.get_mut(&client) {
                    c.players.remove(&id);
                    if c.active.as_ref() == Some(&id) {
                        c.active = None;
                    }
                }
            }
            Some(55) => {
                self.client(
                    msg.update_client
                        .as_ref()
                        .ok_or(Error::Protocol)?
                        .client
                        .as_ref(),
                )?;
            }
            Some(56) => {
                let value = msg.update_items.as_ref().ok_or(Error::Protocol)?;
                if value.items.len() > 128 {
                    return Err(Error::Protocol);
                }
                let player = self.player(value.path.as_ref())?;
                for new in &value.items {
                    if new.id.as_ref().is_none_or(|id| id.is_empty()) {
                        continue;
                    }
                    if let Some(old) = player.items.iter_mut().find(|i| i.id == new.id) {
                        if let Some(metadata) = &new.metadata {
                            let old_meta = old.metadata.get_or_insert_default();
                            // Proto2 optional fields distinguish absent from zero/empty;
                            // merge encoded fields without erasing unchanged metadata.
                            prost::Message::merge(
                                old_meta,
                                prost::Message::encode_to_vec(metadata).as_slice(),
                            )
                            .map_err(|_| Error::Protocol)?;
                        }
                        if new.artwork.is_some() {
                            old.artwork = new.artwork.clone();
                        }
                    }
                }
            }
            _ => {}
        }
        let cached: usize = self
            .clients
            .values()
            .flat_map(|c| c.players.values())
            .flat_map(|p| p.items.iter())
            .map(prost::Message::encoded_len)
            .sum();
        if cached > 4 * 1024 * 1024 {
            return Err(Error::Protocol);
        }
        Ok(())
    }
    pub fn snapshot(&self) -> NowPlaying {
        let Some(id) = &self.active else {
            return NowPlaying::default();
        };
        let Some(client) = self.clients.get(id) else {
            return NowPlaying::default();
        };
        let player_id = client
            .active
            .as_deref()
            .unwrap_or("MediaRemote-DefaultPlayer");
        let mut output = NowPlaying {
            app_id: Some(id.clone()),
            app_name: client.name.clone(),
            player_id: Some(player_id.into()),
            ..Default::default()
        };
        let Some(player) = client.players.get(player_id) else {
            return output;
        };
        output.state = match player.state {
            Some(1) => PlaybackState::Playing,
            Some(2) => PlaybackState::Paused,
            Some(3) => PlaybackState::Stopped,
            Some(4) => PlaybackState::Interrupted,
            Some(5) => PlaybackState::Seeking,
            _ => PlaybackState::Unknown,
        };
        let Some(item) = usize::try_from(player.location)
            .ok()
            .and_then(|i| player.items.get(i))
        else {
            return output;
        };
        output.item_id = text(&item.id);
        let Some(m) = &item.metadata else {
            return output;
        };
        output.title = text(&m.title);
        output.subtitle = text(&m.subtitle);
        output.artist = text(&m.artist).or_else(|| text(&m.album_artist));
        output.album = text(&m.album);
        output.series = text(&m.series);
        output.genre = text(&m.genre);
        output.season = m.season.filter(|n| *n >= 0);
        output.episode = m.episode.filter(|n| *n >= 0);
        output.duration = number(m.duration);
        output.position = number(m.elapsed);
        output.is_live = m.live;
        output.playback_rate = m.rate.filter(|r| r.is_finite());
        output.position_timestamp = number(m.elapsed_timestamp)
            .map(|n| n + 978_307_200.0)
            .filter(|n| n.is_finite());
        output.artwork_id = text(&m.artwork_id);
        output.artwork_available = m.artwork_available;
        output.artwork_url =
            text(&m.artwork_url).filter(|u| u.starts_with("https://") || u.starts_with("http://"));
        output.artwork_mime = text(&m.artwork_mime);
        output
    }
}
