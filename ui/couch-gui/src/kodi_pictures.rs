//! Pictures of the built-in Kodi player screen, taken from fixture data with
//! no network and no device: PR-D0 of the protocol 3 "Media" plan
//! (`docs/plans` - the built-in Kodi screen is photographed before a
//! packaged Kodi has to match it; the built-in path is left alone here on
//! purpose, because it is only being photographed, not generalised).
//!
//! `activity::Controller` reaches Kodi over its own TCP worker thread, not
//! through a fakeable trait the way `media_player::Backend` lets the Sonos
//! screen be driven from a fixture speaker in
//! `media_player.rs`'s `the_player_screen_is_the_same_picture_for_the_same_speaker`.
//! So this file does not exercise that worker: it sets the same `App`
//! properties `activity::Controller::poll` would have set for each state,
//! the way `screen_pictures.rs` already does for the screens it covers. What
//! a raw Kodi answer turns into those properties is `activity.rs`'s own
//! tests' job; what the screen looks like once it has turned into them is
//! this file's only job.
//!
//! `COUCH_KODI_SCREENSHOTS=<dir>` writes each 480x800 picture there as a PNG,
//! and the words on the screen alongside it as `kodi-screen.txt`.
//! `COUCH_KODI_GOLDENS=<dir>` compares each picture with the PNG of the same
//! name already in that directory, pixel for pixel - the
//! `COUCH_PLAYER_GOLDENS` arm in `media_player.rs`, copied. Neither is set in
//! CI, so the words compared against `tests/golden/kodi-screen.txt` below are
//! the gate that actually runs there. Nothing binary is kept in the tree.
use crate::{activity_art, App, PlayerChoice};
use slint::{ModelRc, VecModel};

/// `activity.rs`'s own `clock`, recreated rather than exposed: a handful of
/// lines, not a reason to open a private helper to a sibling module.
fn clock(t: f64) -> String {
    let t = t.max(0.) as u64;
    if t >= 3600 {
        format!("{}:{:02}:{:02}", t / 3600, t / 60 % 60, t % 60)
    } else {
        format!("{}:{:02}", t / 60, t % 60)
    }
}

/// A backdrop, generated rather than checked in: a different pattern from
/// `media_player.rs`'s square cover art, so the two are never mistaken for
/// each other in a side-by-side. Decoded through the real `activity_art`
/// path - the same `Shape::Backdrop` resize and legibility gradient a fanart
/// JPEG gets - so this is what the panel actually draws, not a stand-in.
fn backdrop() -> slint::Image {
    let picture = image::RgbImage::from_fn(960, 540, |x, y| {
        let band = (x / 60 + y / 60) % 2 == 0;
        image::Rgb([
            if band { 70 } else { 25 },
            (y * 170 / 540) as u8,
            (x * 210 / 960) as u8,
        ])
    });
    let mut bytes = std::io::Cursor::new(Vec::new());
    picture
        .write_to(&mut bytes, image::ImageFormat::Png)
        .unwrap();
    let pixels = activity_art::decode(&bytes.into_inner(), activity_art::Shape::Backdrop).unwrap();
    activity_art::slint_image(pixels)
}

/// A film long enough, and old enough, to need both a clock and a year
/// rather than a show's "S1 E1": `main.rs`'s own mock-up already picked
/// Tarkovsky's "Andrei Rublev" as what built-in Kodi shows paused
/// (`act(1, "Paused - Andrei Rublev", ...)`), so its eight parts are also
/// where the chapters fixture below gets its names.
struct Film {
    title: &'static str,
    year: &'static str,
    duration_s: f64,
    position_s: f64,
}
const ANDREI_RUBLEV: Film = Film {
    title: "Andrei Rublev",
    year: "1966",
    duration_s: 12_300.,
    position_s: 2_537.,
};

