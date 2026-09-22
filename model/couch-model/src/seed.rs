//! The house a never-configured remote shows.
//!
//! This is `ui/couch-gui/src/main.rs`'s hard-coded seed, re-expressed in the
//! real model. Keeping it here rather than in the daemon means the round trip
//! - GUI mock-up to config model and back to the same rendered strings - is
//! covered by this crate's own tests.
//!
//! The per-area room, scene and activity counts are the mock-up's, exactly -
//! 3/1/5/0 activities and 5/3/3/4 scenes - because those are what the hub
//! draws and a seed that renders differently from the design is not a seed of
//! the design. The GUI's live titles ("Paused - Andrei Rublev") are not here:
//! those are what is playing, and this file only says what could play.
//!
//! One place it deliberately does not match: the GUI gives Study an empty
//! `detail` despite two devices, which was mock noise rather than a rule.

use alloc::string::ToString;
use alloc::vec;
use alloc::vec::Vec;

use crate::{
    Action, Activity, ActivityKind, Area, Config, Device, DeviceKind, Icon, Id, Integration, Room,
    Scene, SCHEMA_VERSION,
};

fn dev(id: &str, name: &str, kind: DeviceKind) -> Device {
    Device::new(Id::new(id), name, kind)
}

fn room(id: &str, name: &str, icon: Icon, devices: Vec<Device>) -> Room {
    Room {
        id: Id::new(id),
        name: name.to_string(),
        icon: Some(icon),
        devices,
    }
}

fn area(id: &str, name: &str, rooms: &[&str], scenes: &[&str], activities: &[&str]) -> Area {
    Area {
        id: Id::new(id),
        name: name.to_string(),
        icon: None,
        rooms: rooms.iter().map(|r| Id::new(*r)).collect(),
        scenes: scenes.iter().map(|s| Id::new(*s)).collect(),
        activities: activities.iter().map(|a| Id::new(*a)).collect(),
        shortcuts: Vec::new(),
    }
}

fn scene(id: &str, name: &str, steps: Vec<Action>) -> Scene {
    Scene {
        id: Id::new(id),
        name: name.to_string(),
        icon: None,
        steps,
        hue: None,
        resource: None,
        rooms: Vec::new(),
    }
}

