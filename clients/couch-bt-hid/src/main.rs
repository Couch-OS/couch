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
//!     (`couch_bt_hid::SOCKET_PATH`) into HID input-report notifications;
//!   * runs **pairing mode** on request: the adapter is pairable and the
//!     advert general-discoverable only during a window the user opens
//!     (`pair`), so a TV that is already bonded reconnects at any time while
//!     laptops in the room do not list a "Couch Remote" they could grab. The
//!     window's progress goes to a state file the GUI and web page show.
//!
//! The socket path, the key vocabulary and the state-file format live in this
//! crate's lib, which the GUI and the system service link; everything below is
//! the daemon and stays here.
//!
//! D-Bus is spoken with zbus (pure Rust) so the binary stays static-musl.
use couch_bt_hid::{
    consumer_usage, keyboard_report, PairPhase, PairStatus, PAIR_STATE_PATH, PAIR_WINDOW_SECS,
    SOCKET_MODE, SOCKET_PATH, WORD_FORGET, WORD_PAIR, WORD_PAIR_STOP,
};
use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;
use std::time::{Duration, Instant};

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
    /// For the log: which characteristic a StartNotify landed on.
    label: &'static str,
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
    /// The host subscribed: from here a pushed report reaches it. Logged
    /// because a TV that pairs but never subscribes is the failure mode that
    /// looks, from the outside, exactly like keys being sent and ignored.
    async fn start_notify(&mut self) {
        println!("couch-bt-hid: StartNotify {}", self.label);
        self.notifying = true;
    }
    async fn stop_notify(&mut self) {
        println!("couch-bt-hid: StopNotify {}", self.label);
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
/// commands write by hand - the flags, the HID service UUID, the appearance
/// and the name. bluetoothd reads the properties once, so changing
/// discoverability means unregistering and registering a fresh object.
struct Advertisement {
    local_name: String,
    appearance: u16,
    service_uuids: Vec<String>,
    discoverable: bool,
}

fn advertisement(discoverable: bool) -> Advertisement {
    Advertisement {
        local_name: ADV_NAME.to_string(),
        appearance: ADV_APPEARANCE,
        service_uuids: vec![uuid16(HID_SERVICE)],
        discoverable,
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
    /// General discoverable (the raw path's flags byte 0x06) during a pairing
    /// window; otherwise only the BR/EDR-not-supported bit (0x04), which a
    /// bonded TV reconnects to but a phone's Bluetooth menu does not list. No
    /// Includes: the raw advert carries no TX power and the two must stay
    /// equivalent.
    #[zbus(property)]
    fn discoverable(&self) -> bool {
        self.discoverable
    }
    /// bluetoothd calls this when it drops the advertisement (adapter down, or
    /// our own unregister). Nothing of ours to tear down.
    async fn release(&self) {}
}

fn owned(path: &str) -> OwnedObjectPath {
    ObjectPath::try_from(path).unwrap().into()
}

fn char_obj(
    uuid: u16,
    label: &'static str,
    service: &str,
    flags: &[&str],
    value: Vec<u8>,
) -> GattChar {
    GattChar {
        uuid: uuid16(uuid),
        label,
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

/// The flags AD byte: LE general discoverable + BR/EDR not supported, or
/// BR/EDR not supported alone outside a pairing window.
fn adv_flags(discoverable: bool) -> u8 {
    if discoverable {
        0x06
    } else {
        0x04
    }
}

/// LE Set Advertising Data: significant length, then the flags, the 16-bit
/// HID service UUID and the appearance, zero-padded to the command's 31 bytes.
fn adv_data(discoverable: bool) -> Vec<String> {
    let [uuid_lo, uuid_hi] = HID_SERVICE.to_le_bytes();
    let [app_lo, app_hi] = ADV_APPEARANCE.to_le_bytes();
    #[rustfmt::skip]
    let mut adv = vec![
        hex(11),                                        // significant length
        hex(2), hex(0x01), hex(adv_flags(discoverable)), // flags
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
/// Re-run to change the flags: the controller refuses new parameters while
/// advertising, so it is disabled first (harmless when it was not).
fn start_advertising(discoverable: bool) -> std::io::Result<bool> {
    let _ = hci("0x000A", &["00"]);
    // LE Set Advertising Parameters: 100-150ms, ADV_IND, public, all channels.
    let params = [
        "A0", "00", "F0", "00", "00", "00", "00", "00", "00", "00", "00", "00", "00", "07", "00",
    ];
    if !hci("0x0006", &params)? {
        return Ok(false);
    }
    if !hci("0x0008", &adv_data(discoverable))? {
        return Ok(false);
    }
    if !hci("0x0009", &scan_rsp_data())? {
        return Ok(false);
    }
    // LE Set Advertise Enable.
    hci("0x000A", &["01"])
}

/// Whether bluetoothd exports LEAdvertisingManager1, which it only does when
/// the kernel's Bluetooth core has the MGMT advertising commands (4.1; see
/// docs/kernel-backports-research.md). A property read is the cheapest way
/// to ask; an error means the 3.18 core, not a failure.
async fn adv_manager_present(conn: &Connection) -> bool {
    let Ok(props) = Proxy::new(
        conn,
        "org.bluez",
        ADAPTER,
        "org.freedesktop.DBus.Properties",
    )
    .await
    else {
        return false;
    };
    props
        .call_method("Get", &(ADV_MANAGER, "SupportedInstances"))
        .await
        .is_ok()
}

/// Drop our managed advertisement, if one is registered. Errors are not
/// interesting: the object is gone either way and the next register decides.
async fn unregister_advertisement(conn: &Connection) {
    if let (Ok(mgr), Ok(path)) = (
        Proxy::new(conn, "org.bluez", ADAPTER, ADV_MANAGER).await,
        ObjectPath::try_from(ADV_PATH),
    ) {
        let _ = mgr.call_method("UnregisterAdvertisement", &(&path,)).await;
    }
    let _ = conn
        .object_server()
        .remove::<Advertisement, _>(ADV_PATH)
        .await;
}

/// Hand advertising to bluetoothd. Worth preferring over raw HCI: on the 3.18
/// core the kernel stops advertising on connect and re-enables nothing that
/// mgmt did not start, and any bluetoothd action overwrites our advertising
/// data. Replaces any advertisement already registered, which is how the
/// discoverable flag changes at a pairing window's edges. Returns false when
/// the caller must fall back to the raw-HCI path.
async fn register_advertisement(conn: &Connection, discoverable: bool) -> bool {
    unregister_advertisement(conn).await;
    if let Err(e) = conn
        .object_server()
        .at(ADV_PATH, advertisement(discoverable))
        .await
    {
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
    if let Err(e) = call_when_free(&mgr, "RegisterAdvertisement", &(&path, options)).await {
        // Non-fatal: a manager that refuses the advertisement still leaves the
        // raw path, so this is a fallback and not a dead daemon.
        eprintln!("couch-bt-hid: RegisterAdvertisement failed: {e}");
        let _ = conn
            .object_server()
            .remove::<Advertisement, _>(ADV_PATH)
            .await;
        return false;
    }
    println!(
        "couch-bt-hid: advertising as \"{ADV_NAME}\" (LEAdvertisingManager1, {})",
        if discoverable {
            "discoverable"
        } else {
            "not discoverable"
        }
    );
    true
}

/// Advertise either way, with the flags the phase wants. `managed` is what
/// the first attempt found: the manager is there or it is not, for the life
/// of the daemon.
async fn advertise(conn: &Connection, managed: bool, discoverable: bool) {
    if managed && register_advertisement(conn, discoverable).await {
        return;
    }
    match start_advertising(discoverable) {
        Ok(true) => println!(
            "couch-bt-hid: advertising as \"{ADV_NAME}\" (raw HCI, flags {:#04x})",
            adv_flags(discoverable)
        ),
        Ok(false) => eprintln!("couch-bt-hid: an advertising HCI command was rejected"),
        Err(e) => eprintln!("couch-bt-hid: could not run hcitool for advertising: {e}"),
    }
}

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

/// Set an adapter property, waiting out the window where bluetoothd has not
/// exported hci0 yet (UnknownObject): a bluetoothd that outlived a bridge
/// restart re-adds the adapter a few seconds after the new hci0 appears.
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
/// BlueZ forwards as a notification when the TV has subscribed. A report
/// nobody subscribed to goes nowhere, and says so in the log: that silence
/// is otherwise indistinguishable from a TV ignoring the key.
async fn push(conn: &Connection, path: &str, value: Vec<u8>) {
    let Ok(iref) = conn.object_server().interface::<_, GattChar>(path).await else {
        return;
    };
    let mut c = iref.get_mut().await;
    if !c.notifying {
        println!(
            "couch-bt-hid: dropped {} report {value:02x?}: nothing subscribed",
            c.label
        );
        return;
    }
    c.value = value;
    let _ = c.value_changed(iref.signal_context()).await;
}

/// Whether the host has subscribed to either input report: the point at
/// which a pairing has actually produced a remote the TV listens to.
async fn any_report_notifying(conn: &Connection) -> bool {
    for path in [KEYBOARD_REPORT, CONSUMER_REPORT] {
        if let Ok(iref) = conn.object_server().interface::<_, GattChar>(path).await {
            if iref.get().await.notifying {
                return true;
            }
        }
    }
    false
}

/// A press and release on the keyboard collection.
async fn press_keyboard(conn: &Connection, report: [u8; 8]) {
    push(conn, KEYBOARD_REPORT, report.to_vec()).await;
    tokio::time::sleep(Duration::from_millis(30)).await;
    push(conn, KEYBOARD_REPORT, vec![0u8; 8]).await;
}

/// A press and release on the consumer-control collection.
async fn press_consumer(conn: &Connection, usage: u16) {
    push(conn, CONSUMER_REPORT, usage.to_le_bytes().to_vec()).await;
    tokio::time::sleep(Duration::from_millis(30)).await;
    push(conn, CONSUMER_REPORT, vec![0, 0]).await;
}

/// One remote device bluetoothd knows under hci0, as far as pairing cares.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Peer {
    path: OwnedObjectPath,
    name: String,
    connected: bool,
    paired: bool,
    bonded: bool,
}

impl Peer {
    /// Paired covers bluetoothd versions without a Bonded property; on ones
    /// that have it, a bonded device is what survives a power cycle.
    fn keyed(&self) -> bool {
        self.paired || self.bonded
    }
}

/// Every Device1 under hci0, from one ObjectManager walk. A poll rather than
/// PropertiesChanged signals: a handful of devices every few seconds is
/// nothing, and a poll cannot miss a change made while we were not looking.
async fn peers(conn: &Connection) -> Vec<Peer> {
    type Objects = HashMap<OwnedObjectPath, HashMap<String, HashMap<String, OwnedValue>>>;
    let Ok(om) = Proxy::new(conn, "org.bluez", "/", "org.freedesktop.DBus.ObjectManager").await
    else {
        return Vec::new();
    };
    let Ok(reply) = om.call_method("GetManagedObjects", &()).await else {
        return Vec::new();
    };
    let Ok(objects) = reply.body().deserialize::<Objects>() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (path, interfaces) in objects {
        let Some(dev) = interfaces.get("org.bluez.Device1") else {
            continue;
        };
        let flag = |key: &str| matches!(dev.get(key).map(|v| &**v), Some(Value::Bool(true)));
        let text = |key: &str| match dev.get(key).map(|v| &**v) {
            Some(Value::Str(s)) => Some(s.to_string()),
            _ => None,
        };
        let adapter = match dev.get("Adapter").map(|v| &**v) {
            Some(Value::ObjectPath(p)) => p.as_str().to_owned(),
            _ => String::new(),
        };
        if adapter != ADAPTER {
            continue;
        }
        let name = text("Name")
            .or_else(|| text("Alias"))
            .or_else(|| text("Address"))
            .unwrap_or_else(|| "TV".into());
        out.push(Peer {
            path,
            name,
            connected: flag("Connected"),
            paired: flag("Paired"),
            bonded: flag("Bonded"),
        });
    }
    out
}

/// Remove every device under hci0, bond and all. A connected one is
/// disconnected by bluetoothd as part of the removal.
async fn forget_all(conn: &Connection) {
    let Ok(adapter) = Proxy::new(conn, "org.bluez", ADAPTER, "org.bluez.Adapter1").await else {
        return;
    };
    for peer in peers(conn).await {
        match adapter.call_method("RemoveDevice", &(&peer.path,)).await {
            Ok(_) => println!("couch-bt-hid: forgot {} ({})", peer.name, peer.path),
            Err(e) => eprintln!("couch-bt-hid: could not forget {}: {e}", peer.name),
        }
    }
}

/// Pairable on for a window, off otherwise. Only Pairable: the advert's own
/// Discoverable flag is what puts the remote in a TV's menu, and setting
/// `Adapter1.Discoverable` as well was a mistake that cost a day. On this
/// dual-mode controller it switched BR/EDR inquiry and page scan on, an LG
/// found "Couch Remote" over classic Bluetooth first, paired with SSP,
/// searched SDP for a HID record we do not have, and dropped the link; the
/// GATT HID service is LE only. The system service now also starts
/// bluetoothd with `ControllerMode = le`, so there is no classic side at all.
async fn set_pairable(conn: &Connection, on: bool) {
    if let Err(e) = set_adapter(conn, "Pairable", Value::from(on)).await {
        eprintln!("couch-bt-hid: could not set Pairable={on}: {e}");
    }
}

/// One boolean adapter property, or None when bluetoothd will not say.
async fn adapter_flag(conn: &Connection, prop: &str) -> Option<bool> {
    let props = Proxy::new(
        conn,
        "org.bluez",
        ADAPTER,
        "org.freedesktop.DBus.Properties",
    )
    .await
    .ok()?;
    let reply = props
        .call_method("Get", &("org.bluez.Adapter1", prop))
        .await
        .ok()?;
    let value: OwnedValue = reply.body().deserialize().ok()?;
    match &*value {
        Value::Bool(b) => Some(*b),
        _ => None,
    }
}

/// Keep Pairable where the window says it should be. bluetoothd applies its
/// own default (pairable, from main.conf) when the adapter finishes starting,
/// which lands after the daemon's first Set at bring-up and would leave the
/// remote pairable to anyone between windows; seen on the .155 build. Checked
/// on every peer poll, so any such override is undone within seconds.
async fn keep_pairable(conn: &Connection, want: bool) {
    if adapter_flag(conn, "Pairable").await == Some(!want) {
        println!(
            "couch-bt-hid: Pairable was {}; setting it back to {want}",
            !want
        );
        set_pairable(conn, want).await;
    }
}

/// Pairing mode: the window, what it has seen, and what it has published.
struct Pairing {
    /// When the window closes; None outside one.
    until: Option<Instant>,
    status: PairStatus,
    /// The last text written, so an unchanged state is not rewritten (the
    /// readers use the file's age to spot a dead daemon, so a live one must
    /// touch it on every poll while a window is open).
    written: String,
}

impl Pairing {
    fn new() -> Self {
        Pairing {
            until: None,
            status: PairStatus::idle(),
            written: String::new(),
        }
    }
    fn open(&self) -> bool {
        self.until.is_some()
    }
    fn publish(&mut self) {
        let text = self.status.render();
        let changed = text != self.written;
        if changed || self.open() {
            if let Err(e) = std::fs::write(PAIR_STATE_PATH, &text) {
                eprintln!("couch-bt-hid: could not write {PAIR_STATE_PATH}: {e}");
            }
        }
        if changed {
            println!(
                "couch-bt-hid: pairing {}",
                text.trim_end().replace('\n', " / ")
            );
            self.written = text;
        }
    }
    fn set(&mut self, phase: PairPhase, detail: &str) {
        self.status.phase = phase;
        self.status.detail = detail.to_owned();
    }
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
                "PnP ID",
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
            char_obj(
                BATTERY_LEVEL,
                "battery level",
                "/couch/hid/app/s1",
                &["read"],
                vec![100],
            ),
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
                "HID information",
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
                "report map",
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
                "HID control point",
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
                "protocol mode",
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
                "keyboard",
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
                "consumer",
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

    // Adapter up. Not pairable and not discoverable: those are what a
    // pairing window turns on, for its two minutes.
    set_adapter(&conn, "Powered", Value::from(true)).await?;
    set_adapter(&conn, "Alias", Value::from("Couch Remote")).await?;
    set_pairable(&conn, false).await;

    // Register the pairing agent.
    let agent_mgr = Proxy::new(&conn, "org.bluez", "/org/bluez", "org.bluez.AgentManager1").await?;
    let agent_path = ObjectPath::try_from(AGENT_PATH)?;
    call_when_free(
        &agent_mgr,
        "RegisterAgent",
        &(&agent_path, "NoInputNoOutput"),
    )
    .await?;
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

    // Advertise, not discoverable: a bonded TV reconnects to this, nobody
    // else sees a remote to pair. Preferably through bluetoothd, which then
    // owns advertising and restores it after a disconnect by itself; raw HCI
    // where there is no manager to hand it to.
    let managed = adv_manager_present(&conn).await;
    if !managed {
        println!("couch-bt-hid: no {ADV_MANAGER} on this kernel");
    }
    advertise(&conn, managed, false).await;
    let mut pairing = Pairing::new();
    pairing.status.peer = peers(&conn)
        .await
        .into_iter()
        .find(|p| p.connected)
        .map(|p| p.name);
    pairing.publish();

    // Key injection socket. bind() honours the umask, which is whatever
    // started us, so the mode is set explicitly straight afterwards: an
    // unprivileged local process must not be able to drive a paired TV's
    // power and volume. couch-control does the same for control.sock.
    let _ = std::fs::remove_file(SOCKET_PATH);
    let socket = tokio::net::UnixDatagram::bind(SOCKET_PATH)?;
    std::fs::set_permissions(SOCKET_PATH, std::fs::Permissions::from_mode(SOCKET_MODE))?;
    println!("couch-bt-hid: keys on {SOCKET_PATH}");
    let mut buf = [0u8; 64];
    let mut readvertise = tokio::time::interval(Duration::from_secs(15));
    // The peer poll: quick while a window is open, so the modal follows the
    // TV step by step; a slow tick otherwise, to keep the link line honest.
    let mut poll = tokio::time::interval(Duration::from_millis(500));
    let mut slow_ticks_left: u32 = 0;
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
            _ = poll.tick() => {
                if !pairing.open() && slow_ticks_left > 0 {
                    slow_ticks_left -= 1;
                    continue;
                }
                slow_ticks_left = 5;
                keep_pairable(&conn, pairing.open()).await;
                let peers = peers(&conn).await;
                let linked = peers.iter().find(|p| p.connected);
                pairing.status.peer = linked.map(|p| p.name.clone());
                if pairing.open() {
                    let expired = pairing.until.is_some_and(|t| Instant::now() >= t);
                    let notifying = any_report_notifying(&conn).await;
                    match linked {
                        Some(p) if p.keyed() && notifying => {
                            pairing.set(PairPhase::Done, &p.name);
                            println!("couch-bt-hid: paired with {} and it subscribed", p.name);
                        }
                        Some(p) if p.keyed() => pairing.set(PairPhase::Paired, &p.name),
                        Some(p) => pairing.set(PairPhase::Connected, &p.name),
                        None => pairing.set(PairPhase::Pairing, ""),
                    }
                    if pairing.status.phase == PairPhase::Done || expired {
                        if expired {
                            pairing.set(PairPhase::Failed, "timeout");
                        }
                        pairing.until = None;
                        set_pairable(&conn, false).await;
                        advertise(&conn, managed, false).await;
                    }
                }
                pairing.publish();
            }
            r = socket.recv(&mut buf) => {
                let Ok(n) = r else { continue };
                let cmd = String::from_utf8_lossy(&buf[..n]);
                let cmd = cmd.trim();
                if cmd == WORD_PAIR {
                    // A fresh start: every bond goes first. A TV that kept
                    // a bond we dropped re-pairs cleanly instead of failing
                    // encryption with a key we no longer hold, and the window
                    // is for one TV, the one chosen now.
                    println!("couch-bt-hid: pairing window open for {PAIR_WINDOW_SECS}s");
                    forget_all(&conn).await;
                    set_pairable(&conn, true).await;
                    advertise(&conn, managed, true).await;
                    pairing.until = Some(Instant::now() + Duration::from_secs(PAIR_WINDOW_SECS));
                    pairing.set(PairPhase::Pairing, "");
                    pairing.status.peer = None;
                    pairing.publish();
                    poll.reset();
                } else if cmd == WORD_PAIR_STOP {
                    if pairing.open() {
                        println!("couch-bt-hid: pairing window cancelled");
                        pairing.until = None;
                        set_pairable(&conn, false).await;
                        advertise(&conn, managed, false).await;
                        pairing.set(PairPhase::Failed, "cancelled");
                        pairing.publish();
                    }
                } else if cmd == WORD_FORGET {
                    forget_all(&conn).await;
                    pairing.status.peer = None;
                    pairing.publish();
                } else if let Some(report) = keyboard_report(cmd) {
                    println!("couch-bt-hid: key {cmd} (keyboard {report:02x?})");
                    press_keyboard(&conn, report).await;
                } else if let Some(usage) = consumer_usage(cmd) {
                    println!("couch-bt-hid: key {cmd} (usage {usage:#06x})");
                    press_consumer(&conn, usage).await;
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
        for discoverable in [true, false] {
            let adv = advertisement(discoverable);
            assert_eq!(adv.local_name, ADV_NAME);
            assert_eq!(adv.service_uuids, vec![uuid16(HID_SERVICE)]);
            assert_eq!(adv.appearance, ADV_APPEARANCE);
            assert_eq!(adv.type_(), "peripheral");
            assert_eq!(adv.discoverable(), discoverable);

            let raw = bytes(&adv_data(discoverable));
            // Flags: BR/EDR not supported always; LE general discoverable
            // only inside a pairing window.
            assert_eq!(
                &raw[1..4],
                [0x02, 0x01, if discoverable { 0x06 } else { 0x04 }]
            );
            assert_eq!(&raw[4..8], [0x03, 0x03, 0x12, 0x18]); // HID service UUID, LE
            assert_eq!(&raw[8..12], [0x03, 0x19, 0xc1, 0x03]); // appearance, LE
        }
        let rsp = bytes(&scan_rsp_data());
        assert_eq!(rsp[2], 0x09); // complete local name
        assert_eq!(&rsp[3..3 + ADV_NAME.len()], ADV_NAME.as_bytes());
    }

    #[test]
    fn pairing_state_renders_what_the_readers_parse() {
        // What the daemon writes is what the lib's parser (the GUI's and the
        // web page's reader) gets back, link line included.
        let mut p = Pairing::new();
        assert!(!p.open());
        p.set(PairPhase::Pairing, "");
        assert_eq!(p.status.render(), "pairing\n");
        p.set(PairPhase::Failed, "timeout");
        p.status.peer = Some("TV".into());
        assert_eq!(p.status.render(), "failed timeout\nlink TV\n");
        assert_eq!(PairStatus::parse(&p.status.render()), p.status);
    }

    #[test]
    fn the_raw_advertising_data_fits_its_hci_command() {
        // Both commands are one significant-length byte then exactly 31 bytes,
        // and the length has to cover whole AD structures: a controller reading
        // past the last one rejects the command and we advertise nothing.
        for data in [adv_data(true), adv_data(false), scan_rsp_data()] {
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
