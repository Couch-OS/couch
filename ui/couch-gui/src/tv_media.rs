//! Optional Cast now-playing observer. Its network and image workers never gate keys.
use crate::App;
use couch_androidtv::cast::{Observer, Status};
use std::{
    io::{Cursor, Read},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

#[derive(Clone, Default, PartialEq)]
struct Target {
    generation: u64,
    connection: String,
    apple: bool,
}
struct Snapshot {
    generation: u64,
    status: Status,
    at: Instant,
}
#[derive(Clone, PartialEq)]
struct ArtKey {
    generation: u64,
    session: i64,
    url: String,
}
struct Pixels {
    width: u32,
    height: u32,
    data: Vec<u8>,
}
struct ArtReply {
    key: ArtKey,
    pixels: Option<Pixels>,
}

#[derive(Clone)]
struct Presentation {
    active: bool,
    has_art: bool,
    art: slint::Image,
    app: slint::SharedString,
    title: slint::SharedString,
    subtitle: slint::SharedString,
    state: slint::SharedString,
    live: bool,
    position: slint::SharedString,
    has_duration: bool,
    duration: slint::SharedString,
    progress: f32,
    key: Option<ArtKey>,
}
impl Presentation {
    fn capture(app: &App, key: Option<ArtKey>) -> Self {
        Self {
            active: app.get_tv_media_active(),
            has_art: app.get_tv_media_has_art(),
            art: app.get_tv_media_art(),
            app: app.get_tv_media_app(),
            title: app.get_tv_media_title(),
            subtitle: app.get_tv_media_subtitle(),
            state: app.get_tv_media_state(),
            live: app.get_tv_media_live(),
            position: app.get_tv_media_position(),
            has_duration: app.get_tv_media_has_duration(),
            duration: app.get_tv_media_duration(),
            progress: app.get_tv_media_progress(),
            key,
        }
    }
    fn apply(&self, app: &App) {
        app.set_tv_media_active(self.active.clone());
        app.set_tv_media_has_art(self.has_art.clone());
        app.set_tv_media_art(self.art.clone());
        app.set_tv_media_app(self.app.clone());
        app.set_tv_media_title(self.title.clone());
        app.set_tv_media_subtitle(self.subtitle.clone());
        app.set_tv_media_state(self.state.clone());
        app.set_tv_media_live(self.live.clone());
        app.set_tv_media_position(self.position.clone());
        app.set_tv_media_has_duration(self.has_duration.clone());
        app.set_tv_media_duration(self.duration.clone());
        app.set_tv_media_progress(self.progress.clone());
    }
}
pub(super) struct Controller {
    target: Arc<Mutex<Target>>,
    latest: Arc<Mutex<Option<Snapshot>>>,
    art_target: Arc<Mutex<Option<ArtKey>>>,
    art_reply: Arc<Mutex<Option<ArtReply>>>,
    art_key: Option<ArtKey>,
    generation: u64,
    cache: super::ViewCache<Presentation>,
    view_key: Option<super::ViewKey>,
    observed_at: Option<Instant>,
    restoring: bool,
}
impl Controller {
    pub fn new() -> Self {
        let target = Arc::new(Mutex::new(Target::default()));
        let latest = Arc::new(Mutex::new(None));
        let art_target = Arc::new(Mutex::new(None));
        let art_reply = Arc::new(Mutex::new(None));
        let (t, l) = (target.clone(), latest.clone());
        std::thread::spawn(move || observe(t, l));
        let (t, r) = (art_target.clone(), art_reply.clone());
        std::thread::spawn(move || artwork(t, r));
        Self {
            target,
            latest,
            art_target,
            art_reply,
            art_key: None,
            generation: 0,
            cache: super::ViewCache::default(),
            view_key: None,
            observed_at: None,
            restoring: false,
        }
    }
    pub fn open(&mut self, app: &App, generation: u64, key: &super::ViewKey) {
        self.clear(app);
        self.generation = generation;
        self.view_key = Some(key.clone());
        if let Some((at, mut view)) = self.cache.get(key) {
            if let Some(art) = &mut view.key {
                art.generation = generation;
            }
            view.apply(app);
            self.art_key = view.key;
            self.observed_at = Some(at);
            self.restoring = true;
        }
        *self.target.lock().unwrap() = Target {
            generation,
            connection: key.connection.clone(),
            apple: app.get_tv_apple(),
        };
    }
    pub fn clear(&mut self, app: &App) {
        if let (Some(key), Some(at)) = (&self.view_key, self.observed_at) {
            if app.get_tv_media_active() {
                self.cache.put(
                    key.clone(),
                    at,
                    Presentation::capture(app, self.art_key.clone()),
                );
            }
        }
        self.view_key = None;
        self.observed_at = None;
        self.restoring = false;
        self.generation = 0;
        *self.target.lock().unwrap() = Target::default();
        *self.latest.lock().unwrap() = None;
        *self.art_target.lock().unwrap() = None;
        self.art_key = None;
        app.set_tv_media_active(false);
        app.set_tv_media_has_art(false);
        app.set_tv_media_art(slint::Image::default());
    }
    pub fn invalidate(&mut self, app: &App) {
        if let Some(key) = &self.view_key {
            self.cache.remove(key);
        }
        self.observed_at = None;
        self.restoring = false;
        self.art_key = None;
        *self.art_target.lock().unwrap() = None;
        app.set_tv_media_active(false);
        app.set_tv_media_has_art(false);
        app.set_tv_media_art(slint::Image::default());
    }
    pub fn poll(&mut self, app: &App) {
        if !app.get_tv_shown() || !(app.get_tv_android() || app.get_tv_apple()) {
            if self.generation != 0 {
                self.clear(app);
            }
            return;
        }
        let latest = self.latest.lock().unwrap();
        let snapshot = latest
            .as_ref()
            .filter(|s| s.generation == self.generation && s.at.elapsed() < Duration::from_secs(3));
        let media = snapshot.and_then(|s| s.status.now_playing.as_ref());
        let changed_app =
            snapshot.is_some_and(|s| app_changed(&s.status, app.get_tv_media_app().as_str()));
        let preserve = !changed_app
            && preserve_loading(
                self.restoring,
                self.observed_at,
                snapshot.map(|s| &s.status),
            );
        if let Some(media) = media {
            self.observed_at = snapshot.map(|s| s.at);
            self.restoring = false;
            app.set_tv_media_active(true);
            app.set_tv_media_app(
                latest
                    .as_ref()
                    .and_then(|s| s.status.app_name.as_deref())
                    .unwrap_or(if app.get_tv_apple() {
                        "Apple TV"
                    } else {
                        "Android TV"
                    })
                    .into(),
            );
            app.set_tv_media_title(media.title.as_str().into());
            app.set_tv_media_subtitle(media.subtitle.as_str().into());
            app.set_tv_media_state(
                match media.player_state.as_str() {
                    "PLAYING" => "Playing",
                    "PAUSED" => "Paused",
                    "BUFFERING" => "Buffering",
                    "STOPPED" => "Stopped",
                    "INTERRUPTED" => "Interrupted",
                    "SEEKING" => "Seeking",
                    _ => "",
                }
                .into(),
            );
            app.set_tv_media_live(media.live);
            let (position, duration, progress) =
                timeline(media.position, media.duration, media.live);
            app.set_tv_media_position(if media.position_known {
                position.into()
            } else {
                "—".into()
            });
            app.set_tv_media_has_duration(duration.is_some() && media.position_known);
            app.set_tv_media_duration(duration.unwrap_or_default().into());
            app.set_tv_media_progress(progress);
            let key = media.artwork_url.as_ref().map(|url| ArtKey {
                generation: self.generation,
                session: media.session_id,
                url: url.clone(),
            });
            if key != self.art_key
                || (key.is_some()
                    && !app.get_tv_media_has_art()
                    && self.art_target.lock().unwrap().is_none())
            {
                app.set_tv_media_has_art(false);
                app.set_tv_media_art(slint::Image::default());
                *self.art_target.lock().unwrap() = key.clone();
                self.art_key = key;
            }
        } else if !preserve {
            self.observed_at = None;
            self.restoring = false;
            if let Some(key) = &self.view_key {
                self.cache.remove(key);
            }
            app.set_tv_media_active(false);
            app.set_tv_media_has_art(false);
            app.set_tv_media_art(slint::Image::default());
            self.art_key = None;
            *self.art_target.lock().unwrap() = None;
        }
        drop(latest);
        if let Some(reply) = self.art_reply.lock().unwrap().take() {
            if Some(&reply.key) == self.art_key.as_ref() {
                if let Some(p) = reply.pixels {
                    app.set_tv_media_art(slint::Image::from_rgba8(slint::SharedPixelBuffer::<
                        slint::Rgba8Pixel,
                    >::clone_from_slice(
                        &p.data, p.width, p.height
                    )));
                    app.set_tv_media_has_art(true);
                }
            }
        }
    }
}
fn app_changed(status: &Status, cached_app: &str) -> bool {
    status
        .app_name
        .as_deref()
        .is_some_and(|name| name != cached_app)
}
fn preserve_loading(restoring: bool, at: Option<Instant>, status: Option<&Status>) -> bool {
    restoring
        && at.is_some_and(|at| at.elapsed() < Duration::from_secs(30))
        && status.is_none_or(|s| !s.media_status_known && s.connected)
}
fn failure(generation: u64) -> Snapshot {
    Snapshot {
        generation,
        status: Status {
            media_status_known: true,
            ..Status::default()
        },
        at: Instant::now(),
    }
}
fn observe(target: Arc<Mutex<Target>>, latest: Arc<Mutex<Option<Snapshot>>>) {
    let mut current = Target::default();
    let mut client = None;
    let mut airplay = None;
    let mut stored_stamp = None;
    let mut retry = Instant::now();
    loop {
        let requested = target.lock().unwrap().clone();
        if requested != current {
            current = requested;
            client = None;
            airplay = None;
            stored_stamp = None;
            retry = Instant::now();
        }
        if current.generation == 0 {
            std::thread::sleep(Duration::from_millis(100));
            continue;
        }
        if current.apple {
            let path = crate::connections::file(&current.connection, "appletv-metadata");
            let stamp = std::fs::metadata(&path)
                .ok()
                .and_then(|m| m.modified().ok().map(|t| (t, m.len())));
            let control_stamp =
                std::fs::metadata(crate::connections::file(&current.connection, "appletv"))
                    .ok()
                    .and_then(|m| m.modified().ok().map(|t| (t, m.len())));
            let stamp = (stamp, control_stamp);
            if Some(stamp) != stored_stamp {
                airplay = None;
                stored_stamp = Some(stamp);
                retry = Instant::now();
            }
            if airplay.is_none() && Instant::now() >= retry {
                airplay = couch_appletv::metadata::StoredConnection::load(&path)
                    .ok()
                    .filter(|c| {
                        couch_control::StreamingConnection::load(&crate::connections::file(
                            &current.connection,
                            "appletv",
                        ))
                        .ok()
                        .is_some_and(|control| {
                            control.kind() == "appletv" && control.address() == c.settings.address
                        })
                    })
                    .and_then(|c| {
                        couch_appletv::metadata::Client::connect(&c.settings, &c.credentials).ok()
                    });
                retry = Instant::now() + Duration::from_secs(10);
            }
            let status = if let Some(c) = airplay.as_mut() {
                match c.poll() {
                    Ok(_) => Some(apple_status(&c.now_playing(), std::time::SystemTime::now())),
                    Err(_) => {
                        airplay = None;
                        retry = Instant::now() + Duration::from_secs(10);
                        None
                    }
                }
            } else {
                None
            };
            if *target.lock().unwrap() == current {
                *latest.lock().unwrap() = Some(Snapshot {
                    generation: current.generation,
                    status: status.unwrap_or_default(),
                    at: Instant::now(),
                });
            }
            std::thread::sleep(Duration::from_millis(100));
            continue;
        }
        if client.is_none() && Instant::now() >= retry {
            client = couch_control::StreamingConnection::load(&crate::connections::file(
                &current.connection,
                "androidtv",
            ))
            .ok()
            .filter(|c| c.kind() == "androidtv")
            .and_then(|c| Observer::connect(c.address(), Duration::from_secs(2)).ok());
            retry = Instant::now() + Duration::from_secs(10);
            if client.is_none() && *target.lock().unwrap() == current {
                *latest.lock().unwrap() = Some(failure(current.generation));
            }
        }
        if let Some(c) = client.as_mut() {
            match c.poll(Duration::from_millis(200)) {
                Ok(status) => {
                    if *target.lock().unwrap() == current {
                        *latest.lock().unwrap() = Some(Snapshot {
                            generation: current.generation,
                            status,
                            at: Instant::now(),
                        });
                    }
                }
                Err(_) => {
                    client = None;
                    if *target.lock().unwrap() == current {
                        *latest.lock().unwrap() = Some(failure(current.generation));
                    }
                    retry = Instant::now() + Duration::from_secs(10);
                }
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}
// Map observed values into the shared presentation. Never fetch an AirPlay artwork URL.
fn apple_status(m: &couch_appletv::metadata::NowPlaying, now: std::time::SystemTime) -> Status {
    use couch_appletv::metadata::PlaybackState;
    let text = |v: Option<&str>| {
        v.unwrap_or("")
            .chars()
            .filter(|c| !c.is_control())
            .take(512)
            .collect::<String>()
    };
    let position = m.position_at(now);
    let known = m.title.is_some() || m.item_id.is_some() || m.state != PlaybackState::Unknown;
    Status {
        connected: true,
        media_status_known: true,
        app_name: m.app_name.as_deref().map(|s| text(Some(s))),
        now_playing: known.then(|| couch_androidtv::cast::NowPlaying {
            title: text(m.title.as_deref()),
            subtitle: text(
                m.subtitle
                    .as_deref()
                    .or(m.artist.as_deref())
                    .or(m.series.as_deref()),
            ),
            position: position.unwrap_or(0.),
            position_known: position.is_some(),
            duration: m.duration,
            live: m.is_live == Some(true),
            player_state: match m.state {
                PlaybackState::Playing => "PLAYING",
                PlaybackState::Paused => "PAUSED",
                PlaybackState::Stopped => "STOPPED",
                PlaybackState::Interrupted => "INTERRUPTED",
                PlaybackState::Seeking => "SEEKING",
                PlaybackState::Unknown => "",
            }
            .into(),
            ..Default::default()
        }),
    }
}
fn timeline(position: f64, duration: Option<f64>, live: bool) -> (String, Option<String>, f32) {
    let duration = duration.filter(|d| d.is_finite() && *d > 0. && !live);
    let position = if position.is_finite() {
        position.max(0.)
    } else {
        0.
    };
    let position = duration.map_or(position, |d| position.min(d));
    (
        clock(position),
        duration.map(clock),
        duration.map_or(0., |d| (position / d) as f32),
    )
}
fn clock(seconds: f64) -> String {
    let seconds = seconds.min(359_999.) as u64;
    if seconds >= 3600 {
        format!(
            "{}:{:02}:{:02}",
            seconds / 3600,
            seconds / 60 % 60,
            seconds % 60
        )
    } else {
        format!("{}:{:02}", seconds / 60, seconds % 60)
    }
}
#[derive(Default)]
struct ArtSchedule {
    key: Option<ArtKey>,
    next: Option<Instant>,
}
impl ArtSchedule {
    fn due(&mut self, wanted: Option<ArtKey>, now: Instant) -> Option<ArtKey> {
        if wanted != self.key {
            self.key = wanted;
            self.next = Some(now);
        }
        if self.next.is_some_and(|at| now >= at) {
            self.next = None;
            self.key.clone()
        } else {
            None
        }
    }
    fn failed(&mut self, now: Instant) {
        self.next = Some(now + Duration::from_secs(15));
    }
}
fn artwork(target: Arc<Mutex<Option<ArtKey>>>, reply: Arc<Mutex<Option<ArtReply>>>) {
    let mut schedule = ArtSchedule::default();
    loop {
        let wanted = target.lock().unwrap().clone();
        if let Some(key) = schedule.due(wanted, Instant::now()) {
            let pixels = fetch(&key.url);
            if pixels.is_none() {
                // A transient DNS/server failure must not suppress this image forever.
                // Never log artwork URLs: providers may put credentials in the query.
                eprintln!("couch-gui: Android artwork unavailable; retrying in 15 seconds");
                schedule.failed(Instant::now());
            }
            if target.lock().unwrap().as_ref() == Some(&key) {
                *reply.lock().unwrap() = Some(ArtReply { key, pixels });
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}
fn fetch(url: &str) -> Option<Pixels> {
    // URLs come from the paired TV. No device credentials accompany artwork.
    let parsed: ureq::http::Uri = url.parse().ok()?;
    if !matches!(parsed.scheme_str(), Some("http" | "https"))
        || parsed.authority()?.as_str().contains('@')
    {
        return None;
    }
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(4)))
        .max_redirects(3)
        .build()
        .into();
    let mut response = agent.get(url).call().ok()?;
    let mut data = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take(4 * 1024 * 1024 + 1)
        .read_to_end(&mut data)
        .ok()?;
    if data.len() > 4 * 1024 * 1024 {
        return None;
    }
    decode(&data)
}
fn decode(data: &[u8]) -> Option<Pixels> {
    let mut reader = image::ImageReader::new(Cursor::new(data))
        .with_guessed_format()
        .ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(32 * 1024 * 1024);
    reader.limits(limits);
    let decoded = reader
        .decode()
        .ok()?
        .resize_to_fill(480, 800, image::imageops::FilterType::Triangle)
        .to_rgba8();
    Some(Pixels {
        width: decoded.width(),
        height: decoded.height(),
        data: decoded.into_raw(),
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn apple_metadata_maps_unknown_live_and_untrusted_artwork() {
        use couch_appletv::metadata::{NowPlaying, PlaybackState};
        let now = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(100);
        assert!(apple_status(&NowPlaying::default(), now)
            .now_playing
            .is_none());
        let mut value = NowPlaying {
            title: Some("Fixture\nmovie".into()),
            state: PlaybackState::Paused,
            artwork_url: Some("http://127.0.0.1/private".into()),
            ..Default::default()
        };
        let m = apple_status(&value, now).now_playing.unwrap();
        assert_eq!(m.title, "Fixturemovie");
        assert!(!m.position_known);
        assert!(m.artwork_url.is_none());
        assert_eq!(m.player_state, "PAUSED");
        value.state = PlaybackState::Playing;
        value.position = Some(20.);
        value.position_timestamp = Some(90.);
        value.duration = Some(25.);
        assert_eq!(apple_status(&value, now).now_playing.unwrap().position, 25.);
        value.is_live = Some(true);
        let m = apple_status(&value, now).now_playing.unwrap();
        assert!(timeline(m.position, m.duration, m.live).1.is_none());
    }
    #[test]
    fn apple_metadata_screen_clears_on_failure_and_generation_change() {
        if std::env::var_os("COUCH_TEST_APPLE_METADATA").is_none() {
            let output=std::process::Command::new(std::env::current_exe().unwrap()).args(["--exact","tv::media::tests::apple_metadata_screen_clears_on_failure_and_generation_change"]).env("COUCH_TEST_APPLE_METADATA","1").output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        use slint::ComponentHandle;
        let window =
            crate::panel::CouchPlatform::install(slint::PhysicalSize::new(480, 800)).unwrap();
        let app = App::new().unwrap();
        app.set_tv_apple(true);
        app.set_tv_shown(true);
        app.set_tv_title("Living room Apple TV".into());
        app.set_tv_status("Connected · Companion".into());
        let mut controller = Controller::new();
        controller.generation = 1;
        let value = couch_appletv::metadata::NowPlaying {
            title: Some("The Long Way Home".into()),
            artist: Some("Episode 3 · A new beginning".into()),
            app_name: Some("TV".into()),
            state: couch_appletv::metadata::PlaybackState::Paused,
            position: Some(624.),
            duration: Some(2820.),
            ..Default::default()
        };
        *controller.latest.lock().unwrap() = Some(Snapshot {
            generation: 1,
            status: apple_status(&value, std::time::SystemTime::now()),
            at: Instant::now(),
        });
        controller.poll(&app);
        assert!(app.get_tv_media_active());
        assert_eq!(app.get_tv_media_title(), "The Long Way Home");
        assert_eq!(app.get_tv_media_state(), "Paused");
        assert!(!app.get_tv_media_has_art());
        let actions = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let received = actions.clone();
        app.on_tv_action(move |a| received.borrow_mut().push(a.to_string()));
        app.show().unwrap();
        app.invoke_focus_tv();
        slint::platform::update_timers_and_animations();
        if let Some(path) = std::env::var_os("COUCH_APPLE_METADATA_SCREENSHOT") {
            window.draw_if_needed(|renderer| {
                let mut pixels = vec![slint::Rgb8Pixel::default(); 480 * 800];
                renderer.render(&mut pixels, 480);
                let bytes: Vec<u8> = pixels.into_iter().flat_map(|p| [p.r, p.g, p.b]).collect();
                image::save_buffer(
                    std::path::Path::new(&path),
                    &bytes,
                    480,
                    800,
                    image::ColorType::Rgb8,
                )
                .unwrap();
            });
        }
        for (x, y) in [
            (84., 586.),
            (186., 586.),
            (288., 586.),
            (390., 586.),
            (230., 687.),
        ] {
            for pressed in [true, false] {
                let position = slint::LogicalPosition::new(x, y);
                let button = slint::platform::PointerEventButton::Left;
                window.dispatch_event(if pressed {
                    slint::platform::WindowEvent::PointerPressed { position, button }
                } else {
                    slint::platform::WindowEvent::PointerReleased { position, button }
                });
            }
        }
        assert_eq!(
            &*actions.borrow(),
            &["previous", "play", "pause", "next", "wake"]
        );
        *controller.latest.lock().unwrap() = Some(failure(1));
        controller.poll(&app);
        assert!(!app.get_tv_media_active());
        assert_eq!(app.get_tv_status(), "Connected · Companion");
        *controller.latest.lock().unwrap() = Some(Snapshot {
            generation: 2,
            status: apple_status(&value, std::time::SystemTime::now()),
            at: Instant::now(),
        });
        controller.poll(&app);
        assert!(!app.get_tv_media_active());
    }
    #[test]
    fn reentry_restores_decoded_art_and_empty_snapshot_clears_it() {
        if std::env::var_os("COUCH_TEST_MEDIA_CACHE").is_none() {
            let result = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "tv::media::tests::reentry_restores_decoded_art_and_empty_snapshot_clears_it",
                ])
                .env("COUCH_TEST_MEDIA_CACHE", "1")
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
            return;
        }
        crate::panel::CouchPlatform::install(slint::PhysicalSize::new(480, 800)).unwrap();
        let app = App::new().unwrap();
        let mut controller = Controller::new();
        let key = super::super::ViewKey {
            connection: "fixture".into(),
            device: "fixture-device".into(),
            name: "TV".into(),
            activity: "movie".into(),
            config: 1,
        };
        controller.open(&app, 1, &key);
        *controller.target.lock().unwrap() = Target::default();
        app.set_tv_android(true);
        app.set_tv_shown(true);
        app.set_tv_media_active(true);
        app.set_tv_media_title("Film".into());
        app.set_tv_media_has_art(true);
        app.set_tv_media_art(slint::Image::from_rgba8(slint::SharedPixelBuffer::<
            slint::Rgba8Pixel,
        >::new(2, 3)));
        controller.observed_at = Some(Instant::now());
        controller.art_key = Some(ArtKey {
            generation: 1,
            session: 7,
            url: "https://example.test/poster".into(),
        });
        controller.clear(&app);
        assert!(!app.get_tv_media_active());
        controller.open(&app, 2, &key);
        *controller.target.lock().unwrap() = Target::default();
        assert!(app.get_tv_media_active());
        assert!(app.get_tv_media_has_art());
        assert_eq!(app.get_tv_media_title(), "Film");
        assert_eq!(app.get_tv_media_art().size().width, 2);
        assert_eq!(app.get_tv_media_art().size().height, 3);
        assert_eq!(controller.art_key.as_ref().unwrap().generation, 2);
        *controller.latest.lock().unwrap() = Some(Snapshot {
            generation: 2,
            status: Status {
                connected: true,
                ..Status::default()
            },
            at: Instant::now(),
        });
        controller.poll(&app);
        assert!(app.get_tv_media_active());
        *controller.latest.lock().unwrap() = Some(failure(2));
        controller.poll(&app);
        assert!(!app.get_tv_media_active());
        assert!(!app.get_tv_media_has_art());
        assert!(controller.cache.get(&key).is_none());
    }
    #[test]
    fn cached_media_survives_only_initial_loading_within_original_ttl() {
        let now = Instant::now();
        let loading = Status {
            connected: true,
            ..Status::default()
        };
        assert!(!app_changed(&loading, "Plex"));
        let switched = Status {
            app_name: Some("YouTube".into()),
            ..loading.clone()
        };
        assert!(app_changed(&switched, "Plex"));
        assert!(!app_changed(&switched, "YouTube"));
        assert!(preserve_loading(true, Some(now), None));
        assert!(preserve_loading(true, Some(now), Some(&loading)));
        let empty = Status {
            media_status_known: true,
            ..loading.clone()
        };
        assert!(!preserve_loading(true, Some(now), Some(&empty)));
        assert!(!preserve_loading(true, Some(now), Some(&Status::default())));
        assert!(!preserve_loading(
            true,
            Some(now - Duration::from_secs(31)),
            Some(&loading)
        ));
        assert!(!preserve_loading(false, Some(now), Some(&loading)));
        assert!(!preserve_loading(true, Some(now), Some(&failure(1).status)));
    }
    #[test]
    fn progress_handles_live_missing_invalid_and_overrun_values() {
        assert_eq!(
            timeline(75., Some(100.), false),
            ("1:15".into(), Some("1:40".into()), 0.75)
        );
        assert_eq!(timeline(120., Some(100.), false).2, 1.);
        assert_eq!(
            timeline(f64::NAN, Some(f64::INFINITY), false),
            ("0:00".into(), None, 0.)
        );
        assert_eq!(timeline(120., Some(100.), true), ("2:00".into(), None, 0.));
        assert_eq!(clock(3661.), "1:01:01");
    }
    #[test]
    fn failed_art_retries_after_backoff_and_new_sessions_do_not_wait() {
        let now = Instant::now();
        let key = ArtKey {
            generation: 1,
            session: 1,
            url: "https://example.test/art.jpg".into(),
        };
        let mut schedule = ArtSchedule::default();
        assert!(schedule.due(Some(key.clone()), now).is_some());
        schedule.failed(now);
        assert!(schedule
            .due(Some(key.clone()), now + Duration::from_secs(14))
            .is_none());
        assert!(schedule
            .due(Some(key.clone()), now + Duration::from_secs(15))
            .is_some());
        assert!(schedule
            .due(Some(key.clone()), now + Duration::from_secs(30))
            .is_none());
        schedule.failed(now + Duration::from_secs(30));
        let next = ArtKey {
            generation: 2,
            ..key
        };
        assert!(schedule
            .due(Some(next.clone()), now + Duration::from_secs(31))
            .is_some());
        assert!(schedule.due(None, now + Duration::from_secs(32)).is_none());
        assert!(schedule
            .due(Some(next), now + Duration::from_secs(33))
            .is_some());
    }
    #[test]
    fn invalid_artwork_falls_back_without_panicking() {
        assert!(decode(b"not an image").is_none());
    }
}
