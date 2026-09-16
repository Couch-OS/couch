//! Local page presentation and bounded asynchronous widget command dispatch.
use crate::{ActivityTile, App};
use couch_model::{Action, Config, Icon, PluginCapability, PluginComponent, PluginStatusField};
use couch_plugin::{Request as PluginRequest, Response as PluginResponse, Selectable, Status};
use slint::{ModelRc, VecModel};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc, Arc,
    },
};
struct Request {
    at: std::time::Instant,
    generation: u64,
    config: Arc<Config>,
    action: Action,
}

#[derive(Clone)]
pub(super) struct PluginTarget {
    pub activity: String,
    pub room: String,
    pub device: String,
    pub connection: String,
    pub label: String,
    pub capabilities: Vec<PluginCapability>,
    pub supports_inputs: bool,
    pub presentation: Vec<PluginComponent>,
}

struct PluginView {
    target: PluginTarget,
    status: Status,
    inputs: Vec<Selectable>,
}

struct PluginWork {
    generation: u64,
    connection: String,
    request: PluginRequest,
}

enum PluginEvent {
    Status(Status),
    Inputs(Vec<Selectable>),
    Error(String),
}

#[derive(Clone)]
enum PluginTileAction {
    Command(String),
    Toggle {
        state: PluginStatusField,
        on: String,
        off: String,
    },
    RefreshStatus,
    RefreshInputs,
}

struct PluginTile {
    label: String,
    detail: String,
    icon: &'static str,
    enabled: bool,
    action: PluginTileAction,
}

struct PluginPanelPage {
    title: String,
    tiles: Vec<PluginTile>,
}

pub struct Pages {
    config: Option<Arc<Config>>,
    activity: String,
    page: usize,
    busy: bool,
    generation: Arc<AtomicU64>,
    tx: mpsc::SyncSender<Request>,
    rx: mpsc::Receiver<(u64, Result<(), String>)>,
    plugin: Option<PluginView>,
    plugin_tx: mpsc::SyncSender<PluginWork>,
    plugin_rx: mpsc::Receiver<(u64, PluginEvent)>,
}
pub fn page_index(current: usize, delta: i32, count: usize) -> usize {
    if count == 0 {
        0
    } else {
        (current as i64 + delta as i64).rem_euclid(count as i64) as usize
    }
}
// A generation owns its provider leases. Polling also releases idle leases when
// the view closes: no subsequent command is required to wake this worker.
fn command_worker<S: Default>(
    work: mpsc::Receiver<Request>,
    reply: mpsc::Sender<(u64, Result<(), String>)>,
    current: Arc<AtomicU64>,
    mut execute: impl FnMut(&Request, &mut S) -> Result<(), String>,
) {
    use std::time::Duration;
    let mut caches = S::default();
    let mut generation = current.load(Ordering::SeqCst);
    let mut config: Option<Arc<Config>> = None;
    loop {
        let request = work.recv_timeout(Duration::from_millis(100));
        let now = current.load(Ordering::SeqCst);
        if now != generation {
            caches = S::default();
            config = None;
            generation = now;
        }
        let request = match request {
            Ok(request) => request,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        };
        if request.generation != generation {
            continue;
        }
        if request.at.elapsed() > Duration::from_millis(750) {
            let _ = reply.send((
                generation,
                Err("Command expired while waiting. Try again.".into()),
            ));
            continue;
        }
        // Opening a different configuration normally advances the generation;
        // also defend against accidental reuse of a generation with a new snapshot.
        if config
            .as_ref()
            .is_some_and(|old| !Arc::ptr_eq(old, &request.config))
        {
            caches = S::default();
        }
        config = Some(request.config.clone());
        let result = execute(&request, &mut caches);
        if current.load(Ordering::SeqCst) != generation {
            // An already-sent command cannot be recalled, but its connection is
            // released immediately when it completes after navigation.
            caches = S::default();
            config = None;
        }
        let _ = reply.send((generation, result));
    }
}

