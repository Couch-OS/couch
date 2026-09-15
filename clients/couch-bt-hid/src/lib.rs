//! What a caller needs to drive the Bluetooth HID daemon: where its key socket
//! is, and which words it answers to.
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

/// How long pairing mode stays open once asked for: long enough to walk to a
/// TV menu, short enough that a forgotten remote does not sit discoverable.
pub const PAIR_WINDOW_SECS: u64 = 120;

/// A `pairing`, `connected` or `paired` line older than this is a daemon that
/// died mid-window: the readers treat it as idle rather than wait forever.
pub const PAIR_STALE_SECS: u64 = PAIR_WINDOW_SECS + 15;

/// Control words: not keys, but instructions to the daemon itself.
pub const WORD_PAIR: &str = "pair";
pub const WORD_PAIR_STOP: &str = "pair-stop";
pub const WORD_FORGET: &str = "forget";

/// Where pairing mode is. The daemon writes it; the GUI, the system service
/// and the web page read it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PairPhase {
    /// No window open. The detail is empty.
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

/// The pairing state file, parsed. Line 1 is `<phase>` or `<phase> <detail>`;
/// an optional line 2 `link <name>` names the TV connected right now, in any
/// phase, so the settings row can say who is on the other end.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PairStatus {
    pub phase: PairPhase,
    pub detail: String,
    pub peer: Option<String>,
}

impl PairStatus {
    pub fn idle() -> Self {
        PairStatus {
            phase: PairPhase::Idle,
            detail: String::new(),
            peer: None,
        }
    }
    /// Lenient: an empty, missing or unrecognised first line reads as idle,
    /// because that is what a reader should show for a daemon it cannot
    /// understand, and the link line is kept if it is there.
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
        let peer = lines
            .map(str::trim)
            .find_map(|l| l.strip_prefix("link "))
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .map(str::to_owned);
        PairStatus {
            phase,
            detail,
            peer,
        }
    }
    /// The file's text: the inverse of [`PairStatus::parse`].
    pub fn render(&self) -> String {
        let mut out = String::from(self.phase.word());
        if !self.detail.is_empty() && self.phase != PairPhase::Idle {
            out.push(' ');
            out.push_str(&self.detail);
        }
        out.push('\n');
        if let Some(peer) = &self.peer {
            out.push_str("link ");
            out.push_str(peer);
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
        for word in [WORD_PAIR, WORD_PAIR_STOP, WORD_FORGET] {
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
    fn pair_status_round_trips_and_reads_leniently() {
        let cases = [
            PairStatus::idle(),
            PairStatus {
                phase: PairPhase::Pairing,
                detail: String::new(),
                peer: None,
            },
            PairStatus {
                phase: PairPhase::Connected,
                detail: "webOS TV OLED48B4PUA".into(),
                peer: Some("webOS TV OLED48B4PUA".into()),
            },
            PairStatus {
                phase: PairPhase::Done,
                detail: "Living room TV".into(),
                peer: Some("Living room TV".into()),
            },
            PairStatus {
                phase: PairPhase::Failed,
                detail: "timeout".into(),
                peer: None,
            },
        ];
        for case in cases {
            assert_eq!(PairStatus::parse(&case.render()), case, "{case:?}");
        }
        // Names keep their spaces; trailing whitespace and CRLF do not matter.
        let parsed = PairStatus::parse("paired  Two  Words \r\nlink Two  Words\r\n");
        assert_eq!(parsed.phase, PairPhase::Paired);
        assert_eq!(parsed.detail, "Two  Words");
        assert_eq!(parsed.peer.as_deref(), Some("Two  Words"));
        // Garbage, an empty file and an unknown phase all read as idle, and
        // idle never carries a detail even if one was written.
        for text in ["", "\n", "what is this", "idle leftover"] {
            let parsed = PairStatus::parse(text);
            assert_eq!(parsed.phase, PairPhase::Idle, "{text:?}");
            assert!(parsed.detail.is_empty());
        }
        // A link line survives an idle first line: the TV is connected while
        // no window is open, which is the normal state after pairing.
        let parsed = PairStatus::parse("idle\nlink TV\n");
        assert_eq!(parsed.peer.as_deref(), Some("TV"));
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
    fn a_stale_window_reads_as_idle_but_a_final_state_does_not() {
        let dir = std::env::temp_dir().join(format!("couch-bt-pair-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("pair.state");
        let path = file.to_str().unwrap();
        let old =
            std::time::SystemTime::now() - std::time::Duration::from_secs(PAIR_STALE_SECS + 5);
        for (text, expect) in [
            ("pairing\n", PairPhase::Idle),
            ("paired TV\nlink TV\n", PairPhase::Idle),
            ("done TV\nlink TV\n", PairPhase::Done),
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
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
