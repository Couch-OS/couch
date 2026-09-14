//! Matter controller for Couch.
//!
//! The remote administers its own small Matter fabric: a certificate authority,
//! one controller certificate and a registry of commissioned nodes, all under a
//! private directory. A device that another ecosystem already set up joins that
//! fabric through the ecosystem's "pair with another app" sharing code, over the
//! LAN; this crate never provisions Wi-Fi or Thread credentials and does not use
//! Bluetooth. Each device therefore keeps its Apple, Google or Home Assistant
//! fabric and gains Couch as an additional controller.
//!
//! `matc` owns the protocol. This crate owns the blocking facade (the tokio
//! runtime stays private on one worker thread), the per-node inventory of
//! endpoints, and the light-shaped view the daemon and GUI consume.

use matc::clusters::codec::{
    basic_information_cluster as basic, descriptor_cluster as descriptor, level_control, on_off,
    operational_credential_cluster as opcreds,
};
use matc::clusters::defs::*;
use matc::controller::Connection;
use matc::devman::{DeviceManager, ManagerConfig};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

pub mod pairing;

/// The single controller identity on the fabric; there is one remote per fabric.
pub const CONTROLLER_ID: u64 = 1;
/// Environment override for the UDP bind address, for hosts whose default is wrong.
pub const BIND_ENV: &str = "COUCH_MATTER_BIND";
const OPERATION_TIMEOUT: Duration = Duration::from_secs(8);
const COMMISSION_TIMEOUT: Duration = Duration::from_secs(75);
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(4);
const INVENTORY_FILE: &str = "inventory.json";

#[derive(Debug)]
pub enum Error {
    Storage(std::io::Error),
    /// Caller input that no network operation would fix.
    Invalid(String),
    /// A device or the protocol failed; the text is for the user.
    Device(String),
    NotFound(String),
    Timeout(&'static str),
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Storage(e) => write!(f, "Matter state could not be saved: {e}"),
            Error::Invalid(m) | Error::Device(m) | Error::NotFound(m) => f.write_str(m),
            Error::Timeout(what) => write!(f, "The device did not answer in time ({what})"),
        }
    }
}
impl std::error::Error for Error {}
impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Storage(e)
    }
}
impl From<anyhow::Error> for Error {
    fn from(e: anyhow::Error) -> Self {
        Error::Device(format!("{e:#}"))
    }
}

/// One controllable endpoint on a node, as read from its Descriptor cluster.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Endpoint {
    pub endpoint: u16,
    pub device_types: Vec<u32>,
    pub on_off: bool,
    pub dimmable: bool,
}

/// A commissioned node. The `name` is what the user typed when pairing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub node_id: u64,
    pub name: String,
    #[serde(default)]
    pub vendor_name: String,
    #[serde(default)]
    pub product_name: String,
    #[serde(default)]
    pub endpoints: Vec<Endpoint>,
}

