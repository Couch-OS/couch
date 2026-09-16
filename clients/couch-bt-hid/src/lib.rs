//! What a caller needs to drive the Bluetooth HID daemon: where its key socket
//! is, which words it answers to, and how to read the state it publishes.
//!
//! The daemon binary is the only thing that speaks D-Bus, so this half links
//! neither zbus nor tokio and the GUI can depend on it. Before it existed the
//! GUI carried its own copies of the socket paths, which is exactly the kind of
//! string that goes stale silently.

/// Where the daemon binds. It is a datagram socket that turns a word into a
/// key press on a paired TV, so it is created at [`SOCKET_MODE`] and stays
/// owned by root: the GUI and the daemon both run as root on the remote, and
/// nothing else on the device has any business sending a TV a power key.
pub const SOCKET_PATH: &str = "/tmp/couch-bt-hid.sock";

/// The same socket seen from the GUI, which may be looking at the Alpine root
/// from outside it. Try them in order and use the first that exists.
pub const SOCKET_PATHS: [&str; 2] = [SOCKET_PATH, "/mnt/alpine/tmp/couch-bt-hid.sock"];

/// Root only. The daemon sets this immediately after binding.
pub const SOCKET_MODE: u32 = 0o600;

/// Where the daemon publishes its pairing state (see [`PairStatus`]). /tmp is
/// shared between the outer root and Alpine, so one path serves the GUI, the
/// system service and the web daemon.
pub const PAIR_STATE_PATH: &str = "/tmp/couch-bt-pair.state";

/// The same file from outside the Alpine root, in the order to try.
pub const PAIR_STATE_PATHS: [&str; 2] = [PAIR_STATE_PATH, "/mnt/alpine/tmp/couch-bt-pair.state"];

/// Where the daemon remembers which bond is the active link, so a reboot
/// restores it without anyone re-sending `activate`: one line, the address,
/// or `none`. Next to `settings.conf`, on the persistent /opt/couch, seen
/// from inside the Alpine root where the daemon runs.
pub const ACTIVE_PATH: &str = "/opt/couch/bluetooth-active";

/// The same file from outside the Alpine root, in the order to try.
pub const ACTIVE_PATHS: [&str; 2] = [ACTIVE_PATH, "/mnt/alpine/opt/couch/bluetooth-active"];

/// How long pairing mode stays open once asked for: long enough to walk to a
/// TV menu, short enough that a forgotten remote does not sit discoverable.
pub const PAIR_WINDOW_SECS: u64 = 120;

/// A `pairing`, `connected` or `paired` line older than this is a daemon that
/// died mid-window: the readers treat it as idle rather than wait forever.
pub const PAIR_STALE_SECS: u64 = PAIR_WINDOW_SECS + 15;

/// Control words: not keys, but instructions to the daemon itself. `forget`
/// and `activate` take an address argument after a space (`forget
/// 44:27:45:4E:33:25`); `activate none` names no bond. See [`Control`].
pub const WORD_PAIR: &str = "pair";
pub const WORD_PAIR_STOP: &str = "pair-stop";
pub const WORD_FORGET: &str = "forget";
pub const WORD_ACTIVATE: &str = "activate";
/// The argument to `activate` that means "no bond may connect".
pub const WORD_NONE: &str = "none";

/// Whether `text` is a Bluetooth address as the daemon writes and expects
/// them: six uppercase hex pairs separated by colons, `44:27:45:4E:33:25`.
pub fn valid_address(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() == 17
        && bytes.iter().enumerate().all(|(i, b)| {
            if i % 3 == 2 {
                *b == b':'
            } else {
                b.is_ascii_digit() || (b'A'..=b'F').contains(b)
            }
        })
}

/// An address in any case, or with surrounding whitespace, brought to the
/// form [`valid_address`] accepts; None for anything that is not one.
pub fn normalize_address(text: &str) -> Option<String> {
    let upper = text.trim().to_ascii_uppercase();
    valid_address(&upper).then_some(upper)
}

