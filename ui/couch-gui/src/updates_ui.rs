//! Software updates from the remote's own Settings menu.
//!
//! The same four operations the web UI's Updates page performs (status, check,
//! download and verify, install and restart) go to the root system service
//! over its control socket, the way the SSH toggle already does. Every call
//! runs on a worker thread: a check talks to GitHub and a download can take a
//! minute, and the UI thread must keep drawing. Restart is armed by one OK and
//! fired by a second within a few seconds, the menu's equivalent of the web
//! page's confirmation checkbox.
use crate::App;
use couch_system::{
    client,
    protocol::{Reply, Request},
};
use couch_updates::{Channel, Status};
use std::{
    cell::RefCell,
    collections::VecDeque,
    rc::Rc,
    sync::mpsc,
    time::{Duration, Instant},
};

/// How often the status is re-read while the Updates section is on screen.
const POLL: Duration = Duration::from_secs(2);
/// How long a first OK on "Install & restart" stays armed for the second.
const ARM: Duration = Duration::from_secs(6);
/// The Settings panel index this section owns (0 root, 1 display, 2 wifi, 3 ssh).
pub const PANEL: i32 = 4;

enum Input {
    Check,
    Install,
    Restart,
    Channel(i32),
}
enum Outcome {
    Status(Status),
    Done,
    Failed(String),
}
pub struct Controller {
    input: Rc<RefCell<VecDeque<Input>>>,
    tx: mpsc::Sender<Request>,
    rx: mpsc::Receiver<Outcome>,
    status: Option<Status>,
    /// A request is in flight; the rows say so and OK is ignored meanwhile.
    busy: bool,
    armed_until: Option<Instant>,
    last_poll: Option<Instant>,
    open: bool,
    /// A line the controller wants shown instead of the service's message,
    /// with when it expires.
    notice: Option<(String, Instant)>,
}
impl Controller {
    pub fn install(app: &App) -> Self {
        let input = Rc::new(RefCell::new(VecDeque::new()));
        let q = input.clone();
        app.on_update_check(move || q.borrow_mut().push_back(Input::Check));
        let q = input.clone();
        app.on_update_install(move || q.borrow_mut().push_back(Input::Install));
        let q = input.clone();
        app.on_update_restart(move || q.borrow_mut().push_back(Input::Restart));
        let q = input.clone();
        app.on_update_channel_step(move |delta| q.borrow_mut().push_back(Input::Channel(delta)));
        let (tx, requests) = mpsc::channel::<Request>();
        let (outcomes, rx) = mpsc::channel();
        std::thread::spawn(move || {
            while let Ok(request) = requests.recv() {
                let outcome = match client::call(request) {
                    Ok(Reply::Update(status)) => Outcome::Status(status),
                    Ok(Reply::Done(Ok(()))) => Outcome::Done,
                    Ok(Reply::Done(Err(error))) => Outcome::Failed(error),
                    Ok(_) => Outcome::Failed("Unexpected system service reply".into()),
                    Err(error) => Outcome::Failed(error),
                };
                if outcomes.send(outcome).is_err() {
                    return;
                }
            }
        });
        Self {
            input,
            tx,
            rx,
            status: None,
            busy: false,
            armed_until: None,
            last_poll: None,
            open: false,
            notice: None,
        }
    }
    fn send(&mut self, request: Request) {
        if self.tx.send(request).is_ok() {
            self.busy = true;
        }
    }
    fn say(&mut self, app: &App, text: &str, for_: Duration) {
        app.set_update_message(text.into());
        self.notice = Some((text.to_owned(), Instant::now() + for_));
    }
    fn present(&self, app: &App) {
        let Some(s) = &self.status else {
            app.set_update_installed("…".into());
            app.set_update_channel("".into());
            app.set_update_summary("Connecting…".into());
            app.set_update_can_install(false);
            app.set_update_ready(false);
            return;
        };
        app.set_update_installed(s.installed.as_str().into());
        app.set_update_channel(channel_label(s.channel).into());
        app.set_update_summary(summary(s, self.busy).into());
        app.set_update_can_install(s.can_install && !self.busy);
        app.set_update_ready(s.phase == "ready");
        if self.notice.is_none() {
            app.set_update_message(s.message.as_str().into());
        }
    }
    pub fn poll(&mut self, app: &App) {
        let open = app.get_settings_shown() && app.get_settings_panel() == PANEL;
        if open != self.open {
            self.open = open;
            self.armed_until = None;
            app.set_update_armed(false);
            if open {
                self.last_poll = None;
                self.notice = None;
                self.present(app);
            } else {
                self.input.borrow_mut().clear();
            }
        }
        while let Ok(outcome) = self.rx.try_recv() {
            self.busy = false;
            match outcome {
                Outcome::Status(status) => {
                    self.status = Some(status);
                }
                Outcome::Done => {
                    // An action was accepted; read where it left things.
                    self.send(Request::UpdateStatus);
                }
                Outcome::Failed(error) => self.say(app, &error, Duration::from_secs(6)),
            }
            if open {
                self.present(app);
            }
        }
        if !open {
            return;
        }
        loop {
            let Some(input) = self.input.borrow_mut().pop_front() else {
                break;
            };
            if self.busy {
                self.say(
                    app,
                    "Still working on the last request…",
                    Duration::from_secs(2),
                );
                continue;
            }
            match input {
                Input::Check => {
                    self.armed_until = None;
                    app.set_update_armed(false);
                    self.say(app, "Checking for updates…", Duration::from_secs(20));
                    self.send(Request::UpdateCheck { automatic: false });
                }
                Input::Install => {
                    let Some(version) = self
                        .status
                        .as_ref()
                        .filter(|s| s.can_install)
                        .and_then(|s| s.available.clone())
                    else {
                        continue;
                    };
                    self.say(app, "Downloading and verifying…", Duration::from_secs(120));
                    self.send(Request::UpdateInstall { version });
                }
                Input::Restart => {
                    if !self.status.as_ref().is_some_and(|s| s.phase == "ready") {
                        continue;
                    }
                    if self.armed_until.is_some_and(|until| Instant::now() < until) {
                        self.armed_until = None;
                        app.set_update_armed(false);
                        self.say(
                            app,
                            "Restarting to apply the update…",
                            Duration::from_secs(120),
                        );
                        self.send(Request::UpdateRestart);
                    } else {
                        self.armed_until = Some(Instant::now() + ARM);
                        app.set_update_armed(true);
                        self.say(
                            app,
                            "Press OK again to restart. Your connections, Wi-Fi and settings are kept; keep the remote charged.",
                            ARM,
                        );
                    }
                }
                Input::Channel(delta) => {
                    let Some(s) = &self.status else { continue };
                    if delta == 0 {
                        continue;
                    }
                    let channel = match s.channel {
                        Channel::Stable => Channel::Alpha,
                        Channel::Alpha => Channel::Stable,
                    };
                    self.send(Request::UpdateSettings {
                        channel,
                        automatic_checks: s.automatic_checks,
                    });
                }
            }
            self.present(app);
        }
        if self
            .armed_until
            .is_some_and(|until| Instant::now() >= until)
        {
            self.armed_until = None;
            app.set_update_armed(false);
        }
        if self
            .notice
            .as_ref()
            .is_some_and(|(_, until)| Instant::now() >= *until)
        {
            self.notice = None;
            self.present(app);
        }
        if !self.busy && self.last_poll.is_none_or(|at| at.elapsed() >= POLL) {
            self.last_poll = Some(Instant::now());
            self.send(Request::UpdateStatus);
        }
    }
}
fn channel_label(channel: Channel) -> &'static str {
    match channel {
        Channel::Stable => "Stable",
        Channel::Alpha => "Alpha",
    }
}
/// The value shown on the "Check for updates" row: one glance at where things are.
fn summary(s: &Status, busy: bool) -> String {
    match s.phase.as_str() {
        "checking" => "Checking…".into(),
        "downloading" => "Downloading…".into(),
        "verifying" => "Verifying…".into(),
        "ready" => "Ready to install".into(),
        "error" => "Failed".into(),
        _ => match &s.available {
            Some(version) => format!("{} available", short(version)),
            None if busy => "Working…".into(),
            None => "Up to date".into(),
        },
    }
}
/// A prerelease tag on one row: `.122` says enough next to the installed build,
/// whose full name is on the row above; a stable version keeps its full name.
fn short(version: &str) -> String {
    match version.rsplit_once('.') {
        Some((head, build)) if head.contains("alpha") && !build.is_empty() => format!(".{build}"),
        _ => version.to_owned(),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn updates_section_renders_and_its_rows_dispatch() {
        if std::env::var_os("COUCH_TEST_UPDATES_MENU").is_none() {
            let out = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "updates_ui::tests::updates_section_renders_and_its_rows_dispatch",
                ])
                .env("COUCH_TEST_UPDATES_MENU", "1")
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
        use slint::{
            platform::{Key, WindowEvent},
            ComponentHandle,
        };
        let window =
            crate::panel::CouchPlatform::install(slint::PhysicalSize::new(480, 800)).unwrap();
        let app = crate::App::new().unwrap();
        let events = Rc::new(RefCell::new(Vec::<String>::new()));
        let e = events.clone();
        app.on_update_check(move || e.borrow_mut().push("check".into()));
        let e = events.clone();
        app.on_update_install(move || e.borrow_mut().push("install".into()));
        let e = events.clone();
        app.on_update_restart(move || e.borrow_mut().push("restart".into()));
        let e = events.clone();
        app.on_update_channel_step(move |d| e.borrow_mut().push(format!("channel:{d}")));
        app.set_update_installed("v0.1.0-alpha.20260913.121".into());
        app.set_update_channel("Alpha".into());
        app.set_update_summary(".122 available".into());
        app.set_update_can_install(true);
        app.set_update_ready(false);
        app.set_update_message(
            "A newer signed build is available on the alpha channel. Download it to verify it on the remote."
                .into(),
        );
        app.set_settings_panel(PANEL);
        app.set_settings_shown(true);
        app.show().unwrap();
        window.dispatch_event(WindowEvent::WindowActiveChanged(true));
        slint::platform::update_timers_and_animations();
        let mut pixels = vec![slint::Rgb8Pixel::default(); 480 * 800];
        window.request_redraw();
        window.draw_if_needed(|r| {
            r.render(&mut pixels, 480);
        });
        if let Some(path) = std::env::var_os("COUCH_UPDATES_SCREENSHOT") {
            let bytes: Vec<u8> = pixels.iter().flat_map(|p| [p.r, p.g, p.b]).collect();
            image::save_buffer(path, &bytes, 480, 800, image::ColorType::Rgb8).unwrap();
        }
        // Row 1 is the channel stepper; row 2 checks; row 3 downloads while a
        // build is offered.
        for key in [
            Key::DownArrow,
            Key::RightArrow,
            Key::DownArrow,
            Key::Return,
            Key::DownArrow,
            Key::Return,
        ] {
            let text = char::from(key).to_string().into();
            window.dispatch_event(WindowEvent::KeyPressed { text });
        }
        assert_eq!(&*events.borrow(), &["channel:1", "check", "install"]);
        // Once staged, the last row is the restart.
        app.set_update_can_install(false);
        app.set_update_ready(true);
        slint::platform::update_timers_and_animations();
        events.borrow_mut().clear();
        let text = char::from(Key::Return).to_string().into();
        window.dispatch_event(WindowEvent::KeyPressed { text });
        assert_eq!(&*events.borrow(), &["restart"]);
        app.hide().unwrap();
    }
    fn status(phase: &str, available: Option<&str>) -> Status {
        Status {
            installed: "v0.1.0-alpha.20260913.121".into(),
            channel: Channel::Alpha,
            available: available.map(str::to_owned),
            notes: String::new(),
            phase: phase.into(),
            message: String::new(),
            checked_at: None,
            can_install: available.is_some() && phase == "idle",
            automatic_checks: true,
        }
    }
    #[test]
    fn the_check_row_says_where_things_stand() {
        assert_eq!(summary(&status("idle", None), false), "Up to date");
        assert_eq!(
            summary(&status("idle", Some("v0.1.0-alpha.20260913.122")), false),
            ".122 available"
        );
        assert_eq!(
            summary(&status("idle", Some("v0.2.0")), false),
            "v0.2.0 available"
        );
        assert_eq!(summary(&status("checking", None), true), "Checking…");
        assert_eq!(
            summary(&status("downloading", Some("x")), true),
            "Downloading…"
        );
        assert_eq!(
            summary(&status("ready", Some("x")), false),
            "Ready to install"
        );
        assert_eq!(summary(&status("error", None), false), "Failed");
        assert_eq!(channel_label(Channel::Stable), "Stable");
    }
}
