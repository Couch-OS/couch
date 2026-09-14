//! Couch as a Bluetooth LE HID peripheral.
//!
//! Runs on the kernel Bluetooth stack (via the couch-bt-bridge vhci controller)
//! and bluetoothd. It:
//!   * powers the adapter and registers a just-works pairing agent;
//!   * registers a standard HID-over-GATT application (Device Information,
//!     Battery, and HID with a keyboard + consumer-control report map) with
//!     bluetoothd's GattManager1, so a bonded TV can use it;
//!   * advertises as "Couch Remote" through bluetoothd's LEAdvertisingManager1
//!     where the kernel's Bluetooth core is new enough to have it (the MGMT
//!     advertising commands are 4.1), and over **raw HCI** on the stock 3.18
//!     core, which has neither them nor the manager;
//!   * turns short text commands on a Unix datagram socket
//!     (`couch_bt_hid::SOCKET_PATH`) into HID input-report notifications.
//!
//! The socket path and the key vocabulary live in this crate's lib, which the
//! GUI links; everything below is the daemon and stays here.
//!
//! D-Bus is spoken with zbus (pure Rust) so the binary stays static-musl.
use couch_bt_hid::{consumer_usage, SOCKET_MODE, SOCKET_PATH};
use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};
use zbus::{interface, Connection, Proxy};

const ADAPTER: &str = "/org/bluez/hci0";
const AGENT_PATH: &str = "/couch/hid/agent";
const APP: &str = "/couch/hid/app";
const ADV_PATH: &str = "/couch/hid/adv0";
const ADV_MANAGER: &str = "org.bluez.LEAdvertisingManager1";

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

// What we advertise. Both advertising paths below build from these, because a
// TV bonds against what it saw: a name or appearance that differs between the
// two is a remote the TV stops recognising when the kernel changes.
const ADV_NAME: &str = "Couch Remote";
const ADV_APPEARANCE: u16 = 0x03c1; // HID keyboard

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

/// The managed advertisement: an LEAdvertisement1 object bluetoothd reads once
/// at RegisterAdvertisement and then owns. Carries the same fields the raw
/// commands write by hand - general-discoverable flags, the HID service UUID,
/// the appearance and the name.
struct Advertisement {
    local_name: String,
    appearance: u16,
    service_uuids: Vec<String>,
}

fn advertisement() -> Advertisement {
    Advertisement {
        local_name: ADV_NAME.to_string(),
        appearance: ADV_APPEARANCE,
        service_uuids: vec![uuid16(HID_SERVICE)],
    }
}

