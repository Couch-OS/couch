//! The home configuration a Couch remote is pointed at.
//!
//! One definition, three consumers: `couch-confd` reads and writes it on the
//! device, `couch-web` edits it in a browser, and `couch-gui` will eventually
//! render it. Sharing the crate is the whole point - a field renamed here is a
//! compile error on both sides rather than a JSON key that silently stops
//! matching.
//!
//! # Shape
//!
//! Rooms, scenes and activities are flat lists with stable ids; an [`Area`]
//! holds *references* to all three rather than owning them. That is not
//! decoration. The GUI's seed data has "Bedroom" in both WHOLE HOME and

//! UPSTAIRS, and "Movie night" in both WHOLE HOME and DOWNSTAIRS: a room is a
//! place with devices in it, and an area is a named view over a set of places.
//! Nesting them would mean editing the same room's device list twice and
//! watching the two copies drift.
//!
//! # Configuration, not state
//!
//! Nothing here says whether a light is on. The GUI's `RoomRow` carries
//! `active-count`, `idle` and `offline`, and its `LiveActivity` is whatever is
//! playing right now; all of that is runtime state a hub daemon will supply,
//! and none of it belongs in a file a human edits. What this crate does carry
//! is the *configured* [`Activity`] - "Watch TV in the living room", a name, a
//! room and the devices it brings up. A live activity is one of these that is
//! currently running, and the strip an area draws is the subset of them that
//! area lists (see [`Config::activities_in_area`]).
//!
//! An area lists its activities rather than deriving them from its rooms,
//! because which of them are worth a strip is a judgement the derivation gets
//! wrong: the GUI's WHOLE HOME shows three where its rooms host five, and
//! DOWNSTAIRS shows five in three rooms. Both readings cannot follow from room
//! membership, so the list is configuration - the same call an area already
//! makes about scenes.
//!
//! Likewise the GUI's `devices: "5 devices"` and `detail: "Kodi, Hue, LG C3"`
//! are rendered strings. They are stored here as the device list they were
//! rendered from, and reproduced by [`Room::device_summary`] and
//! [`Room::device_detail`].

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;
#[cfg(feature = "std")]
extern crate std;

use alloc::string::String;
use alloc::vec::Vec;

use serde::{Deserialize, Serialize};

pub mod buttons;
pub mod commands;
mod shortcuts;
pub use shortcuts::{Shortcut, ShortcutAction, SHORTCUT_BUTTONS};
mod app_shortcuts;
pub use app_shortcuts::{AppShortcut, valid_app_url};
pub mod activity_setup;
pub use activity_setup::{ActivityPage, ActivitySetup, ActivityWidget, SequenceStep};
mod device;
mod connection;
mod appearance;
mod remote;
pub use remote::RemoteSettings;
pub use appearance::Appearance;
pub use connection::{Connection, Provider};
mod icon;
mod id;
mod seed;
mod validate;

pub use device::{
    Action, Device, DeviceBluetooth, DeviceIr, DeviceKind, Integration, Transport, ALL_DEVICE_KINDS,
    ALL_TRANSPORTS,
};
mod transport;
pub use icon::{Icon, ALL_ICONS};
pub use id::Id;
pub use validate::{Problem, ValidationError};

/// Ids are all the same type; the aliases only say what a field points at.
///
/// Distinct newtypes per entity would catch a room id passed where a scene id
/// belongs, but every one of them would need its own `Serialize`, `Display`,
/// `From<&str>` and slugging, and the API layer converts everything through
/// path strings anyway - where the type is gone regardless.
pub type AreaId = Id;
pub type RoomId = Id;
pub type SceneId = Id;
pub type DeviceId = Id;
pub type ActivityId = Id;

/// Bumped whenever a change to these types cannot be read by an older build.
///
/// Adding an optional field with `#[serde(default)]` is not such a change and
/// must not bump it; removing or renaming one is. `couch-confd` refuses to load
/// a file from the future rather than silently dropping what it cannot parse.
pub const SCHEMA_VERSION: u32 = 1;

