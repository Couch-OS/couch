//! Rendering coverage for the screens that had no test at all: the home hub
//! (the actual room list), the settings menu's root page, the camera, first-run
//! setup, "nothing configured yet", and the microphone, pairing, Bluetooth-pairing,
//! keyboard and Wi-Fi-setup overlays.
//!
//! `CouchPlatform::install` (`panel.rs`) makes every screen a child of one
//! `App` root, gated by an `*-shown` property, so rendering one is
//! `app.set_<x>_shown(true)` and a draw - see `draw_room` in `lights.rs` and
//! the player test in `media_player.rs`, which this file follows as closely
//! as it can: one process (Slint's platform installs once, hence the
//! re-exec-into-self trick below), one `App`, one reused pixel buffer, ten
//! pictures in a row.
//!
//! `COUCH_SCREEN_PICTURES=<dir>` writes each screen as `<dir>/<screen>.png`.
//! `COUCH_SCREEN_GOLDENS=<dir>` compares each picture pixel-for-pixel against
//! a PNG of the same name already in that directory (the compare arm is
//! copied from `media_player.rs`'s `COUCH_PLAYER_GOLDENS`). Neither is set in
//! CI - there are no committed goldens - so the assertions below (not blank,
//! and the words a person would read) are what actually runs there.
use crate::{App, ChoiceItem, LiveActivity, RoomRow, SceneCell};
use slint::{platform::WindowEvent, ComponentHandle, Model, ModelRc, VecModel};
use std::{rc::Rc, time::Duration};

const W: usize = 480;
const H: usize = 800;

// `Theme.bg` and `Theme.surface` (`ui/theme.slint`), pinned here as plain sRGB
// bytes rather than imported: this file stays Rust-only, no `.slint` changes.
const THEME_BG: (u8, u8, u8) = (0x15, 0x13, 0x0F);
const THEME_SURFACE: (u8, u8, u8) = (0x1F, 0x1C, 0x17);

fn pixel_count(pixels: &[slint::Rgb8Pixel], rgb: (u8, u8, u8)) -> usize {
    pixels.iter().filter(|p| (p.r, p.g, p.b) == rgb).count()
}

/// Not a flat fill (the light screen's own check, see `lights.rs`), and more
/// than a sliver of the picture is something other than the page background -
/// the "sensible fraction of non-background pixels" the audit asked for.
/// `min_fraction` is lower for the one screen that is legitimately mostly
/// empty (the camera, waiting for a frame that never arrives in a test).
fn assert_not_blank(name: &str, pixels: &[slint::Rgb8Pixel], min_fraction: f32) {
    assert!(
        pixels.iter().any(|p| *p != pixels[0]),
        "{name}: the screen drew nothing"
    );
    let bg = pixel_count(pixels, THEME_BG);
    let fraction = (pixels.len() - bg) as f32 / pixels.len() as f32;
    assert!(
        fraction > min_fraction,
        "{name}: only {:.1}% of the picture is not the page background",
        fraction * 100.0
    );
}

/// Turn off every overlay and screen the shell can show, so the next fixture
/// draws alone. The real navigation flips one of these at a time; here it
/// happens between fixtures instead of between key presses.
fn hide_everything(app: &App) {
    app.set_setup_mode(false);
    app.set_no_config(false);
    app.set_chooser_shown(false);
    app.set_custom_activity_shown(false);
    app.set_camera_shown(false);
    app.set_thermostat_shown(false);
    app.set_thermostat_modes_shown(false);
    app.set_thermostat_feedback_shown(false);
    app.set_tv_shown(false);
    app.set_player_shown(false);
    app.set_light_shown(false);
    app.set_light_screen_shown(false);
    app.set_brightness_shown(false);
    app.set_sonos_feedback_shown(false);
    app.set_scene_feedback_shown(false);
    app.set_keyboard_shown(false);
    app.set_settings_shown(false);
    app.set_bt_pair_shown(false);
    app.set_wifi_setup_shown(false);
    app.set_recording(false);
    app.set_mic_result_shown(false);
    app.set_pair_shown(false);
    app.set_volume_shown(false);
    app.set_activity_busy(false);
    app.set_activity_running(false);
}

