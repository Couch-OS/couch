//! What the one light-and-blind control screen shows, worked out here so the
//! screen itself (ui/screens/light.slint) only draws.
//!
//! Nothing in this module touches a device or a connection: it turns the
//! reading a room row already holds - whatever produced it, built-in Hue, Home
//! Assistant, Matter or a packaged child - into the words, the two bar fills
//! and the one adjustment the screen needs. The row and the screen therefore
//! agree by construction: both describe the same `DeviceState`.
use crate::lights::DeviceState;
use couch_ha::Light;

/// Whether OK on a row opens this screen.
///
/// A device with nothing beyond on and off keeps OK as a toggle, because an
/// empty screen would be worse than the switch it replaced. What "beyond on
/// and off" means is read from the device's own reading; a row that has not
/// answered yet keeps the toggle it has always had, rather than opening a
/// screen on a guess. (A packaged child is the exception, and is decided from
/// its declared traits by its caller, which has them without any reading.)
pub(crate) fn has_controls(state: &DeviceState) -> bool {
    match state {
        DeviceState::Light(light) => light.dimmable || light.mirek_range.is_some(),
        DeviceState::Cover(cover) => cover.can_set_position || cover.can_stop,
        // A thermostat has a screen of its own, and never a row that opens this.
        DeviceState::Climate(_) => false,
    }
}

/// Colour temperature as people read it, rounded to the nearest fifty so that
/// a one-mirek difference does not jitter the number on screen.
pub(crate) fn kelvin(mirek: u16) -> u32 {
    let exact = u32::from(couch_ha::kelvin_of(mirek));
    ((exact + 25) / 50) * 50
}

/// The white a lamp is given when it is asked for a colour temperature and
/// has none to step from: 2700 K, an ordinary warm bulb.
const WARM_WHITE_MIREK: u16 = 370;

/// One step of colour temperature, in mirek.
///
/// A lamp that is showing a colour, or a scene made of colours, has a range
/// and no colour temperature at all - Hue says so with `mirek_valid: false` -
/// and it stays that way until somebody gives it one. So the first press on
/// such a lamp does not step: it turns it to warm white, inside the lamp's
/// own limits, and the presses after that step from there.
///
/// The step is a twentieth of this light's own range, so the whole range is
/// about twenty presses whatever the lamp's limits are, and never finer than
/// five mirek. A positive delta is cooler, which is a *lower* mirek: the bar
/// on screen runs warm at the bottom to cool at the top, so the up key, the
/// marker and the rising Kelvin read-out all travel the same way.
pub(crate) fn mirek_step(
    light: &Light,
    target: Option<u16>,
    delta: i32,
) -> Result<u16, &'static str> {
    if light.on.is_none() {
        return Err("This light is unavailable.");
    }
    let Some((cool, warm)) = light.mirek_range else {
        return Err("This light does not support colour temperature.");
    };
    let Some(current) = target.or(light.mirek) else {
        return Ok(WARM_WHITE_MIREK.clamp(cool, warm));
    };
    let step = i32::from((warm - cool) / 20).max(5);
    Ok((i32::from(current) - delta.signum() * step).clamp(i32::from(cool), i32::from(warm)) as u16)
}

/// Where a mirek sits on the screen's warm-to-cool bar, as a percentage: 0 is
/// this lamp's warmest, at the bottom of the bar, and 100 its coolest, at the
/// top.
fn mirek_position(range: (u16, u16), mirek: u16) -> i32 {
    let (cool, warm) = range;
    if warm <= cool {
        return 0;
    }
    let from_warm = u32::from(warm.saturating_sub(mirek));
    (from_warm * 100 / u32::from(warm - cool)).min(100) as i32
}

/// What a row is before any reading has arrived: what a packaged child
/// declared about itself, or what a Home Assistant domain says. It decides
/// which shape the screen takes - a lamp or a blind - while the device has
/// not answered, so a blind never opens drawn as a light.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Declared {
    pub cover: bool,
    pub can_stop: bool,
    pub dimmable: bool,
    pub mirek: Option<(u16, u16)>,
}