/// The whole configuration, as it is stored on disk.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Config {
    pub schema_version: u32,
    /// Incremented by the daemon on every accepted write.
    ///
    /// Two phones open on the same page is the realistic collision here, and a
    /// counter the client can echo back as `If-Match` turns a silent overwrite
    /// into a 409. It is deliberately not a hash: it has to be cheap to compare
    /// and readable in a `curl` transcript.
    #[serde(default)]
    pub revision: u64,
    #[serde(default)]
    pub appearance: Appearance,
    #[serde(default)]
    pub remote: RemoteSettings,
    #[serde(default)]
    pub connections: Vec<Connection>,
    #[serde(default, skip_serializing_if = "alloc::collections::BTreeMap::is_empty")]
    pub app_shortcuts: alloc::collections::BTreeMap<Id, Vec<AppShortcut>>,
    #[serde(default)]
    pub areas: Vec<Area>,
    #[serde(default)]
    pub rooms: Vec<Room>,
    #[serde(default)]
    pub scenes: Vec<Scene>,
    #[serde(default)]
    pub activities: Vec<Activity>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            schema_version: SCHEMA_VERSION,
            revision: 0,
            appearance: Appearance::default(),
            remote: RemoteSettings::default(),
            connections: Vec::new(),
            app_shortcuts: Default::default(),
            areas: Vec::new(),
            rooms: Vec::new(),
            scenes: Vec::new(),
            activities: Vec::new(),
        }
    }
}

/// A named view over a set of rooms: the hub's left/right axis.
///
/// The three lists are the three rows of one page on the remote, top to
/// bottom: the activity strip, the rooms, the scenes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Area {
    pub id: AreaId,
    pub name: String,
    #[serde(default)]
    pub icon: Option<Icon>,
    /// Ordered, and the order is what the hub scrolls through.
    #[serde(default)]
    pub rooms: Vec<RoomId>,
    #[serde(default)]
    pub scenes: Vec<SceneId>,
    /// The activities this page's strip draws when they are running.
    ///
    /// Normally activities in this area's rooms, but the model does not force
    /// that: an area is a view, and "everything happening downstairs" is a
    /// reasonable thing to put on the whole-home page.
    #[serde(default)]
    pub activities: Vec<ActivityId>,
    /// What the shortcut and color keys reach while this page is showing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shortcuts: Vec<Shortcut>,
}

/// A place with devices in it: the hub's up/down axis.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Room {
    pub id: RoomId,
    pub name: String,
    #[serde(default)]
    pub icon: Option<Icon>,
    #[serde(default)]
    pub devices: Vec<Device>,
}

/// A bridge scene reached through a saved Hue connection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HueScene {
    pub connection_id: Id,
    pub scene_id: String,
}

/// One press that puts several devices into a known state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Scene {
    pub id: SceneId,
    pub name: String,
    #[serde(default)]
    pub icon: Option<Icon>,
    #[serde(default)]
    pub steps: Vec<Action>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hue: Option<HueScene>,
    #[serde(default)]
    pub rooms: Vec<RoomId>,
}

/// A thing you sit down to do, in one room, with a source device.
///
/// This is the Harmony sense of the word rather than Home Assistant's: "Watch
/// TV" names both the goal and the set of devices that have to come up for it.
/// The remote shows one of these in its activity strip while it is running.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Activity {
    #[serde(default)]
    pub setup: ActivitySetup,
    pub id: ActivityId,
    pub name: String,
    #[serde(default)]
    pub kind: ActivityKind,
    pub room: RoomId,
    /// The device the transport keys drive while this is running.
    #[serde(default)]
    pub source: Option<DeviceId>,
    /// Overrides apply only while this activity screen is open.
    #[serde(default)]
    pub buttons: Vec<buttons::Binding>,
    #[serde(default)]
    pub steps: Vec<Action>,
}

/// Which glyph the activity strip draws, and which keys make sense.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ActivityKind {
    /// Bars, in the GUI's `LiveActivity { kind: 0 }`.
    #[default]
    Audio,
    /// A play triangle, `kind: 1`.
    Video,
}

impl ActivityKind {
    /// The integer the GUI's `LiveActivity.kind` expects.
    pub fn glyph_index(self) -> i32 {
        match self {
            ActivityKind::Audio => 0,
            ActivityKind::Video => 1,
        }
    }
}