impl Node {
    /// `<node_id>/<endpoint>` for every endpoint with an On/Off cluster.
    pub fn light_ids(&self) -> impl Iterator<Item = String> + '_ {
        self.endpoints
            .iter()
            .filter(|e| e.on_off)
            .map(move |e| format!("{}/{}", self.node_id, e.endpoint))
    }
    fn light_name(&self, endpoint: u16) -> String {
        if self.endpoints.iter().filter(|e| e.on_off).count() > 1 {
            format!("{} ({endpoint})", self.name)
        } else {
            self.name.clone()
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Inventory {
    nodes: Vec<Node>,
}

/// The same shape the Hue and Home Assistant clients report, so the web UI and
/// the remote's light controls need no new cases. `entity_id` is
/// `<node_id>/<endpoint>`; `on` is `None` when the device did not answer.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Light {
    pub entity_id: String,
    pub name: String,
    pub on: Option<bool>,
    pub brightness_percent: Option<u8>,
    pub dimmable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Command {
    On,
    Off,
    Toggle,
    /// 0 turns the light off; anything else moves the level with the light on.
    Brightness(u8),
}

/// A device advertising for commissioning on this network. Sharing a device
/// from another ecosystem makes it appear here for a few minutes.
#[derive(Debug, Clone, Serialize)]
pub struct Discovered {
    pub name: Option<String>,
    pub vendor_id: Option<String>,
    pub product_id: Option<String>,
    pub discriminator: Option<String>,
    pub open: bool,
    pub addresses: Vec<String>,
}

/// One fabric: a private directory with the CA, controller key, `matc`'s
/// device registry and this crate's endpoint inventory. Cheap to call from
/// many threads; expensive to open, so hold on to it.
pub struct Controller {
    runtime: tokio::runtime::Runtime,
    manager: DeviceManager,
    dir: PathBuf,
    inventory: Mutex<Inventory>,
}

/// Dual-stack on Linux, where an IPv6 socket also sends to IPv4 peers and Matter
/// devices commonly answer only over IPv6 link-local. macOS refuses that mix.
pub fn default_bind_address() -> String {
    if let Ok(bind) = std::env::var(BIND_ENV) {
        return bind;
    }
    if cfg!(target_os = "linux") {
        "[::]:0".into()
    } else {
        "0.0.0.0:0".into()
    }
}

fn restrict(path: &Path, mode: u32) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
    }
    #[cfg(not(unix))]
    let _ = (path, mode);
}

impl Controller {
    /// Whether a fabric has been created under `dir`.
    pub fn exists(dir: &Path) -> bool {
        dir.join("config.json").is_file()
    }

