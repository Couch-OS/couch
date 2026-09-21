//! The built-in Sonos client behind the player screen: one backend of the
//! device-neutral controller in `media_player.rs`. Everything that knows it is
//! talking to a Sonos player is here: the connection the GUI holds to the
//! player, the group snapshot and how it reads as [`Media`], source ids, the
//! wait for a skip to land, and which Sonos errors mean what. It goes away
//! with built-in Sonos; a package then supplies the same things through the
//! daemon.
use crate::media_player::{
    line, Backend, Can, Choice, Done, Failure, Media, ModeChange, NextItem, Op, PlayState, Watched,
};
use couch_sonos::{Client, PlayModeChange, Snapshot, SourceId};
use std::{net::Ipv4Addr, time::Duration};

pub(crate) struct BuiltIn {
    host: Ipv4Addr,
    client: Option<Client>,
}
impl BuiltIn {
    pub fn new(host: Ipv4Addr) -> Self {
        Self { host, client: None }
    }
    fn client(&self) -> Result<&Client, Failure> {
        self.client.as_ref().ok_or(Failure::NotConnected)
    }
}
impl Backend for BuiltIn {
    fn open(&mut self) -> Result<(), Failure> {
        self.client = None;
        self.client = Some(Client::connect(self.host).map_err(failure)?);
        Ok(())
    }
    fn close(&mut self) {
        self.client = None;
    }
    /// The player is read directly, so there is nothing to wait for and every
    /// read is a new revision.
    fn watch(&mut self, after: u64, _wait: Duration) -> Result<Watched, Failure> {
        let snapshot = self.client()?.snapshot().map_err(failure)?;
        Ok(Watched {
            revision: after.wrapping_add(1),
            age: Duration::ZERO,
            media: media(snapshot),
        })
    }
    fn perform(&mut self, op: &Op, current: &dyn Fn() -> bool) -> Result<Done, Failure> {
        let c = self.client()?;
        let outcome = match op {
            Op::PlayPause => c
                .command_if_current("play-pause", current)
                .map(|_| Done::Nothing),
            Op::Next | Op::Previous => {
                let before = c.now_playing().ok().and_then(|n| n.current);
                let command = if *op == Op::Next { "next" } else { "previous" };
                c.command_if_current(command, current).map(|_| {
                    // Let the player switch before the refresh reads it.
                    now_playing_after_skip(c, before.as_ref());
                    Done::Nothing
                })
            }
            Op::Seek { position_ms } => c
                .seek_if_current(*position_ms, current)
                .map(|_| Done::Nothing),
            Op::StepVolume(delta) => c
                .nudge_volume(*delta)
                .and_then(|_| c.volume())
                .map(Done::Volume),
            Op::ToggleMute => c
                .muted()
                .and_then(|muted| c.set_muted(!muted).map(|_| Done::Muted(!muted))),
            Op::Source { id, .. } => match source_id(id) {
                Some(source) => c
                    .select_source_if_current(&source, current)
                    .map(|_| Done::Nothing),
                None => {
                    return Err(Failure::Message(
                        "That source is not there any more.".into(),
                    ))
                }
            },
            Op::Modes(change) => c
                .set_play_modes_if_current(play_modes(*change), current)
                .map(|_| Done::Nothing),
            // A speaker's screen draws no time skip, no list of its own and
            // no keys for the speaker: these only ever reach a device that
            // declared them.
            Op::SeekBy { .. } | Op::Choose { .. } | Op::Key { .. } => {
                return Err(Failure::Message(
                    "That is not something a speaker does.".into(),
                ))
            }
        };
        match outcome {
            Err(couch_sonos::Error::Cancelled) => Err(Failure::Expired),
            other => other.map_err(failure),
        }
    }
    fn sources(&mut self) -> Result<Vec<Choice>, Failure> {
        let sources = self.client()?.sources().map_err(failure)?;
        Ok(sources
            .into_iter()
            .map(|s| Choice {
                id: input_id(&s.id),
                title: s.name,
                detail: s.detail,
                // A player does not say which favourite it is playing.
                current: false,
            })
            .collect())
    }
    /// The art handle is the absolute URL the player gave.
    fn artwork(&mut self, art: &str) -> Result<Vec<u8>, Failure> {
        self.client()?.artwork(art).map_err(failure)
    }
}
/// A group snapshot as the player screen's view of a device. Strings move;
/// nothing is copied.
pub(crate) fn media(snapshot: Snapshot) -> Media {
    let Snapshot {
        status,
        playback,
        now_playing,
    } = snapshot;
    let some = |s: String| Some(s).filter(|s| !s.is_empty());
    let mut media = Media {
        state: match playback.state.as_str() {
            "PLAYING" => PlayState::Playing,
            "BUFFERING" => PlayState::Buffering,
            "PAUSED" => PlayState::Paused,
            _ => PlayState::Idle,
        },
        // The position stands still while the group buffers.
        rate_percent: if playback.state == "PLAYING" { 100 } else { 0 },
        can: Can {
            next: playback.can_skip,
            previous: playback.can_skip_back,
            seek: false,
        },
        modes: crate::media_player::Modes {
            shuffle: playback.modes.shuffle,
            repeat: playback.modes.repeat,
            repeat_one: playback.modes.repeat_one,
            crossfade: playback.modes.crossfade,
        },
        next: now_playing.next.map(|t| NextItem {
            detail: line(&t.artist, &t.album),
            title: t.name,
        }),
        // A member's playback belongs to its group's coordinator.
        follows: (status.coordinator != status.player.uuid).then_some(status.coordinator_name),
        device_name: Some(status.player.name),
        ..Media::default()
    };
    match now_playing.current {
        Some(track) => {
            media.can.seek =
                playback.can_seek && track.duration_ms.is_some() && media.follows.is_none();
            media.title = Some(track.name);
            media.artist = some(track.artist);
            media.album = some(track.album);
            media.subtitle = some(now_playing.container);
            media.source = some(track.service);
            media.art = some(track.image_url);
            media.duration_ms = track.duration_ms;
            media.position_ms = Some(playback.position_ms);
        }
        // A TV or line-in input, or a container with nothing current in it:
        // its name, and what kind of thing it is.
        None if !now_playing.container.is_empty() => {
            media.title = Some(now_playing.container);
            media.subtitle = some(now_playing.container_type.replace('_', " ").to_lowercase());
        }
        None => {}
    }
    media
}
fn play_modes(change: ModeChange) -> PlayModeChange {
    PlayModeChange {
        shuffle: change.shuffle,
        repeat: change.repeat,
        repeat_one: change.repeat_one,
        crossfade: change.crossfade,
    }
}
/// A source as the opaque id of a Sources row, in the words the Sonos package
/// uses for its inputs (`couch-sonos/src/sdk.rs`).
fn input_id(source: &SourceId) -> String {
    match source {
        SourceId::HomeTheater => "tv".to_owned(),
        SourceId::LineIn => "line-in".to_owned(),
        SourceId::Favorite(id) => format!("favorite.{id}"),
        SourceId::Playlist(id) => format!("playlist.{id}"),
    }
}
fn source_id(id: &str) -> Option<SourceId> {
    Some(match id {
        "tv" => SourceId::HomeTheater,
        "line-in" => SourceId::LineIn,
        _ => match id.split_once('.')? {
            ("favorite", item) => SourceId::Favorite(item.to_owned()),
            ("playlist", item) => SourceId::Playlist(item.to_owned()),
            _ => return None,
        },
    })
}
/// Which of the screen's failures a Sonos error is.
pub(crate) fn failure(error: couch_sonos::Error) -> Failure {
    match error {
        couch_sonos::Error::NotCoordinator { coordinator } => Failure::Follows {
            leader: coordinator,
        },
        couch_sonos::Error::Transport => Failure::Unreachable,
        couch_sonos::Error::Api(code) if code == "ERROR_PLAYBACK_NO_CONTENT" => {
            Failure::NothingToPlay
        }
        other => Failure::Message(other.to_string()),
    }
}
/// The group's metadata once a skip has taken effect. A skip is acknowledged
/// before the player switches tracks, so a read straight after it still shows
/// the track being left; poll briefly until the current track differs from
/// `before` (or until a second has passed and the read is what it is).
pub(crate) fn now_playing_after_skip(
    client: &Client,
    before: Option<&couch_sonos::Track>,
) -> Option<couch_sonos::NowPlaying> {
    let same = |now: &couch_sonos::NowPlaying| match (before, now.current.as_ref()) {
        (Some(b), Some(c)) => {
            b.name == c.name && b.artist == c.artist && b.image_url == c.image_url
        }
        (None, None) => true,
        _ => false,
    };
    let mut latest = None;
    for attempt in 0..6 {
        if attempt > 0 {
            std::thread::sleep(Duration::from_millis(200));
        }
        match client.now_playing() {
            Ok(now) => {
                let unchanged = same(&now);
                latest = Some(now);
                if !unchanged {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    latest
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::media_player::{describe, fixtures};
    fn track(
        name: &str,
        artist: &str,
        album: &str,
        art: &str,
        ms: Option<u64>,
    ) -> couch_sonos::Track {
        couch_sonos::Track {
            name: name.into(),
            artist: artist.into(),
            album: album.into(),
            image_url: art.into(),
            duration_ms: ms,
            service: "Apple Music".into(),
        }
    }
    /// The Lounge speaker playing the second track of an album from a playlist.
    fn playing() -> Snapshot {
        Snapshot {
            status: couch_sonos::Status {
                player: couch_sonos::Player {
                    uuid: "RINCON_LOUNGE".into(),
                    name: "Lounge".into(),
                    model: "Era 100".into(),
                },
                coordinator: "RINCON_LOUNGE".into(),
                coordinator_name: "Lounge".into(),
                transport: "PLAYING".into(),
                volume: 18,
                muted: false,
            },
            playback: couch_sonos::PlaybackStatus {
                state: "PLAYING".into(),
                position_ms: 74_210,
                modes: couch_sonos::PlayModes::default(),
                can_seek: true,
                can_skip: true,
                can_skip_back: true,
            },
            now_playing: couch_sonos::NowPlaying {
                container: "Evening".into(),
                container_type: "playlist".into(),
                current: Some(track(
                    "Weird Fishes / Arpeggi",
                    "Radiohead",
                    "In Rainbows",
                    "http://192.0.2.9:1400/getaa?s=1&u=weird-fishes",
                    Some(318_000),
                )),
                next: Some(track(
                    "All I Need",
                    "Radiohead",
                    "In Rainbows",
                    "http://192.0.2.9:1400/getaa?s=1&u=all-i-need",
                    Some(229_000),
                )),
            },
        }
    }
    fn paused() -> Snapshot {
        let mut s = playing();
        s.playback.state = "PAUSED".into();
        s.status.transport = "PAUSED".into();
        s
    }
    fn member(mut s: Snapshot) -> Snapshot {
        s.status.coordinator = "RINCON_KITCHEN".into();
        s.status.coordinator_name = "Kitchen".into();
        s
    }
    /// The player screen's pictures are taken from `media_player::fixtures`.
    /// These are the same speaker states as a Sonos player reports them, and
    /// they read as exactly those fixtures: so the pictures are what the
    /// built-in Sonos screen shows.
    #[test]
    fn a_sonos_snapshot_reads_as_the_state_the_pictures_are_taken_from() {
        assert_eq!(media(playing()), fixtures::playing());
        assert_eq!(media(paused()), fixtures::paused());
        assert_eq!(
            media(member(playing())),
            fixtures::following(fixtures::playing())
        );
        assert_eq!(
            media(member(paused())),
            fixtures::following(fixtures::paused())
        );
        let mut radio = paused();
        radio.now_playing.container = "BBC Radio 6 Music".into();
        radio.now_playing.container_type = "station".into();
        radio.now_playing.current = Some(track(
            "BBC Radio 6 Music",
            "",
            "",
            "http://192.0.2.9:1400/getaa?s=1&u=6music",
            None,
        ));
        radio.now_playing.next = None;
        assert_eq!(media(radio), fixtures::radio());
        let mut tv = paused();
        tv.now_playing = couch_sonos::NowPlaying {
            container: "TV".into(),
            container_type: "linein.homeTheater".into(),
            current: None,
            next: None,
        };
        assert_eq!(media(tv.clone()), fixtures::tv_input());
        tv.now_playing.container.clear();
        tv.now_playing.container_type.clear();
        tv.playback.state = "IDLE".into();
        assert_eq!(media(tv), fixtures::idle());
    }
    #[test]
    fn buffering_shows_as_playing_with_the_clock_standing_still() {
        let mut s = playing();
        s.playback.state = "BUFFERING".into();
        let media = media(s);
        assert_eq!(media.state, PlayState::Buffering);
        assert_eq!(media.rate_percent, 0);
    }
    #[test]
    fn source_ids_survive_the_trip_through_a_sources_row() {
        for source in [
            SourceId::HomeTheater,
            SourceId::LineIn,
            SourceId::Favorite("4".into()),
            SourceId::Playlist("0".into()),
        ] {
            assert_eq!(source_id(&input_id(&source)), Some(source));
        }
        assert_eq!(source_id("queue.1"), None);
    }
    #[test]
    fn errors_are_sentences_for_the_screen() {
        assert_eq!(
            describe(
                failure(couch_sonos::Error::NotCoordinator {
                    coordinator: "Kitchen".into()
                }),
                "Sonos"
            ),
            "Playback is controlled by Kitchen. Open that speaker to change it."
        );
        assert_eq!(
            describe(failure(couch_sonos::Error::Transport), "Sonos"),
            "Cannot reach the speaker."
        );
        assert_eq!(
            describe(
                failure(couch_sonos::Error::Api("ERROR_PLAYBACK_NO_CONTENT".into())),
                "Sonos"
            ),
            "Sonos found nothing to play there."
        );
        assert_eq!(
            describe(failure(couch_sonos::Error::Http(500)), "Sonos"),
            "Sonos HTTP error 500"
        );
    }
}