/// One picture: settle whatever just changed, draw into the buffer the whole
/// run shares (`CouchPlatform::install` uses `RepaintBufferType::ReusedBuffer`,
/// so a fresh buffer per screen would leave unchanged regions - the status
/// bar's clock, say - however they happened to start: reusing one buffer for
/// every screen is what `media_player.rs`'s 29-picture test does, and why).
/// Writes the PNG and/or compares it with a golden, then asserts it is not
/// blank.
fn shoot(
    window: &Rc<slint::platform::software_renderer::MinimalSoftwareWindow>,
    pixels: &mut [slint::Rgb8Pixel],
    name: &str,
    min_fraction: f32,
    differing: &mut Vec<String>,
) {
    for _ in 0..20 {
        slint::platform::update_timers_and_animations();
        std::thread::sleep(Duration::from_millis(16));
    }
    window.request_redraw();
    window.draw_if_needed(|r| {
        r.render(pixels, W);
    });
    let bytes: Vec<u8> = pixels.iter().flat_map(|p| [p.r, p.g, p.b]).collect();
    let file = format!("{name}.png");
    if let Some(dir) = std::env::var_os("COUCH_SCREEN_PICTURES") {
        std::fs::create_dir_all(&dir).unwrap();
        image::save_buffer(
            std::path::Path::new(&dir).join(&file),
            &bytes,
            W as u32,
            H as u32,
            image::ColorType::Rgb8,
        )
        .unwrap();
    }
    if let Some(dir) = std::env::var_os("COUCH_SCREEN_GOLDENS") {
        let golden = image::open(std::path::Path::new(&dir).join(&file))
            .unwrap()
            .to_rgb8();
        let wrong = golden
            .as_raw()
            .chunks(3)
            .zip(bytes.chunks(3))
            .filter(|(a, b)| a != b)
            .count();
        if golden.dimensions() != (W as u32, H as u32) || wrong != 0 {
            differing.push(format!("{file}: {wrong} pixels"));
        }
    }
    assert_not_blank(name, pixels, min_fraction);
}

/// The home hub's status line, mirroring `home_hub.slint`'s own ternary
/// (`area.offline ? "OFFLINE" : area.idle ? "IDLE" : (active-count + " ON")`)
/// so the assertion below is in the same words the row binds to, the way
/// `draw_room` in `lights.rs` mirrors its chevron.
fn hub_status(row: &RoomRow) -> String {
    if row.offline {
        "OFFLINE".to_string()
    } else if row.idle {
        "IDLE".to_string()
    } else {
        format!("{} ON", row.active_count)
    }
}

/// The room row's second line (`area.detail == "" ? area.devices : area.devices
/// + " · " + area.detail`).
fn hub_line(row: &RoomRow) -> String {
    if row.detail.is_empty() {
        row.devices.to_string()
    } else {
        format!("{} · {}", row.devices, row.detail)
    }
}

