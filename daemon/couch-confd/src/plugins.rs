//! Shared external integration ownership for HTTP and the panel's private socket.
//! Settings remain daemon-owned; children receive only their own connection data.
use couch_plugin::{Endpoint, Error, Failure, FieldKind, HostPolicy, Manifest, Request, Response};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

const IDLE: Duration = Duration::from_secs(60);
const MAX_ENDPOINTS: usize = 64;
const STORE_READ_WAIT: Duration = Duration::from_millis(250);
/// How long the children of a connection stay good without being read again.
/// A bridge's lamps are added and renamed by a person, not by a program, so
/// minutes are the right order; the browser can always ask for them afresh.
const CHILDREN_TTL: Duration = Duration::from_secs(5 * 60);

fn store_request_error(error: couch_integrations::Error) -> Error {
    if error.is_busy() {
        Error::Busy
    } else {
        Error::Invalid
    }
}

/// Why settings were not saved, or a connection not prepared: the words for a
/// person, and, when it was the host or the package that refused, the code and
/// the reason a protocol 3 package gave. The words are the ones this always
/// said; only a reason changes them, to the package's own.
#[derive(Debug, PartialEq)]
pub struct Refusal {
    pub text: String,
    pub failure: Option<Failure>,
}
impl From<Failure> for Refusal {
    fn from(failure: Failure) -> Self {
        Self {
            text: failure.to_string(),
            failure: Some(failure),
        }
    }
}
impl From<Error> for Refusal {
    fn from(code: Error) -> Self {
        Failure::from(code).into()
    }
}
impl From<String> for Refusal {
    fn from(text: String) -> Self {
        Self {
            text,
            failure: None,
        }
    }
}
impl From<&str> for Refusal {
    fn from(text: &str) -> Self {
        text.to_owned().into()
    }
}
impl From<Refusal> for String {
    fn from(refusal: Refusal) -> Self {
        refusal.text
    }
}
impl From<couch_integrations::Error> for Refusal {
    fn from(error: couch_integrations::Error) -> Self {
        error.to_string().into()
    }
}

/// Start the package and let it check these settings. Configure contacts no
/// device, so what comes back is about the settings alone; a protocol 3
/// package may say which one.
fn check_settings(
    directory: &Path,
    manifest: &Manifest,
    settings: &Value,
    policy: HostPolicy,
) -> Result<(), Failure> {
    let mut host =
        couch_plugin::Host::spawn_with_policy(directory, manifest, Duration::from_secs(3), policy)?;
    match host.request_detailed(Request::Configure {
        settings: settings.clone(),
    })? {
        Response::Ok => Ok(()),
        _ => Err(Error::Protocol.into()),
    }
}

/// A conversion has no form to mark a field on, so the setting is named in
/// the sentence.
fn named(manifest: &Manifest, failure: Failure) -> Refusal {
    let label = failure
        .reason
        .as_ref()
        .and_then(|reason| reason.field())
        .and_then(|id| manifest.settings.iter().find(|field| field.id == id))
        .map(|field| field.label.clone());
    let mut refusal = Refusal::from(failure);
    if let Some(label) = label {
        refusal.text = format!("{label}: {}", refusal.text);
    }
    refusal
}

struct Running {
    generation: String,
    settings: Value,
    endpoint: Arc<Endpoint>,
    used: Instant,
}

/// Protocol 3 (unreleased). Every child of one connection as its package last
/// listed them, with what they were read through: a package update or a
/// settings change makes the listing stale, because either can change what the
/// connection has behind it.
///
/// Memory only. Nothing here is written to disk: what a person chooses out of
/// a listing becomes a room device with its own snapshot, and that is the only
/// part that has to survive a restart.
struct Listing {
    generation: String,
    settings: Value,
    at: Instant,
    children: Vec<couch_sdk::Child>,
}

/// The children of a connection as a caller gets them.
#[derive(Debug)]
pub struct Children {
    pub children: Vec<couch_sdk::Child>,
    /// How long ago the package was asked. Zero for a listing just read.
    pub age: Duration,
    /// Whether the package was asked now, rather than the cache answering.
    /// Only a listing read afresh can heal a device a rollback stripped.
    pub fresh: bool,
}

/// How the children of a connection are read. `daemon` can never run a
/// protocol 3 package - the guard test below is what keeps the preview out of
/// every shipped build - so the one step that needs one is replaceable, and
/// the daemon's own tests put a listing in its place. The real one is
/// `Runtime::ask_package`, and it is what runs everywhere but in a test.
#[cfg(test)]
type Lister = Box<dyn Fn(&str) -> Result<Vec<couch_sdk::Child>, Failure> + Send + Sync>;

/// Settings a package has accepted for a legacy connection, and the package
/// selection they were accepted by.
pub struct LegacyAdoption {
    package: String,
    generation: String,
    manifest: Manifest,
    settings: Value,
}

pub struct Runtime {
    home: PathBuf,
    packages: couch_integrations::Store,
    endpoints: Mutex<HashMap<String, Running>>,
    catalog_generations: Mutex<HashMap<String, String>>,
    children: Mutex<HashMap<String, Listing>>,
    /// The number of user-table rebuilds this runtime has already answered
    /// for. A rebuild can move a package to a different user, so the children
    /// started under the old one are retired and come back under the new one.
    identity_rebuilds: AtomicU64,
    #[cfg(test)]
    lister: Mutex<Option<Lister>>,
}