/// A control word, parsed. What a caller sends is one datagram holding the
/// word and, for the two that take one, an address after a single space.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Control {
    /// Open a pairing window.
    Pair,
    /// Close the window early.
    PairStop,
    /// Remove one bond, or every bond with `None`.
    Forget(Option<String>),
    /// Make one bond the active link, or let nobody connect with `None`.
    Activate(Option<String>),
}

impl Control {
    /// The datagram for this control: the inverse of [`Control::parse`].
    pub fn word(&self) -> String {
        match self {
            Control::Pair => WORD_PAIR.into(),
            Control::PairStop => WORD_PAIR_STOP.into(),
            Control::Forget(None) => WORD_FORGET.into(),
            Control::Forget(Some(address)) => format!("{WORD_FORGET} {address}"),
            Control::Activate(None) => format!("{WORD_ACTIVATE} {WORD_NONE}"),
            Control::Activate(Some(address)) => format!("{WORD_ACTIVATE} {address}"),
        }
    }
    /// `Ok(None)` when `cmd` is not a control word at all (a key, then, or
    /// junk); `Err` when it is one whose argument is malformed, with the
    /// reason, so the daemon can log it rather than guess. Addresses are
    /// accepted in either case and come back uppercase.
    pub fn parse(cmd: &str) -> Result<Option<Control>, String> {
        let cmd = cmd.trim();
        let (word, arg) = match cmd.split_once(char::is_whitespace) {
            Some((w, a)) => (w, Some(a.trim())),
            None => (cmd, None),
        };
        Ok(Some(match (word, arg) {
            (WORD_PAIR, None) => Control::Pair,
            (WORD_PAIR_STOP, None) => Control::PairStop,
            (WORD_FORGET, None) => Control::Forget(None),
            (WORD_FORGET, Some(address)) => Control::Forget(Some(
                normalize_address(address)
                    .ok_or_else(|| format!("forget: {address:?} is not an address"))?,
            )),
            (WORD_ACTIVATE, Some(WORD_NONE)) => Control::Activate(None),
            (WORD_ACTIVATE, Some(address)) => Control::Activate(Some(
                normalize_address(address)
                    .ok_or_else(|| format!("activate: {address:?} is not an address"))?,
            )),
            (WORD_ACTIVATE, None) => {
                return Err("activate needs an address, or none".into());
            }
            (WORD_PAIR | WORD_PAIR_STOP, Some(_)) => {
                return Err(format!("{word} takes no argument"));
            }
            _ => return Ok(None),
        }))
    }
}

/// Where pairing mode is. The daemon writes it; the GUI, the system service
/// and the web page read it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PairPhase {
    /// No window open. The detail is empty.
    #[default]
    Idle,
    /// The window is open and nobody has connected.
    Pairing,
    /// A TV is connected but has not paired yet (detail: its name).
    Connected,
    /// The TV has paired or bonded and has not subscribed to a report yet
    /// (detail: its name). Some TVs ask for a key press here.
    Paired,
    /// Bonded and subscribed: the window closed on success (detail: name).
    Done,
    /// The window closed without a TV (detail: `timeout` or `cancelled`).
    Failed,
}

impl PairPhase {
    pub fn word(self) -> &'static str {
        match self {
            PairPhase::Idle => "idle",
            PairPhase::Pairing => "pairing",
            PairPhase::Connected => "connected",
            PairPhase::Paired => "paired",
            PairPhase::Done => "done",
            PairPhase::Failed => "failed",
        }
    }
    pub fn parse(word: &str) -> Option<Self> {
        Some(match word {
            "idle" => PairPhase::Idle,
            "pairing" => PairPhase::Pairing,
            "connected" => PairPhase::Connected,
            "paired" => PairPhase::Paired,
            "done" => PairPhase::Done,
            "failed" => PairPhase::Failed,
            _ => return None,
        })
    }
    /// The window is open: the daemon is still working towards a TV.
    pub fn in_window(self) -> bool {
        matches!(
            self,
            PairPhase::Pairing | PairPhase::Connected | PairPhase::Paired
        )
    }
}

/// A TV as the daemon names it in its state file: its address and the name
/// it gave (possibly empty: a TV that has not sent one yet).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Peer {
    pub address: String,
    pub name: String,
}