/// Ten screens with no rendering test before this file: the home hub, the
/// settings menu's root page, the camera, first-run setup, "nothing
/// configured yet", and the mic/pair/bt-pair overlays, the keyboard and the
/// Wi-Fi setup list. One process, one `App`, one picture each.
#[test]
fn every_screen_the_audit_found_untested_draws_something_real() {
    const NAME: &str =
        "screen_pictures::every_screen_the_audit_found_untested_draws_something_real";
    if std::env::var_os("COUCH_TEST_SCREEN_PICTURES").is_none() {
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", NAME])
            .env("COUCH_TEST_SCREEN_PICTURES", "1")
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

    let window =
        crate::panel::CouchPlatform::install(slint::PhysicalSize::new(W as u32, H as u32)).unwrap();
    let app = App::new().unwrap();
    app.show().unwrap();
    window.dispatch_event(WindowEvent::WindowActiveChanged(true));
    let mut pixels = vec![slint::Rgb8Pixel::default(); W * H];
    let mut differing = Vec::new();

    // 1. Home hub: three rooms (idle, offline, and one with something on),
    // an activity strip, and a scenes card.
    hide_everything(&app);
    app.set_area_name("HOME".into());
    app.set_area_index(0);
    app.set_area_dots(ModelRc::new(VecModel::from(vec![true])));
    app.set_activities(ModelRc::new(VecModel::from(vec![LiveActivity {
        kind: 0,
        title: "Midnight Ferry".into(),
        source: "SONOS".into(),
        place: "KITCHEN".into(),
    }])));
    let rooms = vec![
        RoomRow {
            name: "Living room".into(),
            devices: "5 devices".into(),
            detail: "Kodi, Hue, LG C3".into(),
            active_count: 2,
            power_state: 1,
            icon: crate::icons::image(couch_model::Icon::Sofa),
            status_known: true,
            idle: false,
            offline: false,
            dimmed: false,
            glyph: couch_model::Icon::Sofa.glyph_index(),
        },
        RoomRow {
            name: "Garage".into(),
            devices: "2 devices".into(),
            detail: "".into(),
            active_count: 0,
            power_state: 0,
            icon: crate::icons::image(couch_model::Icon::Car),
            status_known: true,
            idle: true,
            offline: false,
            dimmed: false,
            glyph: couch_model::Icon::Car.glyph_index(),
        },
        RoomRow {
            name: "Garden".into(),
            devices: "3 devices".into(),
            detail: "Cameras".into(),
            active_count: 0,
            power_state: -1,
            icon: crate::icons::image(couch_model::Icon::Trees),
            status_known: true,
            idle: false,
            offline: true,
            dimmed: false,
            glyph: couch_model::Icon::Trees.glyph_index(),
        },
    ];
    app.set_rooms(ModelRc::new(VecModel::from(rooms)));
    app.set_scenes(ModelRc::new(VecModel::from(vec![
        SceneCell {
            name: "Movie night".into(),
            active: false,
        },
        SceneCell {
            name: "Good morning".into(),
            active: false,
        },
        SceneCell {
            name: "Away".into(),
            active: true,
        },
    ])));
    app.invoke_page_swapped();
    shoot(&window, &mut pixels, "home-hub", 0.03, &mut differing);
    // The words a person reads, read back off the same model the rows bind
    // to (see `draw_room` in `lights.rs` for the same trick), formatted the
    // way `home_hub.slint` formats them (see `hub_line`/`hub_status` above).
    let rooms_back: Vec<RoomRow> = app.get_rooms().iter().collect();
    assert_eq!(rooms_back.len(), 3);
    assert_eq!(rooms_back[0].name, "Living room");
    assert_eq!(hub_line(&rooms_back[0]), "5 devices · Kodi, Hue, LG C3");
    assert_eq!(hub_status(&rooms_back[0]), "2 ON");
    assert_eq!(rooms_back[1].name, "Garage");
    assert_eq!(hub_line(&rooms_back[1]), "2 devices");
    assert_eq!(hub_status(&rooms_back[1]), "IDLE");
    assert_eq!(rooms_back[2].name, "Garden");
    assert_eq!(hub_line(&rooms_back[2]), "3 devices · Cameras");
    assert_eq!(hub_status(&rooms_back[2]), "OFFLINE");
    let activities_back: Vec<LiveActivity> = app.get_activities().iter().collect();
    assert_eq!(activities_back.len(), 1);
    assert_eq!(activities_back[0].title, "Midnight Ferry");
    assert_eq!(
        format!(
            "{} · {}",
            activities_back[0].source, activities_back[0].place
        ),
        "SONOS · KITCHEN"
    );
    let scenes_back: Vec<SceneCell> = app.get_scenes().iter().collect();
    assert_eq!(scenes_back.len(), 3);
    assert_eq!(scenes_back[0].name, "Movie night");

    // 2. Settings, the root page: seven sections (Display, Wi-Fi, Bluetooth,
    // Network, SSH, Updates, Power). Their names are Slint literals with no
    // output property carrying them back to Rust, so unlike every other
    // screen here this is a structural check rather than a text one: seven
    // card-coloured rows over the page background, not just "something
    // drew" - the cheapest thing that would catch a row going missing.
    hide_everything(&app);
    app.set_settings_shown(true);
    app.set_settings_panel(0);
    shoot(&window, &mut pixels, "settings", 0.03, &mut differing);
    let card_pixels = pixel_count(&pixels, THEME_SURFACE);
    assert!(
        card_pixels > 100_000,
        "settings: only {card_pixels} card-coloured pixels; expected the seven section rows"
    );

    // 3. Camera: there is no live UniFi Protect stream to connect to in a
    // test, so this renders the state the screen shows while dialling in -
    // see `camera.rs`'s "Connecting securely…" - rather than a frame.
    hide_everything(&app);
    app.set_camera_shown(true);
    app.set_camera_title("Front door".into());
    app.set_camera_message("Connecting securely…".into());
    app.set_camera_image(slint::Image::default());
    shoot(&window, &mut pixels, "camera", 0.01, &mut differing);
    assert_eq!(app.get_camera_title(), "Front door");
    assert_eq!(app.get_camera_message(), "Connecting securely…");

    // 4. Setup: first-run Wi-Fi provisioning, waiting for a phone to scan
    // the QR and join the open network it advertises.
    hide_everything(&app);
    app.set_setup_mode(true);
    app.set_ssid("Couch-Setup".into());
    app.set_portal_url("http://192.168.4.1".into());
    app.set_qr(crate::qr::render(&crate::qr::wifi_join_record("Couch-Setup"), 296).unwrap());
    app.set_approval(0);
    shoot(&window, &mut pixels, "setup", 0.03, &mut differing);
    assert_eq!(app.get_ssid(), "Couch-Setup");
    assert_eq!(app.get_portal_url(), "http://192.168.4.1");

    // 5. NoConfig: nothing saved yet, or nothing readable - over the empty
    // hub, so the address and QR to reach the web UI are all there is.
    hide_everything(&app);
    app.set_no_config(true);
    app.set_no_config_web("http://192.168.1.127:8090".into());
    app.set_no_config_qr(crate::qr::render("http://192.168.1.127:8090", 200).unwrap());
    app.set_no_config_has_qr(true);
    shoot(&window, &mut pixels, "no-config", 0.03, &mut differing);
    assert_eq!(app.get_no_config_web(), "http://192.168.1.127:8090");

    // 6. MicOverlay: the moment after the microphone closes, with what Home
    // Assistant heard and said still on screen.
    hide_everything(&app);
    app.set_mic_result_shown(true);
    app.set_recording(false);
    app.set_mic_latched(false);
    app.set_mic_phase("done".into());
    app.set_mic_phase_label("DONE".into());
    app.set_mic_target("assistant".into());
    app.set_mic_heard("Turn on the kitchen lights".into());
    app.set_mic_said("Turning on the kitchen lights.".into());
    app.set_mic_detail("".into());
    shoot(&window, &mut pixels, "mic-overlay", 0.03, &mut differing);
    assert_eq!(app.get_mic_heard(), "Turn on the kitchen lights");
    assert_eq!(app.get_mic_said(), "Turning on the kitchen lights.");

    // 7. PairOverlay: a browser asking to pair with this remote.
    hide_everything(&app);
    app.set_pair_shown(true);
    app.set_pair_pin("284916".into());
    app.set_pair_seconds(87);
    shoot(&window, &mut pixels, "pair-overlay", 0.03, &mut differing);
    assert_eq!(app.get_pair_pin(), "284916");
    assert_eq!(app.get_pair_seconds(), 87);

    // 8. BtPairOverlay: pairing with a Bluetooth TV, finished.
    hide_everything(&app);
    app.set_bt_pair_shown(true);
    app.set_bt_pair_phase("done".into());
    app.set_bt_pair_detail("Living Room TV".into());
    shoot(
        &window,
        &mut pixels,
        "bt-pair-overlay",
        0.03,
        &mut differing,
    );
    assert_eq!(app.get_bt_pair_phase(), "done");
    assert_eq!(app.get_bt_pair_detail(), "Living Room TV");

    // 9. Keyboard: mid-entry, the hidden-network-name field (see
    // `network_ui.rs`'s `keyboard()` for the same title/placeholder pair).
    hide_everything(&app);
    app.set_keyboard_shown(true);
    app.set_keyboard_title("NETWORK NAME".into());
    app.set_keyboard_placeholder("Enter the hidden network name".into());
    app.set_keyboard_password(false);
    app.set_keyboard_text("Garden Studio".into());
    shoot(&window, &mut pixels, "keyboard", 0.03, &mut differing);
    assert_eq!(app.get_keyboard_title(), "NETWORK NAME");
    assert_eq!(app.get_keyboard_text(), "Garden Studio");

    // 10. WifiSetup: the network list a scan found (see `network_ui.rs`'s
    // `list()` for the same title and row shape).
    hide_everything(&app);
    app.set_wifi_setup_shown(true);
    app.set_wifi_setup_title("Choose a network".into());
    app.set_wifi_setup_detail("3 networks found".into());
    let choice = |title: &str, detail: &str| ChoiceItem {
        title: title.into(),
        detail: detail.into(),
        active: false,
        light: false,
        media: false,
        activity: false,
        kind: 0,
        power_known: false,
        controls: false,
        icon: slint::Image::default(),
    };
    app.set_wifi_setup_items(ModelRc::new(VecModel::from(vec![
        choice("Home Wi-Fi", "-45 dBm · Password required"),
        choice("Neighbour 5G", "-67 dBm · Open network"),
        choice("Enter a hidden network", "Type its network name"),
    ])));
    shoot(&window, &mut pixels, "wifi-setup", 0.03, &mut differing);
    assert_eq!(app.get_wifi_setup_title(), "Choose a network");
    let items_back: Vec<ChoiceItem> = app.get_wifi_setup_items().iter().collect();
    assert_eq!(items_back.len(), 3);
    assert_eq!(items_back[0].title, "Home Wi-Fi");
    assert_eq!(items_back[0].detail, "-45 dBm · Password required");

    app.hide().unwrap();
    assert!(
        differing.is_empty(),
        "pictures differ from the goldens: {differing:?}"
    );
}
