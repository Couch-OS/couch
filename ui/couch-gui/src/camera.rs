//! Native camera view. One decoder and one latest frame, scoped to the screen.
use crate::App;
use couch_camera::{HEIGHT, WIDTH};
use couch_unifi_protect::{
    player::{Failure, Player, Status},
    settings::Settings,
    Error,
};
use slint::ComponentHandle;
use std::{
    cell::RefCell,
    io::{Read, Write},
    process::Child,
    rc::Rc,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

pub(crate) enum CameraTarget {
    BuiltIn {
        connection: String,
        resource: String,
    },
    Package {
        connection: String,
        resource: String,
    },
}

pub(crate) fn target(
    config: &couch_model::Config,
    device: &couch_model::Device,
) -> Option<CameraTarget> {
    let resolved = config.resolve_integration(&device.integration)?;
    match resolved {
        couch_model::Integration::UnifiProtect { camera_id } => {
            let (connection, resource) = camera_id.split_once('/')?;
            Some(CameraTarget::BuiltIn {
                connection: connection.into(),
                resource: resource.into(),
            })
        }
        couch_model::Integration::Plugin {
            connection_id,
            resource_id,
            child: Some(_),
            ..
        } if config
            .device_child_kind(&device.integration)
            .is_some_and(|kind| kind.component == couch_model::ChildComponent::Camera) =>
        {
            Some(CameraTarget::Package {
                connection: connection_id.to_string(),
                resource: resource_id,
            })
        }
        _ => None,
    }
}

enum CameraPlayer {
    BuiltIn(Player),
    Package(PackagePlayer),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PackageStatus {
    Connecting,
    Playing,
    Ended,
    Unavailable,
}

enum PackageFailure {
    Open(couch_plugin::Failure),
    Decoder(couch_camera::DecoderFailure),
    Stream(couch_plugin::Error),
}

struct PackagePlayer {
    stop: Arc<AtomicBool>,
    interrupt: Arc<Mutex<Option<couch_plugin::LocalCameraInterrupt>>>,
    child: Arc<Mutex<Option<Child>>>,
    latest: Arc<Mutex<Option<Vec<u8>>>>,
    status: Arc<Mutex<PackageStatus>>,
    failure: Arc<Mutex<Option<PackageFailure>>>,
}

impl PackagePlayer {
    fn start(connection: String, resource: String) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let interrupt = Arc::new(Mutex::new(None));
        let child = Arc::new(Mutex::new(None));
        let latest = Arc::new(Mutex::new(None));
        let status = Arc::new(Mutex::new(PackageStatus::Connecting));
        let failure = Arc::new(Mutex::new(None));
        let player = Self {
            stop: stop.clone(),
            interrupt: interrupt.clone(),
            child: child.clone(),
            latest: latest.clone(),
            status: status.clone(),
            failure: failure.clone(),
        };
        let deadline = Instant::now() + Duration::from_secs(60);
        package_watchdog(stop.clone(), interrupt.clone(), child.clone(), deadline);
        thread::spawn(move || {
            let run = (|| -> Result<(), PackageFailure> {
                let mut camera = couch_plugin::LocalCamera::open(
                    &crate::home::path("plugin.sock"),
                    &connection,
                    &resource,
                    Duration::from_secs(15),
                )
                .map_err(PackageFailure::Open)?;
                *interrupt.lock().unwrap() =
                    Some(camera.interrupter().map_err(PackageFailure::Stream)?);
                if stop.load(Ordering::Acquire) {
                    return Ok(());
                }
                let decoder = couch_camera::Decoder::spawn().map_err(PackageFailure::Decoder)?;
                let (process, mut input, mut output) = decoder.into_parts();
                *child.lock().unwrap() = Some(process);
                let frame_stop = stop.clone();
                let frame_latest = latest.clone();
                let frame_status = status.clone();
                let reader = thread::spawn(move || {
                    while !frame_stop.load(Ordering::Acquire) {
                        let mut pixels = vec![0; couch_camera::FRAME_BYTES];
                        if output.read_exact(&mut pixels).is_err() {
                            break;
                        }
                        *frame_latest.lock().unwrap() = Some(pixels);
                        *frame_status.lock().unwrap() = PackageStatus::Playing;
                    }
                });
                let result = (|| -> Result<(), PackageFailure> {
                    while !stop.load(Ordering::Acquire) {
                        let Some(nal) = camera.next_record().map_err(PackageFailure::Stream)?
                        else {
                            break;
                        };
                        input.write_all(&nal).map_err(|error| {
                            PackageFailure::Decoder(couch_camera::DecoderFailure::Input(
                                error.kind(),
                            ))
                        })?;
                    }
                    Ok(())
                })();
                drop(input);
                if let Some(process) = child.lock().unwrap().as_mut() {
                    let _ = process.kill();
                }
                let _ = reader.join();
                result
            })();
            let cancelled = stop.swap(true, Ordering::AcqRel);
            if let Some(interrupt) = interrupt.lock().unwrap().take() {
                interrupt.cancel();
            }
            if let Some(mut process) = child.lock().unwrap().take() {
                let _ = process.kill();
                let _ = process.wait();
            }
            let succeeded = run.is_ok();
            if let Err(reason) = run {
                *failure.lock().unwrap() = Some(reason);
            }
            *status.lock().unwrap() = if succeeded || cancelled || Instant::now() >= deadline {
                PackageStatus::Ended
            } else {
                PackageStatus::Unavailable
            };
        });
        player
    }

    fn take_frame(&self) -> Option<Vec<u8>> {
        self.latest.lock().unwrap().take()
    }

    fn message(&self) -> String {
        match *self.status.lock().unwrap() {
            PackageStatus::Connecting => "Connecting securely…".into(),
            PackageStatus::Playing => "LIVE · Low quality · View closes after 60 seconds".into(),
            PackageStatus::Ended => {
                "View ended. Go back and open the camera to watch again.".into()
            }
            PackageStatus::Unavailable => match self.failure.lock().unwrap().as_ref() {
                Some(PackageFailure::Open(failure))
                    if failure.code == couch_plugin::Error::Unpaired =>
                {
                    "Reconnect this NVR in the Couch web app.".into()
                }
                Some(PackageFailure::Open(failure)) => match &failure.reason {
                    Some(reason) if !reason.text().is_empty() => reason.text().into(),
                    _ => "Could not open this camera integration.".into(),
                },
                Some(PackageFailure::Decoder(failure)) => {
                    let _decoder_stage = failure;
                    "The camera decoder is unavailable.".into()
                }
                Some(PackageFailure::Stream(couch_plugin::Error::Timeout)) => {
                    "The camera stream timed out.".into()
                }
                Some(PackageFailure::Stream(error)) => {
                    let _stream_stage = error;
                    "The camera stream stopped.".into()
                }
                None => "Camera unavailable.".into(),
            },
        }
    }
}

impl Drop for PackagePlayer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(interrupt) = self.interrupt.lock().unwrap().as_ref() {
            interrupt.cancel();
        }
        if let Some(process) = self.child.lock().unwrap().as_mut() {
            let _ = process.kill();
        }
    }
}