impl Pages {
    pub fn new() -> Self {
        let (tx, work) = mpsc::sync_channel::<Request>(1);
        let (reply, rx) = mpsc::channel();
        let generation = Arc::new(AtomicU64::new(0));
        let current = generation.clone();
        std::thread::spawn(move || {
            command_worker(
                work,
                reply,
                current,
                |request,
                 caches: &mut (HashMap<_, _>, HashMap<_, _>, HashMap<_, _>, HashMap<_, _>)| {
                    crate::activity_buttons::execute(
                        &request.config,
                        &request.action,
                        &mut caches.0,
                        &mut caches.1,
                        &mut caches.2,
                        &mut caches.3,
                        &crate::connections::matter(),
                    )
                },
            );
        });
        let (plugin_tx, plugin_work) = mpsc::sync_channel::<PluginWork>(4);
        let (plugin_reply, plugin_rx) = mpsc::channel();
        std::thread::spawn(move || {
            while let Ok(work) = plugin_work.recv() {
                let event = match couch_plugin::local_request(
                    &crate::home::path("plugin.sock"),
                    &work.connection,
                    work.request,
                    couch_plugin::REQUEST_TIMEOUT + std::time::Duration::from_secs(1),
                ) {
                    Ok(PluginResponse::Status { status }) => PluginEvent::Status(status),
                    Ok(PluginResponse::Inputs { inputs }) => PluginEvent::Inputs(inputs),
                    Ok(PluginResponse::Error { code }) => PluginEvent::Error(code.to_string()),
                    Ok(_) => PluginEvent::Error("The integration sent an unexpected reply".into()),
                    Err(error) => PluginEvent::Error(error.to_string()),
                };
                let _ = plugin_reply.send((work.generation, event));
            }
        });
        Self {
            config: None,
            activity: String::new(),
            page: 0,
            busy: false,
            generation,
            tx,
            rx,
            plugin: None,
            plugin_tx,
            plugin_rx,
        }
    }
    pub fn open(&mut self, app: &App, config: Arc<Config>, id: &str) {
        self.close(app);
        self.config = Some(config);
        self.activity = id.into();
        self.page = 0;
        app.set_custom_activity_shown(true);
        self.render(app);
    }
    pub fn open_plugin(&mut self, app: &App, config: Arc<Config>, target: PluginTarget) {
        self.close(app);
        self.config = Some(config);
        self.activity.clear();
        self.page = 0;
        app.set_player_activity(target.activity.as_str().into());
        app.set_player_room(target.room.as_str().into());
        self.plugin = Some(PluginView {
            target,
            status: Status::default(),
            inputs: vec![],
        });
        app.set_custom_activity_shown(true);
        app.set_custom_activity_available(true);
        self.request_plugin(app, PluginRequest::Status, "Refreshing status…");
        if self
            .plugin
            .as_ref()
            .is_some_and(|view| view.target.supports_inputs)
        {
            self.request_plugin(app, PluginRequest::Inputs, "Loading inputs…");
        }
        self.render(app);
    }
    pub fn close(&mut self, app: &App) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.config = None;
        self.plugin = None;
        self.busy = false;
        app.set_custom_activity_shown(false);
        app.set_custom_activity_available(false);
        app.set_custom_activity_busy(false);
        app.set_custom_activity_status("".into());
    }
    fn request_plugin(&self, app: &App, request: PluginRequest, message: &str) {
        let Some(plugin) = &self.plugin else { return };
        let result = self.plugin_tx.try_send(PluginWork {
            generation: self.generation.load(Ordering::SeqCst),
            connection: plugin.target.connection.clone(),
            request,
        });
        app.set_custom_activity_status(if result.is_ok() {
            message.into()
        } else {
            "Integration request queue is busy. Try again.".into()
        });
    }
    fn render(&self, app: &App) {
        if let Some(plugin) = &self.plugin {
            let pages = plugin_pages(plugin);
            let Some(page) = pages.get(self.page) else {
                return;
            };
            app.set_custom_activity_title(page.title.as_str().into());
            app.set_custom_activity_page(self.page as i32);
            app.set_custom_activity_count(pages.len() as i32);
            app.set_custom_activity_source(false);
            app.set_custom_activity_tiles(ModelRc::new(VecModel::from(
                page.tiles
                    .iter()
                    .map(|tile| ActivityTile {
                        label: tile.label.as_str().into(),
                        detail: tile.detail.as_str().into(),
                        icon: crate::icons::image(
                            Icon::from_name(tile.icon)
                                .unwrap_or_else(|| Icon::from_name("circle-dot").unwrap()),
                        ),
                        enabled: tile.enabled,
                    })
                    .collect::<Vec<_>>(),
            )));
            return;
        }
        let Some(config) = &self.config else { return };
        let Some(activity) = config
            .activities
            .iter()
            .find(|a| a.id.as_str() == self.activity)
        else {
            return;
        };
        let Some(page) = activity.setup.pages.get(self.page) else {
            return;
        };
        app.set_custom_activity_title(page.title.as_str().into());
        app.set_custom_activity_page(self.page as i32);
        app.set_custom_activity_count(activity.setup.pages.len() as i32);
        app.set_custom_activity_source(activity.source.as_ref().is_some_and(|id| {
            config
                .devices()
                .find(|(_, d)| &d.id == id)
                .is_some_and(|(_, d)| crate::activity_runtime::has_screen(config, d))
        }));
        app.set_custom_activity_tiles(ModelRc::new(VecModel::from(
            page.widgets
                .iter()
                .map(|w| ActivityTile {
                    label: w.label.as_str().into(),
                    detail: config
                        .devices()
                        .find(|(_, d)| d.id == w.action.device)
                        .map(|(_, d)| d.name.as_str())
                        .unwrap_or("Device unavailable")
                        .into(),
                    icon: crate::icons::image(
                        w.icon
                            .unwrap_or_else(|| Icon::from_name("circle-dot").unwrap()),
                    ),
                    enabled: true,
                })
                .collect::<Vec<_>>(),
        )));
    }
    pub fn handle(&mut self, app: &App, action: &str, value: i32) {
        if self.plugin.is_some() {
            self.handle_plugin(app, action, value);
            return;
        }
        let Some(config) = &self.config else { return };
        let Some(activity) = config
            .activities
            .iter()
            .find(|a| a.id.as_str() == self.activity)
        else {
            return;
        };
        if action == "page" {
            self.page = page_index(self.page, value, activity.setup.pages.len());
            self.render(app);
        } else if action == "command" && !self.busy {
            let Some(widget) = activity
                .setup
                .pages
                .get(self.page)
                .and_then(|p| usize::try_from(value).ok().and_then(|i| p.widgets.get(i)))
            else {
                return;
            };
            let result = self.tx.try_send(Request {
                at: std::time::Instant::now(),
                generation: self.generation.load(Ordering::SeqCst),
                config: config.clone(),
                action: widget.action.clone(),
            });
            if result.is_ok() {
                self.busy = true;
                app.set_custom_activity_busy(true);
                app.set_custom_activity_status(format!("Sending {}…", widget.label).into());
            } else {
                app.set_custom_activity_status("Command worker is busy. Try again.".into());
            }
        }
    }
    fn handle_plugin(&mut self, app: &App, action: &str, value: i32) {
        let Some(plugin) = &self.plugin else { return };
        let pages = plugin_pages(plugin);
        if action == "page" {
            self.page = page_index(self.page, value, pages.len());
            self.render(app);
            return;
        }
        if action != "command" || self.busy {
            return;
        }
        let Some(tile) = pages
            .get(self.page)
            .and_then(|page| usize::try_from(value).ok().and_then(|i| page.tiles.get(i)))
        else {
            return;
        };
        let command = match &tile.action {
            PluginTileAction::Command(_) | PluginTileAction::Toggle { .. } => {
                plugin_command(&tile.action, &plugin.status)
            }
            PluginTileAction::RefreshStatus => {
                self.request_plugin(app, PluginRequest::Status, "Refreshing status…");
                return;
            }
            PluginTileAction::RefreshInputs => {
                self.request_plugin(app, PluginRequest::Inputs, "Loading inputs…");
                return;
            }
        };
        let Some(command) = command else { return };
        let Some(config) = &self.config else { return };
        let result = self.tx.try_send(Request {
            at: std::time::Instant::now(),
            generation: self.generation.load(Ordering::SeqCst),
            config: config.clone(),
            action: Action::new(plugin.target.device.as_str(), command),
        });
        if result.is_ok() {
            self.busy = true;
            app.set_custom_activity_busy(true);
            app.set_custom_activity_status(format!("Sending {}…", tile.label).into());
        } else {
            app.set_custom_activity_status("Command worker is busy. Try again.".into());
        }
    }
    pub fn poll(&mut self, app: &App) {
        for (generation, result) in self.rx.try_iter() {
            if generation != self.generation.load(Ordering::SeqCst) {
                continue;
            }
            self.busy = false;
            app.set_custom_activity_busy(false);
            let succeeded = result.is_ok();
            app.set_custom_activity_status(match result {
                Ok(()) => "Command sent".into(),
                Err(e) => e.into(),
            });
            if succeeded && self.plugin.is_some() {
                self.request_plugin(app, PluginRequest::Status, "Refreshing status…");
            }
        }
        let events = self.plugin_rx.try_iter().collect::<Vec<_>>();
        for (generation, event) in events {
            if let Some(message) = self.apply_plugin_event(generation, event) {
                app.set_custom_activity_status(message.into());
                self.render(app);
            }
        }
    }

    fn apply_plugin_event(&mut self, generation: u64, event: PluginEvent) -> Option<String> {
        if generation != self.generation.load(Ordering::SeqCst) {
            return None;
        }
        let plugin = self.plugin.as_mut()?;
        Some(match event {
            PluginEvent::Status(status) => {
                plugin.status = status;
                "Status updated".into()
            }
            PluginEvent::Inputs(inputs) => {
                plugin.inputs = inputs;
                "Inputs updated".into()
            }
            PluginEvent::Error(error) => error,
        })
    }
}