pub(crate) fn seed() -> Config {
    let rooms = vec![
        room(
            "living-room",
            "Living room",
            Icon::Sofa,
            vec![
                dev("living-kodi", "Kodi", DeviceKind::MediaPlayer).with_integration(
                    Integration::Kodi {
                        host: "kodi.local".to_string(),
                        port: 9090,
                    },
                ),
                dev("living-hue", "Hue", DeviceKind::Light).with_integration(
                    Integration::HomeAssistant {
                        entity_id: "light.living_room".to_string(),
                    },
                ),
                dev("living-tv", "LG C3", DeviceKind::Tv).with_integration(Integration::Ir {
                    codeset: "lg-tv".to_string(),
                }),
                dev("living-soundbar", "Soundbar", DeviceKind::Speaker),
                dev("living-lamp", "Corner lamp", DeviceKind::Light),
            ],
        ),
        room(
            "kitchen",
            "Kitchen",
            Icon::CookingPot,
            vec![
                dev("kitchen-sonos", "Sonos Move", DeviceKind::Speaker).with_integration(
                    Integration::HomeAssistant {
                        entity_id: "media_player.kitchen".to_string(),
                    },
                ),
                dev("kitchen-hue", "Hue", DeviceKind::Light),
            ],
        ),
        room(
            "bedroom",
            "Bedroom",
            Icon::Bed,
            vec![
                dev("bedroom-hue", "Hue", DeviceKind::Light),
                dev("bedroom-sonos", "Sonos One", DeviceKind::Speaker),
                dev("bedroom-blind", "Blind", DeviceKind::Blind),
            ],
        ),
        room(
            "study",
            "Study",
            Icon::BookOpen,
            vec![
                dev("study-hue", "Hue", DeviceKind::Light),
                dev("study-sonos", "Sonos One", DeviceKind::Speaker),
            ],
        ),
        room(
            "hallway",
            "Hallway",
            Icon::DoorOpen,
            vec![
                dev("hallway-hue", "Hue", DeviceKind::Light),
                dev("hallway-cam", "Front door", DeviceKind::Camera),
            ],
        ),
        room(
            "loft",
            "Loft",
            Icon::Lamp,
            vec![dev("loft-hue", "Hue", DeviceKind::Light)],
        ),
        room(
            "garden",
            "Garden",
            Icon::Trees,
            vec![
                dev("garden-hue", "Hue", DeviceKind::Light),
                dev("garden-cam-front", "Front camera", DeviceKind::Camera),
                dev("garden-cam-back", "Back camera", DeviceKind::Camera),
            ],
        ),
        room(
            "garage",
            "Garage",
            Icon::Car,
            vec![
                dev("garage-hue", "Hue", DeviceKind::Light),
                dev("garage-door", "Door", DeviceKind::Switch),
            ],
        ),
        room(
            "porch",
            "Porch",
            Icon::DoorOpen,
            vec![dev("porch-hue", "Hue", DeviceKind::Light)],
        ),
    ];

    let scenes = vec![
        scene(
            "movie-night",
            "Movie night",
            vec![
                Action::new(Id::new("living-tv"), "on"),
                Action::new(Id::new("living-tv"), "input:hdmi2"),
                Action::new(Id::new("living-kodi"), "on"),
                Action::new(Id::new("living-hue"), "off"),
            ],
        ),
        scene(
            "good-morning",
            "Good morning",
            vec![
                Action::new(Id::new("bedroom-blind"), "on"),
                Action::new(Id::new("kitchen-hue"), "on"),
            ],
        ),
        scene(
            "away",
            "Away",
            vec![
                Action::new(Id::new("living-hue"), "off"),
                Action::new(Id::new("kitchen-hue"), "off"),
                Action::new(Id::new("bedroom-hue"), "off"),
            ],
        ),
        scene(
            "dinner",
            "Dinner",
            vec![Action::new(Id::new("kitchen-hue"), "on")],
        ),
        scene(
            "all-off",
            "All off",
            vec![
                Action::new(Id::new("living-hue"), "off"),
                Action::new(Id::new("living-tv"), "off"),
                Action::new(Id::new("kitchen-hue"), "off"),
                Action::new(Id::new("bedroom-hue"), "off"),
                Action::new(Id::new("study-hue"), "off"),
            ],
        ),
        scene(
            "bedtime",
            "Bedtime",
            vec![Action::new(Id::new("bedroom-hue"), "off")],
        ),
        scene(
            "wake-up",
            "Wake up",
            vec![Action::new(Id::new("bedroom-blind"), "on")],
        ),
        scene(
            "upstairs-off",
            "Upstairs off",
            vec![Action::new(Id::new("bedroom-hue"), "off")],
        ),
        scene(
            "cooking",
            "Cooking",
            vec![Action::new(Id::new("kitchen-hue"), "on")],
        ),
        scene(
            "downstairs-off",
            "Downstairs off",
            vec![Action::new(Id::new("living-hue"), "off")],
        ),
        scene(
            "evening",
            "Evening",
            vec![Action::new(Id::new("garden-hue"), "on")],
        ),
        scene(
            "security-on",
            "Security on",
            vec![Action::new(Id::new("garden-cam-front"), "on")],
        ),
        scene("watering", "Watering", vec![]),
        scene(
            "outside-off",
            "Outside off",
            vec![Action::new(Id::new("garden-hue"), "off")],
        ),
    ];

    let activities = vec![
        Activity {
            setup: Default::default(),
            id: Id::new("watch-tv"),
            name: "Watch TV".to_string(),
            kind: ActivityKind::Video,
            room: Id::new("living-room"),
            source: Some(Id::new("living-kodi")),
            buttons: Vec::new(),
            steps: vec![
                Action::new(Id::new("living-tv"), "on"),
                Action::new(Id::new("living-tv"), "input:hdmi2"),
                Action::new(Id::new("living-kodi"), "on"),
            ],
        },
        Activity {
            setup: Default::default(),
            id: Id::new("kitchen-radio"),
            name: "Kitchen radio".to_string(),
            kind: ActivityKind::Audio,
            room: Id::new("kitchen"),
            source: Some(Id::new("kitchen-sonos")),
            buttons: Vec::new(),
            steps: vec![Action::new(Id::new("kitchen-sonos"), "play")],
        },
        Activity {
            setup: Default::default(),
            id: Id::new("study-music"),
            name: "Study music".to_string(),
            kind: ActivityKind::Audio,
            room: Id::new("study"),
            source: Some(Id::new("study-sonos")),
            buttons: Vec::new(),
            steps: vec![Action::new(Id::new("study-sonos"), "play")],
        },
        Activity {
            setup: Default::default(),
            id: Id::new("white-noise"),
            name: "White noise".to_string(),
            kind: ActivityKind::Audio,
            room: Id::new("bedroom"),
            source: Some(Id::new("bedroom-sonos")),
            buttons: Vec::new(),
            steps: vec![Action::new(Id::new("bedroom-sonos"), "play")],
        },
        Activity {
            setup: Default::default(),
            id: Id::new("front-door"),
            name: "Front door".to_string(),
            kind: ActivityKind::Video,
            room: Id::new("hallway"),
            source: Some(Id::new("hallway-cam")),
            buttons: Vec::new(),
            steps: vec![],
        },
        // The mock's DOWNSTAIRS strip has three things running in the living
        // room at once, which only works if the room has more than one thing
        // to run. These are the other two, on devices it already has.
        Activity {
            setup: Default::default(),
            id: Id::new("living-podcast"),
            name: "Podcast".to_string(),
            kind: ActivityKind::Audio,
            room: Id::new("living-room"),
            source: Some(Id::new("living-soundbar")),
            buttons: Vec::new(),
            steps: vec![Action::new(Id::new("living-soundbar"), "on")],
        },
        Activity {
            setup: Default::default(),
            id: Id::new("living-sport"),
            name: "Sport".to_string(),
            kind: ActivityKind::Video,
            room: Id::new("living-room"),
            source: Some(Id::new("living-tv")),
            buttons: Vec::new(),
            steps: vec![
                Action::new(Id::new("living-tv"), "on"),
                Action::new(Id::new("living-tv"), "input:hdmi1"),
            ],
        },
    ];

    // The strips read as the mock-up's do: WHOLE HOME is a digest of the house
    // rather than everything in it, DOWNSTAIRS is the busy page, and OUTSIDE
    // has nothing to play.
    let areas = vec![
        area(
            "whole-home",
            "WHOLE HOME",
            &["living-room", "kitchen", "bedroom", "study"],
            &["movie-night", "good-morning", "away", "dinner", "all-off"],
            &["kitchen-radio", "watch-tv", "study-music"],
        ),
        area(
            "upstairs",
            "UPSTAIRS",
            &["bedroom", "study", "loft"],
            &["bedtime", "wake-up", "upstairs-off"],
            &["white-noise"],
        ),
        area(
            "downstairs",
            "DOWNSTAIRS",
            &["living-room", "kitchen", "hallway"],
            &["movie-night", "cooking", "downstairs-off"],
            &[
                "watch-tv",
                "kitchen-radio",
                "front-door",
                "living-podcast",
                "living-sport",
            ],
        ),
        area(
            "outside",
            "OUTSIDE",
            &["garden", "garage", "porch"],
            &["evening", "security-on", "watering", "outside-off"],
            &[],
        ),
    ];

    Config {
        schema_version: SCHEMA_VERSION,
        revision: 0,
        appearance: crate::Appearance::default(),
        remote: crate::RemoteSettings::default(),
        connections: Vec::new(),
        denon_migrations: Default::default(),
        app_shortcuts: Default::default(),
        areas,
        rooms,
        scenes,
        activities,
    }
}
