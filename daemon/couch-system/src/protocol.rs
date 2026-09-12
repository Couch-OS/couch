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
    Hotspot,
    Ssh {
        enabled: bool,
    },
    SshAvailable,
    SshAuto,
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
    }
}