impl Peer {
    /// The name, or the address for a TV without one: what a row shows.
    pub fn label(&self) -> &str {
        if self.name.is_empty() {
            &self.address
        } else {
            &self.name
        }
    }
    /// `<ADDR> <name>` as a state line's tail; the older daemon wrote the
    /// name alone, which reads as a peer with no address.
    fn parse(text: &str) -> Peer {
        let text = text.trim();
        match text.split_once(' ') {
            Some((address, name)) if valid_address(address) => Peer {
                address: address.into(),
                name: name.trim().into(),
            },
            _ if valid_address(text) => Peer {
                address: text.into(),
                name: String::new(),
            },
            _ => Peer {
                address: String::new(),
                name: text.into(),
            },
        }
    }
    fn render(&self) -> String {
        if self.name.is_empty() {
            self.address.clone()
        } else if self.address.is_empty() {
            self.name.clone()
        } else {
            format!("{} {}", self.address, self.name)
        }
    }
}

/// The pairing state file, parsed. Line 1 is `<phase>` or `<phase> <detail>`;
/// the lines after it, in any order and each optional, are `link <ADDR>
/// <name>` (the TV connected right now, in any phase), `active <ADDR>` (the
/// bond that is allowed to connect) and `peer <ADDR> <name>` (the TV that
/// bonded in the last window that ended in `done`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PairStatus {
    pub phase: PairPhase,
    pub detail: String,
    /// The connected TV's label, for the rows that only show a name. Derived
    /// from `link` when parsing; `link` is what is written.
    pub peer: Option<String>,
    /// `link <ADDR> <name>`: the TV connected right now.
    pub link: Option<Peer>,
    /// `active <ADDR>`: the bond that may connect; None means nobody.
    pub active: Option<String>,
    /// `peer <ADDR> <name>`: the TV the last successful window bonded.
    pub bonded: Option<Peer>,
}

impl PairStatus {
    pub fn idle() -> Self {
        PairStatus::default()
    }
    /// Set the connected TV, keeping `peer` (its label) in step.
    pub fn set_link(&mut self, link: Option<Peer>) {
        self.peer = link.as_ref().map(|p| p.label().to_owned());
        self.link = link;
    }
    /// Lenient: an empty, missing or unrecognised first line reads as idle,
    /// because that is what a reader should show for a daemon it cannot
    /// understand; the other lines are kept if they are there and skipped
    /// if they are not well formed.
    pub fn parse(text: &str) -> Self {
        let mut lines = text.lines();
        let first = lines.next().unwrap_or("").trim();
        let (word, detail) = match first.split_once(' ') {
            Some((w, d)) => (w, d.trim()),
            None => (first, ""),
        };
        let phase = PairPhase::parse(word).unwrap_or(PairPhase::Idle);
        let detail = if phase == PairPhase::Idle {
            String::new()
        } else {
            detail.to_owned()
        };
        let mut status = PairStatus {
            phase,
            detail,
            ..Default::default()
        };
        for line in lines.map(str::trim) {
            let Some((key, rest)) = line.split_once(' ') else {
                continue;
            };
            let rest = rest.trim();
            if rest.is_empty() {
                continue;
            }
            match key {
                "link" => status.set_link(Some(Peer::parse(rest))),
                "active" if valid_address(rest) => status.active = Some(rest.to_owned()),
                "peer" => status.bonded = Some(Peer::parse(rest)),
                _ => {}
            }
        }
        status
    }
    /// The file's text: the inverse of [`PairStatus::parse`].
    pub fn render(&self) -> String {
        let mut out = String::from(self.phase.word());
        if !self.detail.is_empty() && self.phase != PairPhase::Idle {
            out.push(' ');
            out.push_str(&self.detail);
        }
        out.push('\n');
        if let Some(link) = &self.link {
            out.push_str("link ");
            out.push_str(&link.render());
            out.push('\n');
        }
        if let Some(active) = &self.active {
            out.push_str("active ");
            out.push_str(active);
            out.push('\n');
        }
        if let Some(bonded) = &self.bonded {
            out.push_str("peer ");
            out.push_str(&bonded.render());
            out.push('\n');
        }
        out
    }
    /// Read the first of `paths` that exists, applying the staleness rule: a
    /// window phase whose file has not been touched for [`PAIR_STALE_SECS`]
    /// belongs to a daemon that is gone, and reads as idle. `done` and
    /// `failed` are final and never expire; the next `pair` replaces them.
    pub fn read(paths: &[&str]) -> Self {
        for path in paths {
            let Ok(text) = std::fs::read_to_string(path) else {
                continue;
            };
            let mut status = Self::parse(&text);
            let age = std::fs::metadata(path)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok());
            if status.phase.in_window() && age.is_none_or(|a| a.as_secs() > PAIR_STALE_SECS) {
                status.phase = PairPhase::Idle;
                status.detail.clear();
            }
            return status;
        }
        Self::idle()
    }
}

