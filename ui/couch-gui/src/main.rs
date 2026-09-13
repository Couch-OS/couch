//! The Couch remote's UI.
//!
//! The screens themselves are still being designed; everything here is the
//! platform underneath them - panel, input, and the device state the UI shows.

// Opt-in comparison against musl; never assume allocator throughput implies
// bounded application latency (page faults and OS calls remain possible).
#[cfg(feature = "mimalloc")]
#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

mod evdev;
mod keypad;
mod mic;
mod motion;
mod panel;
mod qr;
mod system;
mod touch;
mod wifi;
mod network;
mod network_ui;
mod home;
mod lights;
mod scenes;
mod activity;
mod activity_runtime;
mod activity_buttons;
mod tv;
mod room_sonos;
mod sonos_player;
mod thermostat;
mod camera;
mod connections;
mod config_snapshot;
mod input;
mod navigation;
use input::{Standby,TouchDisposition,touch_disposition,wake};
use navigation::Intent;
mod remote_clock;
mod activity_art;
mod icons;
use home::Area;

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use slint::platform::{PointerEventButton, WindowEvent};
use slint::LogicalPosition;
use slint::{Model, ModelRc, PhysicalSize, SharedString, VecModel};

use keypad::{now_monotonic_us, Keypad};
use panel::{Arrive, CouchPlatform, Panel, SLIDE};
use system::Approval;

slint::include_modules!();

const BACKGROUND: u32 = 0x09090b;