/// Chapters as `activity.rs::panel` lists them: a name when the file has
/// one, the time as `clock` reads it. Andrei Rublev's own eight parts,
/// trimmed to five so the sheet does not scroll off the bottom of the
/// picture.
fn chapters() -> Vec<PlayerChoice> {
    [
        ("The Jester", 0.),
        ("Theophanes the Greek", 1_620.),
        ("The Passion According to Andrei", 4_080.),
        ("The Raid", 7_020.),
        ("The Bell", 10_500.),
    ]
    .into_iter()
    .map(|(title, time)| PlayerChoice {
        title: title.into(),
        detail: clock(time).into(),
    })
    .collect()
}
/// Audio streams as `activity.rs::panel` lists them: a name or language,
/// then the codec.
fn audio_tracks() -> Vec<PlayerChoice> {
    [("Russian (Original)", "AC3"), ("English", "AAC")]
        .into_iter()
        .map(|(title, detail)| PlayerChoice {
            title: title.into(),
            detail: detail.into(),
        })
        .collect()
}
/// Subtitle streams the same way, with the "Off" row the screen always adds
/// first.
fn subtitle_tracks() -> Vec<PlayerChoice> {
    [("Off", ""), ("English", "subrip"), ("Russian", "subrip")]
        .into_iter()
        .map(|(title, detail)| PlayerChoice {
            title: title.into(),
            detail: detail.into(),
        })
        .collect()
}

/// The header and transport every "something is loaded" picture shares:
/// `Controller::open`'s target resolution and the `Event::State(Ok(..))` arm
/// of `Controller::poll`, in `activity.rs`, for a film playing or paused.
fn show_playing(app: &App, film: &Film, paused: bool) {
    app.set_player_shown(true);
    app.set_player_music(false);
    app.set_player_activity("Watch TV".into());
    app.set_player_room("Living room".into());
    app.set_player_icon(crate::icons::image(couch_model::Icon::Tv));
    app.set_player_known(true);
    app.set_player_active(true);
    app.set_player_ready(true);
    app.set_player_connected(true);
    app.set_player_paused(paused);
    app.set_player_can_seek(true);
    app.set_player_title(film.title.into());
    app.set_player_metadata(film.year.into());
    app.set_player_elapsed(clock(film.position_s).into());
    app.set_player_remaining(format!("−{}", clock(film.duration_s - film.position_s)).into());
    app.set_player_progress((film.position_s / film.duration_s * 100.) as f32);
    app.set_player_has_art(true);
    app.set_player_fanart(backdrop());
    app.set_player_has_logo(false);
    app.set_player_logo(slint::Image::default());
    app.set_player_panel(0);
    app.set_player_panel_detail("".into());
    app.set_player_choices(ModelRc::new(VecModel::from(Vec::<PlayerChoice>::new())));
    app.set_player_message("".into());
}
/// Connected, with nothing loaded: the `s.playing.is_none()` arm of the same
/// `Event::State(Ok(..))` handler, word for word
/// ("Connected to Kodi.\nUse the remote to choose something on your TV.").
fn show_idle(app: &App) {
    app.set_player_shown(true);
    app.set_player_music(false);
    app.set_player_activity("Watch TV".into());
    app.set_player_room("Living room".into());
    app.set_player_icon(crate::icons::image(couch_model::Icon::Tv));
    app.set_player_known(true);
    app.set_player_active(true);
    app.set_player_ready(false);
    app.set_player_connected(true);
    app.set_player_paused(true);
    app.set_player_can_seek(false);
    app.set_player_title(
        "Connected to Kodi.\nUse the remote to choose something on your TV.".into(),
    );
    app.set_player_metadata("".into());
    app.set_player_elapsed("".into());
    app.set_player_remaining("".into());
    app.set_player_progress(0.);
    app.set_player_has_art(false);
    app.set_player_fanart(slint::Image::default());
    app.set_player_has_logo(false);
    app.set_player_logo(slint::Image::default());
    app.set_player_panel(0);
    app.set_player_panel_detail("".into());
    app.set_player_choices(ModelRc::new(VecModel::from(Vec::<PlayerChoice>::new())));
    app.set_player_message("".into());
}
/// The read failing: the `Event::State(Err(e))` arm, with the worker's own
/// words for an unreachable player.
fn show_error(app: &App) {
    app.set_player_shown(true);
    app.set_player_music(false);
    app.set_player_activity("Watch TV".into());
    app.set_player_room("Living room".into());
    app.set_player_icon(crate::icons::image(couch_model::Icon::Tv));
    app.set_player_known(true);
    app.set_player_active(true);
    app.set_player_ready(false);
    app.set_player_connected(false);
    app.set_player_paused(true);
    app.set_player_can_seek(false);
    app.set_player_title(
        "Kodi is unavailable. Check the player and its remote-control settings.".into(),
    );
    app.set_player_metadata("".into());
    app.set_player_elapsed("".into());
    app.set_player_remaining("".into());
    app.set_player_progress(0.);
    app.set_player_has_art(false);
    app.set_player_fanart(slint::Image::default());
    app.set_player_has_logo(false);
    app.set_player_logo(slint::Image::default());
    app.set_player_panel(0);
    app.set_player_panel_detail("".into());
    app.set_player_choices(ModelRc::new(VecModel::from(Vec::<PlayerChoice>::new())));
    app.set_player_message("".into());
}
/// Open a sheet over whatever is already showing: `activity.rs::panel`'s own
/// tail end.
fn open_sheet(app: &App, panel: i32, rows: Vec<PlayerChoice>) {
    app.set_player_choices(ModelRc::new(VecModel::from(rows)));
    app.set_player_panel_detail("".into());
    app.set_player_panel(panel);
}