impl Room {
    /// `"5 devices"`, the GUI's `RoomRow.devices`.
    pub fn device_summary(&self) -> String {
        let n = self.devices.len();
        let mut s = String::new();
        push_u64(&mut s, n as u64);
        s.push_str(if n == 1 { " device" } else { " devices" });
        s
    }

    /// `"Kodi, Hue, LG C3"`, the GUI's `RoomRow.detail`.
    ///
    /// Capped at three because the row elides past roughly that width at
    /// 19px, and a truncated fourth name reads as a bug rather than a list.
    pub fn device_detail(&self) -> String {
        let mut s = String::new();
        for d in self.devices.iter().take(3) {
            if !s.is_empty() {
                s.push_str(", ");
            }
            s.push_str(&d.name);
        }
        s
    }

    pub fn device(&self, id: &DeviceId) -> Option<&Device> {
        self.devices.iter().find(|d| &d.id == id)
    }

    pub fn device_mut(&mut self, id: &DeviceId) -> Option<&mut Device> {
        self.devices.iter_mut().find(|d| &d.id == id)
    }

    /// The room's own icon, or the one its most prominent device suggests.
    pub fn effective_icon(&self) -> Icon {
        self.icon
            .or_else(|| self.devices.first().map(|d| d.kind.default_icon()))
            .unwrap_or(Icon::House)
    }
}

impl Config {
    /// A believable house, for a device that has never been configured.
    ///
    /// This is the GUI's hard-coded seed re-expressed in the real model, which
    /// is also the proof that the model can express it.
    pub fn seed() -> Config {
        seed::seed()
    }

    pub fn area(&self, id: &AreaId) -> Option<&Area> {
        self.areas.iter().find(|a| &a.id == id)
    }
    pub fn area_mut(&mut self, id: &AreaId) -> Option<&mut Area> {
        self.areas.iter_mut().find(|a| &a.id == id)
    }
    pub fn room(&self, id: &RoomId) -> Option<&Room> {
        self.rooms.iter().find(|r| &r.id == id)
    }
    pub fn room_mut(&mut self, id: &RoomId) -> Option<&mut Room> {
        self.rooms.iter_mut().find(|r| &r.id == id)
    }
    pub fn scene(&self, id: &SceneId) -> Option<&Scene> {
        self.scenes.iter().find(|s| &s.id == id)
    }
    pub fn scene_mut(&mut self, id: &SceneId) -> Option<&mut Scene> {
        self.scenes.iter_mut().find(|s| &s.id == id)
    }
    pub fn activity(&self, id: &ActivityId) -> Option<&Activity> {
        self.activities.iter().find(|a| &a.id == id)
    }
    pub fn activity_mut(&mut self, id: &ActivityId) -> Option<&mut Activity> {
        self.activities.iter_mut().find(|a| &a.id == id)
    }