fn package_watchdog(
    stop: Arc<AtomicBool>,
    interrupt: Arc<Mutex<Option<couch_plugin::LocalCameraInterrupt>>>,
    child: Arc<Mutex<Option<Child>>>,
    deadline: Instant,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        while !stop.load(Ordering::Acquire) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(25));
        }
        stop.store(true, Ordering::Release);
        if let Some(interrupt) = interrupt.lock().unwrap().as_ref() {
            interrupt.cancel();
        }
        if let Some(process) = child.lock().unwrap().as_mut() {
            let _ = process.kill();
        }
    })
}

pub struct Cameras {
    pending: Rc<RefCell<Option<String>>>,
    /// A Back that has been pressed and not yet performed, so the frame loop
    /// sees the screen close where it can do something about it.
    closing: Rc<std::cell::Cell<bool>>,
    player: Option<CameraPlayer>,
    serial: Option<u64>,
}
impl Cameras {
    pub fn new(app: &App) -> Self {
        let pending = Rc::new(RefCell::new(None));
        let queue = pending.clone();
        let weak = app.as_weak();
        app.on_open_camera(move |resource, name| {
            if let Some(app) = weak.upgrade() {
                app.set_camera_title(name);
                app.set_camera_message("Connecting securely…".into());
                app.set_camera_image(slint::Image::default());
                app.set_camera_shown(true);
                app.invoke_focus_camera();
                *queue.borrow_mut() = Some(resource.to_string());
            }
        });
        let closing = Rc::new(std::cell::Cell::new(false));
        let asked = closing.clone();
        app.on_close_camera(move || {
            // Not closed here. The loop reads which screen is up before it
            // reads the keys, so a screen that takes itself down inside a key
            // callback is gone before anything notices, and it gets no
            // transition at all - it would lift out of its row on the way in
            // and vanish on the way out. Queued for the poll below instead,
            // which is where every other device screen closes.
            asked.set(true);
        });
        Self {
            pending,
            closing,
            player: None,
            serial: None,
        }
    }
    /// Whether a Back is waiting to close the feed, so the frame loop can
    /// keep the picture that is on the panel before it happens.
    pub fn navigation_pending(&self) -> bool {
        self.closing.get()
    }
    pub fn poll(&mut self, app: &App, awake: bool) {
        if self.closing.replace(false) {
            app.set_camera_shown(false);
            app.invoke_focus_light();
        }
        if self.player.is_some()
            && self.serial != crate::config_snapshot::current().map(|s| s.serial)
        {
            app.invoke_close_camera();
        }
        if !awake
            || !app.get_camera_shown()
            || app.get_settings_shown()
            || app.get_dock_clock_shown()
        {
            self.player.take();
            self.pending.borrow_mut().take();
            if app.get_camera_shown() {
                app.invoke_close_camera();
            }
            app.set_camera_image(slint::Image::default());
            return;
        }
        if let Some(resource) = self.pending.borrow_mut().take() {
            self.serial = crate::config_snapshot::current().map(|s| s.serial);
            self.player.take();
            let configured = crate::connections::config().and_then(|config| {
                config
                    .devices()
                    .find(|(_, d)| d.id.as_str() == resource.trim_start_matches("device:"))
                    .and_then(|(_, d)| target(&config, d))
            });
            match configured {
                Some(CameraTarget::BuiltIn {
                    connection,
                    resource,
                }) => {
                    if let Ok(settings) =
                        Settings::load(&crate::connections::file(&connection, "protect"))
                    {
                        self.player =
                            Some(CameraPlayer::BuiltIn(Player::start(settings, resource)));
                    }
                }
                Some(CameraTarget::Package {
                    connection,
                    resource,
                }) => {
                    self.player = Some(CameraPlayer::Package(PackagePlayer::start(
                        connection, resource,
                    )));
                }
                None => (),
            }
            if self.player.is_none() {
                app.set_camera_message("Set up this camera in the web app.".into());
            }
        }
        if let Some(player) = &self.player {
            let frame = match player {
                CameraPlayer::BuiltIn(player) => player.take_frame(),
                CameraPlayer::Package(player) => player.take_frame(),
            };
            if let Some(frame) = frame {
                let buffer = slint::SharedPixelBuffer::<slint::Rgb8Pixel>::clone_from_slice(
                    &frame, WIDTH, HEIGHT,
                );
                app.set_camera_image(slint::Image::from_rgb8(buffer));
            }
            let message: String = match player {
                CameraPlayer::BuiltIn(player) => match player.status() {
                    Status::Connecting => "Connecting securely…".into(),
                    Status::Playing => "LIVE · Low quality · View closes after 60 seconds".into(),
                    Status::Ended => {
                        "View ended. Go back and open the camera to watch again.".into()
                    }
                    Status::Unavailable => unavailable_message(player.failure()).into(),
                },
                CameraPlayer::Package(player) => player.message(),
            };
            app.set_camera_message(message.into());
        }
    }
}