fn status_bool(status: &Status, field: PluginStatusField) -> Option<bool> {
    match field {
        PluginStatusField::On => status.on,
        PluginStatusField::Playing => status.playing,
        PluginStatusField::Muted => status.muted,
        _ => None,
    }
}

fn plugin_command(action: &PluginTileAction, status: &Status) -> Option<String> {
    match action {
        PluginTileAction::Command(command) => Some(command.clone()),
        PluginTileAction::Toggle { state, on, off } => {
            status_bool(status, *state)
                .map(|enabled| if enabled { off.clone() } else { on.clone() })
        }
        PluginTileAction::RefreshStatus | PluginTileAction::RefreshInputs => None,
    }
}

fn status_text(status: &Status, field: PluginStatusField) -> String {
    match field {
        PluginStatusField::On => status.on.map(|v| if v { "On" } else { "Off" }.into()),
        PluginStatusField::Playing => status
            .playing
            .map(|v| if v { "Playing" } else { "Paused" }.into()),
        PluginStatusField::Muted => status
            .muted
            .map(|v| if v { "Muted" } else { "Unmuted" }.into()),
        PluginStatusField::Volume => status.volume.map(|v| format!("{v}%")),
        PluginStatusField::Input => status.input.clone(),
        PluginStatusField::Title => status.title.clone(),
    }
    .unwrap_or_else(|| "Unavailable".into())
}