impl Runtime {
    pub fn new(home: PathBuf) -> Self {
        let directory = std::env::var_os("COUCH_INTEGRATIONS_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("integrations"));
        let runtime = Self {
            home,
            packages: couch_integrations::Store::new(directory),
            endpoints: Mutex::new(HashMap::new()),
            catalog_generations: Mutex::new(HashMap::new()),
            children: Mutex::new(HashMap::new()),
            identity_rebuilds: AtomicU64::new(0),
            #[cfg(test)]
            lister: Mutex::new(None),
        };
        // Packages installed by an older Couch have no user of their own yet.
        // Give them one here, once, rather than have the first key press of
        // the day wait for the store's exclusive lock. A store that has never
        // held a package is left alone: there is nothing to name.
        if runtime.packages.root().join("state").is_dir() {
            if let Err(error) = runtime.packages.assign_identities() {
                // Said once, here. Nothing is printed per spawn: a package
                // still without a user is named when its request is refused.
                eprintln!("couch-confd: cannot give integration packages their own users: {error}");
            }
        }
        // Whatever happened above, including a table rebuilt during it, is the
        // state this runtime starts from; it has no children to retire yet.
        runtime
            .identity_rebuilds
            .store(couch_integrations::identity_rebuilds(), Ordering::Relaxed);
        runtime
    }

    /// Who this package's children run as. No fallback: a store that cannot
    /// say is reported, and the caller refuses rather than start a package
    /// under a user that belongs to another one.
    ///
    /// The package has to be installed first. Every id here came out of a
    /// request, a user is never given back, and a browser must not be able to
    /// spend the store's range on names of nothing.
    fn policy(&self, plugin: &str) -> Result<HostPolicy, couch_integrations::Error> {
        self.packages.generation(plugin)?;
        let (uid, gid) = self.packages.identity(plugin)?;
        Ok(HostPolicy::for_package(uid, gid))
    }

    /// A rebuilt user table can have moved a package to a different user, so
    /// every child this runtime holds may be running as the wrong one. Retire
    /// them; the next request starts each again under the user the table now
    /// says. Reading the counter is an atomic load, so this sits on the
    /// request path without costing anything.
    fn retire_children_of_a_rebuilt_table(&self, endpoints: &mut HashMap<String, Running>) {
        let rebuilds = couch_integrations::identity_rebuilds();
        if self.identity_rebuilds.swap(rebuilds, Ordering::Relaxed) == rebuilds {
            return;
        }
        eprintln!(
            "couch-confd: the integration user table was rebuilt; \
             package children restart under the users it now gives them"
        );
        endpoints.clear();
    }

    /// Read this connection's children with `lister` instead of asking its
    /// package. See [`Lister`].
    #[cfg(test)]
    pub fn list_with(&self, lister: Lister) {
        *self.lister.lock().unwrap_or_else(|e| e.into_inner()) = Some(lister);
    }

    pub fn catalog(&self) -> Result<Vec<Manifest>, String> {
        self.packages.list().map_err(|e| e.to_string())
    }

    pub fn manifest(&self, id: &str) -> Result<Manifest, String> {
        self.packages
            .resolve_wait(id, STORE_READ_WAIT)
            .map(|(_, manifest)| manifest)
            .map_err(|e| e.to_string())
    }
    pub fn changed_manifests(&self, ids: &[String]) -> Vec<Manifest> {
        let Ok(mut seen) = self.catalog_generations.lock() else {
            return Vec::new();
        };
        seen.retain(|id, _| ids.contains(id));
        let mut updates = Vec::new();
        for id in ids {
            let Ok(generation) = self.packages.generation(id) else {
                seen.remove(id);
                continue;
            };
            if seen.get(id) == Some(&generation) {
                continue;
            }
            if let Ok(manifest) = self.manifest(id) {
                updates.push(manifest);
                seen.insert(id.clone(), generation);
            }
        }
        updates
    }

    fn settings_path(&self, connection: &str) -> Result<PathBuf, Error> {
        if connection.is_empty() {
            return Err(Error::Invalid);
        }
        couch_sdk::connection_file(&self.home, connection, "plugin").map_err(|_| Error::Invalid)
    }

    pub fn settings(&self, connection: &str, plugin: &str) -> Result<Value, String> {
        let manifest = self.manifest(plugin)?;
        let path = self.settings_path(connection).map_err(|e| e.to_string())?;
        let saved = load_settings(&path).map_err(|e| e.to_string())?;
        Ok(redacted(&manifest, saved.as_ref()))
    }

    pub fn save_settings(
        &self,
        connection: &str,
        plugin: &str,
        patch: Value,
    ) -> Result<Value, Refusal> {
        // Before the lease: allocating a user of its own for a package seen
        // for the first time needs the store's exclusive lock, which a lease
        // held here would block.
        let policy = self.policy(plugin)?;
        // Admission validates every saved connection before activating a new
        // package. Keep its selection stable until these settings are durable,
        // so validation by an old child cannot race a package activation.
        let _lease = self.packages.read_lease()?;
        let (directory, manifest) = self.packages.resolve_wait(plugin, STORE_READ_WAIT)?;
        let path = self.settings_path(connection)?;
        let lock = crate::api::connections::lock_for(&path);
        let _guard = lock
            .try_lock()
            .map_err(|_| "Integration connection is busy")?;
        let saved = load_settings(&path)?;
        let settings = merge_settings(&manifest, saved.as_ref(), patch)?;
        // Configure validates the adapter's typed settings without requiring an
        // online TV. Do not save a schema-valid but unusable host/port.
        check_settings(&directory, &manifest, &settings, policy)?;
        let parent = path.parent().ok_or("Invalid settings path")?;
        fs::create_dir_all(parent).map_err(|_| "Cannot create connection settings directory")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
                .map_err(|_| "Cannot protect connection settings directory")?;
        }
        couch_sdk::save_private(&path, &settings)
            .map_err(|_| "Cannot save integration settings")?;
        self.endpoints
            .lock()
            .map_err(|_| "Integration registry lock failed")?
            .remove(connection);
        // Another address is another bridge, so what the old one listed says
        // nothing about this one.
        self.forget_children(connection);
        Ok(redacted(&manifest, Some(&settings)))
    }

    /// First half of giving a connection whose built-in client has left the
    /// OS to its installed package, and the slow half: start the package and
    /// let it check the settings carried over from the old connection
    /// (configure validates them and contacts no device). Nothing is written
    /// and no configuration lock is needed.
    pub fn prepare_legacy(
        &self,
        package: &str,
        settings: Value,
    ) -> Result<LegacyAdoption, Refusal> {
        let policy = self.policy(package)?;
        let _lease = self.packages.read_lease()?;
        let (directory, manifest) = self.packages.resolve_wait(package, STORE_READ_WAIT)?;
        let generation = self.packages.generation(package)?;
        let settings = manifest.with_defaults(settings)?;
        check_settings(&directory, &manifest, &settings, policy)
            .map_err(|failure| named(&manifest, failure))?;
        Ok(LegacyAdoption {
            package: package.to_owned(),
            generation,
            manifest,
            settings,
        })
    }

