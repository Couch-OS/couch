//! Mapping a button to a protocol, address and command.
//!
//! `couch-model`'s `Integration::Ir { codeset }` names *which* remote's codes a
//! device answers to, but not the codes themselves - deliberately, so the model
//! crate (which also builds for wasm) never carries a table of IR values. This
//! is where the name resolves to codes: a small line-oriented file the daemon
//! or the CLI loads.
//!
//! ```text
//! # lg-tv.codeset  - one button per line:
//! #   <button> <protocol> <address> <command>
//! power   nec   0x04 0x08
//! volup   nec   0x04 0x02
//! voldn   nec   0x04 0x03
//! mute    nec   0x04 0x09
//! input   nec   0x04 0x0b
//! ```
//!
//! Button names are codeset-owned identifiers. Couch's standard IR command names
//! retain their existing meanings; any other valid name can be exposed as an
//! additional IR command by the assigned codeset. The name grammar is shared
//! with `couch-model` by documentation and tests, not by a crate dependency:
//! `couch-ir` deliberately stays independent so the model remains wasm-clean.
//! Resolving `codeset` to a file is likewise the daemon's job; the documented
//! convention is `/opt/couch/ir/<codeset>.codeset`.
//!
//! `couch-model` does not carry the codes themselves: `Integration::Ir`
//! continues to identify the assigned codeset by name, keeping the model
//! independent of protocol values and the IR client.

use crate::error::{Error, Result};
use crate::proto::{self, Message, Protocol};

/// One button's codes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub button: String,
    pub protocol: Protocol,
    pub address: u32,
    pub command: u32,
    pub raw: Option<proto::Frame>,
}

impl Entry {
    /// Encode this button into a sendable message. `toggle` is only consulted
    /// by RC5/RC6.
    pub fn encode(&self, toggle: bool) -> Result<Message> {
        if let Some(frame) = &self.raw {
            return proto::raw(frame.carrier_hz, frame.pattern_us.clone());
        }
        proto::encode(self.protocol, self.address, self.command, toggle)
    }
}

/// A parsed codeset: buttons in file order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Codeset {
    pub entries: Vec<Entry>,
}

impl Codeset {
    /// Parse a codeset. `path` is only for error messages.
    pub fn parse(path: &str, text: &str) -> Result<Codeset> {
        if text.len() > 256 * 1024 {
            return Err(Error::Encode("codeset exceeds 256 KiB".into()));
        }
        let mut entries: Vec<Entry> = Vec::new();
        for (i, raw) in text.lines().enumerate() {
            let line = i + 1;
            // Strip comments (everything after `#`) and surrounding space.
            let content = raw.split('#').next().unwrap_or("").trim();
            if content.is_empty() {
                continue;
            }
            let fields: Vec<&str> = content.split_whitespace().collect();
            if fields.len() != 4 {
                return Err(Error::Codeset {
                    path: path.to_string(),
                    line,
                    detail: format!(
                        "expected `<button> <protocol> <address> <command>`, got {} field(s)",
                        fields.len()
                    ),
                });
            }
            let protocol = Protocol::from_name(fields[1]).ok_or_else(|| Error::Codeset {
                path: path.to_string(),
                line,
                detail: format!("unknown protocol {:?}", fields[1]),
            })?;
            if entries.len() >= 256
                || fields[0].len() > 80
                || !fields[0]
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "_-:+.".contains(c))
                || entries
                    .iter()
                    .any(|e| e.button.eq_ignore_ascii_case(fields[0]))
            {
                return Err(Error::Codeset {
                    path: path.into(),
                    line,
                    detail: "invalid/duplicate button name or more than 256 commands".into(),
                });
            }
            if protocol == Protocol::Raw {
                let hz = parse_u32(fields[2]).map_err(Error::Encode)?;
                let timings = fields[3]
                    .split(',')
                    .map(parse_u32)
                    .collect::<std::result::Result<Vec<_>, _>>()
                    .map_err(Error::Encode)?;
                let frame = proto::raw(hz, timings)?.frame;
                entries.push(Entry {
                    button: fields[0].into(),
                    protocol,
                    address: 0,
                    command: 0,
                    raw: Some(frame),
                });
                continue;
            }
            let address = parse_u32(fields[2]).map_err(|e| Error::Codeset {
                path: path.to_string(),
                line,
                detail: format!("address: {e}"),
            })?;
            let command = parse_u32(fields[3]).map_err(|e| Error::Codeset {
                path: path.to_string(),
                line,
                detail: format!("command: {e}"),
            })?;
            // Reject codes the protocol cannot hold, at load time rather than
            // at the first press months later.
            proto::encode(protocol, address, command, false).map_err(|e| Error::Codeset {
                path: path.to_string(),
                line,
                detail: e.to_string(),
            })?;
            entries.push(Entry {
                button: fields[0].to_string(),
                protocol,
                address,
                command,
                raw: None,
            });
        }
        Ok(Codeset { entries })
    }

    /// Find a button by name (case-insensitive, since a codeset author and a
    /// caller need not agree on capitalisation).
    pub fn get(&self, button: &str) -> Option<&Entry> {
        self.entries
            .iter()
            .find(|e| e.button.eq_ignore_ascii_case(button))
    }

    /// The button names, in file order, for a "no such button" message.
    pub fn buttons(&self) -> Vec<&str> {
        self.entries.iter().map(|e| e.button.as_str()).collect()
    }
}