/// The remembered active bond, from the first of `paths` that exists: the
/// address, or None for `none`, a missing file or anything malformed.
pub fn read_active(paths: &[&str]) -> Option<String> {
    paths
        .iter()
        .find_map(|p| std::fs::read_to_string(p).ok())
        .and_then(|text| normalize_address(&text))
}

/// Send one word to the daemon's socket: the first of [`SOCKET_PATHS`] that
/// exists. A datagram, so the caller neither waits nor learns whether the
/// word did anything; the pairing state file and the daemon's log say.
pub fn send_word(word: &str) -> std::io::Result<()> {
    let socket = SOCKET_PATHS
        .into_iter()
        .find(|p| std::path::Path::new(p).exists())
        .ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::NotFound, "Bluetooth is not running")
        })?;
    std::os::unix::net::UnixDatagram::unbound()?.send_to(word.as_bytes(), socket)?;
    Ok(())
}

/// Keyboard-page words. A TV that asks to "press any key on the keyboard"
/// after pairing wants a report from the keyboard collection, which the
/// consumer-control words never produce. `kbd:<hex>` is any usage by number
/// (`kbd:28` is Enter), with two extra leading hex digits for modifiers
/// (`kbd:0204` is shift+a).
pub fn keyboard_usage(cmd: &str) -> Option<u8> {
    Some(match cmd {
        "enter" => 0x28,
        "escape" | "esc" => 0x29,
        "backspace" => 0x2a,
        "tab" => 0x2b,
        "space" => 0x2c,
        _ => return None,
    })
}

/// The 8-byte boot-style keyboard report for a word: `[modifiers, reserved,
/// key, 0, 0, 0, 0, 0]`. None for anything that is not a keyboard word.
pub fn keyboard_report(cmd: &str) -> Option<[u8; 8]> {
    let (mods, key) = if let Some(hex) = cmd.strip_prefix("kbd:") {
        match hex.len() {
            2 => (0, u8::from_str_radix(hex, 16).ok()?),
            4 => (
                u8::from_str_radix(&hex[..2], 16).ok()?,
                u8::from_str_radix(&hex[2..], 16).ok()?,
            ),
            _ => return None,
        }
    } else {
        (0, keyboard_usage(cmd)?)
    };
    Some([mods, 0, key, 0, 0, 0, 0, 0])
}