    /// Second half, quick, for the caller to run with the configuration
    /// locked: save the checked settings, then let `commit` switch the
    /// configuration. The package selection is held still from here to the end
    /// of the commit, and must be the one the settings were checked against;
    /// if a package operation came in between, the caller prepares again. A
    /// failed commit leaves an inert settings file the next attempt rewrites,
    /// and the old configuration in charge.
    pub fn adopt_legacy<T>(
        &self,
        connection: &str,
        prepared: &LegacyAdoption,
        commit: impl FnOnce(&Manifest) -> Result<T, String>,
    ) -> Result<T, String> {
        let _lease = self.packages.read_lease().map_err(|e| e.to_string())?;
        if self.packages.generation(&prepared.package).ok().as_ref() != Some(&prepared.generation) {
            return Err("The package changed while this connection was being prepared".into());
        }
        let path = self.settings_path(connection).map_err(|e| e.to_string())?;
        let lock = crate::api::connections::lock_for(&path);
        let _guard = lock
            .try_lock()
            .map_err(|_| "Integration connection is busy")?;
        let parent = path.parent().ok_or("Invalid settings path")?;
        fs::create_dir_all(parent).map_err(|_| "Cannot create connection settings directory")?;
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
            .map_err(|_| "Cannot protect connection settings directory")?;
        // The address saved with the old connection is the one in use. A file
        // already under this id is a leftover (an earlier trial of the
        // package, a deleted connection) and gives way to it.
        couch_sdk::save_private(&path, &prepared.settings)
            .map_err(|_| "Cannot save integration settings")?;
        let result = commit(&prepared.manifest)?;
        self.endpoints
            .lock()
            .map_err(|_| "Integration registry lock failed")?
            .remove(connection);
        self.forget_children(connection);
        Ok(result)
    }

    /// The reason a protocol 3 package gives for a refusal comes back with the
    /// code. Everything decided here, before the package is asked, is a code
    /// alone.
    ///
    /// `kind` is the kind of child `request`'s resource names, taken from the
    /// saved configuration by the caller. It is what the host's gate checks
    /// the request against and never reaches the wire; a request aimed at the
    /// connection itself passes `None`.
    pub fn execute(
        &self,
        connection: &str,
        plugin: &str,
        kind: Option<&str>,
        request: Request,
    ) -> Result<Response, Failure> {
        let queued = Instant::now();
        // The bridge cannot reconfigure a child or bypass the package
        // handshake, and it cannot ask for a listing: that is a conversation
        // of up to 64 round trips with limits only `children` below keeps, so
        // it is this daemon's to hold and neither `plugin.sock` nor an HTTP
        // body can start one.
        if !matches!(
            request,
            Request::Command { .. }
                | Request::Action { .. }
                | Request::Status { .. }
                | Request::Inputs
        ) {
            return Err(Error::Unsupported.into());
        }
        let path = self.settings_path(connection)?;
        let lock = crate::api::connections::lock_for(&path);
        let _guard = lock.try_lock().map_err(|_| Error::Busy)?;
        let generation = self
            .packages
            .generation(plugin)
            .map_err(|_| Error::Invalid)?;
        let settings = load_settings(&path)?.ok_or(Error::Invalid)?;
        let endpoint = self.endpoint_for(connection, plugin, &generation, settings, queued)?;
        if queued.elapsed() >= couch_plugin::QUEUE_TTL {
            return Err(Error::Expired.into());
        }
        endpoint.request_child_detailed(kind, request)
    }

    /// The running child of this connection, started if there is not one.
    /// The caller holds the connection's lock, so nothing else can start a
    /// second child for the same connection while this runs.
    fn endpoint_for(
        &self,
        connection: &str,
        plugin: &str,
        generation: &str,
        settings: Value,
        queued: Instant,
    ) -> Result<Arc<Endpoint>, Failure> {
        let existing = {
            let mut endpoints = self.endpoints.lock().map_err(|_| Error::Transport)?;
            self.retire_children_of_a_rebuilt_table(&mut endpoints);
            endpoints.retain(|_, entry| {
                entry.used.elapsed() < IDLE || Arc::strong_count(&entry.endpoint) > 1
            });
            if endpoints
                .get(connection)
                .is_some_and(|entry| entry.generation != generation || entry.settings != settings)
            {
                endpoints.remove(connection);
            }
            endpoints.get_mut(connection).map(|entry| {
                entry.used = Instant::now();
                entry.endpoint.clone()
            })
        };
        let endpoint = if let Some(endpoint) = existing {
            endpoint
        } else {
            // Handshake one child without holding the global registry lock:
            // another room's dead integration must not block healthy devices.
            let remaining = couch_plugin::QUEUE_TTL.saturating_sub(queued.elapsed());
            if remaining.is_zero() {
                return Err(Error::Expired.into());
            }
            // The version and the package's user, read together under one
            // shared lock: an install landing between the two would otherwise
            // turn a key press into a refusal. Bounded by what is left of this
            // request, so a key press never waits a store mutation's three
            // seconds; a press refused as busy succeeds on the next one.
            let (directory, manifest, (uid, gid)) = self
                .packages
                .resolve_with_identity(plugin, STORE_READ_WAIT.min(remaining))
                .map_err(store_request_error)?;
            manifest.validate_settings(&settings)?;
            let policy = HostPolicy::for_package(uid, gid);
            let endpoint = Arc::new(Endpoint::start_as(
                &directory,
                manifest,
                settings.clone(),
                couch_plugin::REQUEST_TIMEOUT,
                policy,
            )?);
            let mut endpoints = self.endpoints.lock().map_err(|_| Error::Transport)?;
            if endpoints.len() >= MAX_ENDPOINTS {
                return Err(Error::Busy.into());
            }
            endpoints.insert(
                connection.to_owned(),
                Running {
                    generation: generation.to_owned(),
                    settings,
                    endpoint: endpoint.clone(),
                    used: Instant::now(),
                },
            );
            endpoint
        };
        Ok(endpoint)
    }

    /// Protocol 3 (unreleased). Every child this connection offers: from the
    /// cache while it is fresh, from the package otherwise. `refresh` skips
    /// the cache.
    ///
    /// Reading them is up to 64 round trips and up to ten seconds, and this
    /// holds the connection's lock for all of it, so everything else aimed at
    /// that connection - another browser tab, the panel - is answered `busy`
    /// while a cold listing runs. That is why the panel never asks: it works
    /// from the saved devices, which carry their own snapshot, and only the
    /// browser and the stamping of a new device list anything.
    ///
    /// A package that answers nonsense loses its process and keeps its last
    /// good listing: the devices already made from it are real, and a stale
    /// list is more use to the person choosing than none.
    pub fn children(
        &self,
        connection: &str,
        plugin: &str,
        refresh: bool,
    ) -> Result<Children, Failure> {
        let path = self.settings_path(connection)?;
        let lock = crate::api::connections::lock_for(&path);
        let _guard = lock.try_lock().map_err(|_| Error::Busy)?;
        let (generation, settings) = self.package_selection(plugin, &path)?;
        if !refresh {
            if let Some(cached) = self.cached_children(connection, &generation, &settings) {
                return Ok(cached);
            }
        }
        match self.ask_package(connection, plugin, &generation, &settings) {
            Ok(children) => {
                if let Ok(mut listings) = self.children.lock() {
                    listings.insert(
                        connection.to_owned(),
                        Listing {
                            generation,
                            settings,
                            at: Instant::now(),
                            children: children.clone(),
                        },
                    );
                }
                Ok(Children {
                    children,
                    age: Duration::ZERO,
                    fresh: true,
                })
            }
            Err(failure) => {
                if failure.code == Error::Protocol {
                    self.retire_endpoint(connection);
                }
                Err(failure)
            }
        }
    }