/// Parse a `u32` written as decimal, `0x`-hex or `0b`-binary. Shared by the CLI
/// for addresses and commands so `send`, `raw` and codesets all read numbers
/// the same way.
pub fn parse_u32(s: &str) -> std::result::Result<u32, String> {
    let t = s.trim();
    let parsed = if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        u32::from_str_radix(h, 16)
    } else if let Some(b) = t.strip_prefix("0b").or_else(|| t.strip_prefix("0B")) {
        u32::from_str_radix(b, 2)
    } else {
        t.parse::<u32>()
    };
    parsed.map_err(|_| format!("{s:?} is not a number"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
# a sample TV
power   nec   0x04 0x08
volup   nec   0x04 0x02   # inline comment
MUTE    rc5   0x00 0x0d

sony    sony12 1 21
";

    #[test]
    fn parses_and_looks_up() {
        let cs = Codeset::parse("tv.codeset", SAMPLE).unwrap();
        assert_eq!(cs.entries.len(), 4);
        let p = cs.get("power").unwrap();
        assert_eq!(p.protocol, Protocol::Nec);
        assert_eq!((p.address, p.command), (0x04, 0x08));
        // Case-insensitive lookup, and inline comments stripped.
        assert_eq!(cs.get("mute").unwrap().protocol, Protocol::Rc5);
        assert_eq!(cs.get("volup").unwrap().command, 0x02);
        assert!(cs.get("nope").is_none());
    }

    #[test]
    fn arbitrary_valid_command_names_are_preserved() {
        let set = Codeset::parse(
            "extra.codeset",
            "HDMI2 nec 4 17\ninput:HDMI2 rc5 0 13\nsource.alt raw 38000 9000,4500,560,560",
        )
        .unwrap();
        assert_eq!(set.buttons(), vec!["HDMI2", "input:HDMI2", "source.alt"]);
        assert!(set.get("hdmi2").is_some());
        assert!(set.get("INPUT:HDMI2").is_some());
    }

    #[test]
    fn an_entry_encodes() {
        let cs = Codeset::parse("tv.codeset", SAMPLE).unwrap();
        let m = cs.get("power").unwrap().encode(false).unwrap();
        assert_eq!(m.frame.carrier_hz, 38_000);
    }

    #[test]
    fn rejects_bad_lines() {
        assert!(Codeset::parse("x", "power nec 0x04").is_err()); // too few fields
        assert!(Codeset::parse("x", "power bogus 1 2").is_err()); // unknown protocol
        assert!(Codeset::parse("x", "power nec 0x100 2").is_err()); // out of range
        assert!(Codeset::parse("x", "power raw 1 2").is_err()); // raw not allowed
    }

    #[test]
    fn number_formats() {
        assert_eq!(parse_u32("16"), Ok(16));
        assert_eq!(parse_u32("0x10"), Ok(16));
        assert_eq!(parse_u32("0b10000"), Ok(16));
        assert!(parse_u32(" zz").is_err());
    }
}

/// IDs are filenames, never paths. Shared by the server and local runtime.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.as_bytes()[0].is_ascii_alphanumeric()
        && id
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-' || c == b'_')
}

pub fn load(directory: &std::path::Path, id: &str) -> Result<Codeset> {
    use std::io::Read;
    if !valid_id(id) {
        return Err(Error::Encode("invalid codeset ID".into()));
    }
    let path = directory.join(format!("{id}.codeset"));
    let metadata = std::fs::symlink_metadata(&path).map_err(|e| Error::Encode(e.to_string()))?;
    if !metadata.is_file() || metadata.len() > 256 * 1024 {
        return Err(Error::Encode(
            "codeset must be a regular file of at most 256 KiB".into(),
        ));
    }
    let mut text = String::new();
    std::fs::File::open(&path)
        .and_then(|f| f.take(256 * 1024 + 1).read_to_string(&mut text))
        .map_err(|e| Error::Encode(e.to_string()))?;
    Codeset::parse(&path.display().to_string(), &text)
}

#[cfg(test)]
mod boundary_tests {
    use super::*;
    #[test]
    fn raw_round_trip_encodes_the_exact_capture() {
        let set = Codeset::parse("capture", "power raw 38000 9000,4500,560,560").unwrap();
        let frame = set.get("power").unwrap().encode(false).unwrap().frame;
        assert_eq!(frame.carrier_hz, 38000);
        assert_eq!(frame.pattern_us, vec![9000, 4500, 560, 560]);
    }
    #[test]
    fn reject_ambiguous_names_paths_and_unbounded_captures() {
        for id in ["../tv", "TV", ".hidden", "tv/x", ""] {
            assert!(!valid_id(id));
        }
        assert!(valid_id("user-tv_2"));
        for text in [
            "power nec 4 8\nPOWER nec 4 9",
            "a/b nec 4 8",
            "power raw 38000 0,500",
            "power raw 38000 4294967295,1",
            "power raw 1000000 500",
        ] {
            assert!(Codeset::parse("bad", text).is_err());
        }
        assert!(proto::raw(38000, vec![1; 1025]).is_err());
    }
}