fn command_tile(view: &PluginView, command: &str, detail: &str) -> PluginTile {
    let label = view
        .target
        .capabilities
        .iter()
        .find(|capability| capability.id == command)
        .map(|capability| capability.label.clone())
        .unwrap_or_else(|| command.to_owned());
    PluginTile {
        label,
        detail: detail.into(),
        icon: "circle-play",
        enabled: true,
        action: PluginTileAction::Command(command.into()),
    }
}

fn chunk_page(title: &str, tiles: Vec<PluginTile>, pages: &mut Vec<PluginPanelPage>) {
    for chunk in tiles.chunks(6) {
        pages.push(PluginPanelPage {
            title: title.into(),
            tiles: chunk
                .iter()
                .map(|tile| PluginTile {
                    label: tile.label.clone(),
                    detail: tile.detail.clone(),
                    icon: tile.icon,
                    enabled: tile.enabled,
                    action: tile.action.clone(),
                })
                .collect(),
        });
    }
}

fn plugin_pages(view: &PluginView) -> Vec<PluginPanelPage> {
    let mut pages = vec![];
    if view.target.presentation.is_empty() {
        let tiles = view
            .target
            .capabilities
            .iter()
            .map(|capability| command_tile(view, &capability.id, "Command"))
            .collect();
        chunk_page(&view.target.label, tiles, &mut pages);
    } else {
        for component in &view.target.presentation {
            match component {
                PluginComponent::CommandGroup { title, commands } => chunk_page(
                    title,
                    commands
                        .iter()
                        .map(|command| command_tile(view, command, "Command"))
                        .collect(),
                    &mut pages,
                ),
                PluginComponent::StatusText { label, field } => pages.push(PluginPanelPage {
                    title: label.clone(),
                    tiles: vec![PluginTile {
                        label: label.clone(),
                        detail: status_text(&view.status, *field),
                        icon: "activity",
                        enabled: true,
                        action: PluginTileAction::RefreshStatus,
                    }],
                }),
                PluginComponent::Toggle {
                    label,
                    state,
                    on,
                    off,
                } => pages.push(PluginPanelPage {
                    title: label.clone(),
                    tiles: vec![PluginTile {
                        label: label.clone(),
                        detail: status_text(&view.status, *state),
                        icon: "power",
                        enabled: status_bool(&view.status, *state).is_some(),
                        action: PluginTileAction::Toggle {
                            state: *state,
                            on: on.clone(),
                            off: off.clone(),
                        },
                    }],
                }),
                PluginComponent::InputSelector { label } => {
                    let tiles = if view.inputs.is_empty() {
                        vec![PluginTile {
                            label: "Refresh inputs".into(),
                            detail: "No inputs reported".into(),
                            icon: "refresh-cw",
                            enabled: true,
                            action: PluginTileAction::RefreshInputs,
                        }]
                    } else {
                        view.inputs
                            .iter()
                            .map(|input| PluginTile {
                                label: input.name.clone(),
                                detail: if view.status.input.as_deref() == Some(input.id.as_str()) {
                                    "Current input".into()
                                } else {
                                    "Input".into()
                                },
                                icon: "list-video",
                                enabled: true,
                                action: PluginTileAction::Command(format!("input:{}", input.id)),
                            })
                            .collect()
                    };
                    chunk_page(label, tiles, &mut pages);
                }
            }
        }
    }
    if pages.is_empty() {
        pages.push(PluginPanelPage {
            title: view.target.label.clone(),
            tiles: vec![PluginTile {
                label: "Refresh status".into(),
                detail: "No controls declared".into(),
                icon: "refresh-cw",
                enabled: true,
                action: PluginTileAction::RefreshStatus,
            }],
        });
    }
    pages
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn plugin_presentation_uses_native_pages_and_live_state() {
        let view = PluginView {
            target: PluginTarget {
                activity: "Listen".into(),
                room: "Living room".into(),
                device: "receiver".into(),
                connection: "denon".into(),
                label: "Community receiver".into(),
                capabilities: vec![
                    PluginCapability {
                        id: "power-on".into(),
                        label: "Turn on".into(),
                    },
                    PluginCapability {
                        id: "power-off".into(),
                        label: "Turn off".into(),
                    },
                ],
                supports_inputs: true,
                presentation: vec![
                    PluginComponent::Toggle {
                        label: "Power".into(),
                        state: PluginStatusField::On,
                        on: "power-on".into(),
                        off: "power-off".into(),
                    },
                    PluginComponent::InputSelector {
                        label: "Source".into(),
                    },
                ],
            },
            status: Status::on(true).with_input("tv"),
            inputs: vec![Selectable::new("tv", "Television")],
        };
        let pages = plugin_pages(&view);
        assert_eq!(pages.len(), 2);
        assert_eq!(pages[0].title, "Power");
        assert_eq!(pages[0].tiles[0].detail, "On");
        assert!(matches!(
            &pages[0].tiles[0].action,
            PluginTileAction::Toggle { off, .. } if off == "power-off"
        ));
        assert_eq!(pages[1].tiles[0].label, "Television");
        assert_eq!(pages[1].tiles[0].detail, "Current input");
        assert!(matches!(
            &pages[1].tiles[0].action,
            PluginTileAction::Command(command) if command == "input:tv"
        ));

        let unknown = Status::default();
        assert_eq!(plugin_command(&pages[0].tiles[0].action, &unknown), None);
        assert_eq!(
            plugin_command(&pages[0].tiles[0].action, &view.status),
            Some("power-off".into())
        );
        let unknown_view = PluginView {
            target: view.target.clone(),
            status: unknown,
            inputs: vec![],
        };
        assert!(!plugin_pages(&unknown_view)[0].tiles[0].enabled);
    }

    #[test]
    fn stale_plugin_reply_cannot_repaint_a_new_view() {
        let mut pages = Pages::new();
        pages.plugin = Some(PluginView {
            target: PluginTarget {
                activity: "New activity".into(),
                room: "Living room".into(),
                device: "receiver".into(),
                connection: "receiver".into(),
                label: "Receiver".into(),
                capabilities: vec![],
                supports_inputs: false,
                presentation: vec![],
            },
            status: Status::default(),
            inputs: vec![],
        });
        pages.generation.store(2, Ordering::SeqCst);
        assert!(pages
            .apply_plugin_event(1, PluginEvent::Status(Status::on(true)))
            .is_none());
        assert_eq!(pages.plugin.as_ref().unwrap().status.on, None);
        assert_eq!(
            pages.apply_plugin_event(2, PluginEvent::Status(Status::on(false))),
            Some("Status updated".into())
        );
        assert_eq!(pages.plugin.as_ref().unwrap().status.on, Some(false));
    }

    #[test]
    fn command_worker_reuses_leases_and_releases_on_close_or_config_change() {
        use std::time::{Duration, Instant};
        struct Lease(Arc<AtomicU64>);
        impl Drop for Lease {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        let created = Arc::new(AtomicU64::new(0));
        let dropped = Arc::new(AtomicU64::new(0));
        let generation = Arc::new(AtomicU64::new(1));
        let (tx, work) = mpsc::sync_channel(1);
        let (reply, rx) = mpsc::channel();
        let (c, d, g) = (created.clone(), dropped.clone(), generation.clone());
        let worker = std::thread::spawn(move || {
            command_worker(work, reply, g, |_, cache: &mut Option<Lease>| {
                if cache.is_none() {
                    c.fetch_add(1, Ordering::SeqCst);
                    *cache = Some(Lease(d.clone()));
                }
                Ok(())
            });
        });
        let config = Arc::new(Config::default());
        let send = |config: Arc<Config>, generation| {
            tx.send(Request {
                at: Instant::now(),
                generation,
                config,
                action: Action::new("fixture", "power-on"),
            })
            .unwrap();
            let (got, result) = rx.recv_timeout(Duration::from_secs(1)).unwrap();
            assert_eq!(got, generation);
            result.unwrap();
        };
        send(config.clone(), 1);
        send(config, 1);
        assert_eq!(
            created.load(Ordering::SeqCst),
            1,
            "two commands share one lease"
        );
        assert_eq!(dropped.load(Ordering::SeqCst), 0);
        send(Arc::new(Config::default()), 1);
        assert_eq!(created.load(Ordering::SeqCst), 2);
        assert_eq!(
            dropped.load(Ordering::SeqCst),
            1,
            "new config releases old lease"
        );
        generation.store(2, Ordering::SeqCst);
        let deadline = Instant::now() + Duration::from_secs(1);
        while dropped.load(Ordering::SeqCst) != 2 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            dropped.load(Ordering::SeqCst),
            2,
            "idle close needs no extra command"
        );
        send(Arc::new(Config::default()), 2);
        drop(tx);
        worker.join().unwrap();
        assert_eq!(
            dropped.load(Ordering::SeqCst),
            3,
            "worker shutdown releases its lease"
        );
    }

    #[test]
    fn source_switching_preserves_activity_and_back_closes_custom_pages() {
        if std::env::var_os("COUCH_TEST_PAGE_SOURCE").is_none() {
            let out=std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact","activity::pages::tests::source_switching_preserves_activity_and_back_closes_custom_pages"])
                .env("COUCH_TEST_PAGE_SOURCE","1").output().unwrap();
            assert!(
                out.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            return;
        }
        use slint::ComponentHandle;
        let mut config = Config::seed();
        for room in &mut config.rooms {
            for device in &mut room.devices {
                if device.id.as_str() == "living-kodi" {
                    device.integration = couch_model::Integration::Kodi {
                        host: "127.0.0.1".into(),
                        port: 1,
                    };
                }
            }
        }
        config.activities[0].setup.custom_screen = true;
        config.activities[0].setup.pages = vec![couch_model::ActivityPage {
            title: "Playback".into(),
            widgets: vec![],
        }];
        let id = config.activities[0].id.to_string();
        let path =
            std::env::temp_dir().join(format!("couch-page-source-{}.json", std::process::id()));
        std::fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
        crate::config_snapshot::start(path.clone());
        let _window =
            crate::panel::CouchPlatform::install(slint::PhysicalSize::new(480, 800)).unwrap();
        let app = App::new().unwrap();
        app.show().unwrap();
        let mut controller = super::super::Controller::new(&app);
        app.invoke_open_activity_ready(id.as_str().into());
        controller.poll(&app);
        assert!(app.get_custom_activity_shown());
        assert!(app.get_player_shown());
        app.invoke_custom_activity_action("source".into(), 0);
        controller.poll(&app);
        assert!(!app.get_custom_activity_shown());
        assert!(app.get_custom_activity_available());
        assert_eq!(app.get_active_activity(), id.as_str());
        app.invoke_player_action("pages".into(), 0.);
        controller.poll(&app);
        assert!(app.get_custom_activity_shown());
        assert_eq!(app.get_active_activity(), id.as_str());
        app.invoke_player_action("back".into(), 0.);
        controller.poll(&app);
        assert!(!app.get_custom_activity_shown());
        assert!(!app.get_player_shown());
        assert!(!app.get_custom_activity_available());
        app.hide().unwrap();
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn custom_page_keys_and_touch_route_to_tiles_and_page_edges() {
        if std::env::var_os("COUCH_TEST_CUSTOM_PAGES").is_none() {
            let out=std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact","activity::pages::tests::custom_page_keys_and_touch_route_to_tiles_and_page_edges"])
                .env("COUCH_TEST_CUSTOM_PAGES","1").output().unwrap();
            assert!(
                out.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            return;
        }
        use slint::{
            platform::{Key, PointerEventButton, WindowEvent},
            ComponentHandle,
        };
        use std::{cell::RefCell, rc::Rc};
        let window =
            crate::panel::CouchPlatform::install(slint::PhysicalSize::new(480, 800)).unwrap();
        let app = App::new().unwrap();
        let mut config = Config::seed();
        config.activities[0].setup.pages = vec![
            couch_model::ActivityPage {
                title: "Playback".into(),
                widgets: vec![
                    couch_model::ActivityWidget {
                        label: "Play / pause".into(),
                        icon: None,
                        action: Action::new("living-kodi", "play-pause")
                    };
                    6
                ],
            },
            couch_model::ActivityPage {
                title: "Lighting".into(),
                widgets: vec![],
            },
        ];
        let id = config.activities[0].id.to_string();
        let mut pages = Pages::new();
        pages.open(&app, Arc::new(config), &id);
        app.set_player_shown(true);
        app.set_player_activity("Watch a movie".into());
        app.show().unwrap();
        app.invoke_focus_player();
        let actions = Rc::new(RefCell::new(Vec::new()));
        let received = actions.clone();
        app.on_custom_activity_action(move |name, index| {
            received.borrow_mut().push((name.to_string(), index))
        });
        let key = |key: Key| {
            window.dispatch_event(WindowEvent::KeyPressed {
                text: char::from(key).to_string().into(),
            })
        };
        key(Key::RightArrow);
        key(Key::Return);
        assert_eq!(&*actions.borrow(), &[("command".into(), 1)]);
        actions.borrow_mut().clear();
        key(Key::RightArrow);
        assert_eq!(&*actions.borrow(), &[("page".into(), 1)]);
        pages.handle(&app, "page", 1);
        assert_eq!(app.get_custom_activity_title(), "Lighting");
        actions.borrow_mut().clear();
        key(Key::Return);
        assert!(actions.borrow().is_empty());
        key(Key::LeftArrow);
        assert_eq!(&*actions.borrow(), &[("page".into(), -1)]);
        pages.handle(&app, "page", -1);
        actions.borrow_mut().clear();
        window.dispatch_event(WindowEvent::PointerPressed {
            position: slint::LogicalPosition::new(40., 160.),
            button: PointerEventButton::Left,
        });
        window.dispatch_event(WindowEvent::PointerReleased {
            position: slint::LogicalPosition::new(40., 160.),
            button: PointerEventButton::Left,
        });
        assert_eq!(&*actions.borrow(), &[("command".into(), 0)]);
        // Optional local review artifact, no real framebuffer or device access.
        if let Some(path) = std::env::var_os("COUCH_CUSTOM_SCREENSHOT") {
            window.draw_if_needed(|renderer| {
                let mut pixels = vec![slint::Rgb8Pixel::default(); 480 * 800];
                renderer.render(&mut pixels, 480);
                let mut bytes = b"P6\n480 800\n255\n".to_vec();
                for p in pixels {
                    bytes.extend_from_slice(&[p.r, p.g, p.b]);
                }
                std::fs::write(path, bytes).unwrap();
            });
        }
        pages.close(&app);
        assert!(!app.get_custom_activity_shown());
        app.hide().unwrap();
    }
    #[test]
    fn page_navigation_wraps_in_both_directions_and_handles_empty_pages() {
        assert_eq!(page_index(0, -1, 3), 2);
        assert_eq!(page_index(2, 1, 3), 0);
        assert_eq!(page_index(0, 1, 0), 0);
        assert_eq!(page_index(0, -1, 1), 0);
    }
}
