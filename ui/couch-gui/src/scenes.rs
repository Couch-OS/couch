//! Scene recall and room-scoped channel navigation; network work stays off the GUI thread.
use crate::App;
use couch_model::{Config, Id, Provider};
use couch_plugin::{Request, Response};
use slint::ComponentHandle;
use std::{
    cell::RefCell,
    collections::{HashMap, VecDeque},
    rc::Rc,
    sync::mpsc,
    time::{Duration, Instant},
};

enum Input {
    Recall(Id),
    Cycle(Id, i32),
}
pub struct Controller {
    input: Rc<RefCell<VecDeque<Input>>>,
    tx: mpsc::SyncSender<(u64, Id)>,
    rx: mpsc::Receiver<(u64, Result<(), String>)>,
    pending: Option<(u64, Id)>,
    busy: bool,
    sequence: u64,
    cursor: HashMap<Id, Id>,
    until: Option<Instant>,
}
fn config() -> Result<std::sync::Arc<Config>, String> {
    crate::connections::config().ok_or_else(|| "Cannot read scenes".into())
}

/// One request to the daemon's panel socket, for a scene that belongs to a
/// package. The tests put a closure here in place of the socket.
type Ask<'a> = &'a mut dyn FnMut(&str, Request) -> Result<Response, couch_plugin::Failure>;

fn plugin_scene(connection: &str, request: Request) -> Result<Response, couch_plugin::Failure> {
    crate::tv::plugin::ask_detailed(connection, request)
}

/// Recall one scene: a Hue scene through the bridge, a package's scene by
/// sending `on` to that child of its connection. Both appear side by side on
/// a room's Scenes button; which one this is was decided when it was saved.
fn recall(cfg: &Config, id: &Id, ask: Ask) -> Result<(), String> {
    let scene = cfg.scene(id).ok_or("Scene was removed")?;
    if let Some(resource) = &scene.resource {
        if !cfg
            .connection(&resource.connection_id)
            .is_some_and(|c| matches!(c.provider, Provider::Plugin { .. }))
        {
            return Err("The integration this scene belongs to was removed".into());
        }
        return match ask(
            resource.connection_id.as_str(),
            Request::command("on").at(resource.resource_id.as_str()),
        ) {
            // A scene has no state to report back; either answer means the
            // package took it.
            Ok(Response::Ok | Response::Status { .. }) => Ok(()),
            Ok(_) => Err("The integration returned an invalid response".into()),
            Err(failure) => Err(crate::tv::plugin::refusal(&failure)),
        };
    }
    if scene.hue.is_some() {
        Err("Needs the Philips Hue package".into())
    } else {
        Err("Device-step scenes are not supported yet".into())
    }
}

