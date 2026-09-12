//! Local fixtures driving the unmodified production Slint components.
use crate::{App, ChoiceItem, LiveActivity, PlayerChoice, RoomRow, SceneCell, TvChoice};
use slint::{ComponentHandle, Model, ModelRc, SharedPixelBuffer, VecModel};
use std::{cell::RefCell, rc::Rc, time::Duration};

thread_local! { static DEMO: RefCell<Option<Rc<RefCell<Demo>>>> = const { RefCell::new(None) }; }
struct Demo {
    app: slint::Weak<App>,
    room: Option<usize>,
    levels: Vec<[i32; 2]>,
    items: Rc<VecModel<ChoiceItem>>,
    elapsed: i32,
    scene: usize,
    timer: slint::Timer,
    toast: slint::Timer,
}
const ROOMS: [&str; 6] = [
    "Living room",
    "Kitchen",
    "Bedroom",
    "Office",
    "Dining room",
    "Hallway",
];
const SCENES: [&str; 3] = ["Relax", "Bright", "Movie night"];
fn png(bytes: &[u8]) -> slint::Image {
    let image = image::load_from_memory(bytes).unwrap().to_rgba8();
    slint::Image::from_rgba8(SharedPixelBuffer::clone_from_slice(
        image.as_raw(),
        image.width(),
        image.height(),
    ))
}
fn lamp() -> slint::Image {
    png(include_bytes!("../../ui/couch-gui/assets/icon-lamp.png"))
}
fn tv() -> slint::Image {
    png(include_bytes!("../../ui/couch-gui/assets/icon-tv.png"))
}
fn with(f: impl FnOnce(&mut Demo, &App)) {
    DEMO.with(|slot| {
        if let Some(d) = slot.borrow().as_ref() {
            let mut d = d.borrow_mut();
            if let Some(app) = d.app.upgrade() {
                f(&mut d, &app);
            }
        }
    });
}
// Callbacks run after input dispatch, as the framebuffer host does on device.
fn later(f: impl FnOnce(&mut Demo, &App) + 'static) {
    slint::Timer::single_shot(Duration::ZERO, move || with(f));
}
impl Demo {
    fn rooms(&self, app: &App) {
        app.set_rooms(ModelRc::new(VecModel::from(
            ROOMS
                .iter()
                .enumerate()
                .map(|(i, n)| {
                    let active = self.levels[i].iter().filter(|v| **v > 0).count() as i32;
                    RoomRow {
                        name: (*n).into(),
                        devices: "3 devices".into(),
                        detail: format!(
                            "{active} {} on",
                            if active == 1 { "light" } else { "lights" }
                        )
                        .into(),
                        icon: png(match i {
                            0 => include_bytes!("../../ui/couch-gui/assets/icon-sofa.png"),
                            1 | 4 => {
                                include_bytes!("../../ui/couch-gui/assets/icon-cooking-pot.png")
                            }
                            2 => include_bytes!("../../ui/couch-gui/assets/icon-bed.png"),
                            3 => include_bytes!("../../ui/couch-gui/assets/icon-monitor.png"),
                            _ => include_bytes!("../../ui/couch-gui/assets/icon-door-open.png"),
                        }),
                        power_state: i32::from(active > 0),
                        active_count: active,
                        status_known: true,
                        ..Default::default()
                    }
                })
                .collect::<Vec<_>>(),
        )));
    }
    fn refresh(&self, app: &App) {
        if let Some(room) = self.room {
            for i in 0..2 {
                let level = self.levels[room][i];
                self.items.set_row_data(
                    i,
                    ChoiceItem {
                        title: ["Ceiling lights", "Floor lamp"][i].into(),
                        detail: if level > 0 {
                            format!("On · {level}%")
                        } else {
                            "Off".into()
                        }
                        .into(),
                        active: level > 0,
                        power_known: true,
                        light: true,
                        icon: lamp(),
                    },
                );
            }
        }
        self.rooms(app);
    }
    fn open_room(&mut self, app: &App, i: usize) {
        if i >= ROOMS.len() {
            return;
        }
        self.clear(app);
        self.room = Some(i);
        self.items = Rc::new(VecModel::from(vec![
            ChoiceItem::default(),
            ChoiceItem::default(),
            ChoiceItem {
                title: "Media player".into(),
                detail: "Tears of Steel · Kodi".into(),
                icon: tv(),
                ..Default::default()
            },
        ]));
        app.set_light_items(self.items.clone().into());
        self.refresh(app);
        app.set_light_title(ROOMS[i].into());
        app.set_light_detail("".into());
        app.set_light_scene_count(3);
        app.set_light_scene_label("3 scenes".into());
        app.set_light_shown(true);
        app.invoke_focus_light();
    }
    fn clear(&self, app: &App) {
        app.set_brightness_shown(false);
        app.set_scene_feedback_shown(false);
        app.set_volume_shown(false);
        app.set_toast("".into());
    }
    fn back(&mut self, app: &App) {
        self.clear(app);
        if app.get_thermostat_shown() {
            if app.get_thermostat_modes_shown() { app.set_thermostat_modes_shown(false); }
            else { app.set_thermostat_shown(false); app.invoke_focus_light(); }
        } else if app.get_chooser_shown() {
            app.set_chooser_shown(false);
        } else if app.get_tv_shown() {
            app.set_tv_panel(0);
            app.set_tv_shown(false);
            if self.room.is_some() {
                app.invoke_focus_light();
            } else {
                app.invoke_focus_home();
            }
        } else if app.get_player_shown() {
            app.set_player_shown(false);
            if self.room.is_some() {
                app.invoke_focus_light();
            } else {
                app.invoke_focus_home();
            }
        } else {
            self.room = None;
            app.set_light_shown(false);
            app.invoke_focus_home();
        }
    }
    fn player(&mut self, app: &App) {
        self.clear(app);
        app.set_chooser_shown(false);
        app.set_player_shown(true);
        app.set_player_ready(true);
        app.set_player_connected(true);
        app.set_player_can_seek(true);
        app.set_player_title("Tears of Steel".into());
        app.set_player_metadata("2012 · Science fiction · 12 min".into());
        app.set_player_activity("Watch a movie".into());
        app.set_player_room(ROOMS[self.room.unwrap_or(0)].into());
        app.set_player_panel(0);
        self.progress(app);
        app.invoke_focus_player();
    }
    fn progress(&self, app: &App) {
        app.set_player_progress(self.elapsed as f32 / 734. * 100.);
        app.set_player_elapsed(format!("{}:{:02}", self.elapsed / 60, self.elapsed % 60).into());
        let left = 734 - self.elapsed;
        app.set_player_remaining(format!("−{}:{:02}", left / 60, left % 60).into());
    }
    fn scenes(&mut self, app: &App) {
        app.set_chooser_title("Scenes".into());
        app.set_chooser_index(0);
        app.set_chooser_items(ModelRc::new(VecModel::from(
            SCENES
                .iter()
                .map(|s| ChoiceItem {
                    title: (*s).into(),
                    ..Default::default()
                })
                .collect::<Vec<_>>(),
        )));
        app.set_chooser_shown(true);
    }
    fn scene(&mut self, app: &App, i: usize) {
        self.scene = i % 3;
        if let Some(r) = self.room {
            self.levels[r] = [[35, 55], [100, 100], [10, 0]][self.scene];
        }
        self.refresh(app);
        app.set_chooser_shown(false);
        app.set_scene_feedback_name(SCENES[self.scene].into());
        app.set_scene_feedback_status("Scene activated".into());
        app.set_scene_feedback_shown(true);
        self.dismiss();
    }
    fn dismiss(&self) {
        let weak = self.app.clone();
        self.toast.start(
            slint::TimerMode::SingleShot,
            Duration::from_millis(1600),
            move || {
                if let Some(a) = weak.upgrade() {
                    a.set_brightness_shown(false);
                    a.set_scene_feedback_shown(false);
                    a.set_volume_shown(false);
                    a.set_toast("".into());
                }
            },
        );
    }
}
pub fn configure(app: &App) {
    app.set_clock("9:41".into());
    app.set_battery(82);
    app.set_wifi_level(4);
    app.set_wifi_signal("−48 dBm".into());
    app.set_accent(slint::Color::from_rgb_u8(176, 139, 250));
    app.set_accent_background(slint::Color::from_rgb_u8(49, 39, 67));
    app.set_area_name("HOME".into());
    app.set_area_dots(ModelRc::new(VecModel::from(vec![true])));
    app.set_activities(ModelRc::new(VecModel::from(vec![LiveActivity {
        title: "Watch a movie".into(),
        source: "Kodi".into(),
        place: "Living room".into(),
        kind: 0,
    }])));
    app.set_scenes(ModelRc::new(VecModel::from(
        SCENES
            .iter()
            .map(|n| SceneCell {
                name: (*n).into(),
                active: false,
            })
            .collect::<Vec<_>>(),
    )));
    let mut art = image::load_from_memory(include_bytes!(
        "../../docs/mockups/kodi-activity/assets/fanart.jpg"
    ))
    .unwrap()
    .resize_to_fill(480, 800, image::imageops::FilterType::Triangle)
    .to_rgba8();
    for (_, y, p) in art.enumerate_pixels_mut() {
        let shade = (0.18 + ((y as f32 - 300.) / 350.).clamp(0., 1.) * 0.78).clamp(0., 0.96);
        for c in 0..3 {
            p[c] = (p[c] as f32 * (1. - shade) + [17., 19., 17.][c] * shade) as u8;
        }
    }
    app.set_player_fanart(slint::Image::from_rgba8(
        SharedPixelBuffer::clone_from_slice(art.as_raw(), 480, 800),
    ));
    app.set_player_has_art(true);
    app.set_player_logo(png(include_bytes!(
        "../../docs/mockups/kodi-activity/assets/clearlogo.png"
    )));
    app.set_player_has_logo(true);
    let d = Rc::new(RefCell::new(Demo {
        app: app.as_weak(),
        room: None,
        levels: vec![[56, 0]; 6],
        items: Rc::new(VecModel::default()),
        elapsed: 312,
        scene: 0,
        timer: slint::Timer::default(),
        toast: slint::Timer::default(),
    }));
    d.borrow().rooms(app);
    DEMO.with(|slot| *slot.borrow_mut() = Some(d.clone()));
    d.borrow()
        .timer
        .start(slint::TimerMode::Repeated, Duration::from_secs(1), || {
            with(|d, a| {
                if a.get_player_shown() && !a.get_player_paused() {
                    d.elapsed = (d.elapsed + 1) % 735;
                    d.progress(a);
                }
            })
        });
    app.on_thermostat_action(|action, index| {
        let action = action.to_string();
        later(move |d, a| match action.as_str() {
            "close" => d.back(a),
            "home" => { a.set_thermostat_modes_shown(false); a.set_thermostat_shown(false); d.room = None; a.set_light_shown(false); a.invoke_focus_home(); }
            "dismiss" => a.set_thermostat_modes_shown(false),
            "modes" if a.get_thermostat_modes().row_count() > 0 => a.set_thermostat_modes_shown(true),
            "mode" => {
                if let Some(mode) = ["Off", "Heat", "Cool", "Heat / cool"].get(index as usize) {
                    a.set_thermostat_mode((*mode).into());
                    a.set_thermostat_status(if index == 0 { "Off" } else { "Idle" }.into());
                    a.set_thermostat_adjustable(index != 0);
                    a.set_thermostat_range(index == 3);
                    a.set_thermostat_target(if index == 3 { "20°C – 24°C" } else { "21°C" }.into());
                }
                a.set_thermostat_modes_shown(false);
            }
            "adjust" if a.get_thermostat_adjustable() => {
                if a.get_thermostat_range() {
                    // This fixture keeps a fixed four-degree deadband like the
                    // production controller, without contacting a thermostat.
                    let value = a.get_thermostat_target().split('°').next().unwrap_or("20").parse::<f64>().unwrap_or(20.0);
                    let low = (value + index.signum() as f64 * 0.5).clamp(16.0, 26.0);
                    a.set_thermostat_target(format!("{low}°C – {}°C", low + 4.0).into());
                } else {
                    let value = a.get_thermostat_target().trim_end_matches("°C").parse::<f64>().unwrap_or(21.0);
                    a.set_thermostat_target(format!("{}°C", (value + index.signum() as f64 * 0.5).clamp(16.0, 30.0)).into());
                }
            }
            _ => {}
        });
    });
    app.on_tv_action(|action| {
        let action = action.to_string();
        later(move |d, a| match action.as_str() {
            "close" => d.back(a),
            "dismiss" => a.set_tv_panel(0),
            "commands" => {
                a.set_tv_choices(ModelRc::new(VecModel::from([("toggle","Power toggle"),("power-on","Power on"),("volume-up","Volume up"),("volume-down","Volume down"),("mute","Mute")].into_iter().map(|(action,title)|TvChoice{action:format!("ir:{action}").into(),title:title.into(),detail:"Send infrared command".into()}).collect::<Vec<_>>())));
                a.set_tv_panel(2);
            }
            "apps" | "inputs" => {
                let apps = action == "apps";
                a.set_tv_choices(ModelRc::new(VecModel::from(if apps {
                    vec![TvChoice {
                        action: if a.get_tv_apple() {
                            "app:com.apple.TVWatchList"
                        } else {
                            "app:https://www.youtube.com/"
                        }
                        .into(),
                        title: if a.get_tv_apple() { "TV" } else { "YouTube" }.into(),
                        detail: if a.get_tv_android() {
                            "Configured shortcut"
                        } else if a.get_tv_apple() {
                            "Open on Apple TV"
                        } else {
                            "App"
                        }
                        .into(),
                    }]
                } else {
                    vec![TvChoice {
                        action: "input:HDMI_1".into(),
                        title: "HDMI 1".into(),
                        detail: "Connected".into(),
                    }]
                })));
                a.set_tv_panel(if apps { 2 } else { 1 });
            }
            "play" | "pause" if a.get_tv_android() && a.get_tv_media_active() => {
                a.set_tv_media_state(if action == "play" { "Playing" } else { "Paused" }.into());
            }
            "picture" => a.set_tv_panel(3),
            "sound" => a.set_tv_panel(4),
            _ => {
                a.set_tv_panel(0);
                a.set_tv_status("Example control sent".into());
            }
        });
    });
    app.on_open_room(|i| later(move |d, a| d.open_room(a, i as usize)));
    app.on_open_strip(|| later(|d, a| d.player(a)));
    app.on_open_scenes(|| later(|d, a| d.scenes(a)));
    app.on_room_scenes(|| later(|d, a| d.scenes(a)));
    app.on_room_scene_step(|n| {
        later(move |d, a| d.scene(a, (d.scene as i32 + n).rem_euclid(3) as usize))
    });
    app.on_chosen(|i| later(move |d, a| d.scene(a, i as usize)));
    app.on_close_chooser(|| later(|d, a| d.back(a)));
    app.on_back(|| later(|d, a| d.back(a)));
    app.on_light_back(|| later(|d, a| d.back(a)));
    app.on_home(|| {
        later(|d, a| {
            d.room = None;
            a.set_light_shown(false);
            d.clear(a);
            a.invoke_focus_home();
        })
    });
    app.on_light_activate(|i| {
        later(move |d, a| {
            if i == 2 {
                d.player(a);
            } else if let Some(r) = d.room {
                if (0..2).contains(&i) {
                    let v = &mut d.levels[r][i as usize];
                    *v = if *v > 0 { 0 } else { 56 };
                    d.refresh(a);
                }
            }
        })
    });
    app.on_light_brightness(|i, step| {
        later(move |d, a| {
            if let Some(r) = d.room {
                if (0..2).contains(&i) {
                    d.levels[r][i as usize] = (d.levels[r][i as usize] + step).clamp(0, 100);
                    d.refresh(a);
                    a.set_brightness_target(["Ceiling lights", "Floor lamp"][i as usize].into());
                    a.set_light_brightness_percent(d.levels[r][i as usize]);
                    a.set_brightness_shown(true);
                    d.dismiss();
                }
            }
        })
    });
    app.on_player_action(|action, value| {
        later(move |d, a| match action.as_str() {
            "back" => {
                if a.get_player_panel() > 0 {
                    a.set_player_panel(0)
                } else {
                    d.back(a)
                }
            }
            "play" => a.set_player_paused(!a.get_player_paused()),
            "seek" => {
                d.elapsed = (value.clamp(0., 100.) * 7.34) as i32;
                d.progress(a)
            }
            "skip" => {
                d.elapsed = (d.elapsed + value as i32).clamp(0, 734);
                d.progress(a)
            }
            "chapters" | "audio" | "subtitles" => {
                let (panel, labels) = match action.as_str() {
                    "chapters" => (
                        1,
                        vec![
                            "Opening",
                            "The promise",
                            "A different future",
                            "The plan",
                            "One more chance",
                            "Credits",
                        ],
                    ),
                    "audio" => (2, vec!["English · 5.1", "Commentary"]),
                    _ => (3, vec!["Off", "English"]),
                };
                a.set_player_panel(panel);
                a.set_player_choices(ModelRc::new(VecModel::from(
                    labels
                        .into_iter()
                        .map(|n| PlayerChoice {
                            title: n.into(),
                            detail: "".into(),
                        })
                        .collect::<Vec<_>>(),
                )));
            }
            "choose" => {
                if a.get_player_panel() == 1 {
                    d.elapsed = [0, 88, 198, 302, 436, 626][(value as usize).min(5)];
                    d.progress(a)
                }
                a.set_player_panel(0)
            }
            "volume" => {
                a.set_volume((a.get_volume() + value as i32).clamp(0, 100));
                a.set_volume_target("Media player".into());
                a.set_volume_shown(true);
                d.dismiss()
            }
            "chapter-step" => {
                d.elapsed = (d.elapsed + 90 * value as i32).clamp(0, 734);
                d.progress(a)
            }
            _ => {
                a.set_toast(format!("Kodi: {}", action.trim_start_matches("Input.")).into());
                d.dismiss()
            }
        })
    });
    app.set_volume(40);
    app.invoke_page_swapped();
    app.invoke_focus_home();
}
pub fn remote_button(name: &str) {
    // External WASM calls do not enter Winit's event processing automatically.
    // Queue a wake event so deferred Slint callbacks run immediately instead of
    // waiting for the next playback clock tick.
    let name = name.to_string();
    let _ = slint::invoke_from_event_loop(move || dispatch_button(&name));
}
fn dispatch_button(name: &str) {
    let in_tv = DEMO.with(|s| {
        s.borrow()
            .as_ref()
            .and_then(|d| d.borrow().app.upgrade())
            .is_some_and(|a| a.get_tv_shown())
    });
    if in_tv
        && [
            "play", "mute", "menu", "power", "red", "green", "blue", "yellow",
        ]
        .contains(&name)
    {
        let action = if name == "mute" { "toggle-mute" } else { name }.to_string();
        later(move |_, a| a.invoke_tv_action(action.into()));
        return;
    }
    match name {
        "home" => {
            let in_player = DEMO.with(|s| {
                s.borrow()
                    .as_ref()
                    .and_then(|d| d.borrow().app.upgrade())
                    .is_some_and(|a| a.get_player_shown() || a.get_tv_shown() || a.get_thermostat_shown())
            });
            if !in_player {
                later(|d, a| {
                    d.room = None;
                    a.set_light_shown(false);
                    a.set_chooser_shown(false);
                    d.clear(a);
                    a.invoke_page_swapped();
                    a.invoke_focus_home();
                });
                return;
            }
        }
        "play" | "mute" => {
            let action = name.to_string();
            later(move |_, a| a.invoke_player_action(action.into(), 0.));
            return;
        }
        "scenes" => {
            later(|d, a| d.scenes(a));
            return;
        }
        "media" => {
            later(|d, a| d.player(a));
            return;
        }
        "red" | "green" | "blue" | "yellow" => {
            let index = match name {
                "green" => 1,
                "blue" => 2,
                _ => 0,
            };
            later(move |d, a| d.scene(a, index));
            return;
        }
        "mic" | "menu" => {
            later(|d, a| {
                a.set_toast("Example control".into());
                d.dismiss();
            });
            return;
        }
        _ => {}
    }
    if name == "back-long" || name == "power" {
        later(|d, a| d.back(a));
        return;
    }
    use slint::platform::{Key, WindowEvent};
    let key = match name {
        "up" => Key::UpArrow,
        "down" => Key::DownArrow,
        "left" => Key::LeftArrow,
        "right" => Key::RightArrow,
        "ok" => Key::Return,
        "back" => Key::Escape,
        "home" => Key::Home,
        "volume-up" => Key::F23,
        "volume-down" => Key::F24,
        "channel-up" => Key::F21,
        "channel-down" => Key::F22,
        _ => return,
    };
    // Never hold the fixture borrow during dispatch: callbacks can inspect state.
    let app = DEMO.with(|s| s.borrow().as_ref().and_then(|d| d.borrow().app.upgrade()));
    if let Some(a) = app {
        let text: slint::SharedString = char::from(key).to_string().into();
        a.window()
            .dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
        a.window().dispatch_event(WindowEvent::KeyReleased { text });
    }
}
pub fn state_json() -> String {
    let mut result = "{}".to_string();
    with(|d, a| {
        result=format!("{{\"room\":{},\"player\":{},\"panel\":{},\"paused\":{},\"chooser\":{},\"brightness\":{},\"level\":{},\"focus\":{},\"tv\":{},\"tv_panel\":{},\"android_tv\":{},\"apple_tv\":{},\"infrared\":{}}}",d.room.map(|r|r.to_string()).unwrap_or("null".into()),a.get_player_shown(),a.get_player_panel(),a.get_player_paused(),a.get_chooser_shown(),a.get_brightness_shown(),d.room.map(|r|d.levels[r][0]).unwrap_or(0),a.get_focus_row(),a.get_tv_shown(),a.get_tv_panel(),a.get_tv_android(),a.get_tv_apple(),a.get_tv_ir());
        result.pop();
        result.push_str(&format!(",\"camera\":{}",a.get_camera_shown()));
        result.push_str(&format!(",\"thermostat\":{},\"thermostat_modes\":{},\"thermostat_target\":\"{}\",\"thermostat_mode\":\"{}\",\"thermostat_adjustable\":{},\"thermostat_range\":{}", a.get_thermostat_shown(), a.get_thermostat_modes_shown(), a.get_thermostat_target(), a.get_thermostat_mode(), a.get_thermostat_adjustable(), a.get_thermostat_range()));
        result.push_str(&format!(",\"media_active\":{},\"media_live\":{},\"media_has_duration\":{},\"media_has_art\":{},\"media_paused\":{}}}", a.get_tv_media_active(), a.get_tv_media_live(), a.get_tv_media_has_duration(), a.get_tv_media_has_art(), a.get_tv_media_state() == "Paused"));
    });
    result
}