#[cfg(test)]
mod tests {
    use super::*;
    use slint::Model;
    use std::time::Duration;

    /// Everything on the screen that is not a pixel, in the same shape
    /// `media_player.rs`'s own `words` dumps the Sonos player in - heading,
    /// title, clock, flags, the open sheet - so a change to either screen
    /// reads the same way in a diff. `music` never appears in the flags
    /// here: built-in Kodi never sets it, and that is exactly the thing this
    /// file exists to keep true.
    fn words(app: &App) -> String {
        let mut flags = Vec::new();
        for (on, name) in [
            (app.get_player_shown(), "shown"),
            (app.get_player_music(), "music"),
            (app.get_player_connected(), "connected"),
            (app.get_player_ready(), "ready"),
            (app.get_player_paused(), "paused"),
            (app.get_player_can_seek(), "can-seek"),
            (app.get_player_has_art(), "art"),
            (app.get_player_has_logo(), "logo"),
        ] {
            if on {
                flags.push(name);
            }
        }
        let mut out = format!(
            "  heading: {} / {}\n  title: {:?}\n  metadata: {:?}\n  clock: {:?} {:?} {:.2}%\n  flags: {}\n  selected: {}\n  sheets: {}\n",
            app.get_player_activity(),
            app.get_player_room(),
            app.get_player_title(),
            app.get_player_metadata(),
            app.get_player_elapsed(),
            app.get_player_remaining(),
            app.get_player_progress(),
            flags.join(" "),
            app.get_player_selected(),
            app.get_player_sheets()
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
                .join(", "),
        );
        if app.get_player_panel() != 0 {
            out += &format!(
                "  sheet {}: {:?}\n",
                app.get_player_panel(),
                app.get_player_panel_detail()
            );
            for row in app.get_player_choices().iter() {
                out += &format!("    {:?} / {:?}\n", row.title, row.detail);
            }
        }
        if !app.get_player_message().is_empty() {
            out += &format!("  notice: {:?}\n", app.get_player_message());
        }
        out
    }

