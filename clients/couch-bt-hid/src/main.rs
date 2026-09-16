//! Couch as a Bluetooth LE HID peripheral.
//!
//! Runs on the kernel Bluetooth stack (via the in-kernel STP driver or the
//! couch-bt-bridge vhci controller) and bluetoothd. It:
//!   * powers the adapter and registers a just-works pairing agent;
//!   * registers a standard HID-over-GATT application (Device Information,
//!     Battery, and HID with a keyboard + consumer-control report map) with
//!     bluetoothd's GattManager1, so a bonded TV can use it;
//!   * holds any number of TV bonds but lets exactly ONE of them, the active
//!     bond, connect: bluetoothd's GATT server notifies every subscribed
//!     client and the TV is the one that initiates, so the single link is
//!     enforced at the controller with the LE white list and an advertising
//!     filter policy that only accepts that address (raw HCI: bluetoothd's
//!     managed advertisement offers no filter policy);
//!   * advertises discoverable, to anyone, only during a **pairing window**
//!     the user opens (`pair`), through bluetoothd's LEAdvertisingManager1
//!     where the kernel's Bluetooth core is new enough to have it and over raw
//!     HCI on the stock 3.18 core; the TV that bonds in the window becomes
//!     the active bond;
//!   * turns short text commands on a Unix datagram socket
//!     (`couch_bt_hid::SOCKET_PATH`) into HID input-report notifications, and
//!     the control words (`pair`, `pair-stop`, `forget [ADDR]`, `activate
//!     ADDR|none`) into what they say; the state file the GUI and web page
//!     show carries the window's progress, the connected TV, the active bond
//!     and the last TV that bonded.
//!
//! The socket path, the key vocabulary and the state-file format live in this
//! crate's lib, which the GUI and the system service link; everything below is
//! the daemon and stays here.
//!
//! D-Bus is spoken with zbus (pure Rust) so the binary stays static-musl.
use couch_bt_hid::{
    consumer_usage, keyboard_report, normalize_address, Control, PairPhase, PairStatus, Peer,
    ACTIVE_PATH, PAIR_STATE_PATH, PAIR_WINDOW_SECS, SOCKET_MODE, SOCKET_PATH,
};
use std::collections::{BTreeMap, HashMap};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
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

// Where each service starts in bluetoothd's attribute table. A bonded TV's
// subscriptions are stored per handle (couch-bluetoothd, third_party/bluez),
// so the table has to come out the same every time. Unpinned, bluetoothd
// places an application after the highest handle it has ever allocated, so
// registering again without restarting bluetoothd moves every handle up;
// pinned, the services land here again. Far above bluetoothd's own services
// (GAP, GATT and Device Information end at 0x0014 on 5.79).
const DEVICE_INFO_HANDLE: u16 = 0x0100;
const BATTERY_HANDLE: u16 = 0x0110;
const HID_HANDLE: u16 = 0x0120;

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

/// A GATT service object: UUID, primary, and the handle it must start at.
struct GattService {
    uuid: String,
    handle: u16,
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
    #[zbus(property)]
    fn handle(&self) -> u16 {
        self.handle
    }
}

/// The application's ObjectManager, in place of zbus's own. bluetoothd lays
/// an application out in the order GetManagedObjects lists it (every
/// service, then every characteristic, then every descriptor, each pass in
/// reply order), and zbus lists objects in HashMap order, which is seeded
/// differently in every process: the dev remote's bluetoothd had recorded
/// three different tables for the same application, the report CCCs at
/// other handles each time. This one answers in object-path order.
struct AppObjects;
type Interfaces = HashMap<String, HashMap<String, OwnedValue>>;
#[interface(name = "org.freedesktop.DBus.ObjectManager")]
impl AppObjects {
    async fn get_managed_objects(
        &self,
        #[zbus(object_server)] server: &zbus::ObjectServer,
    ) -> zbus::fdo::Result<BTreeMap<ObjectPath<'static>, Interfaces>> {
        use zbus::object_server::Interface;
        let mut objects = BTreeMap::new();
        for path in APP_OBJECTS {
            let mut interfaces = Interfaces::new();
            if let Ok(i) = server.interface::<_, GattService>(*path).await {
                interfaces.insert(
                    GattService::name().to_string(),
                    i.get().await.get_all().await?,
                );
            } else if let Ok(i) = server.interface::<_, GattChar>(*path).await {
                interfaces.insert(GattChar::name().to_string(), i.get().await.get_all().await?);
            } else if let Ok(i) = server.interface::<_, ReportRef>(*path).await {
                interfaces.insert(
                    ReportRef::name().to_string(),
                    i.get().await.get_all().await?,
                );
            } else {
                continue;
            }
            objects.insert(ObjectPath::from_static_str_unchecked(path), interfaces);
        }
        Ok(objects)
    }
}