// Documentation freezes fixture clocks, never the production component tree.
// A fresh browser page per image keeps each screenshot independent.
pub fn documentation_screen(name: &str) {
    let name = name.to_string();
    let _ = slint::invoke_from_event_loop(move || {
        with(|d, a| {
            d.timer.stop();
            d.clear(a);
            a.set_camera_shown(false);
            a.set_thermostat_shown(false);
            a.set_thermostat_feedback_shown(false);
            a.set_thermostat_modes_shown(false);
            a.set_tv_shown(false);
            a.set_player_shown(false);
            match name.as_str() {
                "protect-camera" => {
                    let mut frame=slint::SharedPixelBuffer::<slint::Rgb8Pixel>::new(480,270);
                    let colors=[[190,190,190],[190,190,0],[0,190,190],[0,190,0],[190,0,190],[190,0,0],[0,0,190],[20,20,20]];
                    for (i,pixel) in frame.make_mut_slice().iter_mut().enumerate(){let c=colors[(i%480)/60];*pixel=slint::Rgb8Pixel::new(c[0],c[1],c[2]);}
                    a.set_camera_image(slint::Image::from_rgb8(frame));
                    a.set_camera_title("Camera preview fixture".into());
                    a.set_camera_message("TEST PATTERN · No camera connection".into());
                    a.set_camera_shown(true);a.invoke_focus_camera();
                }
                "thermostat-feedback" => {
                    d.open_room(a,0);
                    a.set_feedback_enabled(true);
                    a.set_thermostat_feedback_name("Living room climate".into());
                    a.set_thermostat_feedback_value("21.5°C".into());
                    a.set_thermostat_feedback_shown(true);
                }
                "thermostat" | "thermostat-modes" | "thermostat-range" | "thermostat-unavailable" => {
                    d.open_room(a, 0);
                    a.set_thermostat_title("Living room climate".into());
                    a.set_thermostat_current("20.5°C".into());
                    a.set_thermostat_target(if name == "thermostat-range" { "20°C – 24°C" } else { "21°C" }.into());
                    a.set_thermostat_status("Heating".into());
                    a.set_thermostat_detail(if name == "thermostat-range" { "Volume adjusts both setpoints.\nOK chooses the thermostat mode." } else { "Volume adjusts the target.\nOK chooses the thermostat mode." }.into());
                    a.set_thermostat_mode(if name == "thermostat-range" { "Heat / cool" } else { "Heat" }.into());
                    a.set_thermostat_modes(ModelRc::new(VecModel::from(["Off", "Heat", "Cool", "Heat / cool"].map(Into::into).to_vec())));
                    a.set_thermostat_range(name == "thermostat-range");
                    a.set_thermostat_adjustable(name != "thermostat-unavailable");
                    a.set_thermostat_pending(false);
                    if name == "thermostat-unavailable" {
                        a.set_thermostat_current("—".into()); a.set_thermostat_target("—".into());
                        a.set_thermostat_status("Unavailable".into()); a.set_thermostat_modes(ModelRc::default());
                        a.set_thermostat_detail("Home Assistant reports this thermostat is unavailable.".into());
                    }
                    a.set_thermostat_shown(true);
                    a.set_thermostat_modes_shown(name == "thermostat-modes");
                    a.invoke_focus_thermostat();
                }
                "home" => {}
                "room" => d.open_room(a, 0),
                "brightness" => {
                    d.open_room(a, 0);
                    a.invoke_light_brightness(0, 5);
                }
                "scenes" => {
                    d.open_room(a, 0);
                    d.scene(a, 2);
                }
                "android-tv" | "android-apps" | "android-live" | "android-no-duration" | "android-no-art" | "android-paused" | "android-idle" | "webos" | "webos-inputs" | "apple-tv"
                | "apple-apps" | "infrared" | "infrared-commands" => {
                    d.open_room(a, 0);
                    let infrared = name.starts_with("infrared");
                    a.set_tv_ir(infrared);
                    let android = name.starts_with("android");
                    let apple = name.starts_with("apple");
                    a.set_tv_android(android);
                    a.set_tv_apple(apple);
                    a.set_tv_panel(0);
                    a.set_tv_media_active(android && name != "android-idle");
                    a.set_tv_media_title("Tears of Steel".into());
                    a.set_tv_media_subtitle("2012 · Science fiction · Blender Foundation".into());
                    a.set_tv_media_state(if name == "android-paused" { "Paused" } else { "Playing" }.into());
                    a.set_tv_media_app("YouTube".into());
                    a.set_tv_media_art(png(include_bytes!("../../docs/mockups/kodi-activity/assets/fanart.jpg")));
                    a.set_tv_media_has_art(android && name != "android-no-art");
                    a.set_tv_media_position("4:12".into());
                    a.set_tv_media_duration("12:14".into());
                    a.set_tv_media_progress(252.0 / 734.0);
                    a.set_tv_media_has_duration(android && name != "android-no-duration");
                    a.set_tv_media_live(name == "android-live");
                    a.set_tv_title(
                        if infrared {"Living room IR TV"} else if android {
                            "Living room Android TV"
                        } else if apple {
                            "Living room Apple TV"
                        } else {
                            "Living room LG TV"
                        }
                        .into(),
                    );
                    a.set_tv_source(
                        if infrared {"Infrared controls"} else if android {
                            "Android / Google TV"
                        } else if apple {
                            "Apple TV"
                        } else {
                            "HDMI 1"
                        }
                        .into(),
                    );
                    a.set_tv_status(
                        if infrared {"Infrared · No device feedback"} else if apple {
                            "Connected · Companion"
                        } else if android {
                            "Connected"
                        } else {
                            "TV on · Volume 12"
                        }
                        .into(),
                    );
                    a.set_tv_sound("TV speakers".into());
                    a.set_tv_picture("Cinema".into());
                    a.set_tv_shown(true);
                    a.invoke_focus_tv();
                    if name == "android-apps" || name == "apple-apps" {
                        a.invoke_tv_action("apps".into());
                    }
                    if name == "infrared-commands" {a.invoke_tv_action("commands".into());}
                    if name == "webos-inputs" {
                        a.invoke_tv_action("inputs".into());
                    }
                }
                "kodi" | "chapters" => {
                    d.open_room(a, 0);
                    d.player(a);
                    if name == "chapters" {
                        a.invoke_player_action("chapters".into(), 0.);
                    }
                }
                _ => panic!("Unknown documentation fixture"),
            }
        });
        // Allow deferred production callbacks to populate the card/sheet first.
        later(|d, _| d.toast.stop());
    });
}