    /// Load the fabric under `dir`, creating it (CA, controller certificate and a
    /// random fabric ID) on first use. Binds the controller's UDP socket and the
    /// mDNS listener; both are shared with other mDNS users on the host.
    pub fn open(dir: &Path) -> Result<Self, Error> {
        std::fs::create_dir_all(dir)?;
        restrict(dir, 0o700);
        let base = dir
            .to_str()
            .ok_or_else(|| Error::Invalid("Matter state directory must be UTF-8".into()))?
            .to_string();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("couch-matter")
            .enable_all()
            .build()?;
        let fresh = !Self::exists(dir);
        let manager = runtime.block_on(async {
            if fresh {
                let config = ManagerConfig {
                    fabric_id: loop {
                        let id: u64 = rand::random();
                        if id != 0 {
                            break id;
                        }
                    },
                    controller_id: CONTROLLER_ID,
                    local_address: default_bind_address(),
                };
                DeviceManager::create(&base, config).await
            } else {
                DeviceManager::load(&base).await
            }
        })?;
        // matc writes its PEM files with the process umask; keys stay private.
        let pem = dir.join("pem");
        restrict(&pem, 0o700);
        if let Ok(entries) = std::fs::read_dir(&pem) {
            for entry in entries.flatten() {
                restrict(&entry.path(), 0o600);
            }
        }
        for name in ["config.json", "devices.json"] {
            restrict(&dir.join(name), 0o600);
        }
        let inventory = match std::fs::read(dir.join(INVENTORY_FILE)) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| Error::Invalid(format!("Matter inventory is unreadable: {e}")))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Inventory::default(),
            Err(e) => return Err(e.into()),
        };
        Ok(Controller {
            runtime,
            manager,
            dir: dir.to_path_buf(),
            inventory: Mutex::new(inventory),
        })
    }

    pub fn fabric_id(&self) -> u64 {
        self.manager.config().fabric_id
    }

    fn save_inventory(&self, inventory: &Inventory) -> Result<(), Error> {
        let bytes = serde_json::to_vec_pretty(inventory).expect("inventory serializes");
        couch_sdk::save_private_bytes(&self.dir.join(INVENTORY_FILE), &bytes)?;
        // matc rewrites its registry with the process umask on every change;
        // the 0700 directory is the real guard, this keeps the listing tidy.
        restrict(&self.dir.join("devices.json"), 0o600);
        Ok(())
    }

    fn next_node_id(&self) -> u64 {
        let inventory = self.inventory.lock().unwrap();
        let registered = self.manager.list_devices().unwrap_or_default();
        inventory
            .nodes
            .iter()
            .map(|n| n.node_id)
            .chain(registered.iter().map(|d| d.node_id))
            .max()
            .unwrap_or(0)
            + 1
    }

    fn block<T>(
        &self,
        what: &'static str,
        timeout: Duration,
        future: impl std::future::Future<Output = anyhow::Result<T>>,
    ) -> Result<T, Error> {
        self.runtime
            .block_on(async { tokio::time::timeout(timeout, future).await })
            .map_err(|_| Error::Timeout(what))?
            .map_err(Error::from)
    }

    /// Commission a device that is advertising with this pairing code, over the
    /// LAN, and record its endpoints. Takes up to a minute: mDNS search, PASE,
    /// certificate exchange and CASE. A failure leaves nothing behind.
    pub fn commission(&self, code: &str, name: &str) -> Result<Node, Error> {
        let digits = pairing::normalize(code)?;
        let name = name.trim();
        if name.is_empty() || name.len() > 64 {
            return Err(Error::Invalid(
                "Give the device a name of up to 64 characters".into(),
            ));
        }
        let node_id = self.next_node_id();
        let node = self.block("commissioning", COMMISSION_TIMEOUT, async {
            let conn = self
                .manager
                .commission_with_code(&digits, node_id, name)
                .await?;
            inspect(&conn, node_id, name).await
        });
        let node = match node {
            Ok(node) => node,
            Err(e) => {
                // matc registers the node before inspection can fail; keep the
                // registry and inventory in step so a retry allocates cleanly.
                let _ = self.manager.remove_device(node_id);
                return Err(e);
            }
        };
        let mut inventory = self.inventory.lock().unwrap();
        inventory.nodes.retain(|n| n.node_id != node_id);
        inventory.nodes.push(node.clone());
        self.save_inventory(&inventory)?;
        Ok(node)
    }

    /// Everything commissioned, from the inventory; no network traffic.
    pub fn nodes(&self) -> Vec<Node> {
        self.inventory.lock().unwrap().nodes.clone()
    }

    pub fn node(&self, node_id: u64) -> Result<Node, Error> {
        self.nodes()
            .into_iter()
            .find(|n| n.node_id == node_id)
            .ok_or_else(|| {
                Error::NotFound("That Matter device is not paired with this remote".into())
            })
    }

    /// Re-read a node's endpoints and product names, for devices that gained
    /// endpoints through a firmware update.
    pub fn refresh(&self, node_id: u64) -> Result<Node, Error> {
        let name = self.node(node_id)?.name;
        let node = self.block("reading device", OPERATION_TIMEOUT * 2, async {
            let conn = self.manager.connect(node_id).await?;
            inspect(&conn, node_id, &name).await
        })?;
        let mut inventory = self.inventory.lock().unwrap();
        inventory.nodes.retain(|n| n.node_id != node_id);
        inventory.nodes.push(node.clone());
        inventory.nodes.sort_by_key(|n| n.node_id);
        self.save_inventory(&inventory)?;
        Ok(node)
    }

    pub fn rename(&self, node_id: u64, name: &str) -> Result<Node, Error> {
        let name = name.trim();
        if name.is_empty() || name.len() > 64 {
            return Err(Error::Invalid(
                "Give the device a name of up to 64 characters".into(),
            ));
        }
        let mut inventory = self.inventory.lock().unwrap();
        let node = inventory
            .nodes
            .iter_mut()
            .find(|n| n.node_id == node_id)
            .ok_or_else(|| {
                Error::NotFound("That Matter device is not paired with this remote".into())
            })?;
        node.name = name.to_string();
        let node = node.clone();
        let _ = self.manager.rename_device(node_id, name);
        self.save_inventory(&inventory)?;
        Ok(node)
    }

    /// Forget a node. Asks the device to drop this remote's fabric first, so it
    /// stops advertising for a controller that no longer exists; an unreachable
    /// device is still forgotten locally and keeps a stale fabric entry until
    /// it is factory reset.
    pub fn remove(&self, node_id: u64) -> Result<bool, Error> {
        self.node(node_id)?;
        let released = self
            .block("removing fabric", OPERATION_TIMEOUT, async {
                let conn = self.manager.connect(node_id).await?;
                let index = opcreds::read_current_fabric_index(&conn, 0).await?;
                let payload = opcreds::encode_remove_fabric(index)?;
                let res = conn
                    .invoke_request(
                        0,
                        CLUSTER_ID_OPERATIONAL_CREDENTIALS,
                        CLUSTER_OPERATIONAL_CREDENTIALS_CMD_ID_REMOVEFABRIC,
                        &payload,
                    )
                    .await?;
                invoke_ok(&res)
            })
            .is_ok();
        let _ = self.manager.remove_device(node_id);
        let mut inventory = self.inventory.lock().unwrap();
        inventory.nodes.retain(|n| n.node_id != node_id);
        self.save_inventory(&inventory)?;
        Ok(released)
    }

    /// Live state of every On/Off endpoint. A node that does not answer reports
    /// its lights with `on: None` rather than failing the whole list.
    pub fn lights(&self) -> Vec<Light> {
        let mut lights = Vec::new();
        for node in self.nodes() {
            let states = self.block("reading lights", OPERATION_TIMEOUT * 2, async {
                let conn = self.manager.connect(node.node_id).await?;
                let mut states = Vec::new();
                for endpoint in node.endpoints.iter().filter(|e| e.on_off) {
                    states.push(read_light(&conn, &node, endpoint).await);
                }
                Ok(states)
            });
            match states {
                Ok(states) => lights.extend(states),
                Err(_) => {
                    lights.extend(node.endpoints.iter().filter(|e| e.on_off).map(|e| Light {
                        entity_id: format!("{}/{}", node.node_id, e.endpoint),
                        name: node.light_name(e.endpoint),
                        on: None,
                        brightness_percent: None,
                        dimmable: e.dimmable,
                    }))
                }
            }
        }
        lights
    }

    /// Live state of one `<node_id>/<endpoint>`.
    pub fn light(&self, id: &str) -> Result<Light, Error> {
        let (node, endpoint) = self.resolve(id)?;
        self.block("reading light", OPERATION_TIMEOUT, async {
            let conn = self.manager.connect(node.node_id).await?;
            Ok(read_light(&conn, &node, &endpoint).await)
        })
    }

    /// Send a command and read the state back. The read is separate from the
    /// acknowledgement: a device may accept a command and report a different
    /// level, and a failed read must not make the command look failed.
    pub fn command(&self, id: &str, command: Command) -> Result<Light, Error> {
        let (node, endpoint) = self.resolve(id)?;
        if matches!(command, Command::Brightness(p) if p > 100) {
            return Err(Error::Invalid(
                "Brightness is a percentage from 0 to 100".into(),
            ));
        }
        if matches!(command, Command::Brightness(1..=100)) && !endpoint.dimmable {
            return Err(Error::Invalid(
                "This device has no brightness control".into(),
            ));
        }
        self.block("sending command", OPERATION_TIMEOUT, async {
            let conn = self.manager.connect(node.node_id).await?;
            let ep = endpoint.endpoint;
            let res = match command {
                Command::On => {
                    conn.invoke_request(ep, CLUSTER_ID_ON_OFF, CLUSTER_ON_OFF_CMD_ID_ON, &[])
                        .await?
                }
                Command::Off | Command::Brightness(0) => {
                    conn.invoke_request(ep, CLUSTER_ID_ON_OFF, CLUSTER_ON_OFF_CMD_ID_OFF, &[])
                        .await?
                }
                Command::Toggle => {
                    conn.invoke_request(ep, CLUSTER_ID_ON_OFF, CLUSTER_ON_OFF_CMD_ID_TOGGLE, &[])
                        .await?
                }
                Command::Brightness(percent) => {
                    let payload = level_control::encode_move_to_level_with_on_off(
                        level_from_percent(percent),
                        Some(0),
                        0,
                        0,
                    )?;
                    conn.invoke_request(
                        ep,
                        CLUSTER_ID_LEVEL_CONTROL,
                        CLUSTER_LEVEL_CONTROL_CMD_ID_MOVETOLEVELWITHONOFF,
                        &payload,
                    )
                    .await?
                }
            };
            invoke_ok(&res)?;
            Ok(read_light(&conn, &node, &endpoint).await)
        })
    }

    /// Devices currently advertising for commissioning on this network.
    pub fn discover(&self) -> Result<Vec<Discovered>, Error> {
        let found = self.block("discovery", DISCOVERY_TIMEOUT * 3, async {
            self.manager
                .discover_commissionable_devices(DISCOVERY_TIMEOUT)
                .await
        })?;
        Ok(found
            .into_iter()
            .map(|(_, info)| Discovered {
                name: info.name.clone(),
                vendor_id: info.vendor_id.clone(),
                product_id: info.product_id.clone(),
                discriminator: info.discriminator.clone(),
                open: !matches!(
                    info.commissioning_mode,
                    Some(matc::discover::CommissioningMode::No)
                ),
                addresses: info.ips.iter().map(|ip| ip.to_string()).collect(),
            })
            .collect())
    }

    fn resolve(&self, id: &str) -> Result<(Node, Endpoint), Error> {
        let (node_id, endpoint) = parse_id(id)?;
        let node = self.node(node_id)?;
        let endpoint = node
            .endpoints
            .iter()
            .find(|e| e.endpoint == endpoint && e.on_off)
            .cloned()
            .ok_or_else(|| Error::NotFound("That endpoint is not a switchable device".into()))?;
        Ok((node, endpoint))
    }
}