/// Everything the screen draws, in the order it draws it.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct View {
    pub title: String,
    /// "Living room · Philips Hue": where it is and what drives it.
    pub room: String,
    /// The same sentence the row shows: "On · 40%", "Off", "Unavailable".
    pub state: String,
    pub active: bool,
    pub level_label: String,
    pub level: String,
    pub level_percent: i32,
    pub level_known: bool,
    pub adjustable: bool,
    pub cover: bool,
    pub can_stop: bool,
    pub tunable: bool,
    pub kelvin: String,
    pub mirek_percent: i32,
    pub mirek_known: bool,
    /// A sentence under the controls: why an adjustment was refused, or what
    /// this device cannot do.
    pub detail: String,
    /// The one line at the bottom that teaches this screen's keys.
    pub hint: String,
}

/// The line along the bottom of the screen.
///
/// It names the two keys that are new here - volume for the bar on the left,
/// channel for the one on the right - and it has to be one line at 480 pixels,
/// so a lamp without a colour temperature spends the room it saves on Back
/// instead.
fn hint(cover: bool, tunable: bool) -> String {
    if cover {
        "Vol: position · Power: open/close · Back: room"
    } else if tunable {
        "Vol: brightness · Ch: warmth · Power: on/off"
    } else {
        "Vol: brightness · Power: on/off · Back: room"
    }
    .to_owned()
}

/// The state line and its colour, told with the level the screen is driving
/// towards rather than the one the device last reported.
///
/// The bar, the big read-out and this line all have to say the same thing
/// while a write is out, or the screen contradicts itself for as long as the
/// lamp takes to answer. Writing a brightness to a lamp that is off turns it
/// on, and writing zero turns it off, so the words follow the target both ways.
fn light_state(light: &Light, target: Option<u8>) -> (String, bool) {
    match target.filter(|_| light.dimmable && light.on.is_some()) {
        Some(0) => ("Off".to_owned(), false),
        Some(percent) => (format!("On · {percent}%"), true),
        None => (crate::lights::description(light), light.on == Some(true)),
    }
}

