//! Native camera view. One decoder and one latest frame, scoped to the screen.
use crate::App;
use couch_unifi_protect::{
    player::{Player, Status, HEIGHT, WIDTH},
    settings::Settings,
};
use slint::ComponentHandle;
use std::{cell::RefCell, rc::Rc};
pub struct Cameras {
    pending: Rc<RefCell<Option<String>>>,
    /// A Back that has been pressed and not yet performed, so the frame loop
    /// sees the screen close where it can do something about it.
    closing: Rc<std::cell::Cell<bool>>,
    player: Option<Player>,
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
                    .and_then(|(_, d)| config.resolve_integration(&d.integration))
            });
            if let Some(couch_model::Integration::UnifiProtect { camera_id }) = configured {
                if let Some((connection, camera)) = camera_id.split_once('/') {
                    if let Ok(settings) =
                        Settings::load(&crate::connections::file(connection, "protect"))
                    {
                        self.player = Some(Player::start(settings, camera.into()));
                    }
                }
            }
            if self.player.is_none() {
                app.set_camera_message("Set up this camera in the web app.".into());
            }
        }
        if let Some(player) = &self.player {
            if let Some(frame) = player.take_frame() {
                let buffer = slint::SharedPixelBuffer::<slint::Rgb8Pixel>::clone_from_slice(
                    &frame, WIDTH, HEIGHT,
                );
                app.set_camera_image(slint::Image::from_rgb8(buffer));
            }
            app.set_camera_message(match player.status(){
                Status::Connecting=>"Connecting securely…",Status::Playing=>"LIVE · Low quality · View closes after 60 seconds",
                Status::Ended=>"View ended. Go back and open the camera to watch again.",
                Status::Unavailable=>"Camera unavailable. Check its low-quality stream, certificate trust and connection.",
            }.into());
        }
    }
}