fn unavailable_message(failure: Option<Failure>) -> &'static str {
    match failure {
        Some(Failure::Descriptor(Error::StreamNotEnabled)) => {
            "Enable this camera's low-quality RTSPS stream in Protect."
        }
        Some(Failure::Setup(_)) => "Reconnect this NVR in the Couch web app.",
        Some(Failure::Descriptor(_)) => "Could not get this camera stream from the NVR.",
        Some(Failure::Media(_)) => "Could not open the NVR's secure camera stream.",
        Some(Failure::Decoder(_)) => "The camera decoder is unavailable.",
        Some(Failure::Stream(_)) => "The camera stream stopped.",
        None => "Camera unavailable.",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_protocol_4_camera_child_opens_the_native_camera_screen() {
        use couch_model::{
            ChildComponent, ChildSnapshot, Connection, Device, DeviceKind, Id, Integration,
            PluginChildKind, Provider,
        };

        let mut config = couch_model::Config::seed();
        config.connections.push(Connection {
            id: Id::new("front-nvr"),
            name: "Front NVR".into(),
            provider: Provider::Plugin {
                id: "unifi-protect".into(),
                label: "UniFi Protect".into(),
                capabilities: vec![],
                supports_inputs: false,
                supports_apps: false,
                presentation: vec![],
                actions: vec![],
                children: vec![PluginChildKind {
                    kind: "camera".into(),
                    label: "Camera".into(),
                    device_kind: DeviceKind::Camera,
                    component: ChildComponent::Camera,
                    capabilities: vec![],
                    actions: vec![],
                }],
            },
        });
        let device = Device::new(Id::new("front-yard"), "Front yard", DeviceKind::Camera)
            .with_integration(Integration::Connection {
                connection_id: Id::new("front-nvr"),
                resource_id: "camera-1".into(),
                child: Some(ChildSnapshot {
                    kind: "camera".into(),
                    light: None,
                    cover: None,
                    climate: None,
                }),
            });
        assert!(matches!(
            target(&config, &device),
            Some(CameraTarget::Package { connection, resource })
                if connection == "front-nvr" && resource == "camera-1"
        ));
    }

    #[test]
    fn camera_failures_name_the_actionable_stage_without_secrets() {
        assert!(
            unavailable_message(Some(Failure::Descriptor(Error::StreamNotEnabled)))
                .contains("low-quality")
        );
        assert!(unavailable_message(Some(Failure::Decoder(
            couch_camera::DecoderFailure::MissingPipe
        )))
        .contains("decoder"));
        assert!(unavailable_message(Some(Failure::Media(Error::MediaTls))).contains("secure"));
    }
}