/// Every object of the GATT application, which `main` registers and
/// `AppObjects` lists.
const APP_OBJECTS: &[&str] = &[
    "/couch/hid/app/s0",
    "/couch/hid/app/s0/c0",
    "/couch/hid/app/s1",
    "/couch/hid/app/s1/c0",
    "/couch/hid/app/s2",
    "/couch/hid/app/s2/c0",
    "/couch/hid/app/s2/c1",
    "/couch/hid/app/s2/c2",
    "/couch/hid/app/s2/c3",
    KEYBOARD_REPORT,
    "/couch/hid/app/s2/c4/d0",
    CONSUMER_REPORT,
    "/couch/hid/app/s2/c5/d0",
];

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
    /// Whether any StartNotify ever arrived, and whether the "sent without
    /// one" line has been logged since the last Start/StopNotify: for the log.
    ever_notified: bool,
    quiet: bool,
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
    async fn read_value(&self, options: HashMap<String, OwnedValue>) -> Vec<u8> {
        read_at(&self.value, &options)
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
        self.ever_notified = true;
        self.quiet = false;
    }
    async fn stop_notify(&mut self) {
        println!("couch-bt-hid: StopNotify {}", self.label);
        self.notifying = false;
        self.quiet = false;
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
        vec!["encrypt-read".to_string()]
    }
    async fn read_value(&self, options: HashMap<String, OwnedValue>) -> Vec<u8> {
        read_at(&self.value, &options)
    }
}

