//! Activity lifecycle: sequence I/O stays on a worker; leaving the controls does
//! not power devices off. Failed sequences stop at the failed step, without retry.
use crate::App;
use couch_model::{Config, SequenceStep};
use std::{
    cell::RefCell,
    collections::HashMap,
    rc::Rc,
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc, Arc,
    },
    time::Duration,
};
enum Intent {
    Open(String),
    Stop,
    Cancel,
}
struct Job {
    id: String,
    config: Arc<Config>,
    steps: Vec<SequenceStep>,
    start: bool,
    generation: u64,
}
enum Event {
    Progress(u64, usize, usize),
    Done(Job, Result<(), String>),
}
pub struct Controller {
    intents: Rc<RefCell<Vec<Intent>>>,
    jobs: mpsc::SyncSender<Job>,
    events: mpsc::Receiver<Event>,
    generation: Arc<AtomicU64>,
    running: HashMap<String, Arc<Config>>,
    busy: bool,
}
fn execute_sequence(
    job: &Job,
    generation: &AtomicU64,
    mut progress: impl FnMut(usize, usize),
) -> Result<(), String> {
    let mut denon = HashMap::new();
    let mut tv = HashMap::new();
    let mut streaming = HashMap::new();
    execute_steps(job, generation, &mut progress, |action| {
        crate::activity_buttons::execute(&job.config, action, &mut denon, &mut tv, &mut streaming)
    })
}
fn execute_steps(
    job: &Job,
    generation: &AtomicU64,
    mut progress: impl FnMut(usize, usize),
    mut command: impl FnMut(&couch_model::Action) -> Result<(), String>,
) -> Result<(), String> {
    for (index, step) in job.steps.iter().enumerate() {
        if generation.load(Ordering::SeqCst) != job.generation {
            return Err("Canceled".into());
        }
        progress(index + 1, job.steps.len());
        match step {
            SequenceStep::Command { action } => command(action).map_err(|e| {
                format!(
                    "Step {} stopped: {e}. Earlier commands were not undone.",
                    index + 1
                )
            })?,
            SequenceStep::Delay { ms } => {
                let mut remaining = *ms;
                while remaining > 0 {
                    if generation.load(Ordering::SeqCst) != job.generation {
                        return Err("Canceled".into());
                    }
                    let pause = remaining.min(50);
                    std::thread::sleep(Duration::from_millis(pause.into()));
                    remaining -= pause;
                }
            }
        }
    }
    Ok(())
}
impl Controller {
    pub fn new(app: &App) -> Self {
        let intents = Rc::new(RefCell::new(Vec::new()));
        let queue = intents.clone();
        app.on_open_activity(move |id| queue.borrow_mut().push(Intent::Open(id.to_string())));
        let queue = intents.clone();
        app.on_end_activity(move || queue.borrow_mut().push(Intent::Stop));
        let queue = intents.clone();
        app.on_cancel_activity(move || queue.borrow_mut().push(Intent::Cancel));
        let (jobs, rx) = mpsc::sync_channel::<Job>(1);
        let (send, events) = mpsc::channel();
        let generation = Arc::new(AtomicU64::new(0));
        let current = generation.clone();
        std::thread::spawn(move || {
            while let Ok(job) = rx.recv() {
                let result = execute_sequence(&job, &current, |step, total| {
                    let _ = send.send(Event::Progress(job.generation, step, total));
                });
                let _ = send.send(Event::Done(job, result));
            }
        });
        Self {
            intents,
            jobs,
            events,
            generation,
            running: HashMap::new(),
            busy: false,
        }
    }
    pub fn poll(&mut self, app: &App) -> Option<String> {
        let mut error = None;
        for intent in std::mem::take(&mut *self.intents.borrow_mut()) {
            if matches!(intent, Intent::Cancel) {
                self.generation.fetch_add(1, Ordering::SeqCst);
                self.busy = false;
                app.set_activity_busy(false);
                error = Some("Activity canceled. Completed commands were not undone.".into());
                continue;
            }
            if self.busy {
                continue;
            }
            let (id, start, config) = match intent {
                Intent::Open(id) => {
                    if id.starts_with("device:") || self.running.contains_key(&id) {
                        app.invoke_open_activity_ready(id.into());
                        continue;
                    }
                    let Some(config) = crate::connections::config() else {
                        error = Some("Configuration unavailable".into());
                        continue;
                    };
                    (id, true, config)
                }
                Intent::Stop => {
                    let id = app.get_active_activity().to_string();
                    let Some(config) = self.running.get(&id).cloned() else {
                        continue;
                    };
                    (id, false, config)
                }
                Intent::Cancel => unreachable!(),
            };
            let Some(activity) = config.activities.iter().find(|a| a.id.as_str() == id) else {
                error = Some("Activity was removed".into());
                continue;
            };
            if start
                && !(activity.setup.custom_screen && !activity.setup.pages.is_empty())
                && !activity.source.as_ref().is_some_and(|id| {
                    config
                        .devices()
                        .find(|(_, d)| &d.id == id)
                        .and_then(|(_, d)| config.resolve_integration(&d.integration))
                        .is_some_and(|i| {
                            matches!(
                                i,
                                couch_model::Integration::Sonos { .. } | couch_model::Integration::Kodi { .. }
                                    | couch_model::Integration::WebOs
                                    | couch_model::Integration::AndroidTv
                                    | couch_model::Integration::AppleTv
                            )
                        })
                })
            {
                error = Some(
                    "Choose custom pages or a main Kodi/TV screen in the web UI before starting."
                        .into(),
                );
                continue;
            }
            let steps = if start {
                activity.setup.on.clone()
            } else {
                activity.setup.off.clone()
            };
            let title = format!(
                "{} {}",
                if start { "Starting" } else { "Ending" },
                activity.name
            );
            let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
            if self
                .jobs
                .try_send(Job {
                    id,
                    config,
                    steps,
                    start,
                    generation,
                })
                .is_err()
            {
                error = Some("Activity worker is busy".into());
                continue;
            }
            self.busy = true;
            app.set_activity_busy(true);
            app.set_activity_progress(title.into());
            app.set_activity_step("".into());
        }
        for event in self.events.try_iter() {
            match event {
                Event::Progress(generation, step, total)
                    if generation == self.generation.load(Ordering::SeqCst) =>
                {
                    app.set_activity_step(format!("Step {step} of {total}").into())
                }
                Event::Done(job, result)
                    if job.generation == self.generation.load(Ordering::SeqCst) =>
                {
                    self.busy = false;
                    app.set_activity_busy(false);
                    app.set_activity_step("".into());
                    match result {
                        Err(e) => error = Some(e),
                        Ok(()) if job.start => {
                            self.running.insert(job.id.clone(), job.config);
                            app.invoke_open_activity_ready(job.id.into());
                        }
                        Ok(()) => {
                            self.running.remove(&job.id);
                            if app.get_tv_shown() {
                                app.invoke_tv_action("close".into());
                            }
                            if app.get_player_shown() {
                                app.set_player_panel(0);
                                app.invoke_player_action("back".into(), 0.);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        app.set_activity_running(
            self.running
                .contains_key(app.get_active_activity().as_str()),
        );
        app.set_activity_keep_awake(self.running.iter().any(|(id, c)| {
            c.activities
                .iter()
                .any(|a| a.id.as_str() == id && a.setup.keep_awake)
        }));
        error
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn commands_execute_in_order_once_with_delay_between_them() {
        let generation = AtomicU64::new(1);
        let job = Job {
            id: "test".into(),
            config: Arc::new(Config::default()),
            start: true,
            generation: 1,
            steps: vec![
                SequenceStep::Command {
                    action: couch_model::Action::new("tv", "power-on"),
                },
                SequenceStep::Delay { ms: 60 },
                SequenceStep::Command {
                    action: couch_model::Action::new("avr", "input:BD"),
                },
            ],
        };
        let mut commands = Vec::new();
        execute_steps(
            &job,
            &generation,
            |_, _| {},
            |action| {
                commands.push((
                    action.device.to_string(),
                    action.command.clone(),
                    std::time::Instant::now(),
                ));
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(commands.len(), 2);
        assert_eq!((&commands[0].0[..], &commands[0].1[..]), ("tv", "power-on"));
        assert_eq!(
            (&commands[1].0[..], &commands[1].1[..]),
            ("avr", "input:BD")
        );
        assert!(commands[1].2.duration_since(commands[0].2) >= Duration::from_millis(60));
    }
    #[test]
    fn cancellation_after_inflight_command_prevents_next_command() {
        let generation = AtomicU64::new(1);
        let job = Job {
            id: "test".into(),
            config: Arc::new(Config::default()),
            start: true,
            generation: 1,
            steps: vec![
                SequenceStep::Command {
                    action: couch_model::Action::new("tv", "power-on")
                };
                2
            ],
        };
        let mut calls = 0;
        let result = execute_steps(
            &job,
            &generation,
            |_, _| {},
            |_| {
                calls += 1;
                generation.store(2, Ordering::SeqCst);
                Ok(())
            },
        );
        assert_eq!(result, Err("Canceled".into()));
        assert_eq!(calls, 1);
    }
    #[test]
    fn cancellation_during_delay_prevents_following_command() {
        let generation = AtomicU64::new(1);
        let job = Job {
            id: "test".into(),
            config: Arc::new(Config::default()),
            steps: vec![
                SequenceStep::Delay { ms: 500 },
                SequenceStep::Command {
                    action: couch_model::Action::new("missing", "on"),
                },
            ],
            start: true,
            generation: 1,
        };
        let mut seen = Vec::new();
        let result = execute_sequence(&job, &generation, |step, _| {
            seen.push(step);
            generation.store(2, Ordering::SeqCst);
        });
        assert_eq!(result, Err("Canceled".into()));
        assert_eq!(seen, vec![1]);
    }
    #[test]
    fn first_failure_stops_the_sequence_without_attempting_later_steps() {
        let generation = AtomicU64::new(1);
        let job = Job {
            id: "test".into(),
            config: Arc::new(Config::default()),
            steps: vec![
                SequenceStep::Command {
                    action: couch_model::Action::new("missing", "on"),
                },
                SequenceStep::Delay { ms: 1000 },
            ],
            start: true,
            generation: 1,
        };
        let mut seen = Vec::new();
        let result = execute_sequence(&job, &generation, |step, _| seen.push(step));
        assert!(result.unwrap_err().starts_with("Step 1 stopped:"));
        assert_eq!(seen, vec![1]);
    }
}
