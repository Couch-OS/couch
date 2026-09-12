//! Deferred callbacks keep keyboard focus changes outside Slint event dispatch.
use crate::{
    network::{self, Event, Request},
    App, ChoiceItem,
};
use slint::{ModelRc, VecModel};
use std::{cell::RefCell, collections::VecDeque, rc::Rc};
#[derive(Clone, Copy, PartialEq)]
enum Step {
    Closed,
    Welcome,
    Recovery,
    Handoff,
    Hotspot,
    Scan,
    List,
    Ssid,
    Password,
    Review,
    Testing,
    Tested,
    Saving,
    Cancelling,
    Done,
}
enum Input {
    Start,
    Recovery,
    Pick(i32),
    Back,
    Text(String),
}
pub struct Controller {
    worker: network::Worker,
    input: Rc<RefCell<VecDeque<Input>>>,
    step: Step,
    networks: Vec<network::Network>,
    ssid: String,
    password: String,
    secured: bool,
    onboarding: bool,
    ip: String,
}
impl Controller {
    pub fn install(app: &App) -> Self {
        let input = Rc::new(RefCell::new(VecDeque::new()));
        let queue = input.clone();
        app.on_setting_change_wifi(move || queue.borrow_mut().push_back(Input::Start));
        let queue = input.clone();
        app.on_setting_recovery_wifi(move || queue.borrow_mut().push_back(Input::Recovery));
        let queue = input.clone();
        app.on_wifi_setup_activate(move |i| queue.borrow_mut().push_back(Input::Pick(i)));
        let queue = input.clone();
        app.on_wifi_setup_back(move || queue.borrow_mut().push_back(Input::Back));
        let queue = input.clone();
        app.on_keyboard_accepted(move |s| queue.borrow_mut().push_back(Input::Text(s.to_string())));
        let queue = input.clone();
        app.on_keyboard_cancelled(move || queue.borrow_mut().push_back(Input::Back));
        if std::path::Path::new("/tmp/couch.onboarding").exists() {
            input.borrow_mut().push_back(Input::Start);
        }
        Self {
            worker: network::Worker::start(),
            input,
            step: Step::Closed,
            networks: Vec::new(),
            ssid: String::new(),
            password: String::new(),
            secured: false,
            onboarding: std::path::Path::new("/tmp/couch.onboarding").exists(),
            ip: String::new(),
        }
    }
    fn page(&self, app: &App, title: &str, detail: &str, labels: Vec<(String, String)>) {
        app.set_wifi_setup_qr(slint::Image::default());
        app.set_wifi_setup_has_qr(false);
        app.set_settings_shown(false);
        app.set_wifi_setup_shown(true);
        app.set_wifi_setup_title(title.into());
        app.set_wifi_setup_detail(detail.into());
        app.set_wifi_setup_items(ModelRc::new(VecModel::from(
            labels
                .into_iter()
                .map(|(title, detail)| ChoiceItem {
                    title: title.into(),
                    detail: detail.into(),
                    active: false,
                    light: false,
                    power_known: false,
                    icon: slint::Image::default(),
                })
                .collect::<Vec<_>>(),
        )));
        app.invoke_focus_wifi_setup();
    }
    fn buttons(&self, app: &App, title: &str, detail: &str, buttons: &[&str]) {
        self.page(
            app,
            title,
            detail,
            buttons
                .iter()
                .map(|s| (s.to_string(), String::new()))
                .collect(),
        );
    }
    fn list(&mut self, app: &App, detail: &str) {
        self.step = Step::List;
        self.password.clear();
        let mut rows: Vec<_> = self
            .networks
            .iter()
            .map(|n| {
                (
                    n.ssid.clone(),
                    format!(
                        "{} dBm · {}",
                        n.dbm,
                        if !n.supported {
                            "Unsupported security"
                        } else if n.secured {
                            "Password required"
                        } else {
                            "Open network"
                        }
                    ),
                )
            })
            .collect();
        rows.extend([
            (
                "Enter a hidden network".into(),
                "Type its network name".into(),
            ),
            ("Scan again".into(), String::new()),
            ("Cancel".into(), String::new()),
        ]);
        self.page(app, "Choose a network", detail, rows);
    }
    fn scan(&mut self, app: &App) {
        self.step = Step::Scan;
        self.worker.send(Request::Scan);
        self.buttons(
            app,
            "Finding networks",
            "Scanning nearby wireless networks…",
            &["Cancel"],
        );
    }
    fn keyboard(&mut self, app: &App, password: bool) {
        self.step = if password { Step::Password } else { Step::Ssid };
        app.set_keyboard_title(
            if password {
                "WI-FI PASSWORD"
            } else {
                "NETWORK NAME"
            }
            .into(),
        );
        app.set_keyboard_placeholder(
            if password {
                if self.secured {
                    "Enter the network password"
                } else {
                    "Leave blank for an open network"
                }
            } else {
                "Enter the hidden network name"
            }
            .into(),
        );
        app.set_keyboard_password(password);
        app.invoke_open_keyboard();
    }
    fn review(&mut self, app: &App, error: &str) {
        self.step = Step::Review;
        self.buttons(app,"Test this network",&format!("{}\n{}\nTesting temporarily switches Wi-Fi. Credentials are saved only when you choose Save.",self.ssid,error),&["Test connection","Edit password","Choose another network","Cancel"]);
    }
    fn cancel(&mut self, app: &App) {
        self.step = Step::Cancelling;
        self.password.clear();
        self.worker.send(Request::Cancel);
        self.buttons(
            app,
            "Restoring connection",
            "Returning to your previous network…",
            &[],
        );
    }
    fn welcome(&mut self, app: &App) {
        self.step = Step::Welcome;
        self.buttons(
            app,
            "Welcome to couch.",
            "Connect to Wi-Fi here, then add your rooms and devices in the browser.",
            &["Set up Wi-Fi", "Recovery hotspot", "Set up later"],
        );
    }
    fn handoff(&mut self, app: &App) {
        self.step = Step::Handoff;
        let url = format!("http://{}:8090", self.ip);
        self.buttons(app, "Make it yours", &format!("{url}\nScan from the same Wi-Fi network. Set your clock, then add connections and rooms."), &["Finish"]);
        if let Some(qr) = crate::qr::render(&url, 180) {
            app.set_wifi_setup_qr(qr);
            app.set_wifi_setup_has_qr(true);
        }
    }
    fn close(&mut self, app: &App) {
        if self.onboarding && self.step != Step::Handoff {
            self.welcome(app);
            return;
        }
        self.step = Step::Closed;
        self.password.clear();
        app.set_wifi_setup_shown(false);
        app.set_settings_shown(!self.onboarding);
        if self.onboarding {
            let _ = std::fs::remove_file("/tmp/couch.onboarding");
            self.onboarding = false;
            app.invoke_focus_home();
        }
    }
    pub fn poll(&mut self, app: &App) {
        loop {
            let input = self.input.borrow_mut().pop_front();
            let Some(input) = input else { break };
            match input {
                Input::Start if self.step == Step::Closed => {
                    if self.onboarding {
                        self.welcome(app);
                    } else {
                        self.scan(app);
                    }
                }
                Input::Recovery if self.step == Step::Closed => {
                    self.step = Step::Recovery;
                    self.buttons(app, "Recovery hotspot", "This disconnects Wi-Fi and opens Couch-Setup for browser-based recovery. Restart the remote to return to normal Wi-Fi.", &["Start hotspot", "Cancel"]);
                }
                Input::Back => match self.step {
                    Step::Closed | Step::Cancelling | Step::Saving | Step::Hotspot => {}
                    Step::Tested | Step::Testing | Step::Scan => self.cancel(app),
                    Step::Ssid | Step::Password | Step::Review => {
                        self.list(app, "Select a network or enter one manually.")
                    }
                    _ => self.close(app),
                },
                Input::Text(s) => match self.step {
                    Step::Ssid => {
                        self.ssid = s;
                        self.secured = false;
                        self.keyboard(app, true);
                    }
                    Step::Password => {
                        self.password = s;
                        self.review(app, "");
                    }
                    _ => {}
                },
                Input::Pick(i) if i >= 0 => {
                    let i = i as usize;
                    match self.step {
                        Step::Welcome => match i {
                            0 => self.scan(app),
                            1 => {
                                self.step = Step::Recovery;
                                self.buttons(app, "Recovery hotspot", "This opens Couch-Setup for browser-based recovery. Restart the remote to return to local setup.", &["Start hotspot", "Cancel"]);
                            }
                            _ => {
                                self.step = Step::Handoff;
                                self.close(app);
                            }
                        },
                        Step::Recovery => {
                            if i == 0 {
                                self.worker.send(Request::Hotspot);
                                self.step = Step::Hotspot;
                                self.buttons(
                                    app,
                                    "Starting recovery hotspot",
                                    "Preparing Couch-Setup…",
                                    &[],
                                );
                            } else {
                                self.close(app);
                            }
                        }
                        Step::Handoff => self.close(app),
                        Step::List => {
                            if let Some(n) = self.networks.get(i).cloned() {
                                if !n.supported {
                                    self.list(app,"This network needs enterprise, WEP or unsupported security.");
                                    continue;
                                }
                                self.ssid = n.ssid;
                                self.secured = n.secured;
                                if n.secured {
                                    self.keyboard(app, true);
                                } else {
                                    self.password.clear();
                                    self.review(app, "Open network — no password.");
                                }
                            } else if i == self.networks.len() {
                                self.keyboard(app, false);
                            } else if i == self.networks.len() + 1 {
                                self.scan(app);
                            } else {
                                self.close(app);
                            }
                        }
                        Step::Review => match i {
                            0 => {
                                if self.secured && self.password.is_empty() {
                                    self.review(app, "This network requires a password.");
                                    continue;
                                }
                                self.worker.send(Request::Test {
                                    ssid: self.ssid.clone(),
                                    password: std::mem::take(&mut self.password),
                                });
                                self.step = Step::Testing;
                                self.buttons(app,"Testing connection","Checking authentication and an assigned IP address. This may take up to 35 seconds.",&["Cancel & restore"]);
                            }
                            1 => self.keyboard(app, true),
                            2 => self.list(app, "Choose a different network."),
                            _ => self.close(app),
                        },
                        Step::Tested => {
                            if i == 0 {
                                self.step = Step::Saving;
                                self.worker.send(Request::Save);
                                self.buttons(
                                    app,
                                    "Saving network",
                                    "Writing the tested configuration…",
                                    &[],
                                );
                            } else {
                                self.cancel(app);
                            }
                        }
                        Step::Scan | Step::Testing => self.cancel(app),
                        Step::Done => self.close(app),
                        _ => {}
                    }
                }
                _ => {}
            }
        }
        while let Ok(event) = self.worker.rx.try_recv() {
            if self.step == Step::Cancelling && !matches!(event, Event::Cancelled(_)) {
                continue;
            }
            match event {
                Event::Hotspot(Ok(())) => {
                    self.onboarding = false;
                    self.close(app);
                }
                Event::Hotspot(Err(error)) => {
                    self.step = Step::Recovery;
                    self.buttons(app, "Could not start hotspot", &error, &["Retry", "Cancel"]);
                }
                Event::Scanned(Ok(networks)) => {
                    self.networks = networks;
                    self.list(app, "Choose a network or enter one that is not shown.");
                }
                Event::Scanned(Err(e)) => self.list(app, &e),
                Event::Tested(Ok((ssid, ip))) => {
                    self.ip = ip.clone();
                    self.step = Step::Tested;
                    self.buttons(app,"Connection test passed",&format!("{ssid}\nIP address: {ip}\nSave within 60 seconds, or the previous network will be restored. Internet access was not tested."),&["Save network","Cancel & restore"]);
                }
                Event::Tested(Err(e)) => self.review(app, &e),
                Event::Saved(Ok(())) => {
                    if self.onboarding {
                        self.handoff(app);
                        continue;
                    }
                    self.step = Step::Done;
                    self.buttons(
                        app,
                        "Network saved",
                        &format!("{} will be available after reboot.", self.ssid),
                        &["Done"],
                    );
                }
                Event::Saved(Err(e)) => {
                    self.step = Step::Tested;
                    self.buttons(
                        app,
                        "Could not save",
                        &e,
                        &["Retry save", "Cancel & restore"],
                    );
                }
                Event::Cancelled(Ok(())) => self.close(app),
                Event::Cancelled(Err(e)) | Event::Expired(Err(e)) => {
                    self.step = Step::Done;
                    self.buttons(app, "Connection needs attention", &e, &["Done"]);
                }
                Event::Expired(Ok(())) => {
                    self.list(app, "Save timed out. Your previous network was restored.")
                }
            }
        }
    }
}