/// Key words to HID consumer-page usages. Both the short test words (typable
/// from a shell on the remote) and the model's function ids (what the GUI sends
/// for a mapped button) are accepted.
pub fn consumer_usage(cmd: &str) -> Option<u16> {
    Some(match cmd {
        "vol+" | "volup" | "volume-up" => 0x00e9,
        "vol-" | "voldown" | "volume-down" => 0x00ea,
        "mute" | "mute-on" | "mute-off" => 0x00e2,
        "power" | "power-off" | "power-on" | "toggle" => 0x0030,
        "play" => 0x00b0,
        "pause" => 0x00b1,
        "playpause" | "play-pause" => 0x00cd,
        "stop" => 0x00b7,
        "next" => 0x00b5,
        "prev" | "previous" => 0x00b6,
        "rew" | "rewind" => 0x00b4,
        "ff" | "fast-forward" => 0x00b3,
        "chan+" | "channel-up" => 0x009c,
        "chan-" | "channel-down" => 0x009d,
        "menu" => 0x0040,
        "ok" | "select" => 0x0041,
        "up" => 0x0042,
        "down" => 0x0043,
        "left" => 0x0044,
        "right" => 0x0045,
        "home" => 0x0223,
        "back" => 0x0224,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use couch_model::{buttons, Integration};

    const LG: &str = "44:27:45:4E:33:25";

    fn peer(name: &str) -> Peer {
        Peer {
            address: LG.into(),
            name: name.into(),
        }
    }

    #[test]
    fn every_advertised_bluetooth_function_has_a_usage() {
        // The catalog is what the button-mapping picker offers and what a saved
        // configuration replays. A function it lists with no usage here is a
        // key that silently does nothing on the TV.
        let missing: Vec<_> = buttons::functions(&Integration::BluetoothTv)
            .iter()
            .filter(|(id, _)| consumer_usage(id).is_none())
            .map(|(id, _)| *id)
            .collect();
        assert!(missing.is_empty(), "no HID usage for {missing:?}");
    }

    #[test]
    fn unknown_words_are_refused_rather_than_guessed() {
        for cmd in ["", "  ", "input:hdmi1", "power off", "VOLUME-UP"] {
            assert!(consumer_usage(cmd).is_none(), "{cmd:?}");
            assert!(keyboard_report(cmd).is_none(), "{cmd:?}");
        }
        // The control words are not keys either: a key loop that treated
        // "pair" as an unknown key would drop pairing mode on the floor.
        for word in [WORD_PAIR, WORD_PAIR_STOP, WORD_FORGET, WORD_ACTIVATE] {
            assert!(consumer_usage(word).is_none());
            assert!(keyboard_report(word).is_none());
        }
    }

    #[test]
    fn keyboard_words_become_boot_reports() {
        assert_eq!(keyboard_report("enter"), Some([0, 0, 0x28, 0, 0, 0, 0, 0]));
        assert_eq!(keyboard_report("kbd:28"), Some([0, 0, 0x28, 0, 0, 0, 0, 0]));
        assert_eq!(keyboard_report("kbd:0204"), Some([2, 0, 4, 0, 0, 0, 0, 0]));
        for bad in ["kbd:", "kbd:2", "kbd:zz", "kbd:12345", "kbd:280"] {
            assert!(keyboard_report(bad).is_none(), "{bad:?}");
        }
        // A keyboard word is never also a consumer word: each goes to one
        // report characteristic.
        for word in ["enter", "escape", "space", "tab", "backspace"] {
            assert!(consumer_usage(word).is_none(), "{word}");
        }
    }

    #[test]
    fn addresses_are_six_uppercase_hex_pairs() {
        assert!(valid_address(LG));
        for bad in [
            "",
            "44:27:45:4e:33:25",
            "44:27:45:4E:33",
            "44:27:45:4E:33:25:00",
            "44-27-45-4E-33-25",
            "4G:27:45:4E:33:25",
            " 44:27:45:4E:33:25",
            "none",
        ] {
            assert!(!valid_address(bad), "{bad:?}");
        }
        assert_eq!(
            normalize_address(" 44:27:45:4e:33:25\n").as_deref(),
            Some(LG)
        );
        assert_eq!(normalize_address("none"), None);
        assert_eq!(normalize_address("44:27:45:4e:33"), None);
    }

    #[test]
    fn control_words_round_trip_and_bad_arguments_are_errors_not_keys() {
        let cases = [
            (Control::Pair, "pair"),
            (Control::PairStop, "pair-stop"),
            (Control::Forget(None), "forget"),
            (Control::Forget(Some(LG.into())), "forget 44:27:45:4E:33:25"),
            (Control::Activate(None), "activate none"),
            (
                Control::Activate(Some(LG.into())),
                "activate 44:27:45:4E:33:25",
            ),
        ];
        for (control, word) in cases {
            assert_eq!(control.word(), word);
            assert_eq!(Control::parse(word), Ok(Some(control.clone())), "{word}");
        }
        // Lowercase and stray whitespace are tolerated on the way in; the
        // address that comes out is the canonical one.
        assert_eq!(
            Control::parse("activate  44:27:45:4e:33:25 \n"),
            Ok(Some(Control::Activate(Some(LG.into()))))
        );
        // Keys and junk are not controls.
        for cmd in ["vol+", "enter", "kbd:28", "", "activated", "pairing"] {
            assert_eq!(Control::parse(cmd), Ok(None), "{cmd:?}");
        }
        // A control with a malformed argument is refused with a reason, not
        // treated as a key and not silently applied to nothing.
        for cmd in [
            "activate",
            "activate vol+",
            "activate 44:27:45:4E:33",
            "forget everything",
            "pair now",
            "pair-stop 44:27:45:4E:33:25",
        ] {
            assert!(Control::parse(cmd).is_err(), "{cmd:?}");
        }
    }

    #[test]
    fn pair_status_round_trips_and_reads_leniently() {
        let cases = [
            PairStatus::idle(),
            PairStatus {
                phase: PairPhase::Pairing,
                ..Default::default()
            },
            PairStatus {
                phase: PairPhase::Connected,
                detail: "webOS TV OLED48B4PUA".into(),
                peer: Some("webOS TV OLED48B4PUA".into()),
                link: Some(peer("webOS TV OLED48B4PUA")),
                active: Some("AA:BB:CC:DD:EE:FF".into()),
                bonded: None,
            },
            PairStatus {
                phase: PairPhase::Done,
                detail: "Living room TV".into(),
                peer: Some("Living room TV".into()),
                link: Some(peer("Living room TV")),
                active: Some(LG.into()),
                bonded: Some(peer("Living room TV")),
            },
            PairStatus {
                phase: PairPhase::Failed,
                detail: "timeout".into(),
                active: Some(LG.into()),
                ..Default::default()
            },
            // A TV with no name yet: the address stands in for it.
            PairStatus {
                phase: PairPhase::Idle,
                peer: Some(LG.into()),
                link: Some(peer("")),
                ..Default::default()
            },
        ];
        for case in cases {
            assert_eq!(PairStatus::parse(&case.render()), case, "{case:?}");
        }
        let full = PairStatus {
            phase: PairPhase::Done,
            detail: "LG TV".into(),
            peer: Some("LG TV".into()),
            link: Some(peer("LG TV")),
            active: Some(LG.into()),
            bonded: Some(peer("LG TV")),
        };
        assert_eq!(
            full.render(),
            "done LG TV\nlink 44:27:45:4E:33:25 LG TV\nactive 44:27:45:4E:33:25\npeer 44:27:45:4E:33:25 LG TV\n"
        );
        // Names keep their spaces; trailing whitespace and CRLF do not matter.
        let parsed =
            PairStatus::parse("paired  Two  Words \r\nlink 44:27:45:4E:33:25 Two  Words\r\n");
        assert_eq!(parsed.phase, PairPhase::Paired);
        assert_eq!(parsed.detail, "Two  Words");
        assert_eq!(parsed.peer.as_deref(), Some("Two  Words"));
        assert_eq!(parsed.link, Some(peer("Two  Words")));
        // Garbage, an empty file and an unknown phase all read as idle, and
        // idle never carries a detail even if one was written.
        for text in ["", "\n", "what is this", "idle leftover"] {
            let parsed = PairStatus::parse(text);
            assert_eq!(parsed.phase, PairPhase::Idle, "{text:?}");
            assert!(parsed.detail.is_empty());
            assert_eq!(parsed, PairStatus::idle());
        }
        // A link line survives an idle first line: the TV is connected while
        // no window is open, which is the normal state after pairing.
        let parsed = PairStatus::parse("idle\nlink 44:27:45:4E:33:25 TV\n");
        assert_eq!(parsed.peer.as_deref(), Some("TV"));
        assert_eq!(parsed.link.as_ref().map(Peer::label), Some("TV"));
        assert!(!PairPhase::Idle.in_window() && !PairPhase::Done.in_window());
        assert!(PairPhase::Pairing.in_window() && PairPhase::Paired.in_window());
        for phase in [
            PairPhase::Idle,
            PairPhase::Pairing,
            PairPhase::Connected,
            PairPhase::Paired,
            PairPhase::Done,
            PairPhase::Failed,
        ] {
            assert_eq!(PairPhase::parse(phase.word()), Some(phase));
        }
    }

    #[test]
    fn older_state_files_and_malformed_lines_still_read() {
        // The daemon before per-device bonds wrote `link <name>` alone: it
        // reads as a peer with a name and no address, labelled by the name.
        let old = PairStatus::parse("idle\nlink webOS TV OLED48B4PUA\n");
        let link = old.link.unwrap();
        assert!(link.address.is_empty());
        assert_eq!(link.name, "webOS TV OLED48B4PUA");
        assert_eq!(link.label(), "webOS TV OLED48B4PUA");
        assert_eq!(old.peer.as_deref(), Some("webOS TV OLED48B4PUA"));
        // A bad active line is ignored rather than trusted; an unknown line
        // is skipped; the lines may come in any order.
        let odd = PairStatus::parse(
            "pairing\nfuture stuff\nactive nope\npeer 44:27:45:4E:33:25 LG\nlink AA:BB:CC:DD:EE:FF\nactive\n",
        );
        assert_eq!(odd.phase, PairPhase::Pairing);
        assert!(odd.active.is_none());
        assert_eq!(odd.bonded, Some(peer("LG")));
        assert_eq!(odd.peer.as_deref(), Some("AA:BB:CC:DD:EE:FF"));
        assert_eq!(
            odd.link.as_ref().map(Peer::label),
            Some("AA:BB:CC:DD:EE:FF")
        );
        // set_link keeps the label in step, both ways.
        let mut s = PairStatus::idle();
        s.set_link(Some(peer("LG")));
        assert_eq!(s.peer.as_deref(), Some("LG"));
        s.set_link(None);
        assert!(s.peer.is_none() && s.link.is_none());
    }

    #[test]
    fn a_stale_window_reads_as_idle_but_a_final_state_does_not() {
        let dir = std::env::temp_dir().join(format!("couch-bt-pair-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("pair.state");
        let path = file.to_str().unwrap();
        let old =
            std::time::SystemTime::now() - std::time::Duration::from_secs(PAIR_STALE_SECS + 5);
        for (text, expect) in [
            ("pairing\n", PairPhase::Idle),
            ("paired TV\nlink 44:27:45:4E:33:25 TV\n", PairPhase::Idle),
            ("done TV\nlink 44:27:45:4E:33:25 TV\n", PairPhase::Done),
            ("failed timeout\n", PairPhase::Failed),
        ] {
            std::fs::write(&file, text).unwrap();
            let f = std::fs::File::options().write(true).open(&file).unwrap();
            f.set_modified(old).unwrap();
            let status = PairStatus::read(&[path]);
            assert_eq!(status.phase, expect, "{text:?}");
            if expect == PairPhase::Idle {
                assert!(status.detail.is_empty());
            }
            // The link line is a fact about now, not about the window.
            if text.contains("link") {
                assert_eq!(status.peer.as_deref(), Some("TV"));
            }
        }
        // Fresh, the same window line is what it says.
        std::fs::write(&file, "pairing\n").unwrap();
        assert_eq!(PairStatus::read(&[path]).phase, PairPhase::Pairing);
        // No file at all: idle, and the second path is tried.
        assert_eq!(
            PairStatus::read(&["/nonexistent/couch-pair", path]).phase,
            PairPhase::Pairing
        );
        assert_eq!(
            PairStatus::read(&["/nonexistent/couch-pair"]),
            PairStatus::idle()
        );
        // The remembered active bond: an address, `none`, junk or no file.
        let active = dir.join("active");
        let apath = active.to_str().unwrap();
        std::fs::write(&active, "44:27:45:4e:33:25\n").unwrap();
        assert_eq!(read_active(&[apath]).as_deref(), Some(LG));
        std::fs::write(&active, "none\n").unwrap();
        assert_eq!(read_active(&[apath]), None);
        std::fs::write(&active, "garbage").unwrap();
        assert_eq!(read_active(&[apath]), None);
        assert_eq!(read_active(&["/nonexistent/active", apath]), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