fn next_scene(ids: &[Id], current: Option<&Id>, delta: i32) -> Option<Id> {
    if ids.is_empty() {
        return None;
    }
    let index = match current.and_then(|id| ids.iter().position(|s| s == id)) {
        Some(i) => (i as i32 + delta.signum()).rem_euclid(ids.len() as i32) as usize,
        None if delta < 0 => ids.len() - 1,
        None => 0,
    };
    Some(ids[index].clone())
}
impl Controller {
    pub fn new(app: &App) -> Self {
        let input = Rc::new(RefCell::new(VecDeque::new()));
        let q = input.clone();
        let weak = app.as_weak();
        app.on_room_scene_step(move |delta| {
            if let Some(app) = weak.upgrade() {
                q.borrow_mut().push_back(Input::Cycle(
                    Id::new(app.get_light_room_id().as_str()),
                    delta,
                ));
            }
        });
        let (tx, requests) = mpsc::sync_channel::<(u64, Id)>(1);
        let (events, rx) = mpsc::channel();
        std::thread::spawn(move || {
            while let Ok((sequence, id)) = requests.recv() {
                let result = config().and_then(|cfg| recall(&cfg, &id, &mut plugin_scene));
                let _ = events.send((sequence, result));
            }
        });
        Self {
            input,
            tx,
            rx,
            pending: None,
            busy: false,
            sequence: 0,
            cursor: HashMap::new(),
            until: None,
        }
    }
    pub fn opener(&self) -> impl Fn(Id) + 'static {
        let input = self.input.clone();
        move |id| input.borrow_mut().push_back(Input::Recall(id))
    }
    pub fn dismiss_feedback(&mut self, app: &App) {
        // Commands may finish, but their feedback belongs to the page we left.
        self.sequence += 1;
        self.until = None;
        app.set_scene_feedback_shown(false);
    }
    fn feedback(&mut self, app: &App, name: &str, status: &str) {
        app.set_feedback_enabled(true);
        app.set_scene_feedback_name(name.into());
        app.set_scene_feedback_status(status.into());
        app.set_scene_feedback_shown(true);
        app.set_brightness_shown(false);
        self.until = Some(Instant::now() + Duration::from_millis(1500));
    }
    pub fn poll(&mut self, app: &App) {
        if self.until.is_some_and(|until| Instant::now() >= until) {
            app.set_scene_feedback_shown(false);
            self.until = None;
        }
        loop {
            let Some(input) = self.input.borrow_mut().pop_front() else {
                break;
            };
            let cfg = match config() {
                Ok(cfg) => cfg,
                Err(error) => {
                    self.feedback(app, &error, "Scene unavailable");
                    continue;
                }
            };
            let id = match input {
                Input::Recall(id) => Some(id),
                Input::Cycle(room, delta) => {
                    let ids: Vec<_> = cfg
                        .scenes
                        .iter()
                        .filter(|s| s.rooms.contains(&room))
                        .map(|s| s.id.clone())
                        .collect();
                    let id = next_scene(&ids, self.cursor.get(&room), delta);
                    if id.is_none() {
                        self.feedback(app, "Add scenes in the web UI", "No scenes in this room");
                    }
                    id
                }
            };
            let Some(id) = id else { continue };
            let Some(scene) = cfg.scene(&id) else {
                self.feedback(app, "Scene was removed", "Scene unavailable");
                continue;
            };
            for room in &scene.rooms {
                self.cursor.insert(room.clone(), id.clone());
            }
            self.sequence += 1;
            // Keep only the latest unsent choice while a previous recall finishes.
            self.pending = Some((self.sequence, id));
            self.feedback(app, &scene.name, "Applying scene…");
        }
        while let Ok((sequence, result)) = self.rx.try_recv() {
            self.busy = false;
            // An older response must not replace feedback for a newer selection.
            if sequence == self.sequence {
                app.set_scene_feedback_shown(true);
                match result {
                    Ok(()) => app.set_scene_feedback_status("Scene activated".into()),
                    Err(error) => {
                        app.set_scene_feedback_status("Scene failed".into());
                        app.set_scene_feedback_name(error.into());
                    }
                }
                self.until = Some(Instant::now() + Duration::from_millis(1500));
            }
        }
        if !self.busy {
            if let Some(request) = self.pending.take() {
                if self.tx.try_send(request).is_ok() {
                    self.busy = true;
                }
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn navigation_discards_late_feedback_but_allows_new_scene_feedback() {
        struct Platform;
        impl slint::platform::Platform for Platform {
            fn duration_since_start(&self) -> Duration {
                Duration::ZERO
            }
            fn create_window_adapter(
                &self,
            ) -> Result<Rc<dyn slint::platform::WindowAdapter>, slint::PlatformError> {
                Ok(
                    slint::platform::software_renderer::MinimalSoftwareWindow::new(
                        slint::platform::software_renderer::RepaintBufferType::ReusedBuffer,
                    ),
                )
            }
        }
        slint::platform::set_platform(Box::new(Platform)).unwrap();
        let app = App::new().unwrap();
        let mut controller = Controller::new(&app);
        let (reply, rx) = mpsc::channel();
        controller.rx = rx;
        controller.sequence = 1;
        controller.busy = true;
        controller.feedback(&app, "Relax", "Applying scene…");
        assert!(app.get_scene_feedback_shown());
        controller.dismiss_feedback(&app);
        reply.send((1, Ok(()))).unwrap();
        controller.poll(&app);
        assert!(!app.get_scene_feedback_shown());
        assert!(!controller.busy);
        assert!(controller.until.is_none());
        controller.feedback(&app, "Bright", "Applying scene…");
        reply.send((controller.sequence, Ok(()))).unwrap();
        controller.poll(&app);
        assert!(app.get_scene_feedback_shown());
        assert_eq!(app.get_scene_feedback_status(), "Scene activated");
    }
    /// A scene that belongs to a package is recalled by sending `on` to that
    /// child of its connection, and it sits on the room's Scenes button
    /// beside any Hue scene.
    #[test]
    fn a_package_scene_is_recalled_by_sending_on_to_its_child() {
        let config: Config = serde_json::from_value(serde_json::json!({"schema_version":1,
            "connections":[
                {"id":"bridge","name":"Hue bridge","provider":{"kind":"plugin","id":"hue","label":"Philips Hue",
                    "children":[{"kind":"scene","label":"Scene","device_kind":"other","component":"scene",
                        "capabilities":[{"id":"on","label":"On"}]}]}},
                {"id":"hue","name":"Built-in Hue","provider":{"kind":"hue"}}],
            "rooms":[{"id":"living-room","name":"Living room","devices":[]}],
            "scenes":[
                {"id":"relax","name":"Relax","rooms":["living-room"],
                 "resource":{"connection_id":"bridge","resource_id":"scene/1","kind":"scene"}},
                {"id":"bright","name":"Bright","rooms":["living-room"],
                 "hue":{"connection_id":"hue","scene_id":"9d2b7c10-35aa-4c0e-8a57-6e1f0b94d2c3"}},
                {"id":"steps","name":"Steps","rooms":["living-room"]}]})).unwrap();
        config.validate().unwrap();
        // Both scenes belong to the room, so the Scenes button offers both.
        assert_eq!(
            config
                .scenes
                .iter()
                .filter(|s| s.rooms.contains(&Id::new("living-room")))
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>(),
            ["Relax", "Bright", "Steps"]
        );
        let mut sent = Vec::new();
        recall(&config, &Id::new("relax"), &mut |connection, request| {
            sent.push((connection.to_owned(), request));
            Ok(Response::Ok)
        })
        .unwrap();
        assert_eq!(
            sent,
            [(
                "bridge".to_string(),
                Request::Command {
                    function: "on".into(),
                    phase: couch_model::KeyPhase::Tap,
                    resource: Some("scene/1".into())
                }
            )]
        );
        // A write the package acknowledged with a state is still a yes.
        recall(&config, &Id::new("relax"), &mut |_, _| {
            Ok(Response::Status {
                status: couch_plugin::Status::default(),
            })
        })
        .unwrap();
        // A refusal is the package's sentence, not a silent failure.
        let refused = recall(&config, &Id::new("relax"), &mut |_, _| {
            Err(couch_plugin::Error::Transport.into())
        });
        assert_eq!(
            refused.err().as_deref(),
            Some("The integration could not be reached")
        );
        // Nothing is sent for a scene that is not a package's.
        let never = &mut |_: &str, _: Request| -> Result<Response, couch_plugin::Failure> {
            panic!("no package may be asked")
        };
        assert_eq!(
            recall(&config, &Id::new("steps"), never).err().as_deref(),
            Some("Device-step scenes are not supported yet")
        );
        assert_eq!(
            recall(&config, &Id::new("gone"), never).err().as_deref(),
            Some("Scene was removed")
        );
        // The connection the scene named is checked before anything is sent.
        let mut moved = config;
        moved
            .connections
            .iter_mut()
            .find(|c| c.id.as_str() == "bridge")
            .unwrap()
            .provider = Provider::LegacyHue;
        assert_eq!(
            recall(&moved, &Id::new("relax"), never).err().as_deref(),
            Some("The integration this scene belongs to was removed")
        );
    }

    #[test]
    fn cycling_wraps_and_handles_empty_or_removed_selections() {
        let ids = vec![Id::new("relax"), Id::new("bright"), Id::new("night")];
        assert_eq!(next_scene(&ids, None, 1), Some(ids[0].clone()));
        assert_eq!(next_scene(&ids, None, -1), Some(ids[2].clone()));
        assert_eq!(next_scene(&ids, Some(&ids[2]), 1), Some(ids[0].clone()));
        assert_eq!(next_scene(&ids, Some(&ids[0]), -1), Some(ids[2].clone()));
        assert_eq!(
            next_scene(&ids, Some(&Id::new("removed")), 1),
            Some(ids[0].clone())
        );
        assert_eq!(next_scene(&[], None, 1), None);
        assert_eq!(
            next_scene(&ids[..1], Some(&ids[0]), 1),
            Some(ids[0].clone())
        );
    }
}