/// `<node_id>/<endpoint>`; the same rule the shared model validates.
pub fn parse_id(id: &str) -> Result<(u64, u16), Error> {
    let invalid = || Error::Invalid("Choose a valid Matter device".into());
    let (node, endpoint) = id.split_once('/').ok_or_else(invalid)?;
    if node.len() > 20 || !node.bytes().all(|b| b.is_ascii_digit()) {
        return Err(invalid());
    }
    let node: u64 = node.parse().map_err(|_| invalid())?;
    let endpoint: u16 = endpoint.parse().map_err(|_| invalid())?;
    if node == 0 || endpoint == 0 {
        return Err(invalid());
    }
    Ok((node, endpoint))
}

/// Matter levels run 1 to 254; both ends of the percentage map to those ends.
pub fn level_from_percent(percent: u8) -> u8 {
    ((u16::from(percent.min(100)) * 253 + 50) / 100) as u8 + 1
}

pub fn percent_from_level(level: u8) -> u8 {
    ((u16::from(level.clamp(1, 254) - 1) * 100 + 126) / 253) as u8
}

fn invoke_ok(res: &matc::messages::Message) -> anyhow::Result<()> {
    // InvokeResponses[0].Status.Status.Status; absent when the device answered
    // with command data instead of a bare status.
    match res.tlv.get_int(&[1, 0, 1, 1, 0]) {
        Some(0) | None => Ok(()),
        Some(status) => {
            anyhow::bail!("The device rejected the command (Matter status {status:#04x})")
        }
    }
}