    /// The rooms of an area, in the area's own order, skipping dangling ids.
    ///
    /// Dangling ids should not exist - [`Config::validate`] rejects them and
    /// [`Config::remove_room`] prevents them - but a hand-edited file can still
    /// contain one, and a missing room is not a reason to show nothing.
    pub fn rooms_in_area<'a>(&'a self, area: &'a Area) -> impl Iterator<Item = &'a Room> + 'a {
        area.rooms.iter().filter_map(move |id| self.room(id))
    }

    pub fn scenes_in_area<'a>(&'a self, area: &'a Area) -> impl Iterator<Item = &'a Scene> + 'a {
        area.scenes.iter().filter_map(move |id| self.scene(id))
    }

    /// The activities in an area's strip, in the area's own order.
    pub fn activities_in_area<'a>(
        &'a self,
        area: &'a Area,
    ) -> impl Iterator<Item = &'a Activity> + 'a {
        area.activities.iter().filter_map(move |id| self.activity(id))
    }

    /// The activities that *happen* in an area, whether it lists them or not.
    ///
    /// What an editor should offer when someone asks to put an activity on a
    /// page: the ones whose room is already there.
    pub fn activities_hosted_by<'a>(
        &'a self,
        area: &'a Area,
    ) -> impl Iterator<Item = &'a Activity> + 'a {
        self.activities
            .iter()
            .filter(move |act| area.rooms.contains(&act.room))
    }


    /// Every device in the home, with the room it lives in.
    pub fn devices(&self) -> impl Iterator<Item = (&Room, &Device)> + '_ {
        self.rooms
            .iter()
            .flat_map(|r| r.devices.iter().map(move |d| (r, d)))
    }

    /// Remove a room, and every reference to it.
    ///
    /// Referential integrity is the price of sharing rooms between areas, and
    /// it is cheap enough to pay here rather than making every reader defensive.
    pub fn remove_room(&mut self, id: &RoomId) -> Option<Room> {
        let at = self.rooms.iter().position(|r| &r.id == id)?;
        let room = self.rooms.remove(at);
        for scene in &mut self.scenes { scene.rooms.retain(|r|r!=id); }
        for area in &mut self.areas {
            area.rooms.retain(|r| r != id);
        }
        // Activities are anchored to a room; without one they have nowhere to
        // run, so they go with it rather than becoming dangling.
        let orphaned: Vec<ActivityId> = self
            .activities
            .iter()
            .filter(|a| &a.room == id)
            .map(|a| a.id.clone())
            .collect();
        self.activities.retain(|a| &a.room != id);
        for area in &mut self.areas {
            area.activities.retain(|a| !orphaned.contains(a));
        }
        for activity in &orphaned {
            self.forget_activity_shortcuts(activity);
        }
        for device in &room.devices {
            self.forget_device(&device.id);
        }
        Some(room)
    }


    pub fn remove_area(&mut self, id: &AreaId) -> Option<Area> {
        let at = self.areas.iter().position(|a| &a.id == id)?;
        let area = self.areas.remove(at);
        self.forget_area_shortcuts(id);
        Some(area)
    }

    pub fn remove_scene(&mut self, id: &SceneId) -> Option<Scene> {
        let at = self.scenes.iter().position(|s| &s.id == id)?;
        let scene = self.scenes.remove(at);
        for area in &mut self.areas {
            area.scenes.retain(|s| s != id);
        }
        Some(scene)
    }

    pub fn remove_activity(&mut self, id: &ActivityId) -> Option<Activity> {
        let at = self.activities.iter().position(|a| &a.id == id)?;
        let activity = self.activities.remove(at);
        for area in &mut self.areas {
            area.activities.retain(|a| a != id);
        }
        self.forget_activity_shortcuts(id);
        Some(activity)
    }


    /// Remove a device from a room, and every scene step and activity that
    /// pointed at it.
    pub fn remove_device(&mut self, room: &RoomId, device: &DeviceId) -> Option<Device> {
        let room = self.room_mut(room)?;
        let at = room.devices.iter().position(|d| &d.id == device)?;
        let removed = room.devices.remove(at);
        self.forget_device(device);
        Some(removed)
    }

    fn forget_device(&mut self, device: &DeviceId) {
        self.forget_device_shortcuts(device);
        for scene in &mut self.scenes {
            scene.steps.retain(|s| &s.device != device);
        }
        for act in &mut self.activities {
            act.setup.forget_device(device);
            act.steps.retain(|s| &s.device != device);
            for binding in &mut act.buttons {
                if binding.action.as_ref().is_some_and(|a| &a.device == device) { binding.action = None; }
            }
            if act.source.as_ref() == Some(device) {
                act.source = None;
            }
        }
    }

    /// An id derived from `name` that no room is using yet.
    pub fn fresh_room_id(&self, name: &str) -> RoomId {
        Id::unique(name, self.rooms.iter().map(|r| &r.id))
    }
    pub fn fresh_area_id(&self, name: &str) -> AreaId {
        Id::unique(name, self.areas.iter().map(|a| &a.id))
    }
    pub fn fresh_scene_id(&self, name: &str) -> SceneId {
        Id::unique(name, self.scenes.iter().map(|s| &s.id))
    }
    pub fn fresh_activity_id(&self, name: &str) -> ActivityId {
        Id::unique(name, self.activities.iter().map(|a| &a.id))
    }
    /// Device ids are unique across the whole home, not just their room: scene
    /// steps reference a device without naming its room.
    pub fn fresh_device_id(&self, name: &str) -> DeviceId {
        let mut taken = Vec::new();
        for room in &self.rooms {
            for d in &room.devices {
                taken.push(&d.id);
            }
        }
        Id::unique(name, taken.into_iter())
    }
}

