//! Couch as a Bluetooth LE HID peripheral.
//!
//! Runs on the kernel Bluetooth stack (via the couch-bt-bridge vhci controller)
//! and bluetoothd. It:
//!   * powers the adapter and registers a just-works pairing agent;
//!   * registers a standard HID-over-GATT application (Device Information,
//!     Battery, and HID with a keyboard + consumer-control report map) with
//!     bluetoothd's GattManager1, so a bonded TV can use it;
//!   * advertises as "Couch Remote" over **raw HCI**, because this 3.18 kernel
//!     predates the MGMT Add Advertising command and bluetoothd exposes no
//!     LEAdvertisingManager1;
//!   * turns short text commands on a Unix datagram socket
//!     (`/run/couch-bt-hid.sock`) into HID input-report notifications.
//!
//! D-Bus is spoken with zbus (pure Rust) so the binary stays static-musl.
use std::collections::HashMap;
use std::process::Command;

use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};
use zbus::{interface, Connection, Proxy};

const ADAPTER: &str = "/org/bluez/hci0";
const AGENT_PATH: &str = "/couch/hid/agent";
const APP: &str = "/couch/hid/app";
const SOCK_PATH: &str = "/tmp/couch-bt-hid.sock";

fn uuid16(x: u16) -> String {
    format!("0000{x:04x}-0000-1000-8000-00805f9b34fb")
}

// HID-over-GATT assigned numbers.
const HID_SERVICE: u16 = 0x1812;
const HID_INFORMATION: u16 = 0x2a4a;
const REPORT_MAP: u16 = 0x2a4b;
const HID_CONTROL_POINT: u16 = 0x2a4c;
const REPORT: u16 = 0x2a4d;
const PROTOCOL_MODE: u16 = 0x2a4e;
const REPORT_REFERENCE: u16 = 0x2908;
const BATTERY_SERVICE: u16 = 0x180f;
const BATTERY_LEVEL: u16 = 0x2a19;
const DEVICE_INFO_SERVICE: u16 = 0x180a;
const PNP_ID: u16 = 0x2a50;

const KEYBOARD_ID: u8 = 1;
const CONSUMER_ID: u8 = 2;
const KEYBOARD_REPORT: &str = "/couch/hid/app/s2/c4";
const CONSUMER_REPORT: &str = "/couch/hid/app/s2/c5";

#[rustfmt::skip]
const REPORT_MAP_BYTES: &[u8] = &[
    0x05, 0x01, 0x09, 0x06, 0xa1, 0x01, 0x85, KEYBOARD_ID,
    0x05, 0x07, 0x19, 0xe0, 0x29, 0xe7, 0x15, 0x00, 0x25, 0x01,
    0x75, 0x01, 0x95, 0x08, 0x81, 0x02, 0x95, 0x01, 0x75, 0x08, 0x81, 0x03,
    0x95, 0x06, 0x75, 0x08, 0x15, 0x00, 0x25, 0x65,
    0x05, 0x07, 0x19, 0x00, 0x29, 0x65, 0x81, 0x00, 0xc0,
    0x05, 0x0c, 0x09, 0x01, 0xa1, 0x01, 0x85, CONSUMER_ID,
    0x15, 0x00, 0x26, 0xff, 0x03, 0x19, 0x00, 0x2a, 0xff, 0x03,
    0x75, 0x10, 0x95, 0x01, 0x81, 0x00, 0xc0,
];