/// The part of a value a ReadValue call asks for. A host whose ATT MTU is
/// smaller than the value (the report map is 72 bytes; macOS reads it 49 at
/// a time) continues with Read Blob, which bluetoothd passes on as an
/// `offset` option. Returning the whole value every time made macOS stitch
/// together the first 49 bytes over and over: a report map with no
/// consumer-control collection, so none of our keys decoded. An offset at
/// or past the end reads as empty, which ends the host's blob loop.
fn read_at(value: &[u8], options: &HashMap<String, OwnedValue>) -> Vec<u8> {
    let offset = match options.get("offset").map(|v| &**v) {
        Some(Value::U16(o)) => usize::from(*o),
        _ => 0,
    };
    value.get(offset..).unwrap_or_default().to_vec()
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
        ever_notified: false,
        quiet: false,
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

/// What the controller should be doing between key presses.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Advert {
    /// Not advertising: no bond is active, so nobody may connect.
    Off,
    /// A pairing window: connectable, general-discoverable, open to any
    /// central (filter policy 0x00), so a new TV can find and connect to us.
    Window,
    /// The active bond: connectable, not discoverable, and the controller
    /// answers scan and connect requests from this one address only (the LE
    /// white list with filter policy 0x03). Anyone else's connect request
    /// is dropped on air, before bluetoothd ever hears of it.
    Only { address: String, random: bool },
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

/// LE Set Advertising Parameters: 100-150 ms, ADV_IND, own address public,
/// no directed peer, all three channels, and the filter policy last (byte
/// 14 of the 15): 0x00 accepts everyone, 0x03 answers scan and connect
/// requests from the white list only.
fn adv_params(filter_policy: u8) -> Vec<String> {
    let mut params: Vec<String> = [
        "A0", "00", "F0", "00", "00", "00", "00", "00", "00", "00", "00", "00", "00", "07",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    params.push(hex(filter_policy));
    params
}

/// LE Add Device To White List: the address type (0 public, 1 random) and
/// the address least-significant byte first, the reverse of how it is
/// written. None for text that is not an address.
fn white_list_entry(address: &str, random: bool) -> Option<Vec<String>> {
    let mut bytes: Vec<String> = address
        .split(':')
        .map(|pair| u8::from_str_radix(pair, 16).map(hex))
        .collect::<Result<_, _>>()
        .ok()?;
    if bytes.len() != 6 {
        return None;
    }
    bytes.reverse();
    bytes.insert(0, hex(u8::from(random)));
    Some(bytes)
}

/// Program the controller for `advert` and enable advertising, over raw HCI.
/// Always disables first: the controller refuses new parameters, and any
/// white-list change, while advertising (harmless when it was not). For
/// `Only`, the white list is cleared and rewritten every time, because the
/// kernel's own LE passive scan rewrites it from its pending-connection
/// lists whenever that scan starts (4.4 `update_white_list`); none of those
/// lists is ours, so we reassert the entry at every (re)start. Ok(false)
/// means a command was rejected.
fn start_advertising(advert: &Advert) -> std::io::Result<bool> {
    let _ = hci("0x000A", &["00"]);
    let (filter_policy, discoverable) = match advert {
        Advert::Off => return Ok(true),
        Advert::Window => (0x00, true),
        Advert::Only { address, random } => {
            // LE Clear White List, LE Add Device To White List.
            if !hci::<&str>("0x0010", &[])? {
                return Ok(false);
            }
            let Some(entry) = white_list_entry(address, *random) else {
                return Ok(false);
            };
            if !hci("0x0011", &entry)? {
                return Ok(false);
            }
            (0x03, false)
        }
    };
    if !hci("0x0006", &adv_params(filter_policy))? {
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

/// Put the controller in the state `advert` says. `managed` is what the
/// first attempt found: the manager is there or it is not, for the life of
/// the daemon. A pairing window goes through bluetoothd where it can, which
/// then owns the advertisement and restores it after a disconnect itself.
/// The other two states are raw HCI on every kernel: the managed advert has
/// no filter policy, and the 4.4 core re-issues its own Set Advertising
/// Parameters (policy 0x00) whenever it touches an instance, so an instance
/// must not exist while a bond is meant to be the only one allowed in. Once
/// we advertise raw, the controller stops on connect and nothing restarts it
/// but us: the poll does, on seeing the link go. Returns whether raw
/// advertising is on.
async fn advertise(conn: &Connection, managed: bool, advert: &Advert) -> bool {
    if managed {
        if *advert == Advert::Window {
            let _ = hci("0x000A", &["00"]);
            if register_advertisement(conn, true).await {
                return false;
            }
        } else {
            unregister_advertisement(conn).await;
        }
    }
    match start_advertising(advert) {
        Ok(true) => {
            match advert {
                Advert::Off => println!("couch-bt-hid: not advertising: no active bond"),
                Advert::Window => println!(
                    "couch-bt-hid: advertising as \"{ADV_NAME}\" (raw HCI, discoverable, open to anyone)"
                ),
                Advert::Only { address, random } => println!(
                    "couch-bt-hid: advertising as \"{ADV_NAME}\" for {address}{} only (raw HCI, white list, filter policy 0x03)",
                    if *random { " (random)" } else { "" }
                ),
            }
            *advert != Advert::Off
        }
        Ok(false) => {
            eprintln!("couch-bt-hid: an advertising HCI command was rejected");
            false
        }
        Err(e) => {
            eprintln!("couch-bt-hid: could not run hcitool for advertising: {e}");
            false
        }
    }
}

/// Stop advertising while the link we want is up: the controller would
/// refuse the enable anyway (one link on a 4.0 controller), and a managed
/// instance must not linger to be re-enabled by the kernel with an open
/// filter policy when the link drops. The poll resumes `advert` then.
async fn pause_advertising(conn: &Connection, managed: bool, advert: &Advert) -> bool {
    if managed {
        unregister_advertisement(conn).await;
    }
    let _ = hci("0x000A", &["00"]);
    if let Advert::Only { address, .. } = advert {
        println!("couch-bt-hid: link up; advertising for {address} resumes when it drops");
    }
    false
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
/// bluetoothd forwards as a notification to every device whose CCC for it
/// is on. Always emitted, whether or not this daemon saw a StartNotify:
/// bluetoothd calls StartNotify only when a CCC is written, and a bonded TV
/// does not write it again after bluetoothd restarts. couch-bluetoothd
/// restores the TV's stored subscription instead (third_party/bluez), so
/// the report reaches it with no StartNotify here at all. The first report
/// sent without a current StartNotify says so in the log, once until the
/// next Start/StopNotify, with whether one was ever seen.
async fn push(conn: &Connection, path: &str, value: Vec<u8>) {
    let Ok(iref) = conn.object_server().interface::<_, GattChar>(path).await else {
        return;
    };
    let mut c = iref.get_mut().await;
    if !c.notifying && !c.quiet {
        println!(
            "couch-bt-hid: sending {} reports with no StartNotify {}: only a subscription bluetoothd restored from storage receives them",
            c.label,
            if c.ever_notified {
                "since the last StopNotify"
            } else {
                "seen since start"
            }
        );
        c.quiet = true;
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

/// One remote device bluetoothd knows under hci0, as far as bonds care.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Device {
    path: OwnedObjectPath,
    /// Uppercase colon-hex, as bluetoothd reports it and the lib validates.
    address: String,
    /// A random (private or static) address rather than a public one: what
    /// the white list entry must say for the controller to match it.
    random: bool,
    name: String,
    connected: bool,
    paired: bool,
    bonded: bool,
}

impl Device {
    /// Paired covers bluetoothd versions without a Bonded property; on ones
    /// that have it, a bonded device is what survives a power cycle.
    fn keyed(&self) -> bool {
        self.paired || self.bonded
    }
    fn peer(&self) -> Peer {
        Peer {
            address: self.address.clone(),
            name: self.name.clone(),
        }
    }
}

/// Every Device1 under hci0, from one ObjectManager walk. A poll rather than
/// PropertiesChanged signals: a handful of devices every few seconds is
/// nothing, and a poll cannot miss a change made while we were not looking.
async fn devices(conn: &Connection) -> Vec<Device> {
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
        let Some(address) = text("Address").and_then(|a| normalize_address(&a)) else {
            continue;
        };
        let name = text("Name").or_else(|| text("Alias")).unwrap_or_default();
        out.push(Device {
            path,
            random: text("AddressType").as_deref() == Some("random"),
            address,
            name,
            connected: flag("Connected"),
            paired: flag("Paired"),
            bonded: flag("Bonded"),
        });
    }
    // A stable order: the log and the link line should not flip between
    // two devices from one poll to the next.
    out.sort_by(|a, b| a.address.cmp(&b.address));
    out
}

/// A device's label for the log: its name, or its address without one.
fn label(device: &Device) -> String {
    if device.name.is_empty() {
        device.address.clone()
    } else {
        format!("{} ({})", device.name, device.address)
    }
}

/// Remove one device, bond and all. A connected one is disconnected by
/// bluetoothd as part of the removal.
async fn forget_device(conn: &Connection, device: &Device) {
    let Ok(adapter) = Proxy::new(conn, "org.bluez", ADAPTER, "org.bluez.Adapter1").await else {
        return;
    };
    match adapter.call_method("RemoveDevice", &(&device.path,)).await {
        Ok(_) => println!("couch-bt-hid: forgot {}", label(device)),
        Err(e) => eprintln!("couch-bt-hid: could not forget {}: {e}", label(device)),
    }
}

/// Drop a device's link. The device stays known and bonded; only the
/// connection goes. bluetoothd answers NotConnected for one that already
/// went, which is not worth more than a line in the log.
async fn disconnect_device(conn: &Connection, device: &Device, why: &str) {
    let Ok(dev) = Proxy::new(conn, "org.bluez", &device.path, "org.bluez.Device1").await else {
        return;
    };
    match dev.call_method("Disconnect", &()).await {
        Ok(_) => println!("couch-bt-hid: disconnected {}: {why}", label(device)),
        Err(e) => eprintln!("couch-bt-hid: could not disconnect {}: {e}", label(device)),
    }
}

/// The active bond, in memory and on disk.
struct Active {
    address: Option<String>,
}

impl Active {
    /// What the last run left, if anything: the file holds an address or
    /// `none`. Without a file (a remote from before bonds were chosen), the
    /// one bond there is becomes the active one, so a TV paired under the
    /// previous daemon keeps working; with several, none is, and the app
    /// chooses.
    fn restore(devices: &[Device]) -> Self {
        if Path::new(ACTIVE_PATH).exists() {
            let address = couch_bt_hid::read_active(&[ACTIVE_PATH]);
            match &address {
                Some(a) => println!("couch-bt-hid: active bond {a} (remembered)"),
                None => println!("couch-bt-hid: no active bond (remembered)"),
            }
            return Active { address };
        }
        let mut keyed = devices.iter().filter(|d| d.keyed());
        let address = match (keyed.next(), keyed.next()) {
            (Some(only), None) => {
                println!(
                    "couch-bt-hid: no active bond remembered; adopting the one bond, {}",
                    label(only)
                );
                Some(only.address.clone())
            }
            _ => None,
        };
        let active = Active { address };
        active.save();
        active
    }
    fn set(&mut self, address: Option<String>) {
        if self.address != address {
            match &address {
                Some(a) => println!("couch-bt-hid: active bond {a}"),
                None => println!("couch-bt-hid: no active bond"),
            }
            self.address = address;
            self.save();
        }
    }
    fn save(&self) {
        let text = format!("{}\n", self.address.as_deref().unwrap_or("none"));
        if let Err(e) = std::fs::write(ACTIVE_PATH, text) {
            eprintln!("couch-bt-hid: could not write {ACTIVE_PATH}: {e}");
        }
    }
    fn is(&self, device: &Device) -> bool {
        self.address.as_deref() == Some(device.address.as_str())
    }
    /// What to advertise for this bond outside a window. The address type
    /// comes from bluetoothd's record of the device; a bond bluetoothd does
    /// not know (forgotten on this side, or never made) is white-listed as
    /// public, and nothing will connect until it pairs again.
    fn advert(&self, devices: &[Device]) -> Advert {
        match &self.address {
            None => Advert::Off,
            Some(address) => Advert::Only {
                address: address.clone(),
                random: devices
                    .iter()
                    .find(|d| d.address == *address)
                    .is_some_and(|d| d.random),
            },
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
    /// The bonds that existed when the window opened. They are not what the
    /// window is for, and a bonded TV that reconnects during it would hold
    /// the one link a 4.0 controller has while the new TV finds nothing to
    /// connect to; so they are dropped on sight until the window ends.
    known: Vec<String>,
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
            known: Vec::new(),
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
    server.at(APP, AppObjects).await?;

    server
        .at(
            "/couch/hid/app/s0",
            GattService {
                uuid: uuid16(DEVICE_INFO_SERVICE),
                handle: DEVICE_INFO_HANDLE,
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
                handle: BATTERY_HANDLE,
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
                handle: HID_HANDLE,
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
                &["encrypt-read"],
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
                &["encrypt-read"],
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
                &["encrypt-read", "encrypt-notify"],
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
                &["encrypt-read", "encrypt-notify"],
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

    // The bonds, and which of them is the active link. Then advertise for
    // it alone, over the white list; or not at all when there is none. The
    // managed advertisement, where bluetoothd has one, is only for windows.
    let managed = adv_manager_present(&conn).await;
    if !managed {
        println!("couch-bt-hid: no {ADV_MANAGER} on this kernel");
    }
    let mut known = devices(&conn).await;
    for d in known.iter().filter(|d| d.keyed()) {
        println!("couch-bt-hid: bond {}", label(d));
    }
    let mut active = Active::restore(&known);
    // A device connected from before (bluetoothd outlived a daemon restart)
    // that is not the active one goes now; the rules below apply from here.
    for d in known.iter().filter(|d| d.connected && !active.is(d)) {
        disconnect_device(&conn, d, "not the active bond").await;
    }
    let mut advert = active.advert(&known);
    let mut linked: Option<Device> = known.iter().find(|d| d.connected && active.is(d)).cloned();
    let mut raw_on = if linked.is_none() {
        advertise(&conn, managed, &advert).await
    } else {
        pause_advertising(&conn, managed, &advert).await
    };
    let mut pairing = Pairing::new();
    pairing.status.set_link(linked.as_ref().map(Device::peer));
    pairing.status.active = active.address.clone();
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
    // Delay, not tokio's default Burst. The tick is only polled while no link
    // is up (`raw_wanted` below), so through a three-minute link it misses a
    // dozen ticks, and Burst fired them all back to back when the link went:
    // on .162.dev hcidump showed eleven raw advertising sequences after one
    // `activate`, the last of them re-enabling advertising after the next TV
    // had connected. Delay fires one at most, then every 15 s from there.
    readvertise.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // The device poll: quick while a window is open, so the modal follows
    // the TV step by step; a slower tick otherwise, which is also how soon
    // after a disconnect the raw advertisement comes back.
    let mut poll = tokio::time::interval(Duration::from_millis(500));
    let mut slow_ticks_left: u32 = 0;
    loop {
        // Raw advertising is ours to keep up: the controller stops it on
        // connect and the kernel restarts nothing it did not start itself.
        // (A managed window is bluetoothd's to restore.)
        let raw_wanted =
            linked.is_none() && advert != Advert::Off && !(managed && advert == Advert::Window);
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            _ = readvertise.tick(), if raw_wanted => {
                // The poll restarts advertising as soon as it sees the link
                // go; this is the backstop for an enable the controller
                // refused because the disconnect had not finished yet, and
                // it reasserts the white list should the kernel's passive
                // scan have rewritten it.
                raw_on = advertise(&conn, managed, &advert).await;
            }
            _ = poll.tick() => {
                if !pairing.open() && slow_ticks_left > 0 {
                    slow_ticks_left -= 1;
                    continue;
                }
                slow_ticks_left = 3;
                keep_pairable(&conn, pairing.open()).await;
                known = devices(&conn).await;
                let connected: Vec<&Device> = known.iter().filter(|d| d.connected).collect();
                if !connected.is_empty() {
                    // Whatever is connected, the controller is no longer
                    // advertising: raw advertising must be restarted once the
                    // link goes.
                    raw_on = false;
                }
                if pairing.open() {
                    // Bonds from before the window are not what it is for.
                    for d in connected.iter().filter(|d| pairing.known.contains(&d.address)) {
                        disconnect_device(&conn, d, "bonded before the window").await;
                    }
                    let fresh = connected.iter().find(|d| !pairing.known.contains(&d.address));
                    linked = fresh.map(|d| (*d).clone());
                    let expired = pairing.until.is_some_and(|t| Instant::now() >= t);
                    let notifying = any_report_notifying(&conn).await;
                    if fresh.is_none() && !raw_on && !managed {
                        // The raw-path window: a TV that connected and left
                        // without bonding took the advertisement with it.
                        raw_on = advertise(&conn, managed, &advert).await;
                    }
                    match fresh {
                        Some(d) if d.keyed() && notifying => {
                            pairing.set(PairPhase::Done, &d.name);
                            pairing.status.bonded = Some(d.peer());
                            println!("couch-bt-hid: paired with {} and it subscribed", label(d));
                            active.set(Some(d.address.clone()));
                        }
                        Some(d) if d.keyed() => pairing.set(PairPhase::Paired, &d.name),
                        Some(d) => pairing.set(PairPhase::Connected, &d.name),
                        None => pairing.set(PairPhase::Pairing, ""),
                    }
                    if pairing.status.phase == PairPhase::Done || expired {
                        if expired {
                            pairing.set(PairPhase::Failed, "timeout");
                        }
                        pairing.until = None;
                        pairing.known.clear();
                        set_pairable(&conn, false).await;
                        advert = active.advert(&known);
                        // Done leaves the new bond connected: the white
                        // list is programmed when that link drops.
                        raw_on = if linked.as_ref().is_some_and(|d| active.is(d)) {
                            pause_advertising(&conn, managed, &advert).await
                        } else {
                            advertise(&conn, managed, &advert).await
                        };
                    }
                } else {
                    // One link, the active bond's. Anyone else who got in
                    // (bonded during a window that then chose another, or
                    // connected before `activate` moved on) goes.
                    for d in connected.iter().filter(|d| !active.is(d)) {
                        disconnect_device(&conn, d, "not the active bond").await;
                    }
                    let now = connected.iter().find(|d| active.is(d)).map(|d| (*d).clone());
                    if let (Some(gone), None) = (&linked, &now) {
                        println!("couch-bt-hid: {} disconnected", label(gone));
                    }
                    if let (None, Some(here)) = (&linked, &now) {
                        println!("couch-bt-hid: {} connected", label(here));
                    }
                    linked = now;
                    if linked.is_none() && !raw_on && advert != Advert::Off {
                        raw_on = advertise(&conn, managed, &advert).await;
                    }
                }
                pairing.status.set_link(linked.as_ref().map(Device::peer));
                pairing.status.active = active.address.clone();
                pairing.publish();
            }
            r = socket.recv(&mut buf) => {
                let Ok(n) = r else { continue };
                let cmd = String::from_utf8_lossy(&buf[..n]);
                let cmd = cmd.trim();
                match Control::parse(cmd) {
                    Ok(Some(Control::Pair)) => {
                        // Open to anyone for the window; the bonds stay, and
                        // the TV that bonds now becomes the active one. Every
                        // link is dropped first: a 4.0 controller does not
                        // advertise while it has one.
                        println!("couch-bt-hid: pairing window open for {PAIR_WINDOW_SECS}s");
                        known = devices(&conn).await;
                        pairing.known = known.iter().filter(|d| d.keyed()).map(|d| d.address.clone()).collect();
                        for d in known.iter().filter(|d| d.connected) {
                            disconnect_device(&conn, d, "pairing window").await;
                        }
                        linked = None;
                        set_pairable(&conn, true).await;
                        advert = Advert::Window;
                        raw_on = advertise(&conn, managed, &advert).await;
                        pairing.until = Some(Instant::now() + Duration::from_secs(PAIR_WINDOW_SECS));
                        pairing.set(PairPhase::Pairing, "");
                        pairing.status.bonded = None;
                        pairing.status.set_link(None);
                        pairing.publish();
                        poll.reset();
                    }
                    Ok(Some(Control::PairStop)) => {
                        if pairing.open() {
                            println!("couch-bt-hid: pairing window cancelled");
                            pairing.until = None;
                            pairing.known.clear();
                            set_pairable(&conn, false).await;
                            advert = active.advert(&known);
                            raw_on = advertise(&conn, managed, &advert).await;
                            pairing.set(PairPhase::Failed, "cancelled");
                            pairing.publish();
                        }
                    }
                    Ok(Some(Control::Forget(which))) => {
                        known = devices(&conn).await;
                        let mut gone = false;
                        for d in known.iter().filter(|d| which.as_ref().is_none_or(|a| *a == d.address)) {
                            forget_device(&conn, d).await;
                            gone |= active.is(d);
                        }
                        if let Some(a) = &which {
                            if !known.iter().any(|d| d.address == *a) {
                                println!("couch-bt-hid: forget {a}: no such device");
                            }
                        }
                        if gone || which.is_none() {
                            active.set(None);
                            if !pairing.open() {
                                advert = Advert::Off;
                                raw_on = advertise(&conn, managed, &advert).await;
                            }
                        }
                        pairing.status.active = active.address.clone();
                        pairing.status.bonded = None;
                        pairing.publish();
                    }
                    Ok(Some(Control::Activate(which))) => {
                        known = devices(&conn).await;
                        if let Some(a) = &which {
                            if !known.iter().any(|d| d.address == *a && d.keyed()) {
                                println!("couch-bt-hid: activate {a}: no bond for it; nothing connects until it pairs");
                            }
                        }
                        active.set(which);
                        if !pairing.open() {
                            for d in known.iter().filter(|d| d.connected && !active.is(d)) {
                                disconnect_device(&conn, d, "not the active bond").await;
                            }
                            linked = known.iter().find(|d| d.connected && active.is(d)).cloned();
                            advert = active.advert(&known);
                            raw_on = if linked.is_none() {
                                advertise(&conn, managed, &advert).await
                            } else {
                                pause_advertising(&conn, managed, &advert).await
                            };
                        }
                        pairing.status.set_link(linked.as_ref().map(Device::peer));
                        pairing.status.active = active.address.clone();
                        pairing.publish();
                    }
                    Ok(None) => {
                        if let Some(report) = keyboard_report(cmd) {
                            println!("couch-bt-hid: key {cmd} (keyboard {report:02x?})");
                            press_keyboard(&conn, report).await;
                        } else if let Some(usage) = consumer_usage(cmd) {
                            println!("couch-bt-hid: key {cmd} (usage {usage:#06x})");
                            press_consumer(&conn, usage).await;
                        } else {
                            eprintln!("couch-bt-hid: unknown key command {cmd:?}");
                        }
                    }
                    Err(why) => eprintln!("couch-bt-hid: refused {cmd:?}: {why}"),
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_honour_the_blob_offset() {
        let map = REPORT_MAP_BYTES;
        let at = |o: Option<u16>| {
            let mut options = HashMap::new();
            if let Some(o) = o {
                options.insert("offset".to_string(), OwnedValue::from(o));
            }
            read_at(map, &options)
        };
        assert_eq!(at(None), map);
        assert_eq!(at(Some(0)), map);
        // Stitching the chunks a small-MTU host reads gives back the map.
        let mut stitched = at(Some(0))[..49].to_vec();
        stitched.extend(at(Some(49)));
        assert_eq!(stitched, map);
        assert!(at(Some(map.len() as u16)).is_empty());
        assert!(at(Some(0x188)).is_empty());
    }

    #[test]
    fn the_gatt_application_is_listed_in_one_fixed_order_and_its_services_do_not_overlap() {
        // AppObjects answers in object-path order; the list is that order,
        // so bluetoothd's table follows it exactly.
        assert!(APP_OBJECTS.windows(2).all(|w| w[0] < w[1]));
        assert!(APP_OBJECTS
            .iter()
            .all(|p| p.starts_with("/couch/hid/app/s")));
        assert!(APP_OBJECTS.contains(&KEYBOARD_REPORT) && APP_OBJECTS.contains(&CONSUMER_REPORT));
        // Attributes per service as bluetoothd counts them: the declaration,
        // two per characteristic, one per CCC and one per descriptor.
        let (device_info, battery, hid) = (1 + 2, 1 + 2, 1 + 4 * 2 + 2 * (2 + 1 + 1));
        assert!(DEVICE_INFO_HANDLE + device_info <= BATTERY_HANDLE);
        assert!(BATTERY_HANDLE + battery <= HID_HANDLE);
        // The report CCCs a TV's stored subscriptions name (docs/bluetooth.md).
        assert_eq!(hid, 17);
        assert_eq!(
            (HID_HANDLE + 1 + 4 * 2 + 2, HID_HANDLE + 1 + 4 * 2 + 4 + 2),
            (0x012b, 0x012f)
        );
    }

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
    fn advertising_parameters_carry_the_filter_policy_last() {
        // LE Set Advertising Parameters is 15 bytes; the white-list filter
        // policy is the last one, which is what `hcidump -R` shows as byte
        // 14 of the command's parameters.
        for policy in [0x00, 0x03] {
            let raw = bytes(&adv_params(policy));
            assert_eq!(raw.len(), 15);
            assert_eq!(&raw[0..4], [0xa0, 0x00, 0xf0, 0x00]); // 100-150 ms
            assert_eq!(raw[4], 0x00); // ADV_IND
            assert_eq!(raw[5], 0x00); // own address public
            assert_eq!(raw[13], 0x07); // all three channels
            assert_eq!(raw[14], policy);
        }
    }

    #[test]
    fn white_list_entries_are_typed_and_little_endian() {
        // The address goes out least-significant byte first, after its type,
        // or the controller white-lists a device that does not exist.
        assert_eq!(
            bytes(&white_list_entry("44:27:45:4E:33:25", false).unwrap()),
            [0x00, 0x25, 0x33, 0x4e, 0x45, 0x27, 0x44]
        );
        assert_eq!(
            bytes(&white_list_entry("6A:B1:00:FF:12:34", true).unwrap())[0],
            0x01
        );
        assert!(white_list_entry("not an address", false).is_none());
        assert!(white_list_entry("44:27:45:4E:33", false).is_none());
    }

    fn device(address: &str, keyed: bool, random: bool) -> Device {
        Device {
            path: owned("/org/bluez/hci0/dev_x"),
            address: address.into(),
            random,
            name: String::new(),
            connected: false,
            paired: keyed,
            bonded: false,
        }
    }

    #[test]
    fn the_active_bond_decides_the_advertisement() {
        let lg = device("44:27:45:4E:33:25", true, false);
        let mac = device("6A:B1:00:FF:12:34", true, true);
        let none = Active { address: None };
        assert_eq!(none.advert(&[lg.clone(), mac.clone()]), Advert::Off);
        let active = Active {
            address: Some(mac.address.clone()),
        };
        // The white-list entry takes the device's address type from
        // bluetoothd; a bond bluetoothd does not know defaults to public.
        assert_eq!(
            active.advert(&[lg.clone(), mac.clone()]),
            Advert::Only {
                address: mac.address.clone(),
                random: true
            }
        );
        assert_eq!(
            active.advert(std::slice::from_ref(&lg)),
            Advert::Only {
                address: mac.address.clone(),
                random: false
            }
        );
        assert!(active.is(&mac) && !active.is(&lg));
    }

    #[test]
    fn pairing_state_renders_what_the_readers_parse() {
        // What the daemon writes is what the lib's parser (the GUI's and the
        // web page's reader) gets back, bond lines included.
        let mut p = Pairing::new();
        assert!(!p.open());
        p.set(PairPhase::Pairing, "");
        assert_eq!(p.status.render(), "pairing\n");
        p.set(PairPhase::Failed, "timeout");
        let tv = device("44:27:45:4E:33:25", true, false);
        p.status.set_link(Some(tv.peer()));
        p.status.active = Some(tv.address.clone());
        assert_eq!(
            p.status.render(),
            "failed timeout\nlink 44:27:45:4E:33:25\nactive 44:27:45:4E:33:25\n"
        );
        assert_eq!(PairStatus::parse(&p.status.render()), p.status);
        assert_eq!(p.status.peer.as_deref(), Some("44:27:45:4E:33:25"));
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