/// `alloc` has no `format!` without pulling the machinery in; this is the only
/// number this crate ever renders.
fn push_u64(out: &mut String, mut n: u64) {
    if n == 0 {
        out.push('0');
        return;
    }
    let mut digits = [0u8; 20];
    let mut i = 0;
    while n > 0 {
        digits[i] = b'0' + (n % 10) as u8;
        n /= 10;
        i += 1;
    }
    while i > 0 {
        i -= 1;
        out.push(digits[i] as char);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    #[test]
    fn seed_is_valid() {
        Config::seed().validate().expect("seed must validate");
    }

    #[test]
    fn display_strings_match_the_gui_seed() {
        let cfg = Config::seed();
        let living = cfg.room(&Id::new("living-room")).unwrap();
        assert_eq!(living.device_summary(), "5 devices");
        assert_eq!(living.device_detail(), "Kodi, Hue, LG C3");

        let study = cfg.room(&Id::new("study")).unwrap();
        assert_eq!(study.device_summary(), "2 devices");
    }

    #[test]
    fn removing_a_room_leaves_nothing_dangling() {
        let mut cfg = Config::seed();
        let kitchen = Id::new("kitchen");
        let radio = Id::new("kitchen-radio");
        assert!(cfg.areas.iter().any(|a| a.activities.contains(&radio)));
        assert!(cfg.remove_room(&kitchen).is_some());
        assert!(cfg.areas.iter().all(|a| !a.rooms.contains(&kitchen)));
        assert!(cfg.activities.iter().all(|a| a.room != kitchen));
        // The activity went with the room, so no area may still point at it.
        assert!(cfg.areas.iter().all(|a| !a.activities.contains(&radio)));
        cfg.validate().expect("still valid after a removal");
    }

    #[test]
    fn removing_an_activity_detaches_it_everywhere() {
        let mut cfg = Config::seed();
        let watch_tv = Id::new("watch-tv");
        assert!(cfg.remove_activity(&watch_tv).is_some());
        assert!(cfg.areas.iter().all(|a| !a.activities.contains(&watch_tv)));
        cfg.validate().expect("still valid after a removal");
    }

    /// The strip counts `ui/couch-gui/src/main.rs` draws, area by area.
    #[test]
    fn area_strips_match_the_gui_seed() {
        let cfg = Config::seed();
        let counts: Vec<usize> = cfg
            .areas
            .iter()
            .map(|a| cfg.activities_in_area(a).count())
            .collect();
        assert_eq!(counts, [3, 1, 5, 0]);

        let scenes: Vec<usize> = cfg
            .areas
            .iter()
            .map(|a| cfg.scenes_in_area(a).count())
            .collect();
        assert_eq!(scenes, [5, 3, 3, 4]);
    }


    #[test]
    fn areas_share_rooms() {
        let cfg = Config::seed();
        let bedroom = Id::new("bedroom");
        let n = cfg.areas.iter().filter(|a| a.rooms.contains(&bedroom)).count();
        assert!(n > 1, "the seed's bedroom belongs to more than one area");
    }

    #[test]
    fn fresh_ids_do_not_collide() {
        let cfg = Config::seed();
        assert_eq!(cfg.fresh_room_id("Kitchen").as_str(), "kitchen-2");
        assert_eq!(cfg.fresh_room_id("Pantry").as_str(), "pantry");
    }

    #[test]
    fn summary_pluralises() {
        let mut room = Room {
            id: Id::new("x"),
            name: "X".to_string(),
            icon: None,
            devices: Vec::new(),
        };
        assert_eq!(room.device_summary(), "0 devices");
        room.devices.push(Device::new(Id::new("d"), "D", DeviceKind::Light));
        assert_eq!(room.device_summary(), "1 device");
    }
}