fn consumer_usage(cmd: &str) -> Option<u16> {
    Some(match cmd {
        "vol+" | "volup" => 0x00e9,
        "vol-" | "voldown" => 0x00ea,
        "mute" => 0x00e2,
        "power" => 0x0030,
        "play" | "playpause" => 0x00cd,
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

/// A GATT service object: UUID and whether it is a primary service.
struct GattService {
    uuid: String,
}
#[interface(name = "org.bluez.GattService1")]
impl GattService {
    #[zbus(property, name = "UUID")]
    fn uuid(&self) -> String {
        self.uuid.clone()
    }
    #[zbus(property)]
    fn primary(&self) -> bool {
        true
    }
}

/// One characteristic. `value` is the current report/attribute value; for the
/// input-report characteristics the key loop updates it and emits a Value
/// change, which BlueZ turns into a notification once the TV has subscribed.
struct GattChar {
    uuid: String,
    service: OwnedObjectPath,
    flags: Vec<String>,
    value: Vec<u8>,
    notifying: bool,
}
#[interface(name = "org.bluez.GattCharacteristic1")]
impl GattChar {
    #[zbus(property, name = "UUID")]
    fn uuid(&self) -> String {
        self.uuid.clone()
    }
    #[zbus(property)]
    fn service(&self) -> OwnedObjectPath {
        self.service.clone()
    }
    #[zbus(property)]
    fn flags(&self) -> Vec<String> {
        self.flags.clone()
    }
    #[zbus(property)]
    fn notifying(&self) -> bool {
        self.notifying
    }
    #[zbus(property)]
    fn value(&self) -> Vec<u8> {
        self.value.clone()
    }
    async fn read_value(&self, _options: HashMap<String, OwnedValue>) -> Vec<u8> {
        self.value.clone()
    }
    async fn write_value(&mut self, value: Vec<u8>, _options: HashMap<String, OwnedValue>) {
        self.value = value;
    }
    async fn start_notify(&mut self) {
        self.notifying = true;
    }
    async fn stop_notify(&mut self) {
        self.notifying = false;
    }
}

/// A Report Reference descriptor: report id + type (input), so the host knows
/// which report a characteristic carries.
struct ReportRef {
    characteristic: OwnedObjectPath,
    value: Vec<u8>,
}
#[interface(name = "org.bluez.GattDescriptor1")]
impl ReportRef {
    #[zbus(property, name = "UUID")]
    fn uuid(&self) -> String {
        uuid16(REPORT_REFERENCE)
    }
    #[zbus(property)]
    fn characteristic(&self) -> OwnedObjectPath {
        self.characteristic.clone()
    }
    #[zbus(property)]
    fn flags(&self) -> Vec<String> {
        vec!["read".to_string()]
    }
    async fn read_value(&self, _options: HashMap<String, OwnedValue>) -> Vec<u8> {
        self.value.clone()
    }
}

/// Just-works pairing agent.
struct Agent;
#[interface(name = "org.bluez.Agent1")]
impl Agent {
    async fn release(&self) {}
    async fn request_confirmation(&self, _device: ObjectPath<'_>, _passkey: u32) {}
    async fn request_authorization(&self, _device: ObjectPath<'_>) {}
    async fn authorize_service(&self, _device: ObjectPath<'_>, _uuid: String) {}
    async fn request_pin_code(&self, _device: ObjectPath<'_>) -> String {
        "0000".to_string()
    }
    async fn request_passkey(&self, _device: ObjectPath<'_>) -> u32 {
        0
    }
    async fn display_pin_code(&self, _device: ObjectPath<'_>, _pincode: String) {}
    async fn display_passkey(&self, _device: ObjectPath<'_>, _passkey: u32, _entered: u16) {}
    async fn cancel(&self) {}
}

fn owned(path: &str) -> OwnedObjectPath {
    ObjectPath::try_from(path).unwrap().into()
}

fn char_obj(uuid: u16, service: &str, flags: &[&str], value: Vec<u8>) -> GattChar {
    GattChar {
        uuid: uuid16(uuid),
        service: owned(service),
        flags: flags.iter().map(|s| s.to_string()).collect(),
        value,
        notifying: false,
    }
}

/// Send one HCI command through hcitool. Raw HCI is the only advertising path on
/// this kernel; hcitool ships in bluez-deprecated.
fn hci(ocf: &str, bytes: &[&str]) -> std::io::Result<bool> {
    let status = Command::new("hcitool")
        .args(["-i", "hci0", "cmd", "0x08", ocf])
        .args(bytes)
        .status()?;
    Ok(status.success())
}

/// Build the LE advertising commands: connectable ADV_IND, flags + HID service
/// UUID + appearance in the advertisement, the name in the scan response.
fn start_advertising() -> std::io::Result<bool> {
    // LE Set Advertising Parameters: 100-150ms, ADV_IND, public, all channels.
    let params = [
        "A0", "00", "F0", "00", "00", "00", "00", "00", "00", "00", "00", "00", "00", "07", "00",
    ];
    if !hci("0x0006", &params)? {
        return Ok(false);
    }
    // LE Set Advertising Data: len, then Flags(0x06), 16-bit UUID 0x1812,
    // Appearance 0x03C1, zero-padded to 31 bytes.
    let mut adv = vec![
        "0B", "02", "01", "06", "03", "03", "12", "18", "03", "19", "C1", "03",
    ];
    while adv.len() < 32 {
        adv.push("00");
    }
    if !hci("0x0008", &adv)? {
        return Ok(false);
    }
    // LE Set Scan Response Data: "Couch Remote" as the complete local name.
    let name = [
        "43", "6F", "75", "63", "68", "20", "52", "65", "6D", "6F", "74", "65",
    ];
    let mut rsp = vec!["0E", "0D", "09"];
    rsp.extend_from_slice(&name);
    while rsp.len() < 32 {
        rsp.push("00");
    }
    if !hci("0x0009", &rsp)? {
        return Ok(false);
    }
    // LE Set Advertise Enable.
    hci("0x000A", &["01"])
}

async fn set_adapter(conn: &Connection, prop: &str, value: Value<'_>) -> zbus::Result<()> {
    let props = Proxy::new(
        conn,
        "org.bluez",
        ADAPTER,
        "org.freedesktop.DBus.Properties",
    )
    .await?;
    props
        .call_method("Set", &("org.bluez.Adapter1", prop, value))
        .await?;
    Ok(())
}

/// Push a report: set the characteristic's value and emit the change, which
/// BlueZ forwards as a notification when the TV has subscribed.
async fn push(conn: &Connection, path: &str, value: Vec<u8>) {
    let Ok(iref) = conn.object_server().interface::<_, GattChar>(path).await else {
        return;
    };
    let mut c = iref.get_mut().await;
    if !c.notifying {
        return;
    }
    c.value = value;
    let _ = c.value_changed(iref.signal_context()).await;
}

/// Connect to the system bus, retrying while dbus is still coming up.
async fn connect() -> zbus::Result<Connection> {
    let mut last = None;
    for _ in 0..40 {
        match Connection::system().await {
            Ok(c) => return Ok(c),
            Err(e) => {
                last = Some(e);
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
        }
    }
    Err(last.unwrap())
}

/// Wait until bluetoothd owns org.bluez and the adapter answers, so the
/// one-time registrations below do not race a cold-starting bluetoothd (which
/// otherwise fails the toggle: the radio is up but the HID service is not).
async fn wait_for_adapter(conn: &Connection) {
    for _ in 0..40 {
        if let Ok(props) = Proxy::new(
            conn,
            "org.bluez",
            ADAPTER,
            "org.freedesktop.DBus.Properties",
        )
        .await
        {
            if props
                .call_method("Get", &("org.bluez.Adapter1", "Address"))
                .await
                .is_ok()
            {
                return;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> zbus::Result<()> {
    let conn = connect().await?;
    let server = conn.object_server();

    // Agent.
    server.at(AGENT_PATH, Agent).await?;

    // GATT application object tree, under an ObjectManager BlueZ enumerates.
    server.at(APP, zbus::fdo::ObjectManager).await?;

    server
        .at(
            "/couch/hid/app/s0",
            GattService {
                uuid: uuid16(DEVICE_INFO_SERVICE),
            },
        )
        .await?;
    server
        .at(
            "/couch/hid/app/s0/c0",
            char_obj(
                PNP_ID,
                "/couch/hid/app/s0",
                &["read"],
                vec![0x02, 0x6b, 0x1d, 0x01, 0x00, 0x01, 0x00],
            ),
        )
        .await?;

    server
        .at(
            "/couch/hid/app/s1",
            GattService {
                uuid: uuid16(BATTERY_SERVICE),
            },
        )
        .await?;
    server
        .at(
            "/couch/hid/app/s1/c0",
            char_obj(BATTERY_LEVEL, "/couch/hid/app/s1", &["read"], vec![100]),
        )
        .await?;

    server
        .at(
            "/couch/hid/app/s2",
            GattService {
                uuid: uuid16(HID_SERVICE),
            },
        )
        .await?;
    server
        .at(
            "/couch/hid/app/s2/c0",
            char_obj(
                HID_INFORMATION,
                "/couch/hid/app/s2",
                &["read"],
                vec![0x11, 0x01, 0x00, 0x03],
            ),
        )
        .await?;
    server
        .at(
            "/couch/hid/app/s2/c1",
            char_obj(
                REPORT_MAP,
                "/couch/hid/app/s2",
                &["read"],
                REPORT_MAP_BYTES.to_vec(),
            ),
        )
        .await?;
    server
        .at(
            "/couch/hid/app/s2/c2",
            char_obj(
                HID_CONTROL_POINT,
                "/couch/hid/app/s2",
                &["write-without-response"],
                vec![0],
            ),
        )
        .await?;
    server
        .at(
            "/couch/hid/app/s2/c3",
            char_obj(
                PROTOCOL_MODE,
                "/couch/hid/app/s2",
                &["read", "write-without-response"],
                vec![0x01],
            ),
        )
        .await?;
    server
        .at(
            KEYBOARD_REPORT,
            char_obj(
                REPORT,
                "/couch/hid/app/s2",
                &["read", "notify"],
                vec![0u8; 8],
            ),
        )
        .await?;
    server
        .at(
            "/couch/hid/app/s2/c4/d0",
            ReportRef {
                characteristic: owned(KEYBOARD_REPORT),
                value: vec![KEYBOARD_ID, 0x01],
            },
        )
        .await?;
    server
        .at(
            CONSUMER_REPORT,
            char_obj(
                REPORT,
                "/couch/hid/app/s2",
                &["read", "notify"],
                vec![0u8; 2],
            ),
        )
        .await?;
    server
        .at(
            "/couch/hid/app/s2/c5/d0",
            ReportRef {
                characteristic: owned(CONSUMER_REPORT),
                value: vec![CONSUMER_ID, 0x01],
            },
        )
        .await?;

    // Wait for bluetoothd to be ready before the one-time registrations.
    wait_for_adapter(&conn).await;

    // Adapter up and pairable.
    set_adapter(&conn, "Powered", Value::from(true)).await?;
    set_adapter(&conn, "Alias", Value::from("Couch Remote")).await?;
    set_adapter(&conn, "Pairable", Value::from(true)).await?;

    // Register the pairing agent.
    let agent_mgr = Proxy::new(&conn, "org.bluez", "/org/bluez", "org.bluez.AgentManager1").await?;
    let agent_path = ObjectPath::try_from(AGENT_PATH)?;
    agent_mgr
        .call_method("RegisterAgent", &(&agent_path, "NoInputNoOutput"))
        .await?;
    agent_mgr
        .call_method("RequestDefaultAgent", &(&agent_path,))
        .await?;

    // Register the GATT application.
    let gatt_mgr = Proxy::new(&conn, "org.bluez", ADAPTER, "org.bluez.GattManager1").await?;
    let app_path = ObjectPath::try_from(APP)?;
    let options: HashMap<String, Value> = HashMap::new();
    gatt_mgr
        .call_method("RegisterApplication", &(&app_path, options))
        .await?;
    println!("couch-bt-hid: HID GATT application registered");

    // Advertise over raw HCI (no LEAdvertisingManager1 on this kernel).
    match start_advertising() {
        Ok(true) => println!("couch-bt-hid: advertising as \"Couch Remote\" (raw HCI)"),
        Ok(false) => eprintln!("couch-bt-hid: an advertising HCI command was rejected"),
        Err(e) => eprintln!("couch-bt-hid: could not run hcitool for advertising: {e}"),
    }

    // Key injection socket.
    let _ = std::fs::remove_file(SOCK_PATH);
    let socket = tokio::net::UnixDatagram::bind(SOCK_PATH)?;
    println!("couch-bt-hid: keys on {SOCK_PATH}");
    let mut buf = [0u8; 64];
    let mut readvertise = tokio::time::interval(std::time::Duration::from_secs(15));
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            _ = readvertise.tick() => {
                // Cheap re-enable; the controller stops advertising on connect,
                // so this brings us back within 15s of a disconnect. Ignored
                // (command disallowed) while a link is up.
                let _ = hci("0x000A", &["01"]);
            }
            r = socket.recv(&mut buf) => {
                let Ok(n) = r else { continue };
                let cmd = String::from_utf8_lossy(&buf[..n]);
                let cmd = cmd.trim();
                if let Some(usage) = consumer_usage(cmd) {
                    push(&conn, CONSUMER_REPORT, usage.to_le_bytes().to_vec()).await;
                    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
                    push(&conn, CONSUMER_REPORT, vec![0, 0]).await;
                } else {
                    eprintln!("couch-bt-hid: unknown key command {cmd:?}");
                }
            }
        }
    }
    Ok(())
}