    /// The package selection and the settings a listing is read through: what
    /// makes a cached one stale the moment either changes.
    fn package_selection(&self, plugin: &str, path: &Path) -> Result<(String, Value), Failure> {
        #[cfg(test)]
        if self
            .lister
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
        {
            // A `daemon` test has no protocol 3 package to select. The
            // settings are still read where there are any, so what they do to
            // the cache is tested by the real code.
            return Ok((String::new(), load_settings(path)?.unwrap_or(Value::Null)));
        }
        let generation = self
            .packages
            .generation(plugin)
            .map_err(|_| Error::Invalid)?;
        Ok((generation, load_settings(path)?.ok_or(Error::Invalid)?))
    }

    /// The cached listing, if it was read for this package selection and these
    /// settings and is not yet [`CHILDREN_TTL`] old. Anything else is dropped
    /// here rather than kept around to be wrong later.
    fn cached_children(
        &self,
        connection: &str,
        generation: &str,
        settings: &Value,
    ) -> Option<Children> {
        let mut listings = self.children.lock().ok()?;
        let listing = listings.get(connection)?;
        let age = listing.at.elapsed();
        if listing.generation != generation || &listing.settings != settings || age >= CHILDREN_TTL
        {
            listings.remove(connection);
            return None;
        }
        Some(Children {
            children: listing.children.clone(),
            age,
            fresh: false,
        })
    }

    /// The one step a `daemon` test cannot take, because it would need a
    /// protocol 3 package. The caller holds the connection's lock.
    fn ask_package(
        &self,
        connection: &str,
        plugin: &str,
        generation: &str,
        settings: &Value,
    ) -> Result<Vec<couch_sdk::Child>, Failure> {
        #[cfg(test)]
        if let Some(lister) = self
            .lister
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            return lister(connection);
        }
        let endpoint = self.endpoint_for(
            connection,
            plugin,
            generation,
            settings.clone(),
            Instant::now(),
        )?;
        // Every limit on a listing - 1024 children, 64 pages, ten seconds, no
        // cursor or id twice - belongs to `list_children`, and the same reader
        // serves the tests in `clients/`.
        couch_plugin::list_children(&mut |request| endpoint.request_detailed(request))
            .map_err(Failure::from)
    }

    /// The kind of one child, from the cache alone. It answers for a child
    /// nothing has been made from yet, which is how a person can try a lamp
    /// before adding it, and it never starts a package: the browser has just
    /// listed, or there is nothing to try.
    pub fn cached_child_kind(&self, connection: &str, resource: &str) -> Option<String> {
        let listings = self.children.lock().ok()?;
        let listing = listings.get(connection)?;
        if listing.at.elapsed() >= CHILDREN_TTL {
            return None;
        }
        listing
            .children
            .iter()
            .find(|child| child.id == resource)
            .map(|child| child.kind.clone())
    }

    /// What the connection last listed, with no chance of a round trip. Used
    /// where a listing would be wrong to start: filling in a device that is
    /// being saved.
    pub fn cached_child(&self, connection: &str, resource: &str) -> Option<couch_sdk::Child> {
        let listings = self.children.lock().ok()?;
        let listing = listings.get(connection)?;
        if listing.at.elapsed() >= CHILDREN_TTL {
            return None;
        }
        listing
            .children
            .iter()
            .find(|child| child.id == resource)
            .cloned()
    }

    fn forget_children(&self, connection: &str) {
        if let Ok(mut listings) = self.children.lock() {
            listings.remove(connection);
        }
    }

    fn retire_endpoint(&self, connection: &str) {
        let retired = self
            .endpoints
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(connection);
        drop(retired);
    }

    /// Stop the package child of a connection that has just been deleted.
    /// The caller holds the connection's settings lock, which `execute` holds
    /// for a whole request, so nothing is in flight and the last reference
    /// goes here: the child is killed and waited for before this returns.
    pub fn retire(&self, connection: &str) {
        self.retire_endpoint(connection);
        self.forget_children(connection);
    }

    pub fn reap(&self) {
        if let Ok(mut endpoints) = self.endpoints.lock() {
            // The ten-second sweep is also where a child left running as a
            // user a rebuilt table no longer gives its package goes, without
            // waiting for that connection to be asked for something.
            self.retire_children_of_a_rebuilt_table(&mut endpoints);
            endpoints.retain(|_, entry| {
                entry.used.elapsed() < IDLE || Arc::strong_count(&entry.endpoint) > 1
            });
        }
        if let Ok(mut listings) = self.children.lock() {
            listings.retain(|_, listing| listing.at.elapsed() < CHILDREN_TTL);
        }
    }
}