    /// The built-in Kodi player screen, picture by picture, from fixture
    /// data alone: playing a film, paused, idle, its three sheets, and an
    /// error. PR-D0 of the protocol 3 "Media" plan - the pictures a
    /// packaged Kodi's player screen must be held to before the built-in one
    /// it is replacing can be deleted.
    ///
    /// Slint is single-threaded on this target: run the window fixture in
    /// its own process, like the other screen-picture tests.
    #[test]
    fn the_kodi_screen_is_the_same_picture_for_the_same_fixture() {
        if std::env::var_os("COUCH_TEST_KODI_PICTURES").is_none() {
            let out = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "kodi_pictures::tests::the_kodi_screen_is_the_same_picture_for_the_same_fixture",
                ])
                .env("COUCH_TEST_KODI_PICTURES", "1")
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            return;
        }
        use slint::{platform::WindowEvent, ComponentHandle};
        let window =
            crate::panel::CouchPlatform::install(slint::PhysicalSize::new(480, 800)).unwrap();
        let app = App::new().unwrap();
        app.show().unwrap();
        window.dispatch_event(WindowEvent::WindowActiveChanged(true));
        let record = std::cell::RefCell::new(String::new());
        let differing = std::cell::RefCell::new(Vec::new());
        // One buffer for the whole run, as on the panel: the renderer
        // redraws only what changed since the last frame.
        let frame = std::cell::RefCell::new(vec![slint::Rgb8Pixel::default(); 480 * 800]);
        // One picture: let the sheet's slide-in settle, draw, and note the
        // words beside it.
        let picture = |name: &str| {
            for _ in 0..15 {
                slint::platform::update_timers_and_animations();
                std::thread::sleep(Duration::from_millis(16));
            }
            slint::platform::update_timers_and_animations();
            let mut pixels = frame.borrow_mut();
            window.request_redraw();
            window.draw_if_needed(|r| {
                r.render(&mut pixels, 480);
            });
            let bytes: Vec<u8> = pixels.iter().flat_map(|p| [p.r, p.g, p.b]).collect();
            let file = format!("kodi-{name}.png");
            if let Some(dir) = std::env::var_os("COUCH_KODI_SCREENSHOTS") {
                std::fs::create_dir_all(&dir).unwrap();
                let path = std::path::Path::new(&dir).join(&file);
                image::save_buffer(path, &bytes, 480, 800, image::ColorType::Rgb8).unwrap();
            }
            if let Some(dir) = std::env::var_os("COUCH_KODI_GOLDENS") {
                let golden = image::open(std::path::Path::new(&dir).join(&file))
                    .unwrap()
                    .to_rgb8();
                let wrong = golden
                    .as_raw()
                    .chunks(3)
                    .zip(bytes.chunks(3))
                    .filter(|(a, b)| a != b)
                    .count();
                if golden.dimensions() != (480, 800) || wrong != 0 {
                    differing
                        .borrow_mut()
                        .push(format!("{file}: {wrong} pixels"));
                }
            }
            let mut record = record.borrow_mut();
            *record += &format!("== {name}\n{}", words(&app));
        };

        let film = ANDREI_RUBLEV;

        show_playing(&app, &film, false);
        app.invoke_focus_player();
        picture("01-playing");

        show_playing(&app, &film, true);
        app.invoke_focus_player();
        picture("02-paused");

        show_idle(&app);
        app.invoke_focus_player();
        picture("03-idle");

        show_playing(&app, &film, false);
        open_sheet(&app, 1, chapters());
        app.invoke_focus_player();
        picture("04-chapters");

        open_sheet(&app, 2, audio_tracks());
        app.invoke_focus_player();
        picture("05-audio");

        open_sheet(&app, 3, subtitle_tracks());
        app.invoke_focus_player();
        picture("06-subtitles");

        show_error(&app);
        app.invoke_focus_player();
        picture("07-error");

        let record = record.into_inner();
        if let Some(dir) = std::env::var_os("COUCH_KODI_SCREENSHOTS") {
            std::fs::write(std::path::Path::new(&dir).join("kodi-screen.txt"), &record).unwrap();
        }
        assert!(
            differing.borrow().is_empty(),
            "pictures differ from the goldens: {:?}",
            differing.borrow()
        );
        let golden = include_str!("../tests/golden/kodi-screen.txt");
        assert!(
            record == golden,
            "the Kodi screen's words changed; this run said:\n{record}"
        );
        app.hide().unwrap();
    }
}
