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
        }
    }
}
