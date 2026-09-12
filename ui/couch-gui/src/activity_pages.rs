//! Local page presentation and bounded asynchronous widget command dispatch.
use crate::{ActivityTile, App};
use couch_model::{Action, Config, Icon};
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
pub struct Pages {
    config: Option<Arc<Config>>,
    activity: String,
    page: usize,
    busy: bool,
    generation: Arc<AtomicU64>,
    tx: mpsc::SyncSender<Request>,
    rx: mpsc::Receiver<(u64, Result<(), String>)>,
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
                |request, caches: &mut (HashMap<_, _>, HashMap<_, _>, HashMap<_, _>)| {
                    crate::activity_buttons::execute(
                        &request.config,
                        &request.action,
                        &mut caches.0,
                        &mut caches.1,
                        &mut caches.2,
                    )
                },
            );
        });
        Self {
            config: None,
            activity: String::new(),
            page: 0,
            busy: false,
            generation,
            tx,
            rx,
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
    pub fn close(&mut self, app: &App) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.config = None;
        self.busy = false;
        app.set_custom_activity_shown(false);
        app.set_custom_activity_available(false);
        app.set_custom_activity_busy(false);
        app.set_custom_activity_status("".into());
    }
    fn render(&self, app: &App) {
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
                .and_then(|(_, d)| config.resolve_integration(&d.integration))
                .is_some_and(|i| {
                    matches!(
                        i,
                        couch_model::Integration::Sonos { .. } | couch_model::Integration::Kodi { .. } | couch_model::Integration::WebOs
                                    | couch_model::Integration::AndroidTv
                                    | couch_model::Integration::AppleTv
                    )
                })
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
                })
                .collect::<Vec<_>>(),
        )));
    }
    pub fn handle(&mut self, app: &App, action: &str, value: i32) {
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
    pub fn poll(&mut self, app: &App) {
        for (generation, result) in self.rx.try_iter() {
            if generation != self.generation.load(Ordering::SeqCst) {
                continue;
            }
            self.busy = false;
            app.set_custom_activity_busy(false);
            app.set_custom_activity_status(match result {
                Ok(()) => "Command sent".into(),
                Err(e) => e.into(),
            });
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
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