async fn inspect(conn: &Connection, node_id: u64, name: &str) -> anyhow::Result<Node> {
    let vendor_name = basic::read_vendor_name(conn, 0).await.unwrap_or_default();
    let product_name = basic::read_product_name(conn, 0).await.unwrap_or_default();
    let parts = descriptor::read_parts_list(conn, 0).await?;
    let mut endpoints = Vec::new();
    for endpoint in parts {
        if endpoint == 0 {
            continue;
        }
        let servers = descriptor::read_server_list(conn, endpoint)
            .await
            .unwrap_or_default();
        let device_types = descriptor::read_device_type_list(conn, endpoint)
            .await
            .unwrap_or_default()
            .into_iter()
            .filter_map(|t| t.device_type)
            .collect();
        endpoints.push(Endpoint {
            endpoint,
            device_types,
            on_off: servers.contains(&CLUSTER_ID_ON_OFF),
            dimmable: servers.contains(&CLUSTER_ID_LEVEL_CONTROL),
        });
    }
    Ok(Node {
        node_id,
        name: name.to_string(),
        vendor_name,
        product_name,
        endpoints,
    })
}

async fn read_light(conn: &Connection, node: &Node, endpoint: &Endpoint) -> Light {
    let on = conn
        .read_request2(
            endpoint.endpoint,
            CLUSTER_ID_ON_OFF,
            CLUSTER_ON_OFF_ATTR_ID_ONOFF,
        )
        .await
        .ok()
        .and_then(|v| on_off::decode_on_off(&v).ok());
    let brightness_percent = if endpoint.dimmable && on.is_some() {
        conn.read_request2(
            endpoint.endpoint,
            CLUSTER_ID_LEVEL_CONTROL,
            CLUSTER_LEVEL_CONTROL_ATTR_ID_CURRENTLEVEL,
        )
        .await
        .ok()
        .and_then(|v| level_control::decode_current_level(&v).ok().flatten())
        .map(percent_from_level)
    } else {
        None
    };
    Light {
        entity_id: format!("{}/{}", node.node_id, endpoint.endpoint),
        name: node.light_name(endpoint.endpoint),
        on,
        brightness_percent,
        dimmable: endpoint.dimmable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_and_percentages_round_trip_at_both_ends() {
        assert_eq!(level_from_percent(0), 1);
        assert_eq!(level_from_percent(100), 254);
        assert_eq!(percent_from_level(1), 0);
        assert_eq!(percent_from_level(254), 100);
        assert_eq!(percent_from_level(level_from_percent(50)), 50);
        for percent in 0..=100u8 {
            assert_eq!(percent_from_level(level_from_percent(percent)), percent);
        }
    }

    #[test]
    fn ids_follow_the_model_rule() {
        assert_eq!(parse_id("7/1").unwrap(), (7, 1));
        for bad in [
            "",
            "7",
            "7/0",
            "0/1",
            "7/1/2",
            "a/1",
            "7/65536",
            "123456789012345678901/1",
            "-1/1",
        ] {
            assert!(matches!(parse_id(bad), Err(Error::Invalid(_))), "{bad:?}");
        }
    }

    #[test]
    fn light_names_distinguish_multi_endpoint_nodes() {
        let node = Node {
            node_id: 3,
            name: "Strip".into(),
            vendor_name: String::new(),
            product_name: String::new(),
            endpoints: vec![
                Endpoint {
                    endpoint: 1,
                    device_types: vec![0x100],
                    on_off: true,
                    dimmable: false,
                },
                Endpoint {
                    endpoint: 2,
                    device_types: vec![0x101],
                    on_off: true,
                    dimmable: true,
                },
                Endpoint {
                    endpoint: 3,
                    device_types: vec![0x0F],
                    on_off: false,
                    dimmable: false,
                },
            ],
        };
        assert_eq!(node.light_ids().collect::<Vec<_>>(), ["3/1", "3/2"]);
        assert_eq!(node.light_name(2), "Strip (2)");
        let single = Node {
            endpoints: node.endpoints[..1].to_vec(),
            ..node.clone()
        };
        assert_eq!(single.light_name(1), "Strip");
    }

    #[test]
    fn a_fresh_directory_gets_a_private_fabric_and_no_devices() {
        let dir = std::env::temp_dir().join(format!("couch-matter-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(!Controller::exists(&dir));
        let controller = Controller::open(&dir).unwrap();
        assert!(Controller::exists(&dir));
        assert_ne!(controller.fabric_id(), 0);
        assert!(controller.nodes().is_empty());
        assert!(controller.lights().is_empty());
        assert!(matches!(controller.light("1/1"), Err(Error::NotFound(_))));
        assert!(matches!(
            controller.commission("34970112333", "Lamp"),
            Err(Error::Invalid(_))
        ));
        assert!(matches!(
            controller.commission("34970112332", " "),
            Err(Error::Invalid(_))
        ));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&dir), 0o700);
            assert_eq!(mode(&dir.join("pem").join("ca-private.pem")), 0o600);
        }
        drop(controller);
        let reopened = Controller::open(&dir).unwrap();
        assert_ne!(reopened.fabric_id(), 0);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