fn load_settings(path: &Path) -> Result<Option<Value>, Error> {
    match couch_sdk::load_private(path) {
        Ok(value) => Ok(Some(value)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(Error::Invalid),
    }
}

fn merge_settings(
    manifest: &Manifest,
    saved: Option<&Value>,
    patch: Value,
) -> Result<Value, Error> {
    let mut merged = saved
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let patch = patch.as_object().ok_or(Error::Invalid)?;
    for (key, value) in patch {
        let field = manifest
            .settings
            .iter()
            .find(|field| &field.id == key)
            .ok_or(Error::Invalid)?;
        if value.is_null() {
            merged.remove(key);
        } else if field.kind != FieldKind::Secret
            || value.as_str() != Some("")
            || !merged.contains_key(key)
        {
            merged.insert(key.clone(), value.clone());
        }
    }
    manifest.with_defaults(Value::Object(merged))
}

fn redacted(manifest: &Manifest, saved: Option<&Value>) -> Value {
    let mut settings = serde_json::Map::new();
    let mut secrets = Vec::new();
    for field in &manifest.settings {
        let value = saved
            .and_then(|s| s.get(&field.id))
            .or(field.default.as_ref());
        if field.kind == FieldKind::Secret {
            if value.and_then(Value::as_str).is_some_and(|s| !s.is_empty()) {
                secrets.push(field.id.clone());
            }
        } else if let Some(value) = value {
            settings.insert(field.id.clone(), value.clone());
        }
    }
    json!({"settings":settings,"configured":saved.is_some_and(|v| manifest.validate_settings(v).is_ok()),"secrets":secrets})
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs::OpenOptions, os::fd::AsRawFd};

    #[test]
    fn store_lock_contention_is_reported_as_busy_not_invalid() {
        let home = std::env::temp_dir().join(format!(
            "couch-plugin-store-busy-{}-{:?}",
            std::process::id(),
            Instant::now()
        ));
        fs::create_dir_all(home.join("integrations")).unwrap();
        let runtime = Runtime::new(home.clone());
        // Create and validate the store layout before taking its advisory lock.
        assert!(runtime.packages.resolve("sample").is_err());
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .open(runtime.packages.root().join(".lock"))
            .unwrap();
        // Another test forking a child at the wrong moment leaves that child
        // holding a copy of the descriptor `resolve` just closed, and with it
        // the shared lock, until its exec. Wait that out rather than fail.
        let deadline = Instant::now() + Duration::from_secs(5);
        while unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            assert!(Instant::now() < deadline, "the store lock never came free");
            std::thread::sleep(Duration::from_millis(10));
        }
        let error = runtime
            .packages
            .resolve_wait("sample", Duration::ZERO)
            .unwrap_err();
        assert!(error.is_busy());
        assert_eq!(store_request_error(error), Error::Busy);
        drop(lock);
        let _ = fs::remove_dir_all(home);
    }

    /// Protocol 3 is unreleased. Its only switch is couch-plugin's
    /// `protocol-3-preview` feature, and Cargo unifies features across a
    /// build, so one dependency (or dev-dependency) anywhere in this workspace
    /// that enabled it would switch it on in the daemon that ships. This runs
    /// with the daemon's own feature set and fails if that ever happens.
    #[test]
    fn the_daemon_is_never_built_with_the_protocol_3_preview() {
        assert_eq!(
            couch_plugin::accepted_protocol_version(),
            couch_plugin::PROTOCOL_VERSION
        );
        let mut next = manifest();
        next.protocol_version = couch_plugin::NEXT_PROTOCOL_VERSION;
        next.min_core_protocol_version = couch_plugin::NEXT_PROTOCOL_VERSION;
        assert_eq!(next.validate(), Err(Error::Incompatible));
    }

    /// A package that answers the handshake and then says `reply` to
    /// configure: a shell script printing two frames, as the conversion tests
    /// use.
    fn scripted(name: &str, protocol: u32, reply: Value) -> (PathBuf, Manifest) {
        use std::os::unix::fs::PermissionsExt;
        let directory = std::env::temp_dir().join(format!(
            "couch-confd-{name}-{}-{:?}",
            std::process::id(),
            Instant::now()
        ));
        fs::create_dir_all(&directory).unwrap();
        let mut described = serde_json::to_value(manifest()).unwrap();
        described["protocol_version"] = json!(protocol);
        described["min_core_protocol_version"] = json!(protocol);
        let manifest: Manifest = serde_json::from_value(described).unwrap();
        let mut script = String::from("#!/bin/sh\n");
        for value in [
            json!({"id":1,"body":{"type":"hello","manifest":manifest}}),
            json!({"id":2,"body":reply}),
        ] {
            let mut frame = Vec::new();
            couch_plugin::write_frame(&mut frame, &value).unwrap();
            let bytes: String = frame.iter().map(|byte| format!("\\{byte:03o}")).collect();
            script.push_str(&format!("printf '{bytes}'\n"));
        }
        script.push_str("sleep 5\n");
        let executable = directory.join("plugin");
        fs::write(&executable, script).unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o755)).unwrap();
        (directory, manifest)
    }

    #[test]
    fn a_refused_setting_comes_back_as_its_code_and_never_with_words_from_an_older_package() {
        let settings = json!({"host":"tv.local","port":23});
        let (directory, manifest) =
            scripted("refused", 1, json!({"type":"error","code":"invalid"}));
        let refused =
            check_settings(&directory, &manifest, &settings, HostPolicy::default()).unwrap_err();
        assert_eq!(refused, Failure::from(Error::Invalid));
        // What the settings form and the conversion say is what they always
        // said: the code's own sentence.
        let refusal = Refusal::from(refused);
        assert_eq!(refusal.text, Error::Invalid.to_string());
        assert_eq!(String::from(refusal), Error::Invalid.to_string());
        let _ = fs::remove_dir_all(directory);

        // Protocol 3 is switched off, so the only packages there are may not
        // give a reason. One that does is broken, and its words go nowhere.
        let (directory, manifest) = scripted(
            "worded",
            2,
            json!({"type":"error","code":"invalid","reason":
                {"kind":"invalid_setting","field":"port","text":"The port must not be 0"}}),
        );
        assert_eq!(
            check_settings(&directory, &manifest, &settings, HostPolicy::default()),
            Err(Error::Protocol.into())
        );
        let _ = fs::remove_dir_all(directory);

        let (directory, manifest) = scripted("accepted", 2, json!({"type":"ok"}));
        assert_eq!(
            check_settings(&directory, &manifest, &settings, HostPolicy::default()),
            Ok(())
        );
        let _ = fs::remove_dir_all(directory);
    }

    /// The panel now says how a key was pressed. Denon 0.2.1 is a protocol 2
    /// package built from an SDK that refuses a field it does not know, so a
    /// held volume key has to reach it as the bytes a tap always was. This is
    /// the daemon's own path (`execute`, the endpoint, the host's gate) with
    /// that package's manifest, and a child that keeps what it was sent.
    #[test]
    fn a_held_key_reaches_a_protocol_2_package_as_the_tap_it_always_was() {
        use couch_plugin::KeyPhase;
        use std::os::unix::fs::PermissionsExt;
        let home = std::env::temp_dir().join(format!(
            "couch-confd-held-{}-{:?}",
            std::process::id(),
            Instant::now()
        ));
        let manifest: Value =
            serde_json::from_str(include_str!("../tests/fixtures/denon-0.2.1-plugin.json"))
                .unwrap();
        assert_eq!(manifest["protocol_version"], 2);
        let directory = home.join("payload/usr/lib/couch/integrations/denon");
        fs::create_dir_all(directory.join("bin")).unwrap();
        fs::write(
            directory.join("manifest.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        let seen = home.join("seen");
        let mut script = String::from("#!/bin/sh\n");
        for value in [
            json!({"id":1,"body":{"type":"hello","manifest":manifest}}),
            json!({"id":2,"body":{"type":"ok"}}),
            json!({"id":3,"body":{"type":"ok"}}),
            json!({"id":4,"body":{"type":"ok"}}),
            json!({"id":5,"body":{"type":"ok"}}),
        ] {
            let mut frame = Vec::new();
            couch_plugin::write_frame(&mut frame, &value).unwrap();
            let bytes: String = frame.iter().map(|byte| format!("\\{byte:03o}")).collect();
            script.push_str(&format!("printf '{bytes}'\n"));
        }
        script.push_str(&format!("exec /bin/cat > '{}'\n", seen.display()));
        let executable = directory.join("bin/couch-plugin-denon");
        fs::write(&executable, script).unwrap();
        fs::set_permissions(executable, fs::Permissions::from_mode(0o755)).unwrap();
        let package = home.join("denon-fixture.apk");
        assert!(std::process::Command::new("tar")
            .args(["-czf"])
            .arg(&package)
            .arg("-C")
            .arg(home.join("payload"))
            .args([
                "usr/lib/couch/integrations/denon/manifest.json",
                "usr/lib/couch/integrations/denon/bin/couch-plugin-denon"
            ])
            .status()
            .unwrap()
            .success());
        let apk = home.join("fixture-apk");
        fs::write(&apk, "#!/bin/sh\nset -eu\nwhile [ $# -gt 0 ]; do\n  if [ \"$1\" = --root ]; then shift; destination=$1; fi\n  last=$1; shift\ndone\ntar -xzf \"$last\" -C \"$destination\"\n").unwrap();
        fs::set_permissions(&apk, fs::Permissions::from_mode(0o755)).unwrap();
        couch_integrations::Store::new(home.join("integrations"))
            .with_apk(apk)
            .install(&package)
            .unwrap();
        let runtime = Runtime::new(home.clone());
        let settings = runtime.settings_path("receiver").unwrap();
        fs::create_dir_all(settings.parent().unwrap()).unwrap();
        couch_sdk::save_private(&settings, &json!({"host":"avr.invalid","port":23})).unwrap();

        for phase in [KeyPhase::Tap, KeyPhase::Repeat, KeyPhase::LongPress] {
            assert_eq!(
                runtime.execute("receiver", "denon", None, Request::key("volume-up", phase)),
                Ok(Response::Ok),
                "{phase:?}"
            );
        }
        let frame = |text: &str| {
            let mut bytes = (text.len() as u32).to_be_bytes().to_vec();
            bytes.extend_from_slice(text.as_bytes());
            bytes
        };
        // The exact bytes, in the host's own field order. The command frame is
        // the one couch-plugin's golden file holds from the SDK revision the
        // published packages were built with.
        let expected = [
            frame(r#"{"id":1,"body":{"method":"hello","protocol_version":2}}"#),
            frame(
                r#"{"id":2,"body":{"method":"configure","settings":{"host":"avr.invalid","port":23}}}"#,
            ),
            frame(r#"{"id":3,"body":{"method":"command","function":"volume-up"}}"#),
            frame(r#"{"id":4,"body":{"method":"command","function":"volume-up"}}"#),
            frame(r#"{"id":5,"body":{"method":"command","function":"volume-up"}}"#),
        ]
        .concat();
        let deadline = Instant::now() + Duration::from_secs(5);
        while fs::read(&seen).unwrap_or_default().len() < expected.len()
            && Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(10));
        }
        let received = fs::read(&seen).unwrap();
        assert_eq!(
            String::from_utf8_lossy(&received),
            String::from_utf8_lossy(&expected)
        );
        assert_eq!(received, expected);
        runtime.retire("receiver");
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn a_reason_replaces_the_sentence_and_a_conversion_names_the_setting_it_blames() {
        let port = Failure {
            code: Error::Invalid,
            reason: Some(couch_plugin::Reason::InvalidSetting {
                field: "port".into(),
                text: "The port must not be 0".into(),
            }),
        };
        let refusal = Refusal::from(port.clone());
        assert_eq!(refusal.text, "The port must not be 0");
        assert_eq!(refusal.failure.as_ref(), Some(&port));
        // The form marks the field; a conversion has no form.
        assert_eq!(
            named(&manifest(), port.clone()).text,
            "Port: The port must not be 0"
        );
        assert_eq!(named(&manifest(), port.clone()).failure, Some(port));
        let pairing = Failure {
            code: Error::Unpaired,
            reason: Some(couch_plugin::Reason::Message {
                text: "Pair this TV again".into(),
            }),
        };
        assert_eq!(named(&manifest(), pairing).text, "Pair this TV again");
        assert_eq!(
            named(&manifest(), Error::Timeout.into()).text,
            Error::Timeout.to_string()
        );
        // Words of this daemon's own carry no code.
        assert_eq!(
            Refusal::from("Integration connection is busy").failure,
            None
        );
    }

    /// A home with one package really installed in its store, through the
    /// store's own admission path, so the package has the user admission gave
    /// it. The child answers the handshake and then says `ok` to everything.
    fn home_with_a_package(name: &str, id: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let home = std::env::temp_dir().join(format!(
            "couch-confd-{name}-{}-{:?}",
            std::process::id(),
            Instant::now()
        ));
        let mut described = serde_json::to_value(manifest()).unwrap();
        described["id"] = json!(id);
        let directory = home.join("payload/usr/lib/couch/integrations").join(id);
        fs::create_dir_all(&directory).unwrap();
        fs::write(
            directory.join("manifest.json"),
            serde_json::to_vec(&described).unwrap(),
        )
        .unwrap();
        let mut script = String::from("#!/bin/sh\n");
        for value in [
            json!({"id":1,"body":{"type":"hello","manifest":described}}),
            json!({"id":2,"body":{"type":"ok"}}),
            json!({"id":3,"body":{"type":"ok"}}),
        ] {
            let mut frame = Vec::new();
            couch_plugin::write_frame(&mut frame, &value).unwrap();
            let bytes: String = frame.iter().map(|byte| format!("\\{byte:03o}")).collect();
            script.push_str(&format!("printf '{bytes}'\n"));
        }
        script.push_str("sleep 5\n");
        let executable = directory.join("plugin");
        fs::write(&executable, script).unwrap();
        fs::set_permissions(executable, fs::Permissions::from_mode(0o755)).unwrap();
        let package = home.join("fixture.apk");
        assert!(std::process::Command::new("tar")
            .args(["-czf"])
            .arg(&package)
            .arg("-C")
            .arg(home.join("payload"))
            .arg(format!("usr/lib/couch/integrations/{id}/manifest.json"))
            .arg(format!("usr/lib/couch/integrations/{id}/plugin"))
            .status()
            .unwrap()
            .success());
        let apk = home.join("fixture-apk");
        fs::write(&apk, "#!/bin/sh\nset -eu\nwhile [ $# -gt 0 ]; do\n  if [ \"$1\" = --root ]; then shift; destination=$1; fi\n  last=$1; shift\ndone\ntar -xzf \"$last\" -C \"$destination\"\n").unwrap();
        fs::set_permissions(&apk, fs::Permissions::from_mode(0o755)).unwrap();
        couch_integrations::Store::new(home.join("integrations"))
            .with_apk(apk)
            .install(&package)
            .unwrap();
        home
    }

    fn user_table(home: &Path) -> Vec<u8> {
        fs::read(home.join("integrations/uids.json")).unwrap_or_default()
    }

    /// Every package id the daemon sees came out of a request, and a user is
    /// never given back while the range lasts. A browser naming packages that
    /// do not exist must therefore not be able to spend the range.
    #[test]
    fn settings_saved_against_a_package_that_is_not_installed_give_away_no_user() {
        let home = home_with_a_package("no-user-for-nothing", "sample");
        let runtime = Runtime::new(home.clone());
        let before = user_table(&home);
        assert!(!before.is_empty(), "admission gave the package its user");
        for invented in 0..20 {
            let refusal = runtime
                .save_settings("conn", &format!("invented-{invented}"), json!({}))
                .unwrap_err();
            assert!(!refusal.text.is_empty());
        }
        assert_eq!(user_table(&home), before);
        let _ = fs::remove_dir_all(home);
    }

    /// A package the store cannot speak about right now is busy, not invalid:
    /// the next key press finds the store free and works. The other refusal
    /// the identity path can give - every user in the range taken by a package
    /// that is still installed - is deliberately not busy, because waiting
    /// would change nothing; `store_request_error` turns that into `Invalid`.
    #[test]
    fn a_press_while_the_store_is_held_is_busy_and_the_next_one_works() {
        use std::{fs::OpenOptions, os::fd::AsRawFd};
        let home = home_with_a_package("busy-identity", "sample");
        let runtime = Runtime::new(home.clone());
        let settings = runtime.settings_path("conn").unwrap();
        fs::create_dir_all(settings.parent().unwrap()).unwrap();
        couch_sdk::save_private(&settings, &json!({"host":"tv.local","port":23})).unwrap();

        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .open(runtime.packages.root().join(".lock"))
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            assert!(Instant::now() < deadline, "the store lock never came free");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            runtime.execute("conn", "sample", None, Request::status()),
            Err(Error::Busy.into())
        );
        drop(lock);
        // Whatever the child then says, the refusal is no longer the store's.
        assert_ne!(
            runtime.execute("conn", "sample", None, Request::status()),
            Err(Error::Busy.into())
        );
        runtime.retire("conn");
        let _ = fs::remove_dir_all(home);
    }

    /// Converting an old built-in connection happens long after the package
    /// was installed, so its user is already in the table. Nothing is
    /// allocated a second time and nothing in the table moves.
    #[test]
    fn converting_an_old_connection_uses_the_user_admission_already_gave() {
        let home = home_with_a_package("legacy-user", "sample");
        let runtime = Runtime::new(home.clone());
        let before = user_table(&home);
        let prepared = runtime
            .prepare_legacy("sample", json!({"host":"tv.local","port":23}))
            .expect("the package accepts the carried-over settings");
        assert_eq!(prepared.package, "sample");
        assert_eq!(user_table(&home), before);
        let table: Value = serde_json::from_slice(&before).unwrap();
        assert_eq!(table["packages"]["sample"], 60000);
        assert_eq!(table["next"], 60001);
        let _ = fs::remove_dir_all(home);
    }

    fn manifest() -> Manifest {
        serde_json::from_value(json!({
            "protocol_version":1,"id":"sample","label":"Sample","version":"1.0.0","executable":"plugin",
            "capabilities":[],"settings":[
                {"id":"host","label":"Host","kind":"text","required":true},
                {"id":"token","label":"Token","kind":"secret"},
                {"id":"port","label":"Port","kind":"integer","default":23}
            ]
        })).unwrap()
    }
    #[test]
    fn secrets_are_never_returned_and_blank_preserves_them() {
        let manifest = manifest();
        let saved = json!({"host":"tv.local","token":"private"});
        let merged = merge_settings(
            &manifest,
            Some(&saved),
            json!({"host":"new.local","token":""}),
        )
        .unwrap();
        assert_eq!(merged["token"], "private");
        assert_eq!(merged["port"], 23);
        let view = redacted(&manifest, Some(&merged));
        assert!(view["settings"].get("token").is_none());
        assert!(!view.to_string().contains("private"));
        assert_eq!(view["secrets"], json!(["token"]));
        let cleared = merge_settings(&manifest, Some(&merged), json!({"token":null})).unwrap();
        assert!(cleared.get("token").is_none());
    }
    #[test]
    fn unknown_fields_and_invalid_values_cannot_be_persisted() {
        let manifest = manifest();
        for patch in [
            json!({"host":""}),
            json!({"host":"tv","port":"23"}),
            json!({"host":"tv","argv":"rm"}),
        ] {
            assert!(merge_settings(&manifest, None, patch).is_err());
        }
        assert_eq!(redacted(&manifest, None)["configured"], false);
    }
}

/// Protocol 3 (unreleased): the children of a connection, as this daemon
/// caches them.
///
/// No test here runs a protocol 3 package, and none can: the guard above is
/// what keeps the preview out of every build in this workspace. The listing
/// itself, and every way a bridge can answer nonsense, are tested against the
/// real thing in `clients/` (`couch-echo`'s bridge and `host::list_children`).
/// What is tested here is what this daemon does with the answer.
#[cfg(test)]
mod children_tests {
    use super::*;
    use couch_sdk::Child;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc,
    };

    fn home(name: &str) -> PathBuf {
        let home = std::env::temp_dir().join(format!(
            "couch-children-{name}-{}-{:?}",
            std::process::id(),
            Instant::now()
        ));
        fs::create_dir_all(&home).unwrap();
        home
    }

    fn lamp(id: &str, name: &str) -> Child {
        Child::new(id, "light", name).with_light(couch_model::LightTraits {
            dimmable: true,
            ..Default::default()
        })
    }

    #[test]
    fn a_listing_is_read_once_and_then_answered_from_memory_until_it_is_refreshed() {
        let home = home("cache");
        let runtime = Runtime::new(home.clone());
        let asked = Arc::new(AtomicUsize::new(0));
        let counter = asked.clone();
        runtime.list_with(Box::new(move |connection| {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(vec![lamp("lamp/1", connection)])
        }));

        let first = runtime.children("bridge", "echo", false).unwrap();
        assert_eq!(asked.load(Ordering::SeqCst), 1);
        assert!(first.fresh);
        assert_eq!(first.age, Duration::ZERO);
        assert_eq!(first.children[0].name, "bridge");

        let again = runtime.children("bridge", "echo", false).unwrap();
        assert_eq!(asked.load(Ordering::SeqCst), 1, "the cache answered");
        assert!(!again.fresh);
        // A second connection is its own listing, and gets its own answer.
        assert_eq!(
            runtime.children("other", "echo", false).unwrap().children[0].name,
            "other"
        );
        assert_eq!(asked.load(Ordering::SeqCst), 2);

        // Asking for it afresh always asks the package.
        assert!(runtime.children("bridge", "echo", true).unwrap().fresh);
        assert_eq!(asked.load(Ordering::SeqCst), 3);

        // Saved settings are part of what the listing was read through: a
        // different address is a different bridge.
        let path = runtime.settings_path("bridge").unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        couch_sdk::save_private(&path, &json!({"host": "bridge.local"})).unwrap();
        assert!(runtime.children("bridge", "echo", false).unwrap().fresh);
        couch_sdk::save_private(&path, &json!({"host": "elsewhere.local"})).unwrap();
        assert!(runtime.children("bridge", "echo", false).unwrap().fresh);
        assert_eq!(asked.load(Ordering::SeqCst), 5);

        // Retiring the connection forgets it; so does a reap once it is old.
        assert!(!runtime.children("bridge", "echo", false).unwrap().fresh);
        runtime.retire("bridge");
        assert!(runtime.children("bridge", "echo", false).unwrap().fresh);
        assert_eq!(asked.load(Ordering::SeqCst), 6);
        runtime
            .children
            .lock()
            .unwrap()
            .get_mut("bridge")
            .unwrap()
            .at -= CHILDREN_TTL;
        runtime.reap();
        assert!(!runtime.children.lock().unwrap().contains_key("bridge"));
        assert!(runtime.children("bridge", "echo", false).unwrap().fresh);
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn a_listing_older_than_the_lifetime_is_read_again() {
        let home = home("ttl");
        let runtime = Runtime::new(home.clone());
        runtime.list_with(Box::new(|_| Ok(vec![lamp("lamp/1", "Desk")])));
        assert!(runtime.children("bridge", "echo", false).unwrap().fresh);
        {
            let mut listings = runtime.children.lock().unwrap();
            let listing = listings.get_mut("bridge").unwrap();
            listing.at -= CHILDREN_TTL - Duration::from_secs(1);
        }
        let nearly = runtime.children("bridge", "echo", false).unwrap();
        assert!(!nearly.fresh);
        assert!(nearly.age >= CHILDREN_TTL - Duration::from_secs(2));
        runtime
            .children
            .lock()
            .unwrap()
            .get_mut("bridge")
            .unwrap()
            .at -= Duration::from_secs(2);
        assert!(runtime.children("bridge", "echo", false).unwrap().fresh);
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn a_bridge_that_answers_nonsense_loses_its_process_and_keeps_its_last_good_listing() {
        let home = home("nonsense");
        let runtime = Runtime::new(home.clone());
        let fail = Arc::new(AtomicUsize::new(0));
        let switch = fail.clone();
        runtime.list_with(Box::new(move |_| {
            if switch.load(Ordering::SeqCst) == 0 {
                Ok(vec![lamp("lamp/1", "Desk")])
            } else {
                Err(Error::Protocol.into())
            }
        }));
        assert_eq!(
            runtime.children("bridge", "echo", false).unwrap().children,
            vec![lamp("lamp/1", "Desk")]
        );
        fail.store(1, Ordering::SeqCst);
        assert_eq!(
            runtime.children("bridge", "echo", true).unwrap_err(),
            Error::Protocol.into()
        );
        // The lamps already in rooms are real, and a stale list is more use to
        // somebody choosing than none: what it last said is still there.
        let kept = runtime.children("bridge", "echo", false).unwrap();
        assert!(!kept.fresh);
        assert_eq!(kept.children, vec![lamp("lamp/1", "Desk")]);
        assert_eq!(
            runtime.cached_child_kind("bridge", "lamp/1").as_deref(),
            Some("light")
        );
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn nothing_else_reaches_a_connection_while_its_children_are_being_read() {
        let home = home("busy");
        let runtime = Arc::new(Runtime::new(home.clone()));
        let (started, listing_started) = mpsc::channel();
        let (release, wait) = mpsc::channel::<()>();
        let wait = Mutex::new(wait);
        runtime.list_with(Box::new(move |_| {
            started.send(()).unwrap();
            wait.lock().unwrap().recv().unwrap();
            Ok(vec![lamp("lamp/1", "Desk")])
        }));
        let listing = {
            let runtime = runtime.clone();
            std::thread::spawn(move || runtime.children("bridge", "echo", false))
        };
        listing_started.recv().unwrap();
        // A listing is up to sixty-four round trips and up to ten seconds, and
        // it holds the connection for all of it. That is why the panel never
        // starts one: it works from the saved devices.
        assert_eq!(
            runtime.execute("bridge", "echo", None, Request::status()),
            Err(Error::Busy.into())
        );
        assert_eq!(
            runtime.children("bridge", "echo", false).unwrap_err(),
            Error::Busy.into()
        );
        // Another connection is not held up by it.
        assert_ne!(
            runtime.execute("other", "echo", None, Request::status()),
            Err(Error::Busy.into())
        );
        release.send(()).unwrap();
        assert!(listing.join().unwrap().unwrap().fresh);
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn a_listing_is_never_something_the_panel_or_a_browser_can_ask_for() {
        let home = home("gate");
        let runtime = Runtime::new(home.clone());
        runtime.list_with(Box::new(|_| Ok(vec![lamp("lamp/1", "Desk")])));
        // `execute` is the whole of what `plugin.sock` and the HTTP routes can
        // reach. A listing has limits only `children` keeps, so it is refused
        // here before a connection is even looked up.
        for request in [
            Request::children(None),
            Request::children(Some("lamp/1".into())),
        ] {
            assert_eq!(
                runtime.execute("bridge", "echo", None, request),
                Err(Error::Unsupported.into())
            );
        }
        assert!(runtime.children.lock().unwrap().is_empty());
        let _ = fs::remove_dir_all(home);
    }
}
