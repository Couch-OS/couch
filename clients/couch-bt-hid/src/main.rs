//! Couch as a Bluetooth LE HID peripheral (foundation: discovery + pairing).
//!
//! Runs on the kernel Bluetooth stack (via the couch-bt-bridge vhci controller)
//! and bluetoothd. This first step powers the adapter, registers a just-works
//! pairing agent, and advertises as "Couch Remote" so a TV can discover and
//! bond with it. The HID-over-GATT service (report map + input reports) is the
//! next step; the report map and key table below are already defined for it.
//!
//! D-Bus is spoken with zbus (pure Rust), so the binary stays static-musl like
//! the other clients; bluer/dbus were rejected because they need libdbus (C).
use std::collections::HashMap;

use zbus::zvariant::{ObjectPath, Value};
use zbus::{interface, Connection, Proxy};

const ADAPTER: &str = "/org/bluez/hci0";
const AGENT_PATH: &str = "/couch/hid/agent";
const ADV_PATH: &str = "/couch/hid/adv0";
const HID_SERVICE_UUID: &str = "00001812-0000-1000-8000-00805f9b34fb";

/// Just-works pairing agent: a remote has no keypad for a passkey, so every
/// request is accepted. Void methods accept; a rejection would return an error.
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

/// The LE advertisement BlueZ reads back after RegisterAdvertisement.
struct Advertisement;

#[interface(name = "org.bluez.LEAdvertisement1")]
impl Advertisement {
    async fn release(&self) {}

    #[zbus(property, name = "Type")]
    fn type_(&self) -> String {
        "peripheral".to_string()
    }
    #[zbus(property, name = "LocalName")]
    fn local_name(&self) -> String {
        "Couch Remote".to_string()
    }
    #[zbus(property, name = "ServiceUUIDs")]
    fn service_uuids(&self) -> Vec<String> {
        vec![HID_SERVICE_UUID.to_string()]
    }
    #[zbus(property, name = "Appearance")]
    fn appearance(&self) -> u16 {
        0x03c1 // HID keyboard
    }
    #[zbus(property, name = "Discoverable")]
    fn discoverable(&self) -> bool {
        true
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
    props
        .call_method("Set", &("org.bluez.Adapter1", prop, value))
        .await?;
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> zbus::Result<()> {
    let conn = Connection::system().await?;

    conn.object_server().at(AGENT_PATH, Agent).await?;
    conn.object_server().at(ADV_PATH, Advertisement).await?;

    set_adapter(&conn, "Powered", Value::from(true)).await?;
    set_adapter(&conn, "Alias", Value::from("Couch Remote")).await?;
    set_adapter(&conn, "Pairable", Value::from(true)).await?;

    let agent_mgr = Proxy::new(&conn, "org.bluez", "/org/bluez", "org.bluez.AgentManager1").await?;
    let agent_path = ObjectPath::try_from(AGENT_PATH)?;
    agent_mgr
        .call_method("RegisterAgent", &(&agent_path, "NoInputNoOutput"))
        .await?;
    agent_mgr
        .call_method("RequestDefaultAgent", &(&agent_path,))
        .await?;

    // Advertising. This kernel is 3.18, which predates the MGMT "Add
    // Advertising" command (Linux 4.x), so bluetoothd exports no
    // LEAdvertisingManager1 on the controller. Try it anyway for a future
    // kernel; when it is absent, fall back to raw HCI (the next step) rather
    // than failing, so the adapter and pairing agent stay active.
    let adv_path = ObjectPath::try_from(ADV_PATH)?;
    let options: HashMap<String, Value> = HashMap::new();
    let advertised = async {
        let adv_mgr =
            Proxy::new(&conn, "org.bluez", ADAPTER, "org.bluez.LEAdvertisingManager1").await?;
        adv_mgr
            .call_method("RegisterAdvertisement", &(&adv_path, options))
            .await?;
        Ok::<(), zbus::Error>(())
    }
    .await;
    match advertised {
        Ok(()) => println!("couch-bt-hid: advertising as \"Couch Remote\" via bluetoothd"),
        Err(_) => {
            let _ = conn.object_server().remove::<Advertisement, _>(&adv_path).await;
            println!(
                "couch-bt-hid: bluetoothd has no LE advertising on this kernel; \
                 the adapter and just-works pairing are up, advertising is the raw-HCI next step"
            );
        }
    }
    tokio::signal::ctrl_c().await.ok();
    Ok(())
}

// --- The next step: HID-over-GATT. Defined now, served next. -----------------

/// Report IDs, matched by the Report Map and each Report Reference descriptor.
#[allow(dead_code)]
const KEYBOARD_ID: u8 = 1;
#[allow(dead_code)]
const CONSUMER_ID: u8 = 2;

/// Keyboard (boot protocol, report id 1) + consumer control (report id 2).
#[allow(dead_code)]
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

/// Command word to Consumer-page usage. Consumer usages navigate TVs more
/// reliably than keyboard arrows, so the remote's keys go out on this page.
#[allow(dead_code)]
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