/// The screen for one row.
///
/// `level` and `mirek` are the optimistic targets the room list already keeps
/// for its rows: a press moves the screen at once and the reading that follows
/// either confirms it or corrects it, exactly as the row's slider behaves. The
/// state line follows them too, so nothing on the screen disagrees with the
/// bar while a write is in flight.
pub(crate) fn view(
    name: &str,
    room: &str,
    declared: Declared,
    state: Option<&DeviceState>,
    level: Option<u8>,
    mirek: Option<u16>,
) -> View {
    let base = View {
        title: name.to_owned(),
        room: room.to_owned(),
        state: "Unavailable".into(),
        level_label: if declared.cover {
            "OPEN POSITION".into()
        } else {
            "BRIGHTNESS".into()
        },
        level: "—".into(),
        kelvin: "—".into(),
        cover: declared.cover,
        can_stop: declared.can_stop,
        tunable: !declared.cover && declared.mirek.is_some(),
        hint: hint(declared.cover, !declared.cover && declared.mirek.is_some()),
        ..View::default()
    };
    match state {
        Some(DeviceState::Light(light)) => {
            let shown = level.or(light.brightness_percent);
            let known = light.on.is_some() && shown.is_some() && light.dimmable;
            let tunable = light.mirek_range.is_some();
            let shown_mirek = mirek.or(light.mirek);
            let (state, active) = light_state(light, level);
            View {
                state,
                active,
                level: if known {
                    format!("{}%", shown.unwrap_or(0))
                } else {
                    base.level.clone()
                },
                level_percent: i32::from(shown.unwrap_or(0)),
                level_known: known,
                adjustable: light.dimmable && light.on.is_some(),
                tunable,
                kelvin: match shown_mirek.filter(|_| tunable) {
                    Some(mirek) => format!("{} K", kelvin(mirek)),
                    None => base.kelvin.clone(),
                },
                mirek_percent: match (light.mirek_range, shown_mirek) {
                    (Some(range), Some(mirek)) => mirek_position(range, mirek),
                    _ => 0,
                },
                mirek_known: tunable && shown_mirek.is_some(),
                detail: if light.on.is_none() {
                    String::new()
                } else if !light.dimmable {
                    "This light has only on and off.".into()
                } else {
                    String::new()
                },
                // What the lamp itself reports, not what its row declared: a
                // Hue light declares nothing and still tunes.
                hint: hint(false, tunable),
                ..base
            }
        }
        Some(DeviceState::Cover(cover)) => {
            let shown = level.or(cover.position_percent);
            let known = cover.state.is_some() && shown.is_some();
            // The line reads the position the bar reads, which while a blind
            // travels is the endpoint it was sent to, not where it is now.
            let mut travelling = cover.clone();
            travelling.position_percent = shown;
            View {
                state: crate::lights::cover_description(&travelling),
                active: cover.state.as_deref().is_some_and(|s| s != "closed"),
                level_label: "OPEN POSITION".into(),
                level: if known {
                    format!("{}%", shown.unwrap_or(0))
                } else {
                    base.level.clone()
                },
                level_percent: i32::from(shown.unwrap_or(0)),
                level_known: known,
                adjustable: cover.can_set_position && cover.state.is_some(),
                cover: true,
                can_stop: cover.can_stop,
                detail: if cover.state.is_some() && !cover.can_set_position {
                    "This blind opens and closes, but cannot be sent to a position.".into()
                } else {
                    String::new()
                },
                hint: hint(true, false),
                ..base
            }
        }
        // A climate row never opens this screen, and a row that has not
        // answered yet is drawn as the unavailable thing it is.
        Some(DeviceState::Climate(_)) | None => base,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use couch_ha::Cover;

    fn lamp(dimmable: bool, mirek_range: Option<(u16, u16)>) -> Light {
        Light {
            entity_id: "lamp".into(),
            name: "Desk lamp".into(),
            on: Some(true),
            brightness_percent: Some(40),
            dimmable,
            mirek: mirek_range.map(|_| 370),
            mirek_range,
        }
    }
    fn blind(position: bool, stop: bool) -> Cover {
        Cover {
            entity_id: "blind".into(),
            name: "Blind".into(),
            state: Some("open".into()),
            position_percent: Some(60),
            can_open: true,
            can_close: true,
            can_set_position: position,
            can_stop: stop,
        }
    }

    /// The exception the owner asked for: a device with nothing but on and
    /// off keeps OK as a toggle.
    #[test]
    fn only_a_device_with_something_to_adjust_gets_a_screen() {
        assert!(has_controls(&DeviceState::Light(lamp(true, None))));
        assert!(has_controls(&DeviceState::Light(lamp(
            false,
            Some((153, 500))
        ))));
        assert!(!has_controls(&DeviceState::Light(lamp(false, None))));
        assert!(has_controls(&DeviceState::Cover(blind(true, false))));
        // A blind that cannot be positioned but can be stopped still has a
        // screen worth opening: its three buttons.
        assert!(has_controls(&DeviceState::Cover(blind(false, true))));
        assert!(!has_controls(&DeviceState::Cover(blind(false, false))));
    }

    #[test]
    fn kelvin_is_rounded_to_something_a_person_would_say() {
        assert_eq!(kelvin(370), 2700);
        assert_eq!(kelvin(153), 6550);
        assert_eq!(kelvin(500), 2000);
        assert_eq!(kelvin(250), 4000);
    }

    #[test]
    fn a_colour_step_crosses_the_range_in_about_twenty_presses() {
        let light = lamp(true, Some((153, 500)));
        // Seventeen mirek a press over a 347-mirek range.
        assert_eq!(mirek_step(&light, None, 1), Ok(353));
        assert_eq!(mirek_step(&light, None, -1), Ok(387));
        // The latest target is what the next press moves from, exactly as
        // brightness does.
        assert_eq!(mirek_step(&light, Some(353), 1), Ok(336));
        // Neither end can be walked past.
        assert_eq!(mirek_step(&light, Some(160), 1), Ok(153));
        assert_eq!(mirek_step(&light, Some(495), -1), Ok(500));
        // A narrow range still moves five mirek at a time rather than nothing.
        let narrow = lamp(true, Some((200, 260)));
        assert_eq!(mirek_step(&narrow, Some(230), 1), Ok(225));
        // A light with no range refuses instead of inventing one.
        assert!(mirek_step(&lamp(true, None), None, 1).is_err());
        // A lamp showing a colour has a range and no colour temperature, for
        // as long as it shows that colour. Either key turns it to warm white,
        // and the next press steps from there.
        let mut unread = lamp(true, Some((153, 500)));
        unread.mirek = None;
        assert_eq!(mirek_step(&unread, None, 1), Ok(370));
        assert_eq!(mirek_step(&unread, None, -1), Ok(370));
        assert_eq!(mirek_step(&unread, Some(370), 1), Ok(353));
        // Warm white is still inside the lamp's own limits.
        let mut cool_only = lamp(true, Some((153, 300)));
        cool_only.mirek = None;
        assert_eq!(mirek_step(&cool_only, None, 1), Ok(300));
        // A lamp that cannot be reached is still refused.
        unread.on = None;
        assert!(mirek_step(&unread, Some(300), 1).is_err());
    }

    /// The screen says what the row says, and adds the two things a row has
    /// no room for: the level as a number and the colour temperature.
    #[test]
    fn the_screen_repeats_the_rows_sentence_and_shows_its_controls() {
        let tunable = view(
            "Desk lamp",
            "Living room · Philips Hue",
            Declared::default(),
            Some(&DeviceState::Light(lamp(true, Some((153, 500))))),
            None,
            None,
        );
        assert_eq!(tunable.state, "On · 40%");
        assert_eq!(tunable.level, "40%");
        assert!(tunable.level_known && tunable.adjustable && tunable.active);
        assert_eq!(tunable.level_label, "BRIGHTNESS");
        assert!(tunable.tunable && tunable.mirek_known);
        assert_eq!(tunable.kelvin, "2700 K");
        // 370 of 153..500, measured from the warm end.
        assert_eq!(tunable.mirek_percent, 37);
        assert_eq!(tunable.detail, "");
        // The line at the bottom teaches the two keys this screen adds.
        assert_eq!(tunable.hint, "Vol: brightness · Ch: warmth · Power: on/off");

        // A pending press moves the screen before the light has answered.
        let pressed = view(
            "Desk lamp",
            "",
            Declared::default(),
            Some(&DeviceState::Light(lamp(true, Some((153, 500))))),
            Some(45),
            Some(336),
        );
        assert_eq!(pressed.level, "45%");
        assert_eq!(pressed.level_percent, 45);
        assert_eq!(pressed.kelvin, "3000 K");
        // And the state line moves with it, so nothing on the screen
        // contradicts the bar while the write is out.
        assert_eq!(pressed.state, "On · 45%");
        assert!(pressed.active);

        // A lamp that only switches says so instead of offering a slider.
        let plain = view(
            "Plug",
            "",
            Declared::default(),
            Some(&DeviceState::Light(lamp(false, None))),
            None,
            None,
        );
        assert_eq!(plain.level, "—");
        assert!(!plain.level_known && !plain.adjustable && !plain.tunable);
        assert_eq!(plain.detail, "This light has only on and off.");
        // No colour temperature, so the channel keys are not offered and the
        // room they leave goes to Back.
        assert_eq!(plain.hint, "Vol: brightness · Power: on/off · Back: room");

        // Unavailable is unavailable: no level, no colour, no invented off.
        let mut gone = lamp(true, Some((153, 500)));
        gone.on = None;
        gone.brightness_percent = None;
        gone.mirek = None;
        let dark = view(
            "Desk lamp",
            "",
            Declared::default(),
            Some(&DeviceState::Light(gone)),
            None,
            None,
        );
        assert_eq!(dark.state, "Unavailable");
        assert_eq!(dark.level, "—");
        assert_eq!(dark.kelvin, "—");
        assert!(!dark.active && !dark.adjustable && !dark.mirek_known);
        // The range is still declared, so the control is still drawn - with
        // nothing in it, which is the honest picture.
        assert!(dark.tunable);
        assert_eq!(dark.hint, "Vol: brightness · Ch: warmth · Power: on/off");

        // And a row with no reading at all is the same picture.
        assert_eq!(
            view("Desk lamp", "", Declared::default(), None, None, None).state,
            "Unavailable"
        );
        // A blind that has not answered is still drawn as a blind, from what
        // its row declared, rather than opening as a lamp.
        let unread = view(
            "Blind",
            "",
            Declared {
                cover: true,
                can_stop: true,
                ..Declared::default()
            },
            None,
            None,
            None,
        );
        assert!(unread.cover && unread.can_stop && !unread.tunable);
        assert_eq!(unread.level_label, "OPEN POSITION");
        assert_eq!(
            unread.hint,
            "Vol: position · Power: open/close · Back: room"
        );
    }

    /// The state line is the bar's line: while a press is in flight it says
    /// where the device is going, not the reading it has already overtaken.
    #[test]
    fn the_state_line_follows_the_press_rather_than_the_last_reading() {
        let lit = DeviceState::Light(lamp(true, Some((153, 500))));
        let screen = |level| {
            view(
                "Desk lamp",
                "",
                Declared::default(),
                Some(&lit),
                level,
                None,
            )
        };
        // No press: the reading, exactly as the row shows it.
        assert_eq!(screen(None).state, "On · 40%");
        assert_eq!(screen(Some(55)).state, "On · 55%");
        // Writing zero turns a lamp off, and the line says so before the
        // reading confirms it - with the accent colour gone too.
        let dark = screen(Some(0));
        assert_eq!(dark.state, "Off");
        assert!(!dark.active);

        // The other way round: a brightness written to a lamp that is off
        // turns it on, so the line does not keep saying "Off".
        let mut resting = lamp(true, None);
        resting.on = Some(false);
        resting.brightness_percent = Some(0);
        let woken = view(
            "Desk lamp",
            "",
            Declared::default(),
            Some(&DeviceState::Light(resting.clone())),
            Some(5),
            None,
        );
        assert_eq!(woken.state, "On · 5%");
        assert!(woken.active);

        // A lamp that cannot dim has no target to believe in, and an
        // unavailable one is still unavailable whatever was pressed.
        let mut plain = resting;
        plain.dimmable = false;
        assert_eq!(
            view(
                "Plug",
                "",
                Declared::default(),
                Some(&DeviceState::Light(plain.clone())),
                Some(5),
                None
            )
            .state,
            "Off"
        );
        plain.on = None;
        assert_eq!(
            view(
                "Plug",
                "",
                Declared::default(),
                Some(&DeviceState::Light(plain)),
                Some(5),
                None
            )
            .state,
            "Unavailable"
        );

        // A blind's line carries the position the bar carries, which while it
        // travels is the endpoint it was sent to.
        let travelling = view(
            "Blind",
            "",
            Declared {
                cover: true,
                ..Declared::default()
            },
            Some(&DeviceState::Cover(blind(true, true))),
            Some(25),
            None,
        );
        assert_eq!(travelling.state, "Open · 25% open");
        assert_eq!(travelling.level, "25%");
    }

    #[test]
    fn a_blind_is_the_same_screen_with_a_position_on_it() {
        let open = view(
            "Blind",
            "Living room · Home Assistant",
            Declared {
                cover: true,
                can_stop: true,
                ..Declared::default()
            },
            Some(&DeviceState::Cover(blind(true, true))),
            None,
            None,
        );
        assert_eq!(open.state, "Open · 60% open");
        assert_eq!(open.level_label, "OPEN POSITION");
        assert_eq!(open.level, "60%");
        assert!(open.cover && open.can_stop && open.adjustable && open.active);
        assert!(!open.tunable);
        assert_eq!(open.hint, "Vol: position · Power: open/close · Back: room");
        let simple = view(
            "Blind",
            "",
            Declared {
                cover: true,
                ..Declared::default()
            },
            Some(&DeviceState::Cover(blind(false, true))),
            None,
            None,
        );
        assert!(!simple.adjustable);
        assert_eq!(
            simple.detail,
            "This blind opens and closes, but cannot be sent to a position."
        );
    }
}
