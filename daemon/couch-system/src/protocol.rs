use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::io::{self, Read, Write};

pub const SOCKET: &str = "/tmp/couch-system/control.sock";
const LIMIT: usize = 16 * 1024;

// No arbitrary commands, filesystem paths or supplicant commands cross this API.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum Request {
    Network,
    Health,
    UpdateStatus,
    UpdateCheck {
        automatic: bool,
    },
    UpdateSettings {
        channel: couch_updates::Channel,
        automatic_checks: bool,
    },
    UpdateInstall {
        version: String,
    },
    UpdateRestart,
    /// Put the saved previous boot image back on the boot partition.
    BootRollback,
    /// Power off, restart, or restart into the recovery image.
    Power {
        action: crate::power::Action,
    },
    Hotspot,
    Ssh {
        enabled: bool,
    },
    SshAvailable,
    SshAuto,
    /// Start or stop the Bluetooth bridge (and with it the radio).
    Bluetooth {
        enabled: bool,
    },
    /// At boot: start the bridge if the saved setting says so.
    BluetoothAuto,
    /// The HID daemon's control words: open a pairing window (for a device,
    /// so the TV that bonds is stored on it), cancel it, forget one bond or
    /// all, press Enter for a TV that asks for a key, or make one bond the
    /// active link. `address` is the TV's, uppercase colon-hex; `device` a
    /// device id from the configuration. See `docs/bluetooth.md`.
    BluetoothPair {
        action: crate::bluetooth::PairAction,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        address: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        device: Option<String>,
    },
    EnrollKey {
        key: String,
    },
    SetPassword {
        password: String,
    },
    PortalJoin {
        ssid: String,
        password: String,
    },
    Scan,
}
#[derive(Serialize, Deserialize)]
pub enum Reply {
    Ready,
    Update(couch_updates::Status),
    Done(Result<(), String>),
    Available(bool),
    Networks(Result<Vec<crate::network::Network>, String>),
}

// Length framing permits partial reads; input is bounded before allocation.
pub fn read<T: DeserializeOwned>(input: &mut impl Read) -> io::Result<T> {
    let mut header = [0; 4];
    input.read_exact(&mut header)?;
    let size = u32::from_be_bytes(header) as usize;
    if size > LIMIT {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "oversize request",
        ));
    }
    let mut bytes = vec![0; size];
    input.read_exact(&mut bytes)?;
    let result = serde_json::from_slice(&bytes)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid request"));
    bytes.fill(0);
    result
}
pub fn write(value: &impl Serialize, output: &mut impl Write) -> io::Result<()> {
    let mut bytes = serde_json::to_vec(value)?;
    let result = if bytes.len() > LIMIT {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "oversize response",
        ))
    } else {
        output
            .write_all(&(bytes.len() as u32).to_be_bytes())
            .and_then(|_| output.write_all(&bytes))
    };
    bytes.fill(0);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_oversized_and_arbitrary_requests() {
        assert!(read::<Request>(&mut &u32::MAX.to_be_bytes()[..]).is_err());
        let mut data = Vec::new();
        write(&serde_json::json!({"Run":{"command":"reboot"}}), &mut data).unwrap();
        assert!(read::<Request>(&mut &data[..]).is_err());
        let mut data = Vec::new();
        write(
            &serde_json::json!({"Power":{"action":"recovery"}}),
            &mut data,
        )
        .unwrap();
        assert!(matches!(
            read::<Request>(&mut &data[..]).unwrap(),
            Request::Power {
                action: crate::power::Action::Recovery
            }
        ));
        let mut data = Vec::new();
        write(
            &serde_json::json!({"Power":{"action":"halt","force":true}}),
            &mut data,
        )
        .unwrap();
        assert!(read::<Request>(&mut &data[..]).is_err());
        // Pairing actions are a closed set: no word reaches the daemon's
        // socket that this enum did not name.
        let mut data = Vec::new();
        write(
            &serde_json::json!({"BluetoothPair":{"action":"pair"}}),
            &mut data,
        )
        .unwrap();
        assert!(matches!(
            read::<Request>(&mut &data[..]).unwrap(),
            Request::BluetoothPair {
                action: crate::bluetooth::PairAction::Pair,
                address: None,
                device: None,
            }
        ));
        // The per-device and per-address forms, as the CLI takes them.
        let mut data = Vec::new();
        write(
            &serde_json::json!({"BluetoothPair":{"action":"activate","address":"44:27:45:4E:33:25"}}),
            &mut data,
        )
        .unwrap();
        assert!(matches!(
            read::<Request>(&mut &data[..]).unwrap(),
            Request::BluetoothPair {
                action: crate::bluetooth::PairAction::Activate,
                address: Some(a),
                device: None,
            } if a == "44:27:45:4E:33:25"
        ));
        let mut data = Vec::new();
        write(
            &serde_json::json!({"BluetoothPair":{"action":"pair","device":"living-tv"}}),
            &mut data,
        )
        .unwrap();
        assert!(matches!(
            read::<Request>(&mut &data[..]).unwrap(),
            Request::BluetoothPair {
                action: crate::bluetooth::PairAction::Pair,
                address: None,
                device: Some(d),
            } if d == "living-tv"
        ));
        let mut data = Vec::new();
        write(
            &serde_json::json!({"BluetoothPair":{"action":"kbd:28"}}),
            &mut data,
        )
        .unwrap();
        assert!(read::<Request>(&mut &data[..]).is_err());
    }
}