fn env_secs(name: &str, default: u64) -> u64 {
    std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // The UI is left on the default affinity deliberately, not pinned off
    // CPU 0. The input EINT interrupts fire only on CPU 0 and freeze it for
    // 46-62ms per press (see README), so it is tempting to pin the UI to the
    // other cores - but the vendor hotplug daemon can take a core offline, and
    // a UI pinned to a core that vanishes freezes for as long as it stays gone
    // (measured: a 4.8s frame). The scheduler already migrates the runnable UI
    // off a core saturated by IRQ time when another is online, which is why the
    // fix is simply to keep a core available (stage2's hotplug floor), never to
    // force the UI onto one.
    let mut screen = Panel::open()?;
    println!("couch-gui: panel {}x{}", screen.width, screen.height);

    let window = CouchPlatform::install(PhysicalSize::new(screen.width, screen.height))
        .map_err(|e| format!("platform: {e:?}"))?;

    Panel::backlight_on();
    screen.claim(BACKGROUND);
    // Whoever resumed the panel before this process started did not
    // re-present it; do it once, so the first frame is seen.
    screen.present();

    let app = App::new().map_err(|e| format!("App::new: {e:?}"))?;

    // Areas are a level above rooms: left and right move between them, up and
    // down between the rooms inside one. The demo remains a fallback when no
    // saved home exists; normal operation reloads the daemon's configuration.
    fn room(name: &str, devices: &str, detail: &str, on: i32, glyph: i32) -> RoomRow {
        RoomRow {
            name: name.into(),
            devices: devices.into(),
            detail: detail.into(),
            active_count: on,
            power_state: if on > 0 { 1 } else { 0 },
            icon: icons::image(couch_model::Icon::House),
            status_known: true,
            idle: on == 0,
            offline: false,
            dimmed: false,
            glyph,
        }
    }
    fn scene(name: &str) -> SceneCell {
        SceneCell { name: name.into(), active: false }
    }
    fn scene_on(name: &str) -> SceneCell {
        SceneCell { name: name.into(), active: true }
    }
    // kind 0 is audio (bars), 1 is video (play triangle).
    fn act(kind: i32, title: &str, source: &str, place: &str) -> LiveActivity {
        LiveActivity {
            kind,
            title: title.into(),
            source: source.into(),
            place: place.into(),
        }
    }

    let mut areas = vec![
        Area {
            activity_ids: Vec::new(),
            name: "WHOLE HOME".into(),
            room_ids: Vec::new(),
            scene_ids: Vec::new(),
            activities: vec![
                act(0, "Midnight Ferry", "SONOS", "KITCHEN"),
                act(1, "Paused - Andrei Rublev", "KODI", "LIVING"),
                act(0, "Radio Paradise", "SONOS", "STUDY"),
            ],
            rooms: vec![
                room("Living room", "5 devices", "Kodi, Hue, LG C3", 2, 0),
                room("Kitchen", "2 devices", "Sonos Move", 1, 2),
                room("Bedroom", "3 devices", "Hue, Sonos One", 0, 1),
                room("Study", "2 devices", "", 0, 3),
            ],
            scenes: vec![
                scene("Movie night"),
                scene("Good morning"),
                scene_on("Away"),
                scene("Dinner"),
                scene("All off"),
            ],
        },
        Area {
            activity_ids: Vec::new(),
            name: "UPSTAIRS".into(),
            room_ids: Vec::new(),
            scene_ids: Vec::new(),
            activities: vec![act(0, "White noise", "SONOS", "BEDROOM")],
            rooms: vec![
                room("Bedroom", "3 devices", "Hue, Sonos One", 0, 1),
                room("Study", "2 devices", "", 0, 3),
                room("Loft", "1 device", "Hue", 0, 7),
            ],
            scenes: vec![scene("Bedtime"), scene("Wake up"), scene("Upstairs off")],
        },
        Area {
            activity_ids: Vec::new(),
            name: "DOWNSTAIRS".into(),
            room_ids: Vec::new(),
            scene_ids: Vec::new(),
            activities: vec![
                act(1, "Paused - Andrei Rublev", "KODI", "LIVING"),
                act(0, "Midnight Ferry", "SONOS", "KITCHEN"),
                act(1, "Front door", "CAMERA", "HALLWAY"),
                act(0, "The Rest Is History", "SONOS", "LIVING"),
                act(1, "Formula 1 - Practice 2", "PLEX", "LIVING"),
            ],
            rooms: vec![
                room("Living room", "5 devices", "Kodi, Hue, LG C3", 2, 0),
                room("Kitchen", "2 devices", "Sonos Move", 1, 2),
                room("Hallway", "2 devices", "Hue", 0, 4),
            ],
            scenes: vec![
                scene_on("Movie night"),
                scene("Cooking"),
                scene("Downstairs off"),
            ],
        },
        Area {
            activity_ids: Vec::new(),
            name: "OUTSIDE".into(),
            room_ids: Vec::new(),
            scene_ids: Vec::new(),
            activities: vec![],
            rooms: vec![
                room("Garden", "3 devices", "Hue, Cameras", 1, 6),
                room("Garage", "2 devices", "Hue", 0, 5),
                room("Porch", "1 device", "Hue", 0, 4),
            ],
            scenes: vec![
                scene("Evening"),
                scene("Security on"),
                scene("Watering"),
                scene("Outside off"),
            ],
        },
    ];

    // COUCH_ROOMS pads the first area, so the scrolling behaviour is testable
    // without waiting for a house with a dozen rooms in one area.
    if let Ok(n) = std::env::var("COUCH_ROOMS").unwrap_or_default().parse::<usize>() {
        let extra = ["Hallway", "Garage", "Garden", "Loft", "Utility", "Porch", "Cellar"];
        let mut i = 0;
        while areas[0].rooms.len() < n {
            areas[0].rooms.push(room(extra[i % extra.len()], "2 devices", "Hue",
                                     (i % 2) as i32, (i % 4) as i32));
            i += 1;
        }
        areas[0].rooms.truncate(n);
    }

    couch_control::use_socket(home::path("control.sock"));
    config_snapshot::start(home::path("config.json"));
    let mut loaded_home = String::new();
    if let Some((raw, saved, accent)) = home::read(&loaded_home) { home::apply_accent(&app,accent); loaded_home = raw; areas = saved; }
    let mut light_controls = lights::Controller::install(&app);
    let mut room_monitor = home::RoomMonitor::new(light_controls.hue_live());
    let mut scene_controls = scenes::Controller::new(&app);
    let mut activity_controls = activity::Controller::new(&app);
    let mut tv_controls = tv::Controller::new(&app);
    let mut thermostat_controls = thermostat::Controller::new(&app);
    let mut sonos_room = room_sonos::Controller::install(&app);
    let mut activity_runtime = activity_runtime::Controller::new(&app);
    let scene_choices = Rc::new(RefCell::new(Vec::<couch_model::Id>::new()));
    app.set_area_dots(ModelRc::new(VecModel::from(vec![true; areas.len()])));

    let areas = Rc::new(std::cell::RefCell::new(areas));
    // The area on screen. One index: a page change is applied in one go and
    // the slide is composed from the frame before it and the frame after, so
    // there is no 180ms during which the pager and the page disagree.
    let current = Rc::new(Cell::new(0usize));
    // What the last dispatched event asked for, performed by the loop. One
    // slot: two asks in one iteration - a key and COUCH_SLIDE's timer, say -
    // keep the first, as a second press mid-slide used to be ignored.
    let intent = Rc::new(Cell::new(None::<Intent>));
    let ask = {
        let intent = intent.clone();
        move |what: Intent| {
            if intent.get().is_none() {
                intent.set(Some(what));
            }
        }
    };

    let put_front = {
        let areas = areas.clone();
        move |app: &App, index: usize| {
            let area_data = areas.borrow();
            let a = &area_data[index];
            app.set_area_name(a.name.as_str().into());
            app.set_area_index(index as i32);
            app.set_activities(ModelRc::new(VecModel::from(a.activities.clone())));
            app.set_rooms(ModelRc::new(VecModel::from(a.rooms.clone())));
            app.set_scenes(ModelRc::new(VecModel::from(a.scenes.clone())));
            // With the models in place the hub knows where its first room is;
            // it parks focus there and lands the ring, nothing animating.
            app.invoke_page_swapped();
        }
    };

    // COUCH_AREA opens on a given page, so each one can be checked without
    // driving the keypad.
    let first = std::env::var("COUCH_AREA")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|i| *i < areas.borrow().len())
        .unwrap_or(0);
    put_front(&app, first);
    current.set(first);

    // COUCH_FOCUS parks focus on a row - 0 is the activity strip, then one per
    // room, then the scenes row - so any focus position can be photographed
    // without driving the keypad. The one place the host names a row, and it
    // is a test hook: the hub interprets it.
    if let Ok(n) = std::env::var("COUCH_FOCUS").unwrap_or_default().parse::<i32>() {
        app.set_focus_row(n);
        app.invoke_settle_focus();
    }

    {
        let ask = ask.clone();
        app.on_area_step(move |delta| ask(Intent::Area(delta)));
    }

    // OK on the strip or the scenes row opens what is there: straight through
    // when there is one of it, as a list when there are several. Room controls
    // use the saved room ID. The hub says which of the three it was and, for
    // a room, which one; it never hands over a row index. The chooser's
    // content is set here, where the list is known; showing it is the
    // transition, which the loop performs.
    {
        let weak = app.as_weak();
        let areas = areas.clone();
        let current = current.clone();
        let ask = ask.clone();
        app.on_open_strip(move || {
            let Some(app) = weak.upgrade() else { return };
            let area_data = areas.borrow();
            let a = &area_data[current.get()];
            match a.activities.len() {
                0 => return,
                1 => {
                    if let Some(id)=a.activity_ids.first(){app.invoke_open_activity(id.as_str().into());}
                    return;
                }
                _ => {}
            }
            let items: Vec<ChoiceItem> = a
                .activities
                .iter()
                .map(|x| ChoiceItem {
                    title: x.title.clone(),
                    detail: SharedString::from(format!("{} · {}", x.source, x.place)),
                    // Everything in this list is playing, so a dot marking
                    // that would be on every row and mean nothing.
                    active: false,
                    light: false,
                    media: false,
                    power_known: false,
                    icon: slint::Image::default(),
                })
                .collect();
            app.set_chooser_title("ACTIVITIES".into());
            app.set_chooser_items(ModelRc::new(VecModel::from(items)));
            app.set_chooser_index(0);
            ask(Intent::OpenChooser);
        });
    }
    {
        let weak = app.as_weak();
        let areas = areas.clone();
        let current = current.clone();
        let ask = ask.clone();
        let choices = scene_choices.clone();
        app.on_open_scenes(move || {
            let Some(app) = weak.upgrade() else { return };
            let data = areas.borrow();
            let area = &data[current.get()];
            if area.scene_ids.is_empty() {
                return;
            }
            *choices.borrow_mut() = area.scene_ids.clone();
            app.set_chooser_items(ModelRc::new(VecModel::from(
                area.scenes
                    .iter()
                    .map(|s| ChoiceItem {
                        title: s.name.clone(),
                        detail: "Press OK to activate".into(),
                        active: false,
                        light: false,
                    media: false,
                        power_known: false,
                        icon: slint::Image::default(),
                    })
                    .collect::<Vec<_>>(),
            )));
            app.set_chooser_title("SCENES".into());
            app.set_chooser_index(0);
            ask(Intent::OpenChooser);
        });
    }
    {
        let weak = app.as_weak();
        let choices = scene_choices.clone();
        let ask = ask.clone();
        app.on_room_scenes(move || {
            let Some(app) = weak.upgrade() else { return };
            let Some(cfg) = connections::config() else {
                return;
            };
            let room = couch_model::Id::new(app.get_light_room_id().as_str());
            let list: Vec<_> = cfg
                .scenes
                .iter()
                .filter(|s| s.rooms.contains(&room))
                .collect();
            if list.is_empty() {
                return;
            }
            *choices.borrow_mut() = list.iter().map(|s| s.id.clone()).collect();
            app.set_chooser_items(ModelRc::new(VecModel::from(
                list.iter()
                    .map(|s| ChoiceItem {
                        title: s.name.as_str().into(),
                        detail: "Press OK to activate".into(),
                        active: false,
                        light: false,
                    media: false,
                        power_known: false,
                        icon: slint::Image::default(),
                    })
                    .collect::<Vec<_>>(),
            )));
            app.set_chooser_title("SCENES".into());
            app.set_chooser_index(0);
            ask(Intent::OpenChooser);
        });
    }
    {
        let areas = areas.clone();
        let current = current.clone();
        let open = light_controls.opener();
        app.on_open_room(move |index| {
            if let Some(id) = areas.borrow()[current.get()].room_ids.get(index as usize) { open(id.clone()); }
        });
    }

    {
        let weak = app.as_weak();
        let ask = ask.clone();
        let choices=scene_choices.clone(); let recall=scene_controls.opener();
        let areas=areas.clone();let current=current.clone();
        let pick_source=sonos_room.chooser();
        app.on_chosen(move |index| {
            let Some(app) = weak.upgrade() else { return };
            let title = app
                .get_chooser_items()
                .row_data(index as usize)
                .map(|i| i.title.to_string())
                .unwrap_or_default();
            if app.get_chooser_title()=="SCENES" {
                if let Some(id)=choices.borrow().get(index as usize) { recall(id.clone()); }
            }
            if app.get_chooser_title()=="ACTIVITIES" {
                if let Some(id)=areas.borrow()[current.get()].activity_ids.get(index as usize){app.invoke_open_activity(id.as_str().into());}
            }
            if app.get_chooser_title()==room_sonos::CHOOSER_TITLE && index>=0 { pick_source(index as usize); }
            println!("couch-gui: chose '{title}'");
            ask(Intent::CloseChooser);
        });
    }
    {
        let ask = ask.clone();
        app.on_close_chooser(move || ask(Intent::CloseChooser));
    }
    app.on_back(|| println!("couch-gui: back"));
    app.on_home(|| println!("couch-gui: home"));

    // A page change, performed. The slide is a copy, not a render: the frame
    // on the panel is kept (page A), the change is applied to the UI in one
    // go with the ring hidden and rendered once into RAM (page B), and the
    // panel then shows A pushed out by B a little further each refresh, by
    // memcpy, while the rows that must not move - the status bar, and the
    // pager on an area change - are B throughout. The ring is hidden across
    // the change so B has none; letting it go again afterwards is what starts
    // its 200ms fade in on the next normal frame.
    //
    // Order matters twice over. The snapshot comes before anything changes,
    // because RAM equals the panel only until something is drawn. And the
    // chooser's `changed shown` handler, which puts its list back to the top,
    // runs from update_timers_and_animations - so that runs before B is
    // rendered, or B would show the list where it was last left.
    //
    // Reports what it cost as frames: B's rasterisation and then one per
    // transition frame, so the five-second line counts them with the rest.
    let navigator = navigation::Navigator::new(areas.clone(),current.clone(),put_front.clone());

    app.show().map_err(|e| format!("show: {e:?}"))?;

    let mut cameras = camera::Cameras::new(&app);
    let mut pad = Keypad::open();
    println!("couch-gui: {} keypad device(s)", pad.device_count());

    // The panel is a touchscreen too. Slint does the hit testing once the
    // pointer events are fed in, so this is only a translation layer.
    let mut mic = mic::Mic::new();
    // Push to talk, with a latch. Hold the key and it records while held;
    // tap it and it stays on until the next tap. The button is small and the
    // thing being dictated is a sentence, so insisting on a hold would be a
    // worse remote - but a hold is what a hand does without being told, and
    // both have to mean the obvious thing.
    let mut physical_input = input::Physical::default();
    let mut pointer = touch::Touch::open();
    println!(
        "couch-gui: touchscreen {}",
        if pointer.present() { "present" } else { "absent" }
    );

    let remote_clock = remote_clock::Clock::new();
    let mut dock_clock_enabled = true;
    let mut wake_on_lift = true;
    let mut motion = motion::Controller::new();
    let mut swallow_dock_repeats = false;
    // COUCH_OPEN presses OK on whatever COUCH_FOCUS selected, so the chooser
    // can be photographed without driving the keypad. Fired from the loop a
    // moment in rather than here: a property set before the first frame has
    // nothing to animate from, so opening it at startup would never show the
    // transition it exists to demonstrate.
    let open_at = std::env::var("COUCH_OPEN")
        .is_ok()
        .then(|| now_monotonic_us() + 2_000_000);
    let mut opened = false;
    // COUCH_KEYBOARD opens the on-screen keyboard a moment in, so it can be
    // driven with the real D-pad and touch on the real renderer.
    let keyboard_at = std::env::var("COUCH_KEYBOARD")
        .is_ok()
        .then(|| now_monotonic_us() + 1_500_000);
    let mut keyboard_opened = false;
    // COUCH_SETTINGS opens the settings menu a moment in, so it can be driven
    // with the mapped D-pad keys (the menu key that normally opens it cannot be
    // injected - the input core drops codes the device does not declare).
    let settings_at = std::env::var("COUCH_SETTINGS")
        .is_ok()
        .then(|| now_monotonic_us() + 1_500_000);
    let mut settings_opened = false;
    let mut network_setup = network_ui::Controller::install(&app);
    if std::env::var_os("COUCH_WIFI_SETUP").is_some() { app.invoke_setting_change_wifi(); }
    let toast_until: Rc<Cell<Option<u64>>> = Rc::new(Cell::new(None));
    let toast = {
        let (weak, until) = (app.as_weak(), toast_until.clone());
        move |msg: String, secs: u64| {
            if let Some(app) = weak.upgrade() {
                app.set_feedback_enabled(true);
                app.set_toast(msg.into());
                until.set(Some(now_monotonic_us() + secs * 1_000_000));
            }
        }
    };
    let mut last_tick = 0u64;
    let mut last_setup: Option<bool> = None;
    // COUCH_NAV walks the focus ring on a timer, so its repaint behaviour is
    // observable without someone pressing buttons.
    let nav = std::env::var("COUCH_NAV").is_ok();
    // COUCH_SLIDE steps the area every 1.5s, so the transition's real cost can
    // be measured rather than extrapolated from simpler content.
    let auto_slide = std::env::var("COUCH_SLIDE").is_ok();
    let mut slide_at = now_monotonic_us() + 1_500_000;
    let mut nav_at = now_monotonic_us() + 700_000;

    // Frame cost is the draw and the copy; the wait for the panel after them
    // is counted apart, because it is slack rather than work.
    let (mut frames, mut render_us, mut frame_max, mut wait_us) = (0u64, 0u64, 0u64, 0u64);

    // Standby timings and brightness come from the saved settings, adjustable
    // live from the menu, so they are shared cells the menu callbacks write and
    // the loop reads. COUCH_DIM_S/COUCH_OFF_S still override for testing: a
    // value of 0 for off means never.
    let cfg = system::load_settings();
    let active_level = Rc::new(Cell::new(system::brightness_level(cfg.brightness)));
    // Dim to a sixth of the set brightness, never below a faint floor and never
    // above the level itself: dim is "less than now", whatever now is.
    let dim_level = Rc::new(Cell::new((active_level.get() / 6).clamp(8, active_level.get())));
    let dim_after_us = Rc::new(Cell::new(
        env_secs("COUCH_DIM_S", system::DIM_SECS[cfg.dim_index as usize]) * 1_000_000,
    ));
    let off_after_us = Rc::new(Cell::new(
        env_secs("COUCH_OFF_S", system::OFF_SECS[cfg.off_index as usize]) * 1_000_000,
    ));
    let settings = Rc::new(std::cell::RefCell::new(cfg));

    // Seed the menu with the choices and the saved values.
    app.set_dim_choices(ModelRc::new(VecModel::from(
        system::DIM_LABELS.iter().map(|s| SharedString::from(*s)).collect::<Vec<_>>(),
    )));
    app.set_off_choices(ModelRc::new(VecModel::from(
        system::OFF_LABELS.iter().map(|s| SharedString::from(*s)).collect::<Vec<_>>(),
    )));
    {
        let s = settings.borrow();
        app.set_setting_brightness(s.brightness);
        app.set_dim_index(s.dim_index);
        app.set_off_index(s.off_index);
    }
    // Apply the saved brightness now: claim() and backlight_on() above lit the
    // panel at full to show the splash, but the level a person chose is what
    // they should see from the first frame, not until the first dim.
    Panel::set_backlight(active_level.get());

    // Brightness applies to the panel at once (the menu is up, so the screen is
    // active); the timeouts take effect on the next idle. All three persist.
    {
        let (al, dl, sett) = (active_level.clone(), dim_level.clone(), settings.clone());
        app.on_setting_brightness_changed(move |pct| {
            let level = system::brightness_level(pct);
            al.set(level);
            dl.set((level / 6).clamp(8, level));
            Panel::set_backlight(level);
            sett.borrow_mut().brightness = pct;
            system::save_settings(&sett.borrow());
        });
    }
    {
        let (da, sett) = (dim_after_us.clone(), settings.clone());
        app.on_setting_dim_changed(move |i| {
            let i = i.clamp(0, system::DIM_SECS.len() as i32 - 1);
            da.set(system::DIM_SECS[i as usize] * 1_000_000);
            sett.borrow_mut().dim_index = i;
            system::save_settings(&sett.borrow());
        });
    }
    {
        let (oa, sett) = (off_after_us.clone(), settings.clone());
        app.on_setting_off_changed(move |i| {
            let i = i.clamp(0, system::OFF_SECS.len() as i32 - 1);
            oa.set(system::OFF_SECS[i as usize] * 1_000_000);
            sett.borrow_mut().off_index = i;
            system::save_settings(&sett.borrow());
        });
    }
    {
        let (weak, sett, toast) = (app.as_weak(), settings.clone(), toast.clone());
        app.on_setting_toggle_ssh(move || {
            let Some(app) = weak.upgrade() else { return };
            if !system::ssh_available() {
                toast("SSH needs a key from the setup page first".into(), 4);
                return;
            }
            let on = if system::ssh_running() {
                system::ssh_stop();
                false
            } else {
                system::ssh_start()
            };
            app.set_ssh_on(on);
            sett.borrow_mut().ssh = on;
            system::save_settings(&sett.borrow());
            toast(if on { "SSH on".into() } else { "SSH off".into() }, 3);
        });
    }
    {
        let ask = ask.clone();
        app.on_settings_enter(move |n| ask(Intent::SettingsEnter(n)));
    }
    {
        let ask = ask.clone();
        app.on_settings_leave(move || ask(Intent::SettingsBack));
    }

    let mut button_controls = activity_buttons::Controller::new();
    let mut standby = Standby::Active;
    let mut manual_sleep = false;
    let mut lift_resume_at = 0;
    let mut swallow_wake_touch = false;
    let mut last_input = now_monotonic_us();
    // When to re-assert the backlight after a wake, forced past the LED
    // layer; None when nothing is owed.
    let mut verify_at: Option<u64> = None;
    println!(
        "couch-gui: standby: dim after {}s, off after {}s (0=never)",
        dim_after_us.get() / 1_000_000,
        off_after_us.get() / 1_000_000
    );
    let (mut in_n, mut in_sum, mut in_max) = (0u64, 0u64, 0u64);
    let mut last_stat = now_monotonic_us();
    let mut rendered_once = false;
    let mut last_health = 0;
    let _ = std::fs::remove_file("/tmp/couch-gui.health");
    let feedback_page = |app: &App| (
        (app.get_light_shown(), app.get_chooser_shown(), app.get_settings_shown(),
         app.get_keyboard_shown(), app.get_wifi_setup_shown(), app.get_pair_shown(),
         app.get_setup_mode(), app.get_recording()),
        app.get_settings_panel(), app.get_area_index(), app.get_light_room_id(), app.get_player_shown(), app.get_tv_shown(), app.get_thermostat_shown(),
    );
    let mut last_feedback_page = feedback_page(&app);
    let dismiss_feedback = |app: &App, scenes: &mut scenes::Controller, lights: &mut lights::Controller| {
        scenes.dismiss_feedback(app);
        lights.clear_brightness(app);
        toast_until.set(None);
        app.set_toast("".into());
        app.set_volume_shown(false);
        app.set_thermostat_feedback_shown(false);
        app.set_sonos_feedback_shown(false);
        // Hide even a card whose exit animation is still running.
        app.set_feedback_enabled(false);
    };

    let mut back_hold = input::BackHold::default();
    let exit_device = |app: &App| {
        if app.get_activity_busy() {
            app.invoke_cancel_activity();
        } else if app.get_camera_shown() {
            app.invoke_close_camera();
        } else if app.get_thermostat_shown() {
            app.invoke_thermostat_action("close".into(),0);
        } else if app.get_tv_shown() {
            app.invoke_tv_action("close".into());
        } else if app.get_player_shown() {
            app.set_player_panel(0);
            app.invoke_player_action("back".into(), 0.);
        }
    };
    loop {
        if motion.poll(wake_on_lift && standby != Standby::Active && now_monotonic_us() >= lift_resume_at) {
            app.set_dock_clock_shown(false);
            if standby == Standby::Off { light_controls.wake(); }
            wake(&mut screen, active_level.get());
            standby = Standby::Active;
            last_input = now_monotonic_us();
            verify_at = Some(last_input + 1_000_000);
            manual_sleep = false;
            println!("couch-gui: standby: wake on lift");
        }
        let back_context = if app.get_activity_busy() { "activity-sequence".into() } else if app.get_camera_shown() || app.get_tv_shown() || app.get_player_shown() || app.get_thermostat_shown() {
            format!("{}:{}:{}:{}:{}", app.get_camera_shown(), app.get_tv_shown(), app.get_player_shown(), app.get_thermostat_shown(), app.get_active_activity())
        } else { String::new() };
        back_hold.context(back_context);
        if back_hold.poll(now_monotonic_us()) { exit_device(&app); }
        let replay = button_controls.next_replay();
        let replayed = replay.is_some();
        let press = replay.or_else(||pad.poll()).and_then(|press| {
            if replayed { return Some(press); }
            if press.code == 158 && !press.released {
                last_input = now_monotonic_us();
                if standby == Standby::Dim && !app.get_dock_clock_shown() {
                    wake(&mut screen, active_level.get());
                    standby = Standby::Active;
                    verify_at = Some(last_input + 1_000_000);
                }
            }
            match back_hold.handle(press, now_monotonic_us(), standby != Standby::Off && !app.get_dock_clock_shown()) {
                input::BackAction::Pass(press) => Some(press),
                input::BackAction::Consume => None,
                input::BackAction::Exit => { exit_device(&app); None },
            }
        });
        if let Some(press) = press {
            // The side KEY_POWER belongs to the appliance, never the TV or
            // activity. Consume both edges; a held button cannot wake itself.
            if press.code == keypad::KEY_SIDE_POWER {
                if !press.released && !press.repeat {
                    last_input = now_monotonic_us();
                    app.set_dock_clock_shown(false);
                    if standby == Standby::Off {
                        manual_sleep = false;
                        light_controls.wake();
                        wake(&mut screen, active_level.get());
                        standby = Standby::Active;
                        verify_at = Some(last_input + 1_000_000);
                        println!("couch-gui: standby: wake on side power");
                    } else {
                        manual_sleep = true;
                        // Let the motion from pressing/setting down settle.
                        lift_resume_at = last_input + 2_000_000;
                        verify_at = None;
                        Panel::set_backlight(0);
                        screen.blank(true);
                        standby = Standby::Off;
                        println!("couch-gui: standby: sleep on side power");
                    }
                }
                continue;
            }
            if press.released && press.mic.is_none() && press.menu.is_none() {
                if !replayed {button_controls.handle(&app,&press);}
                continue;
            }
            if !press.repeat {
                in_n += 1;
                in_sum += press.latency_us;
                in_max = in_max.max(press.latency_us);
            }
            if !press.repeat {swallow_dock_repeats = app.get_dock_clock_shown();}
            if press.repeat && swallow_dock_repeats {continue;}
            last_input = now_monotonic_us();
            manual_sleep = false;
            // A key on a dark panel wakes it and does nothing else - except
            // the microphone key, whose press is the whole intent.
            let swallow = (standby == Standby::Off || app.get_dock_clock_shown()) && press.mic != Some(true);
            app.set_dock_clock_shown(false);
            if standby != Standby::Active {
                println!("couch-gui: standby: wake on key ({:?})", standby);
                if standby == Standby::Off { light_controls.wake(); }
                wake(&mut screen, active_level.get());
                standby = Standby::Active;
                verify_at = Some(now_monotonic_us() + 1_000_000);
            }
            // The menu key's edges drive the hold-to-open below. Recorded
            // before the wake swallow, so a hold that begins on a dark panel
            // still opens settings once the wake is done.
            physical_input.menu_edge(press.menu, now_monotonic_us());
            if swallow {
                continue;
            }
            if app.get_activity_busy() { continue; }
            if !replayed && button_controls.handle(&app,&press) {continue;}
            if press.code == 60 && app.get_activity_running() && !press.repeat {
                app.invoke_end_activity();
                continue;
            }
            if press.menu == Some(true) && !app.get_pair_shown() {
                if app.get_tv_shown() {app.invoke_tv_action("menu".into());}
                else if app.get_player_shown() {app.invoke_player_action("Input.ContextMenu".into(),0.);}
                // In a room, Menu on a highlighted Sonos row opens its source
                // picker; on any other row it is left to the settings hold.
                else if app.get_light_shown() && !app.get_chooser_shown() && !app.get_settings_shown() && !app.get_keyboard_shown() {sonos_room.menu();}
            }
            // Hold to talk. The key is not routed into the UI: it opens the
            // microphone and nothing else, so there is no screen on which it
            // means something different.
            match physical_input.microphone(press.mic, now_monotonic_us(), mic.recording()) {
                input::MicAction::Start => mic.start(),
                input::MicAction::Stop => mic.stop(),
                input::MicAction::None => {},
            }
            if let Some(key) = press.key.filter(|key| {
                !app.get_pair_shown()
                    && !(press.repeat && (app.get_light_shown() || app.get_chooser_title()=="SCENES" && app.get_chooser_shown())
                        && *key == slint::platform::Key::Return)
            }) {
                let text = SharedString::from(char::from(key));
                tv_controls.physical_input(press.repeat, || {
                    activity_controls.physical_input(press.repeat, || {
                        light_controls.physical_input(press.repeat, || {
                            thermostat_controls.physical_input(press.repeat, || {
                                sonos_room.physical_input(press.repeat, || {
                                    window.dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
                                    window.dispatch_event(WindowEvent::KeyReleased { text });
                                });
                            });
                        });
                    });
                });
            }
        }

        // Slint's own hit testing decides what a tap lands on, so nothing here
        // needs to know what is on screen.
        while let Some(event) = pointer.poll() {
            // Full powerdown sleeps the touch controller; button wake remains
            // required. While only dimmed, the first tap restores brightness
            // without also activating whatever happens to be underneath it.
            match touch_disposition(standby, &event, &mut swallow_wake_touch) {
                TouchDisposition::Ignore => continue,
                TouchDisposition::Wake => {
                    app.set_dock_clock_shown(false);
                    wake(&mut screen, active_level.get());
                    standby = Standby::Active;
                    last_input = now_monotonic_us();
                    verify_at = Some(last_input + 1_000_000);
                    println!("couch-gui: standby: wake from dim on touch");
                    continue;
                }
                TouchDisposition::Dispatch => {}
            }
            last_input = now_monotonic_us();
            let (position, ev) = match event {
                touch::Event::Pressed { x, y } => (
                    LogicalPosition::new(x, y),
                    0,
                ),
                touch::Event::Moved { x, y } => (LogicalPosition::new(x, y), 1),
                touch::Event::Released { x, y } => (LogicalPosition::new(x, y), 2),
            };
            window.dispatch_event(match ev {
                0 => WindowEvent::PointerPressed { position, button: PointerEventButton::Left },
                1 => WindowEvent::PointerMoved { position },
                _ => WindowEvent::PointerReleased { position, button: PointerEventButton::Left },
            });
        }

        // The meter has to keep up with a voice, so it is read every frame
        // rather than on the one-second tick.
        if app.get_recording() != mic.recording() {
            app.set_recording(mic.recording());
        }
        if mic.recording() {
            app.set_mic_level(mic.meter());
        }
        physical_input.sync_microphone(mic.recording());
        if app.get_mic_latched() != physical_input.latched() {
            app.set_mic_latched(physical_input.latched());
        }

        let now = now_monotonic_us();
        if let Some(at) = keyboard_at {
            if !keyboard_opened && now >= at {
                keyboard_opened = true;
                app.set_keyboard_title("TRY THE KEYBOARD".into());
                app.set_keyboard_placeholder("Type something".into());
                app.invoke_open_keyboard();
            }
        }
        if let Some(at) = settings_at {
            if !settings_opened && now >= at {
                settings_opened = true;
                ask(Intent::OpenSettings);
            }
        }
        if let Some(at) = open_at {
            if !opened && now >= at {
                opened = true;
                app.invoke_activate();
            }
        }

        // A hold of the menu key on the home screen opens settings. Only there:
        // a modal is already up owns the key, and the hub is what settings sits
        // over. The state it shows - the SSID, whether SSH is up - is read here,
        // once, at open time rather than on the tick.
            let on_home = !app.get_tv_shown() && !app.get_player_shown() && !app.get_light_shown() && !app.get_wifi_setup_shown() && !app.get_settings_shown()
                && !app.get_keyboard_shown()
                && !app.get_chooser_shown()
                && !app.get_pair_shown()
                && !app.get_setup_mode()
                && !app.get_recording();
        if physical_input.settings_hold_due(now, on_home) {
            ask(Intent::OpenSettings);
        }

        // Clear a toast when its time is up (cheap, every loop).
        if toast_until.get().is_some_and(|t| now_monotonic_us() >= t) {
            toast_until.set(None);
            app.set_toast("".into());
        }

        network_setup.poll(&app);
        // Capture only navigation, then slide framebuffer snapshots so the
        // room animation does not rasterize the entire Slint scene every frame.
        let was_room = app.get_light_shown();
        let room_navigation = !app.get_pair_shown() && light_controls.navigation_pending();
        if room_navigation {
            screen.snapshot();
        }
        light_controls.poll(&app);
        // Before the intent is performed below, so a source list opens its
        // chooser in this same iteration.
        if sonos_room.poll(&app) { ask(Intent::OpenChooser); }
        if feedback_page(&app) != last_feedback_page {
            dismiss_feedback(&app, &mut scene_controls, &mut light_controls);
            last_feedback_page = feedback_page(&app);
        }
        room_monitor.poll(&app, &mut areas.borrow_mut(), current.get());
        if room_navigation && was_room != app.get_light_shown() {
            slint::platform::update_timers_and_animations();
            if let Some(us) = screen.render_offscreen(&window) {
                frames += 1;
                render_us += us;
                frame_max = frame_max.max(us);
                let from = if app.get_light_shown() { Arrive::FromRight } else { Arrive::FromLeft };
                let status = (0, app.get_status_h().round() as u32);
                let cost = screen.slide(from, &[status], SLIDE);
                frames += cost.frames;
                render_us += cost.work_us;
                wait_us += cost.wait_us;
                frame_max = frame_max.max(cost.max_us);
                println!("couch-gui: room slide {} ({} frames)",
                    if app.get_light_shown() { "in" } else { "out" }, cost.frames);
            }
            slint::platform::update_timers_and_animations();
        }

        // Device state changes in seconds, not frames.
        if now - last_tick > 1_000_000 {
            last_tick = now;
            if let Some((clock, settings)) = remote_clock.poll() {
                app.set_clock(clock.into());
                dock_clock_enabled = settings.dock_clock;
                wake_on_lift = settings.wake_on_lift;
            }
            if let Some((raw, saved, accent)) = home::read(&loaded_home) {
                home::apply_accent(&app,accent);
                // Appearance updates in overlays too; defer home navigation changes
                // until returning home so an open device control remains in place.
                if !app.get_tv_shown() && !app.get_player_shown() && !app.get_light_shown() && !app.get_wifi_setup_shown() && !app.get_keyboard_shown() && !app.get_settings_shown() && !app.get_chooser_shown() {
                    loaded_home = raw; *areas.borrow_mut() = saved;
                    current.set(0); app.set_area_dots(ModelRc::new(VecModel::from(vec![true;areas.borrow().len()])));
                    put_front(&app,0);
                }
            }


            // The deadline is the daemon's, carried in the file, so restarting
            // this process cannot extend a PIN that is already on its way out.
            match system::pairing_pin() {
                Some((pin, left)) => {
                    app.set_pair_pin(SharedString::from(pin));
                    app.set_pair_seconds(left);
                    app.set_pair_shown(left > 0);
                }
                None => app.set_pair_shown(false),
            }
            match system::battery() {
                Some(b) => {
                    app.set_battery(b.percent);
                    app.set_charging(b.charging);
                }
                None => {app.set_battery(0);app.set_charging(false);},
            }
            app.set_wifi_level(system::wifi_level());
            app.set_wifi_ssid(system::wifi_ssid().into());
            app.set_wifi_signal(system::wifi_dbm().map(|dbm| format!("{dbm} dBm")).unwrap_or_else(|| "—".into()).into());

            let setup = system::in_setup_mode();
            if last_setup != Some(setup) {
                last_setup = Some(setup);
                app.set_setup_mode(setup);
                if setup {
                    let ssid = system::setup_ssid();
                    app.set_ssid(SharedString::from(ssid.clone()));
                    if let Some(img) = qr::render(&qr::wifi_join_record(&ssid), 296) {
                        app.set_qr(img);
                    }
                }
            }
            app.set_approval(match system::approval_state() {
                Approval::Idle => 0,
                Approval::Waiting => 1,
                Approval::Granted => 2,
                Approval::TimedOut => 3,
            });

            // Standby. Anything a person is looking at or waiting on holds
            // the panel awake and restarts the clock; otherwise it dims, then
            // powers down, on the two idle timers.
            let hold = app.get_activity_busy() || app.get_activity_keep_awake() || app.get_pair_shown() || mic.recording() || app.get_setup_mode() || app.get_wifi_setup_shown() || app.get_keyboard_shown();
            let mut idle = now.saturating_sub(last_input);
            // The panel is meant to be showing something in every state but
            // Off. If the driver says it is asleep anyway - it has happened,
            // with nothing in this process asking - bring it back now rather
            // than at the next wake, and re-assert the level the LED node
            // thinks it already has.
            if standby != Standby::Off && screen.unblank_if_asleep() {
                println!("couch-gui: standby: panel found asleep while {:?}, unblanked", standby);
                Panel::force_backlight(if standby == Standby::Dim { dim_level.get() } else { active_level.get() });
                slint::platform::update_timers_and_animations();
            }
            // Off-after of 0 means never power the panel down, only dim.
            let off_us = off_after_us.get();
            let dock = dock_clock_enabled && app.get_charging() && !hold;
            if !dock && app.get_dock_clock_shown() {
                app.set_dock_clock_shown(false);
                wake(&mut screen, active_level.get());
                standby = Standby::Active;
                last_input = now;
                idle = 0;
            }
            if manual_sleep {
                // Explicit sleep takes precedence over activity and dock holds.
            } else if dock && idle >= dim_after_us.get() {
                if standby == Standby::Off {wake(&mut screen, dim_level.get());}
                if !app.get_dock_clock_shown() {Panel::set_backlight(dim_level.get());}
                standby = Standby::Dim;
                app.set_dock_clock_shown(true);
                app.set_dock_clock_shift(((now / 60_000_000) % 5) as i32);
            } else if hold {
                app.set_dock_clock_shown(false);
                last_input = now;
                if standby != Standby::Active {
                    println!("couch-gui: standby: wake to show something ({:?})", standby);
                    wake(&mut screen, active_level.get());
                    standby = Standby::Active;
                    verify_at = Some(now + 1_000_000);
                }
            } else if standby == Standby::Active && idle >= dim_after_us.get() {
                println!("couch-gui: standby: dim after {}s idle", idle / 1_000_000);
                Panel::set_backlight(dim_level.get());
                standby = Standby::Dim;
            } else if standby == Standby::Dim && off_us > 0 && idle >= off_us {
                println!("couch-gui: standby: off after {}s idle", idle / 1_000_000);
                Panel::set_backlight(0);
                screen.blank(true);
                standby = Standby::Off;
            }
        }

        if nav && now >= nav_at {
            // The hub owns the row arithmetic; this only wraps.
            let rows = app.get_row_count();
            if rows > 0 {
                app.set_focus_row((app.get_focus_row() + 1) % rows);
                app.invoke_settle_focus();
            }
            nav_at = now + 700_000;
        }

        if auto_slide && now >= slide_at {
            app.invoke_area_step(1);
            slide_at = now + 1_500_000;
        }

        // Everything above only asked; this is where a page change happens,
        // with no event being dispatched and the draw free to run. It blocks
        // for the slide's duration: keys pressed meanwhile queue in the
        // keypad and are taken one per frame afterwards, so a second Left or
        // Right simply slides again.
        if feedback_page(&app) != last_feedback_page {
            dismiss_feedback(&app, &mut scene_controls, &mut light_controls);
        }
        if let Some(what) = intent.take() {
            dismiss_feedback(&app, &mut scene_controls, &mut light_controls);
            if let Some(cost) = navigator.transition(&mut screen, &window, &app, what) {
                frames += cost.frames;
                render_us += cost.work_us;
                wait_us += cost.wait_us;
                frame_max = frame_max.max(cost.max_us);
            }
        }

        if let Some(error) = activity_runtime.poll(&app) { toast(error, 5); }
        let was_activity = (app.get_player_shown(), app.get_tv_shown(), app.get_thermostat_shown());
        let activity_navigation = activity_controls.navigation_pending(&app) || tv_controls.navigation_pending() || thermostat_controls.navigation_pending();
        if activity_navigation {screen.snapshot();}
        activity_controls.poll(&app);
        tv_controls.poll(&app);
        cameras.poll(&app, standby != Standby::Off);
        if let Some(message)=thermostat_controls.poll(&app){toast(message,2);}
        if let Some(error)=button_controls.poll(&app) {toast(error,3);}
        if !app.get_player_shown() && !app.get_tv_shown() {app.set_active_activity("".into());}
        if activity_navigation && was_activity != (app.get_player_shown(), app.get_tv_shown(), app.get_thermostat_shown()) {
            dismiss_feedback(&app, &mut scene_controls, &mut light_controls);
            slint::platform::update_timers_and_animations();
            if let Some(us) = screen.render_offscreen(&window) {
                frames += 1; render_us += us; frame_max = frame_max.max(us);
                let entering = app.get_player_shown() || app.get_tv_shown() || app.get_thermostat_shown();
                let cost = screen.slide(if entering {Arrive::FromRight} else {Arrive::FromLeft}, &[], SLIDE);
                frames += cost.frames; render_us += cost.work_us; wait_us += cost.wait_us;
                frame_max = frame_max.max(cost.max_us);
                println!("couch-gui: activity slide {} ({} frames)", if entering {"in"} else {"out"},cost.frames);
            }
            slint::platform::update_timers_and_animations();
        }
        if feedback_page(&app) != last_feedback_page {
            dismiss_feedback(&app, &mut scene_controls, &mut light_controls);
        }
        last_feedback_page = feedback_page(&app);
        // A scene picked in a chooser reports on the destination page. Older
        // in-flight replies were invalidated before navigation above.
        scene_controls.poll(&app);
        slint::platform::update_timers_and_animations();

        // A drawn frame comes back paced to the panel's refresh, so an
        // animation costs one rasterisation per refresh rather than as many
        // as the CPU can manage. Nothing to draw: a short sleep, and back to
        // polling input.
        // A powered-down panel shows nothing, so nothing is drawn for it:
        // Slint's state keeps advancing (the clock, a PIN arriving) and the
        // first frame after waking catches up. Keep local health updates
        // alive in standby; they do not depend on any network service.
        let health_now = now_monotonic_us();
        if rendered_once && health_now - last_health >= 1_000_000 {
            last_health = health_now;
            if let Err(error) = system::report_gui_health() {
                eprintln!("couch-gui: health marker: {error}");
            }
        }
        if standby == Standby::Off {
            pad.wait_for_input();
            continue;
        }

        match screen.render(&window) {
            Some(cost) => {
                rendered_once = true;
                render_us += cost.work_us;
                wait_us += cost.wait_us;
                frames += 1;
                frame_max = frame_max.max(cost.work_us);
            }
            None => {
                // Idle, so the ~220ms this costs hitches nothing on screen.
                if verify_at.is_some_and(|t| now_monotonic_us() >= t) && standby == Standby::Active {
                    verify_at = None;
                    if screen.unblank_if_asleep() {
                        println!("couch-gui: standby: panel asleep after wake, unblanked");
                    }
                    Panel::force_backlight(active_level.get());
                    slint::platform::update_timers_and_animations();
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }

        if now_monotonic_us() - last_stat > 5_000_000 && frames > 0 {
            println!(
                "couch-gui: {frames} frames, {} us/frame, {frame_max} us worst, {} us paced | input {} us avg, {in_max} us max (n={in_n})",
                render_us / frames,
                wait_us / frames,
                if in_n > 0 { in_sum / in_n } else { 0 }
            );
            last_stat = now_monotonic_us();
            frames = 0;
            render_us = 0;
            frame_max = 0;
            wait_us = 0;
            in_n = 0;
            in_sum = 0;
            in_max = 0;
        }
    }
}

#[cfg(test)]
mod standby_tests {
    use super::*;

    #[test]
    fn dim_wake_consumes_the_whole_contact_then_allows_the_next_tap() {
        let press = touch::Event::Pressed { x: 20.0, y: 30.0 };
        let moved = touch::Event::Moved { x: 25.0, y: 35.0 };
        let release = touch::Event::Released { x: 25.0, y: 35.0 };
        let mut swallow = false;
        assert_eq!(touch_disposition(Standby::Dim, &press, &mut swallow), TouchDisposition::Wake);
        assert_eq!(touch_disposition(Standby::Active, &moved, &mut swallow), TouchDisposition::Ignore);
        assert_eq!(touch_disposition(Standby::Active, &release, &mut swallow), TouchDisposition::Ignore);
        assert_eq!(touch_disposition(Standby::Active, &press, &mut swallow), TouchDisposition::Dispatch);
        assert_eq!(touch_disposition(Standby::Off, &press, &mut swallow), TouchDisposition::Ignore);
        assert!(!swallow);
    }
}
