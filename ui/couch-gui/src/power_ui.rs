//! The Power section of the remote's Settings menu: power off, restart, and
//! restart into recovery.
//!
//! Each goes to the root system service the way the Updates section's restart
//! does. Recovery is armed by one OK and fired by a second within a few
//! seconds: it leaves the remote on a screen with no UI until the operator
//! clears the flag over USB or SSH, so it must not happen by accident. Power
//! off and restart act on the first OK.
use crate::App;
use couch_system::{
    client,
    power::Action,
    protocol::{Reply, Request},
};
use std::{
    cell::RefCell,
    collections::VecDeque,
    rc::Rc,
    sync::mpsc,
    time::{Duration, Instant},
};

/// The Settings panel index this section owns.
pub const PANEL: i32 = 6;
/// How long a first OK on recovery stays armed for the second.
const ARM: Duration = Duration::from_secs(6);

enum Outcome {
    Accepted(Action),
    Failed(String),
}
pub struct Controller {
    input: Rc<RefCell<VecDeque<Action>>>,
    tx: mpsc::Sender<Action>,
    rx: mpsc::Receiver<Outcome>,
    busy: bool,
    armed_until: Option<Instant>,
    notice_until: Option<Instant>,
    open: bool,
}
impl Controller {
    pub fn install(app: &App) -> Self {
        let input = Rc::new(RefCell::new(VecDeque::new()));
        let q = input.clone();
        app.on_power_action(move |row| {
            let action = match row {
                0 => Action::Off,
                1 => Action::Restart,
                _ => Action::Recovery,
            };
            q.borrow_mut().push_back(action);
        });
        let (tx, requests) = mpsc::channel::<Action>();
        let (outcomes, rx) = mpsc::channel();
        std::thread::spawn(move || {
            while let Ok(action) = requests.recv() {
                let outcome = match client::call(Request::Power { action }) {
                    Ok(Reply::Done(Ok(()))) => Outcome::Accepted(action),
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
            busy: false,
            armed_until: None,
            notice_until: None,
            open: false,
        }
    }
    fn say(&mut self, app: &App, text: &str, for_: Duration) {
        app.set_power_message(text.into());
        self.notice_until = Some(Instant::now() + for_);
    }
    fn disarm(&mut self, app: &App) {
        self.armed_until = None;
        app.set_power_armed(false);
    }
    pub fn poll(&mut self, app: &App) {
        let open = app.get_settings_shown() && app.get_settings_panel() == PANEL;
        if open != self.open {
            self.open = open;
            self.disarm(app);
            if open {
                app.set_power_message("".into());
                self.notice_until = None;
            } else {
                self.input.borrow_mut().clear();
            }
        }
        while let Ok(outcome) = self.rx.try_recv() {
            match outcome {
                Outcome::Accepted(action) => {
                    // Stays busy: the remote is about to go away.
                    self.say(app, action.label(), Duration::from_secs(120));
                }
                Outcome::Failed(error) => {
                    self.busy = false;
                    app.set_power_busy(false);
                    self.say(app, &error, Duration::from_secs(8));
                }
            }
        }
        if !open {
            return;
        }
        loop {
            let Some(action) = self.input.borrow_mut().pop_front() else {
                break;
            };
            if self.busy {
                continue;
            }
            match action {
                Action::Recovery
                    if !self.armed_until.is_some_and(|until| Instant::now() < until) =>
                {
                    self.armed_until = Some(Instant::now() + ARM);
                    app.set_power_armed(true);
                    self.say(
                        app,
                        "Press OK again to restart into recovery. Recovery has no remote UI: it brings up Wi-Fi and SSH and a shell on USB, and the remote stays there until the flag is cleared (docs/device-recovery.md).",
                        ARM,
                    );
                }
                action => {
                    self.disarm(app);
                    self.busy = true;
                    app.set_power_busy(true);
                    self.say(app, "Asking the system service…", Duration::from_secs(20));
                    if self.tx.send(action).is_err() {
                        self.busy = false;
                        app.set_power_busy(false);
                        self.say(app, "System service is unavailable", Duration::from_secs(8));
                    }
                }
            }
        }
        if self
            .armed_until
            .is_some_and(|until| Instant::now() >= until)
        {
            self.disarm(app);
            if self
                .notice_until
                .is_some_and(|until| Instant::now() >= until)
            {
                app.set_power_message("".into());
            }
        }
        if self
            .notice_until
            .is_some_and(|until| Instant::now() >= until)
        {
            self.notice_until = None;
            if !self.busy {
                app.set_power_message("".into());
            }
        }
    }
}