#[interface(name = "org.bluez.LEAdvertisement1")]
impl Advertisement {
    /// Connectable undirected advertising: the ADV_IND the raw path asks for.
    #[zbus(property, name = "Type")]
    fn type_(&self) -> String {
        "peripheral".to_string()
    }
    #[zbus(property, name = "ServiceUUIDs")]
    fn service_uuids(&self) -> Vec<String> {
        self.service_uuids.clone()
    }
    #[zbus(property)]
    fn local_name(&self) -> String {
        self.local_name.clone()
    }
    #[zbus(property)]
    fn appearance(&self) -> u16 {
        self.appearance
    }
    /// General discoverable, i.e. the raw path's flags byte 0x06. No Includes:
    /// the raw advert carries no TX power and the two must stay equivalent.
    #[zbus(property)]
    fn discoverable(&self) -> bool {
        true
    }
    /// bluetoothd calls this when it drops the advertisement (adapter down, or
    /// our own unregister). Nothing of ours to tear down.
    async fn release(&self) {}
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

/// Send one HCI command through hcitool. Raw HCI is the advertising path on a
/// kernel with no advertising manager; hcitool ships in bluez-deprecated.
fn hci<S: AsRef<std::ffi::OsStr>>(ocf: &str, bytes: &[S]) -> std::io::Result<bool> {
    let status = Command::new("hcitool")
        .args(["-i", "hci0", "cmd", "0x08", ocf])
        .args(bytes)
        .status()?;
    Ok(status.success())
}

fn hex(b: u8) -> String {
    format!("{b:02X}")
}

/// LE Set Advertising Data: significant length, then Flags(0x06), the 16-bit
/// HID service UUID and the appearance, zero-padded to the command's 31 bytes.
fn adv_data() -> Vec<String> {
    let [uuid_lo, uuid_hi] = HID_SERVICE.to_le_bytes();
    let [app_lo, app_hi] = ADV_APPEARANCE.to_le_bytes();
    #[rustfmt::skip]
    let mut adv = vec![
        hex(11),                                        // significant length
        hex(2), hex(0x01), hex(0x06),                   // flags: general discoverable
        hex(3), hex(0x03), hex(uuid_lo), hex(uuid_hi),  // complete 16-bit UUID list
        hex(3), hex(0x19), hex(app_lo), hex(app_hi),    // appearance
    ];
    adv.resize(32, hex(0));
    adv
}

/// LE Set Scan Response Data: the name as the complete local name. It rides in
/// the scan response because the advertisement above is already full enough.
fn scan_rsp_data() -> Vec<String> {
    let name = ADV_NAME.as_bytes();
    let len = name.len() as u8;
    let mut rsp = vec![hex(len + 2), hex(len + 1), hex(0x09)];
    rsp.extend(name.iter().map(|b| hex(*b)));
    rsp.resize(32, hex(0));
    rsp
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
    if !hci("0x0008", &adv_data())? {
        return Ok(false);
    }
    if !hci("0x0009", &scan_rsp_data())? {
        return Ok(false);
    }
    // LE Set Advertise Enable.
    hci("0x000A", &["01"])
}

/// Hand advertising to bluetoothd, which only exports LEAdvertisingManager1
/// when the kernel's Bluetooth core has the MGMT advertising commands (4.1;
/// see docs/kernel-backports-research.md). Worth preferring: on the 3.18 core
/// the kernel stops advertising on connect and re-enables nothing that mgmt
/// did not start, and any bluetoothd action overwrites our advertising data.
/// Returns false when the caller must fall back to the raw-HCI path.
async fn register_advertisement(conn: &Connection) -> bool {
    let props = match Proxy::new(
        conn,
        "org.bluez",
        ADAPTER,
        "org.freedesktop.DBus.Properties",
    )
    .await
    {
        Ok(p) => p,
        Err(e) => {
            eprintln!("couch-bt-hid: no properties proxy for the adapter: {e}");
            return false;
        }
    };
    // A property read is the cheapest way to ask whether the interface is
    // there at all; an error means it is not, which is the 3.18 core and not a
    // failure. The value is bluetoothd's business, we register one instance.
    if props
        .call_method("Get", &(ADV_MANAGER, "SupportedInstances"))
        .await
        .is_err()
    {
        println!("couch-bt-hid: no {ADV_MANAGER} on this kernel");
        return false;
    }
    if let Err(e) = conn.object_server().at(ADV_PATH, advertisement()).await {
        eprintln!("couch-bt-hid: could not export the advertisement: {e}");
        return false;
    }
    let Ok(mgr) = Proxy::new(conn, "org.bluez", ADAPTER, ADV_MANAGER).await else {
        return false;
    };
    let Ok(path) = ObjectPath::try_from(ADV_PATH) else {
        return false;
    };
    let options: HashMap<String, Value> = HashMap::new();
    if let Err(e) = mgr
        .call_method("RegisterAdvertisement", &(&path, options))
        .await
    {
        // Non-fatal: a manager that refuses the advertisement still leaves the
        // raw path, so this is a fallback and not a dead daemon.
        eprintln!("couch-bt-hid: RegisterAdvertisement failed: {e}");
        let _ = conn
            .object_server()
            .remove::<Advertisement, _>(ADV_PATH)
            .await;
        return false;
    }
    true
}

/// Set an adapter property, waiting out the window where bluetoothd has not
/// exported hci0 yet (UnknownObject): a bluetoothd that outlived a bridge
/// restart re-adds the adapter a few seconds after the new hci0 appears.

/// Call a bluetoothd method, waiting out `org.bluez.Error.Busy`: bluetoothd
/// answers that while it resets the adapter (the backported core does that
/// once at setup, and after a whole-chip reset) and a moment later succeeds.
async fn call_when_free<B>(proxy: &Proxy<'_>, method: &str, body: &B) -> zbus::Result<()>
where
    B: zbus::zvariant::DynamicType + zbus::export::serde::Serialize + Sync,
{
    let mut attempt = 0;
    loop {
        match proxy.call_method(method, body).await {
            Ok(_) => return Ok(()),
            Err(zbus::Error::MethodError(name, _, _))
                if attempt < 20 && name.as_str() == "org.bluez.Error.Busy" =>
            {
                attempt += 1;
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
            Err(e) => return Err(e),
        }
    }
}

async fn set_adapter(conn: &Connection, prop: &str, value: Value<'_>) -> zbus::Result<()> {
    let props = Proxy::new(
        conn,
        "org.bluez",
        ADAPTER,
        "org.freedesktop.DBus.Properties",
    )
    .await?;
    let mut attempt = 0;
    loop {
        match props
            .call_method("Set", &("org.bluez.Adapter1", prop, &value))
            .await
        {
            Ok(_) => return Ok(()),
            Err(zbus::Error::MethodError(name, _, _))
                if attempt < 20
                    && (name.as_str() == "org.freedesktop.DBus.Error.UnknownObject"
                        || name.as_str() == "org.bluez.Error.Busy") =>
            {
                attempt += 1;
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
            Err(e) => return Err(e),
        }
    }
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
async fn wait_for_adapter(conn: &Connection) -> bool {
    for _ in 0..60 {
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
                return true;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    false
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
    if !wait_for_adapter(&conn).await {
        eprintln!("couch-bt-hid: bluetoothd never exported hci0; giving up");
        let _ = std::fs::write(
            "/tmp/couch-bt.state",
            "error Bluetooth started but bluetoothd never saw the controller; turn it off and on again\n",
        );
        std::process::exit(2);
    }

    // Adapter up and pairable.
    set_adapter(&conn, "Powered", Value::from(true)).await?;
    set_adapter(&conn, "Alias", Value::from("Couch Remote")).await?;
    set_adapter(&conn, "Pairable", Value::from(true)).await?;

    // Register the pairing agent.
    let agent_mgr = Proxy::new(&conn, "org.bluez", "/org/bluez", "org.bluez.AgentManager1").await?;
    let agent_path = ObjectPath::try_from(AGENT_PATH)?;
    call_when_free(&agent_mgr, "RegisterAgent", &(&agent_path, "NoInputNoOutput")).await?;
    call_when_free(&agent_mgr, "RequestDefaultAgent", &(&agent_path,)).await?;

    // Register the GATT application.
    let gatt_mgr = Proxy::new(&conn, "org.bluez", ADAPTER, "org.bluez.GattManager1").await?;
    let app_path = ObjectPath::try_from(APP)?;
    // bluetoothd answers Busy while it is resetting the adapter (the
    // backported core does that once at setup, and after a whole-chip reset);
    // registering a moment later succeeds, so wait it out rather than die.
    let options: HashMap<String, Value> = HashMap::new();
    call_when_free(&gatt_mgr, "RegisterApplication", &(&app_path, options)).await?;
    println!("couch-bt-hid: HID GATT application registered");

    // Advertise. Preferably through bluetoothd, which then owns advertising and
    // restores it after a disconnect by itself; raw HCI where there is no
    // manager to hand it to.
    let managed = register_advertisement(&conn).await;
    if managed {
        println!("couch-bt-hid: advertising as \"{ADV_NAME}\" (LEAdvertisingManager1)");
    } else {
        match start_advertising() {
            Ok(true) => println!("couch-bt-hid: advertising as \"{ADV_NAME}\" (raw HCI)"),
            Ok(false) => eprintln!("couch-bt-hid: an advertising HCI command was rejected"),
            Err(e) => eprintln!("couch-bt-hid: could not run hcitool for advertising: {e}"),
        }
    }

    // Key injection socket. bind() honours the umask, which is whatever
    // started us, so the mode is set explicitly straight afterwards: an
    // unprivileged local process must not be able to drive a paired TV's
    // power and volume. couch-control does the same for control.sock.
    let _ = std::fs::remove_file(SOCKET_PATH);
    let socket = tokio::net::UnixDatagram::bind(SOCKET_PATH)?;
    std::fs::set_permissions(SOCKET_PATH, std::fs::Permissions::from_mode(SOCKET_MODE))?;
    println!("couch-bt-hid: keys on {SOCKET_PATH}");
    let mut buf = [0u8; 64];
    let mut readvertise = tokio::time::interval(std::time::Duration::from_secs(15));
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            _ = readvertise.tick(), if !managed => {
                // Raw path only. Cheap re-enable; the controller stops
                // advertising on connect and this kernel re-enables nothing it
                // did not start itself, so this brings us back within 15s of a
                // disconnect. Ignored (command disallowed) while a link is up.
                // A managed advertisement needs none of it.
                let _ = hci("0x000A", &["01"]);
            }
            r = socket.recv(&mut buf) => {
                let Ok(n) = r else { continue };
                let cmd = String::from_utf8_lossy(&buf[..n]);
                let cmd = cmd.trim();
                if let Some(usage) = consumer_usage(cmd) {
                    println!("couch-bt-hid: key {cmd} (usage {usage:#06x})");
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

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(data: &[String]) -> Vec<u8> {
        data.iter()
            .map(|h| u8::from_str_radix(h, 16).unwrap())
            .collect()
    }

    #[test]
    fn the_managed_advertisement_says_what_the_raw_one_says() {
        // A TV that bonded against the raw advert has to keep finding us when
        // the kernel gains an advertising manager, so name, HID service UUID,
        // appearance and discoverability must match on both paths.
        let adv = advertisement();
        assert_eq!(adv.local_name, ADV_NAME);
        assert_eq!(adv.service_uuids, vec![uuid16(HID_SERVICE)]);
        assert_eq!(adv.appearance, ADV_APPEARANCE);
        assert_eq!(adv.type_(), "peripheral");
        assert!(adv.discoverable());

        let raw = bytes(&adv_data());
        assert_eq!(&raw[1..4], [0x02, 0x01, 0x06]); // flags: general discoverable
        assert_eq!(&raw[4..8], [0x03, 0x03, 0x12, 0x18]); // HID service UUID, LE
        assert_eq!(&raw[8..12], [0x03, 0x19, 0xc1, 0x03]); // appearance, LE
        let rsp = bytes(&scan_rsp_data());
        assert_eq!(rsp[2], 0x09); // complete local name
        assert_eq!(&rsp[3..3 + ADV_NAME.len()], ADV_NAME.as_bytes());
    }

    #[test]
    fn the_raw_advertising_data_fits_its_hci_command() {
        // Both commands are one significant-length byte then exactly 31 bytes,
        // and the length has to cover whole AD structures: a controller reading
        // past the last one rejects the command and we advertise nothing.
        for data in [adv_data(), scan_rsp_data()] {
            let raw = bytes(&data);
            assert_eq!(raw.len(), 32);
            let significant = raw[0] as usize;
            assert!(significant <= 31);
            let mut i = 1;
            while i < 1 + significant {
                i += 1 + raw[i] as usize;
            }
            assert_eq!(i, 1 + significant, "AD structures overrun the length byte");
            assert!(raw[1 + significant..].iter().all(|b| *b == 0));
        }
    }
}
