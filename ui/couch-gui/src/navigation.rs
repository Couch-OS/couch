//! Navigation owns framebuffer snapshot/slide ordering and UI focus restoration.
use crate::{
    home::Area,
    panel::{Arrive, Panel, SlideCost, SLIDE},
    system, App,
};
use slint::platform::software_renderer::MinimalSoftwareWindow;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};
/// A page change the UI asked for. The callback that asked records it and
/// nothing else; the loop performs it once the event that caused it has been
/// dispatched. A transition renders a frame and draws to the panel, and the
/// window's `draw_if_needed` cannot be re-entered from inside a Slint
/// callback - so a callback only ever says what it wants, and the ways the
/// chooser closes from inside app.slint are callbacks too, for the same
/// reason.
#[derive(Copy, Clone, Debug)]
pub enum Intent {
    /// Step the area by this many, wrapping; the sign is the direction.
    Area(i32),
    /// Show the chooser. Its content is already set by the time this is asked.
    OpenChooser,
    CloseChooser,
    /// The settings menu, and its tiers - all slid the way the chooser is.
    OpenSettings,
    /// Enter a settings panel (1 display, 2 wifi, 3 ssh); slides from the right.
    SettingsEnter(i32),
    /// Climb a settings tier, or close from the root; slides from the left.
    SettingsBack,
}

pub struct Navigator<F: Fn(&App, usize)> {
    areas: Rc<RefCell<Vec<Area>>>,
    current: Rc<Cell<usize>>,
    put_front: F,
    report: bool,
}
impl<F: Fn(&App, usize)> Navigator<F> {
    pub fn new(areas: Rc<RefCell<Vec<Area>>>, current: Rc<Cell<usize>>, put_front: F) -> Self {
        Self {
            areas,
            current,
            put_front,
            report: std::env::var_os("COUCH_REGION").is_some(),
        }
    }
    pub fn transition(
        &self,
        screen: &mut Panel,
        window: &MinimalSoftwareWindow,
        app: &App,
        what: Intent,
    ) -> Option<SlideCost> {
        let areas = &self.areas;
        let current = &self.current;
        let put_front = &self.put_front;
        let report = self.report;
        let chooser = app.get_chooser_shown();
        let status = (0u32, app.get_status_h().round() as u32);
        let dots = (
            app.get_dots_y().round() as u32,
            app.get_dots_h().round() as u32,
        );
        let (above, above_and_pager) = ([status], [status, dots]);
        let from;
        let keep: &[(u32, u32)];
        match what {
            Intent::Area(delta) => {
                let cur = current.get();
                let next = (cur as i32 + delta).rem_euclid(areas.borrow().len() as i32) as usize;
                if next == cur {
                    return None;
                }
                current.set(next);
                if chooser {
                    // The hub is off screen behind the chooser (COUCH_SLIDE
                    // under COUCH_OPEN): change the page where it is. There
                    // is nothing to see, so nothing to slide.
                    put_front(app, next);
                    return None;
                }
                screen.snapshot();
                app.set_ring_hidden(true);
                put_front(app, next);
                from = if delta > 0 {
                    Arrive::FromRight
                } else {
                    Arrive::FromLeft
                };
                keep = &above_and_pager[..];
            }
            Intent::OpenChooser => {
                if chooser {
                    return None;
                }
                screen.snapshot();
                app.set_ring_hidden(true);
                app.set_chooser_shown(true);
                from = Arrive::FromRight;
                keep = &above[..];
            }
            Intent::CloseChooser => {
                if !chooser {
                    return None;
                }
                screen.snapshot();
                app.set_ring_hidden(true);
                app.set_chooser_shown(false);
                from = Arrive::FromLeft;
                keep = &above[..];
            }
            Intent::OpenSettings => {
                if app.get_settings_shown() {
                    return None;
                }
                // Prime settings; Wi-Fi status also refreshes on the service tick.
                app.set_wifi_ssid(system::wifi_ssid().into());
                app.set_wifi_signal(
                    system::wifi_dbm()
                        .map(|dbm| format!("{dbm} dBm"))
                        .unwrap_or_else(|| "—".into())
                        .into(),
                );
                // SSH is not read here: both its reads block on the system
                // service, and the tick probes them on a thread when the menu
                // opens.
                app.set_bt_available(system::bluetooth_available());
                app.set_bt_state(system::bluetooth_state().into());
                screen.snapshot();
                app.set_settings_panel(0);
                app.set_settings_shown(true);
                from = Arrive::FromRight;
                keep = &above[..];
            }
            Intent::SettingsEnter(panel) => {
                if !app.get_settings_shown() {
                    return None;
                }
                screen.snapshot();
                app.set_settings_panel(panel);
                from = Arrive::FromRight;
                keep = &above[..];
            }
            Intent::SettingsBack => {
                if !app.get_settings_shown() {
                    return None;
                }
                screen.snapshot();
                // From a panel, back to the root; from the root, out to the
                // hub. Both slide the same way, back the way we came.
                if app.get_settings_panel() > 0 {
                    app.set_settings_panel(0);
                } else {
                    app.set_settings_shown(false);
                }
                from = Arrive::FromLeft;
                keep = &above[..];
            }
        }
        slint::platform::update_timers_and_animations();
        let mut cost = SlideCost::default();
        if let Some(us) = screen.render_offscreen(window) {
            if report {
                println!("couch-gui: slide: page B rendered in {us} us");
            }
            cost.frames += 1;
            cost.work_us += us;
            cost.max_us = us;
            let slid = screen.slide(from, keep, SLIDE);
            cost.frames += slid.frames;
            cost.work_us += slid.work_us;
            cost.wait_us += slid.wait_us;
            cost.max_us = cost.max_us.max(slid.max_us);
        }
        // The ring's fade takes its start time from the animation tick
        // at the moment the flag changes, and that tick only advances in
        // update_timers_and_animations - last called before B, a slide
        // ago. Advance it first, or the fade begins 180ms in and the ring
        // pops rather than fades.
        slint::platform::update_timers_and_animations();
        app.set_ring_hidden(false);
        Some(cost)
    }
}
