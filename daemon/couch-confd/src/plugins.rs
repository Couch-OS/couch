//! Shared external integration ownership for HTTP and the panel's private socket.
//! Settings remain daemon-owned; children receive only their own connection data.
use couch_plugin::{
    Credential, Endpoint, Error, Failure, FieldKind, Host, HostPolicy, Manifest, PairFailure,
    PairInput, PairPrompt, PairStep, Request, Response,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant, SystemTime},
};

const IDLE: Duration = Duration::from_secs(60);
const MAX_ENDPOINTS: usize = 64;
const STORE_READ_WAIT: Duration = Duration::from_millis(250);
/// How long the children of a connection stay good without being read again.
/// A bridge's lamps are added and renamed by a person, not by a program, so
/// minutes are the right order; the browser can always ask for them afresh.
const CHILDREN_TTL: Duration = Duration::from_secs(5 * 60);
/// Protocol 3 (unreleased). How many pairings may be in flight across the
/// whole remote. Each one is a package child of its own, waiting on somebody's
/// television; eight is more browsers than this house has.
const MAX_PAIRINGS: usize = 8;
/// Protocol 3 (unreleased). How many packages may keep a child alive through
/// the idle reaper at once (the owner's cap). Beyond it the least recently
/// used lose the exemption and are reaped as anything else is.
const MAX_KEEP_ALIVE: usize = 8;
/// How long the write that ends a pairing waits for the connection's settings
/// lock. Longer than any request that could be holding it, because the key is
/// already in hand and asking the device again would mean pairing again.
#[cfg(not(test))]
const DONE_LOCK_WAIT: Duration = Duration::from_secs(15);
/// The same wait, short enough that a test can watch a key be parked and then
/// written on the next poll rather than spend fifteen seconds doing it. The
/// shipped value is above.
#[cfg(test)]
const DONE_LOCK_WAIT: Duration = Duration::from_millis(300);
/// The least time a dialog that polls may go quiet before Couch decides the
/// browser is gone and ends the attempt.
const BROWSER_GONE: Duration = Duration::from_secs(15);
/// How long the package gets to be told a pairing is over.
const CANCEL_TIMEOUT: Duration = Duration::from_secs(2);
/// What a pairing that is finishing asks the dialog to wait before it polls
/// again, while the settings lock is busy.
const FINISHING_POLL_MS: u32 = 1000;

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
    credential: Option<&Credential>,
    policy: HostPolicy,
) -> Result<(), Failure> {
    let mut host =
        couch_plugin::Host::spawn_with_policy(directory, manifest, Duration::from_secs(3), policy)?;
    match host.request_detailed(Request::configure_with(settings.clone(), credential))? {
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
    /// The key this child was configured with. A key that has been written,
    /// rotated or forgotten since is a different child.
    credential: Option<Credential>,
    endpoint: Arc<Endpoint>,
    used: Instant,
    /// Protocol 3 (unreleased): its package asked to stay alive between
    /// requests. False for every package a shipped build can run.
    keep_alive: bool,
    /// Whether that request is being honoured right now, which the reaper
    /// decides: only a connection a device still refers to, and only for the
    /// [`MAX_KEEP_ALIVE`] most recently used of them.
    pinned: bool,
}

/// Whether this child stays for now. The two idle retains are this one
/// predicate: an entry is kept while it is young, while somebody is holding it
/// mid-request, or while it is pinned by `keep_alive`.
fn retained(entry: &Running) -> bool {
    entry.used.elapsed() < IDLE || Arc::strong_count(&entry.endpoint) > 1 || entry.pinned
}

/// Protocol 3 (unreleased): one pairing conversation, and the package child
/// that is having it.
///
/// The child is this session's alone and is never in `endpoints`, so no reap,
/// no generation change and no settings write can take it away, and the
/// connection's ordinary child keeps serving on the old key for as long as
/// this runs. It is never restarted either: a package that dies mid-pairing
/// ends the attempt rather than starting again behind the person's back.
/// What [`Runtime::pair_package`] answers with.
struct PairPackage {
    manifest: Manifest,
    /// How this package pairs, or `None` if it does not. `None` for every
    /// package a shipped build can run.
    pairing: Option<couch_plugin::Pairing>,
    generation: String,
}

struct PairingSlot {
    /// The `<session>` in the URL: 128 bits of the host's own, never the id
    /// the package minted for itself (which the [`Host`] keeps). It is out
    /// here so that cancelling can name a session without waiting on a call
    /// that is already in flight.
    token: String,
    session: Mutex<PairingSession>,
}

struct PairingSession {
    plugin: String,
    generation: String,
    manifest: Manifest,
    /// The package child having this conversation, until the conversation is
    /// over. Never restarted, and never in `endpoints`.
    child: Option<Box<dyn PairChild>>,
    deadline: Instant,
    last_poll: Instant,
    /// The prompt the dialog is showing, and how long it was told to wait.
    prompt: Option<PairPrompt>,
    poll_after_ms: u32,
    /// A `done` the package has already given that could not be written yet,
    /// because the connection's settings lock was busy. The next poll retries
    /// the write; the package is never asked again.
    pending_done: Option<(Credential, Option<Value>, String)>,
}

impl PairingSession {
    /// Whether this attempt is over, whoever stopped watching it. A dialog
    /// that polls has to keep polling; one waiting for a typed code has only
    /// the overall deadline, because a person typing is not a browser gone.
    fn expired(&self, now: Instant) -> bool {
        if now >= self.deadline {
            return true;
        }
        if self.poll_after_ms == 0 {
            return false;
        }
        let quiet = BROWSER_GONE.max(Duration::from_millis(u64::from(self.poll_after_ms) * 3));
        now.saturating_duration_since(self.last_poll) >= quiet
    }

    /// Tell the package it is over and let its child go.
    fn end(&mut self) {
        if let Some(mut child) = self.child.take() {
            child.cancel();
        }
    }
}

/// What a pairing call answers, with the key already gone: the API layer
/// cannot see one, because this type has nowhere to put it.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum PairReply {
    Waiting {
        prompt: PairPrompt,
        poll_after_ms: u32,
    },
    Done {
        summary: String,
        /// The redacted view of the settings now saved for the connection,
        /// the same shape `GET …/plugin/settings` answers with.
        settings: Value,
    },
    Failed {
        reason: PairFailure,
        #[serde(skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
}

impl PairReply {
    pub fn is_final(&self) -> bool {
        !matches!(self, Self::Waiting { .. })
    }
    fn failed(reason: PairFailure, message: impl Into<String>) -> Self {
        Self::Failed {
            reason,
            message: Some(message.into()),
        }
    }
}

/// A pairing that has just begun.
pub struct PairStarted {
    pub session: String,
    pub step: PairReply,
    /// How long the whole attempt has left, in seconds.
    pub expires_in: u64,
}

/// Why a pairing call was refused before it became a step.
#[derive(Debug)]
pub enum PairError {
    /// This connection's package does not pair, or there is no package.
    DoesNotPair,
    /// Too many pairings are already in flight.
    TooMany,
    /// No such pairing: never started, already finished, or expired.
    Unknown,
    /// The input is not what the prompt asked for. The package is not told.
    BadInput,
    /// Everything with words for a person: refused settings, a busy store, a
    /// package that could not be started.
    Refused(Refusal),
    /// The key could not be written. The pairing itself worked.
    Storage(String),
}

impl From<Refusal> for PairError {
    fn from(refusal: Refusal) -> Self {
        Self::Refused(refusal)
    }
}
impl From<Error> for PairError {
    fn from(code: Error) -> Self {
        Self::Refused(code.into())
    }
}
impl From<Failure> for PairError {
    fn from(failure: Failure) -> Self {
        Self::Refused(failure.into())
    }
}
impl From<couch_integrations::Error> for PairError {
    fn from(error: couch_integrations::Error) -> Self {
        Self::Refused(error.into())
    }
}
impl From<&str> for PairError {
    fn from(text: &str) -> Self {
        Self::Refused(text.into())
    }
}

/// What is kept beside the key so a page can say what was paired. Not a
/// secret, and removed with the key.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PairingRecord {
    summary: String,
    /// Seconds since the epoch, for a page that wants to say when.
    paired_at: u64,
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

/// Protocol 3 (unreleased): the package a pairing conversation is being had
/// with.
///
/// [`Host`] is the only one there is, and it is a package process of this
/// pairing's own. A `daemon` test cannot have one - the guard test above is
/// what keeps a protocol 3 package out of every build in this workspace - so
/// this is the seam its tests script, exactly as `Lister` is for a listing.
/// Everything above it, which is all of the rules, is the real code.
pub(crate) trait PairChild: Send {
    fn start(
        &mut self,
        settings: Value,
        credential: Option<&Credential>,
    ) -> Result<PairStep, Failure>;
    fn step(&mut self, input: Option<PairInput>) -> Result<PairStep, Failure>;
    /// Tell the package it is over. Best effort: nothing is stored either
    /// way, and the child is killed when this is dropped.
    fn cancel(&mut self);
}

impl PairChild for Host {
    fn start(
        &mut self,
        settings: Value,
        credential: Option<&Credential>,
    ) -> Result<PairStep, Failure> {
        self.pair_start(settings, credential).map(|(_, step)| step)
    }
    fn step(&mut self, input: Option<PairInput>) -> Result<PairStep, Failure> {
        self.pair_continue(input)
    }
    fn cancel(&mut self) {
        let _ = self.set_timeout(CANCEL_TIMEOUT);
        let _ = Host::pair_cancel(self);
    }
}

/// How a `daemon` test stands in for a protocol 3 package.
///
/// Exactly two things are supplied that a shipped build can never have. One is
/// the `pairing` a manifest would declare, which `Manifest::validate` refuses
/// below protocol 3 while no protocol 3 manifest validates with the switch off.
/// The other is the conversation itself. `manifest` is an ordinary one this
/// build does accept, so every settings rule below it is the real one. See
/// [`PairChild`].
#[cfg(test)]
struct ScriptedPairing {
    manifest: Manifest,
    pairing: couch_plugin::Pairing,
    generation: String,
    open: OpenPairChild,
}

/// How a scripted pairing starts a conversation for one connection.
#[cfg(test)]
type OpenPairChild = Box<dyn Fn(&str) -> Result<Box<dyn PairChild>, Failure> + Send + Sync>;

/// Settings a package has accepted for a legacy connection, and the package
/// selection they were accepted by.
pub struct LegacyAdoption {
    package: String,
    generation: String,
    manifest: Manifest,
    settings: Value,
    /// Protocol 3 (unreleased): the key the built-in client kept, as its
    /// package's credential. `None` for every row there is today.
    credential: Option<Credential>,
}

pub struct Runtime {
    home: PathBuf,
    packages: couch_integrations::Store,
    endpoints: Mutex<HashMap<String, Running>>,
    catalog_generations: Mutex<HashMap<String, String>>,
    children: Mutex<HashMap<String, Listing>>,
    /// Protocol 3 (unreleased): the pairing in flight for each connection, at
    /// most one apiece and [`MAX_PAIRINGS`] in all. The map lock is held only
    /// to find a session; the conversation itself holds the session's own, so
    /// one television nobody is standing next to never blocks another.
    pairings: Mutex<HashMap<String, Arc<PairingSlot>>>,
    /// When a pairing on each connection last wrote a key. A key the device
    /// rotated on a request queued before that is not stored: the pairing's
    /// is the newer one.
    paired_at: Mutex<HashMap<String, Instant>>,
    /// The number of user-table rebuilds this runtime has already answered
    /// for. A rebuild can move a package to a different user, so the children
    /// started under the old one are retired and come back under the new one.
    identity_rebuilds: AtomicU64,
    #[cfg(test)]
    lister: Mutex<Option<Lister>>,
    #[cfg(test)]
    scripted_pairing: Mutex<Option<ScriptedPairing>>,
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
            pairings: Mutex::new(HashMap::new()),
            paired_at: Mutex::new(HashMap::new()),
            identity_rebuilds: AtomicU64::new(0),
            #[cfg(test)]
            lister: Mutex::new(None),
            #[cfg(test)]
            scripted_pairing: Mutex::new(None),
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

    /// Pair through `pairing` instead of a package of this store's. See
    /// [`PairChild`]. `generation` is the package selection the conversation
    /// is at, which a test moves to prove what a package update does to one.
    #[cfg(test)]
    pub fn pair_with(
        &self,
        manifest: Manifest,
        pairing: couch_plugin::Pairing,
        generation: &str,
        open: impl Fn(&str) -> Result<Box<dyn PairChild>, Failure> + Send + Sync + 'static,
    ) {
        *self
            .scripted_pairing
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(ScriptedPairing {
            manifest,
            pairing,
            generation: generation.to_owned(),
            open: Box::new(open),
        });
    }

    /// The package selection a scripted pairing is at, moved as a package
    /// update would move it.
    #[cfg(test)]
    pub fn pair_generation(&self, generation: &str) {
        if let Some(scripted) = self
            .scripted_pairing
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_mut()
        {
            scripted.generation = generation.to_owned();
        }
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

    /// Protocol 3 (unreleased): where this connection's pairing key lives,
    /// beside its settings and under the same lock. Root, mode 0600, never
    /// read by anything but the host that configures the package.
    fn credential_path(&self, connection: &str) -> Result<PathBuf, Error> {
        Ok(self
            .settings_path(connection)?
            .with_file_name("plugin-credential.json"))
    }

    /// The one line a page shows about a pairing, beside the key. Not secret,
    /// and removed with it.
    fn pairing_path(&self, connection: &str) -> Result<PathBuf, Error> {
        Ok(self
            .settings_path(connection)?
            .with_file_name("plugin-pairing.json"))
    }

    fn credential(&self, connection: &str) -> Result<Option<Credential>, Error> {
        load_credential(&self.credential_path(connection)?)
    }

    fn pairing_record(&self, connection: &str) -> Option<PairingRecord> {
        couch_sdk::load_private(&self.pairing_path(connection).ok()?).ok()
    }

    pub fn settings(&self, connection: &str, plugin: &str) -> Result<Value, String> {
        let manifest = self.manifest(plugin)?;
        let path = self.settings_path(connection).map_err(|e| e.to_string())?;
        let saved = load_settings(&path).map_err(|e| e.to_string())?;
        let mut view = redacted(&manifest, saved.as_ref());
        // Protocol 3 (unreleased). Absent for every package a shipped build
        // can run: none of them declares pairing, so none of these keys is
        // ever written and the reply is byte for byte the one it always was.
        if let Some(pairing) = self.pair_package(plugin).ok().and_then(|p| p.pairing) {
            let paired = self
                .credential(connection)
                .map_err(|e| e.to_string())?
                .is_some();
            view["paired"] = json!(paired);
            view["pairing"] = json!({ "required": pairing.required });
            if let Some(record) = self.pairing_record(connection).filter(|_| paired) {
                view["summary"] = json!(record.summary);
            }
        }
        Ok(view)
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
        // The key this connection is already paired with, if any: a package
        // that will not speak without one has to be given it to say whether
        // these settings are usable. The gate strips it for anything that may
        // not be told one, so this is safe whatever the package is.
        let credential = self.credential(connection)?;
        // Configure validates the adapter's typed settings without requiring an
        // online TV. Do not save a schema-valid but unusable host/port.
        check_settings(
            &directory,
            &manifest,
            &settings,
            credential.as_ref(),
            policy,
        )?;
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
        credential: Option<Credential>,
    ) -> Result<LegacyAdoption, Refusal> {
        let policy = self.policy(package)?;
        let _lease = self.packages.read_lease()?;
        let (directory, manifest) = self.packages.resolve_wait(package, STORE_READ_WAIT)?;
        let generation = self.packages.generation(package)?;
        let settings = manifest.with_defaults(settings)?;
        // The package is checked with the key the built-in client kept, so a
        // connection that only answers when it is paired is not refused here
        // for the want of something Couch is holding.
        check_settings(
            &directory,
            &manifest,
            &settings,
            credential.as_ref(),
            policy,
        )
        .map_err(|failure| named(&manifest, failure))?;
        Ok(LegacyAdoption {
            package: package.to_owned(),
            generation,
            manifest,
            settings,
            credential,
        })
    }

    /// Protocol 3 (unreleased): the key a departed built-in stored for this
    /// connection, as its package's credential.
    ///
    /// The old file is read and **left where it is**: a Couch rolled back to
    /// one that still has the built-in client has to find its pairing, and
    /// deleting the connection removes both anyway. `None` whenever this row
    /// hands nothing over (which is every row there is today), the file is not
    /// there, or the mapping refuses what it finds.
    pub fn legacy_credential(
        &self,
        connection: &str,
        row: &couch_model::LegacyBuiltin,
    ) -> Option<Credential> {
        let file = row.credential_file?;
        let path = self.settings_path(connection).ok()?.with_file_name(file);
        let stored: Value = couch_sdk::load_private(&path).ok()?;
        Credential::new(row.map_credential(&stored)?).ok()
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
        // The key first, so a package whose connection is unusable without one
        // never finds itself converted and unpaired. The built-in's own file
        // stays where it is.
        if let Some(credential) = &prepared.credential {
            let key = self
                .credential_path(connection)
                .map_err(|e| e.to_string())?;
            couch_sdk::save_private(&key, credential)
                .map_err(|_| "Cannot save the integration's key")?;
        }
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
        // Read beside the settings and handed to the child on its configure.
        // Never returned, never logged: from here it only goes into the host.
        let credential = self.credential(connection)?;
        let paired = credential.is_some();
        let endpoint = self.endpoint_for(
            connection,
            plugin,
            &generation,
            settings,
            credential,
            queued,
        )?;
        if queued.elapsed() >= couch_plugin::QUEUE_TTL {
            return Err(Error::Expired.into());
        }
        let (response, rotated) = endpoint.request_child_full(kind, request)?;
        // A key the device issued while answering. Written here, under the
        // connection's lock, before the body goes back, so nothing can read a
        // status through a key Couch has not kept.
        if let Some(rotated) = rotated {
            self.store_rotated(connection, paired, queued, rotated);
        }
        Ok(response)
    }

    /// Protocol 3 (unreleased): keep a key the device rotated under us.
    ///
    /// A rotation cannot create a pairing - only a pairing does that - so one
    /// arriving for a connection Couch holds no key for is dropped. So is one
    /// that a pairing has overtaken: a `done` that landed after this request
    /// was queued holds the newer key, and writing this one would undo it. A
    /// failure to write is said once, without the key, and the reply the
    /// request asked for still goes back: the device answered.
    fn store_rotated(
        &self,
        connection: &str,
        paired: bool,
        queued: Instant,
        credential: Credential,
    ) {
        if !paired {
            eprintln!(
                "couch-confd: connection {connection}: an integration offered a new key for a \
                 connection that is not paired; it was not stored"
            );
            return;
        }
        if !credential.fits() {
            eprintln!(
                "couch-confd: connection {connection}: an integration's new key is too large to \
                 store"
            );
            return;
        }
        if self
            .paired_at
            .lock()
            .ok()
            .and_then(|marks| marks.get(connection).copied())
            .is_some_and(|done| done > queued)
        {
            eprintln!(
                "couch-confd: connection {connection}: a new key arrived from a request older \
                 than the pairing that has just finished; it was not stored"
            );
            return;
        }
        let Ok(path) = self.credential_path(connection) else {
            return;
        };
        if let Err(error) = save_credential(&path, &credential) {
            eprintln!(
                "couch-confd: connection {connection}: an integration's new key was not saved: \
                 {error}"
            );
            return;
        }
        // The child that issued it is the one configured with it, so the entry
        // keeps its key up to date rather than being evicted and started again
        // on the next request. The caller holds the connection's lock, so
        // nothing else can be starting a child for it.
        if let Ok(mut endpoints) = self.endpoints.lock() {
            if let Some(entry) = endpoints.get_mut(connection) {
                entry.credential = Some(credential);
            }
        }
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
        credential: Option<Credential>,
        queued: Instant,
    ) -> Result<Arc<Endpoint>, Failure> {
        let existing = {
            let mut endpoints = self.endpoints.lock().map_err(|_| Error::Transport)?;
            self.retire_children_of_a_rebuilt_table(&mut endpoints);
            endpoints.retain(|_, entry| retained(entry));
            if endpoints.get(connection).is_some_and(|entry| {
                entry.generation != generation
                    || entry.settings != settings
                    || entry.credential != credential
            }) {
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
            // Protocol 3 (unreleased). A package that says it is unusable
            // without a key is not started at all while Couch holds none: the
            // answer is the same `unpaired` the package itself would give,
            // and it costs no process.
            if manifest.pairing.is_some_and(|pairing| pairing.required)
                && manifest.pairs()
                && credential.is_none()
            {
                return Err(Error::Unpaired.into());
            }
            let keep_alive = manifest.keep_alive;
            let policy = HostPolicy::for_package(uid, gid);
            let endpoint = Arc::new(Endpoint::start_paired(
                &directory,
                manifest,
                settings.clone(),
                credential.as_ref(),
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
                    credential,
                    endpoint: endpoint.clone(),
                    used: Instant::now(),
                    keep_alive,
                    pinned: false,
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
            self.credential(connection)?,
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
        self.end_pairing(connection, None);
        if let Ok(mut marks) = self.paired_at.lock() {
            marks.remove(connection);
        }
    }

    /// One sweep, every ten seconds.
    ///
    /// `in_use` says whether a connection is still referred to by a device,
    /// which is the only thing that earns a package's `keep_alive` its
    /// exemption from the idle reaper. The reaper computes it from the saved
    /// configuration; nothing else here knows what a room is.
    pub fn reap(&self, in_use: &dyn Fn(&str) -> bool) {
        if let Ok(mut endpoints) = self.endpoints.lock() {
            // The ten-second sweep is also where a child left running as a
            // user a rebuilt table no longer gives its package goes, without
            // waiting for that connection to be asked for something.
            self.retire_children_of_a_rebuilt_table(&mut endpoints);
            // Protocol 3 (unreleased). Which children may outstay the idle
            // retains: the ones whose package asked and whose connection a
            // device still refers to, and at most MAX_KEEP_ALIVE of those.
            // Beyond the cap the least recently used lose the exemption and
            // are reaped exactly as anything else is. With the switch off no
            // manifest may say `keep_alive`, so this set is always empty.
            let mut asking: Vec<(String, Instant)> = endpoints
                .iter()
                .filter(|(connection, entry)| entry.keep_alive && in_use(connection))
                .map(|(connection, entry)| (connection.clone(), entry.used))
                .collect();
            asking.sort_by_key(|(_, used)| std::cmp::Reverse(*used));
            let pinned: HashSet<String> = asking
                .into_iter()
                .take(MAX_KEEP_ALIVE)
                .map(|(connection, _)| connection)
                .collect();
            for (connection, entry) in endpoints.iter_mut() {
                entry.pinned = pinned.contains(connection);
            }
            endpoints.retain(|_, entry| retained(entry));
        }
        if let Ok(mut listings) = self.children.lock() {
            listings.retain(|_, listing| listing.at.elapsed() < CHILDREN_TTL);
        }
        self.sweep_pairings();
    }

    /// Protocol 3 (unreleased). End every pairing whose deadline has passed,
    /// whose browser has stopped polling, or whose package has been changed
    /// under it. The package is told, with two seconds to hear it, and its
    /// child is killed; nothing is written either way.
    ///
    /// A session somebody is mid-call on is left for the next sweep: its own
    /// lock is what says so, and waiting on it here would hold up every other
    /// connection's.
    fn sweep_pairings(&self) {
        let now = Instant::now();
        let mut ended = Vec::new();
        if let Ok(mut pairings) = self.pairings.lock() {
            pairings.retain(|connection, slot| {
                let Ok(session) = slot.session.try_lock() else {
                    return true;
                };
                let moved = self
                    .pair_package(&session.plugin)
                    .ok()
                    .map(|package| package.generation)
                    .as_ref()
                    != Some(&session.generation);
                if !session.expired(now) && !moved {
                    return true;
                }
                if moved {
                    eprintln!(
                        "couch-confd: connection {connection}: the integration changed while it \
                         was being paired; the attempt was dropped"
                    );
                }
                drop(session);
                ended.push(slot.clone());
                false
            });
        }
        for slot in ended {
            if let Ok(mut session) = slot.session.lock() {
                session.end();
            }
        }
    }

    // ---------------------------------------------------------------------
    // Protocol 3 (unreleased): pairing.
    //
    // None of it is reachable in a shipped build. `pairs` is false for every
    // package such a build can run, because only a protocol 3 manifest may
    // declare `pairing` and no protocol 3 manifest is accepted, so every
    // route below is refused before anything happens.
    // ---------------------------------------------------------------------

    /// Whether this package describes a way of pairing.
    pub fn pairs(&self, plugin: &str) -> bool {
        self.pair_package(plugin)
            .is_ok_and(|package| package.pairing.is_some())
    }

    /// What a pairing goes by: the manifest whose settings rules apply, the
    /// way it pairs (`None` for every package a shipped build can run), and
    /// the package selection it is all at.
    fn pair_package(&self, plugin: &str) -> Result<PairPackage, couch_integrations::Error> {
        #[cfg(test)]
        if let Some(scripted) = self
            .scripted_pairing
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            return Ok(PairPackage {
                manifest: scripted.manifest.clone(),
                pairing: Some(scripted.pairing),
                generation: scripted.generation.clone(),
            });
        }
        let (_, manifest) = self.packages.resolve_wait(plugin, STORE_READ_WAIT)?;
        let generation = self.packages.generation(plugin)?;
        // `pairs` is both at once: only a protocol 3 manifest may declare a
        // way of pairing, and only a protocol 3 package may be sent one.
        let pairing = manifest.pairing.filter(|_| manifest.pairs());
        Ok(PairPackage {
            manifest,
            pairing,
            generation,
        })
    }

    /// Start the package child this pairing is its own conversation with.
    fn open_pair_child(
        &self,
        connection: &str,
        plugin: &str,
    ) -> Result<Box<dyn PairChild>, PairError> {
        #[cfg(test)]
        if let Some(scripted) = self
            .scripted_pairing
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            let _ = plugin;
            return (scripted.open)(connection).map_err(PairError::from);
        }
        let _ = connection;
        let policy = self.policy(plugin)?;
        let _lease = self.packages.read_lease()?;
        let (directory, manifest) = self.packages.resolve_wait(plugin, STORE_READ_WAIT)?;
        // Spawning checks the package is still what it says it is, including
        // that a package holding a key has closed its own `/proc` entry.
        let mut host =
            Host::spawn_with_policy(&directory, &manifest, couch_plugin::STARTUP_TIMEOUT, policy)
                .map_err(Failure::from)?;
        host.set_timeout(couch_plugin::REQUEST_TIMEOUT)
            .map_err(Failure::from)?;
        Ok(Box::new(host))
    }

    /// Begin pairing this connection.
    ///
    /// `patch` is what the person has typed into the connection's settings,
    /// merged over whatever is saved. It is what the package is asked to pair
    /// with; it is **not** saved, because a pairing that fails has to leave
    /// the connection exactly as it was. Only a `done` that carries corrected
    /// settings writes any.
    ///
    /// The child started here is this attempt's alone: the connection's
    /// ordinary child keeps answering on the old key throughout, which is
    /// what makes pairing again safe on a device that is working.
    pub fn pair_start(
        &self,
        connection: &str,
        plugin: &str,
        patch: Value,
    ) -> Result<PairStarted, PairError> {
        let started = Instant::now();
        let PairPackage {
            manifest,
            pairing,
            generation,
        } = self.pair_package(plugin)?;
        let Some(pairing) = pairing else {
            return Err(PairError::DoesNotPair);
        };
        let path = self.settings_path(connection).map_err(Refusal::from)?;
        let settings = merge_settings(&manifest, load_settings(&path)?.as_ref(), patch)
            .map_err(Refusal::from)?;
        manifest
            .validate_settings(&settings)
            .map_err(Refusal::from)?;
        let credential = self.credential(connection)?;
        // Cheaply, before a process is started; again when it is inserted.
        if self.pairings_full(connection) {
            return Err(PairError::TooMany);
        }
        // The owner's cap is five minutes whatever the package asks for.
        let window = pairing.max_seconds.min(couch_plugin::Pairing::MAX_SECONDS);
        let deadline = started + Duration::from_secs(u64::from(window));
        let mut child = self.open_pair_child(connection, plugin)?;
        let step = match child.start(settings.clone(), credential.as_ref()) {
            Ok(step) => step,
            Err(failure) => return Ok(Self::started(token(), lost(failure), deadline)),
        };
        let mut session = PairingSession {
            plugin: plugin.to_owned(),
            generation,
            manifest,
            child: Some(child),
            deadline,
            last_poll: started,
            prompt: None,
            poll_after_ms: 0,
            pending_done: None,
        };
        let reply = self.follow(connection, &mut session, step);
        if reply.is_final() {
            session.end();
            return Ok(Self::started(token(), reply, deadline));
        }
        let slot = Arc::new(PairingSlot {
            token: token(),
            session: Mutex::new(session),
        });
        let token = slot.token.clone();
        let replaced = {
            let mut pairings = self
                .pairings
                .lock()
                .map_err(|_| Refusal::from("Pairing registry lock failed"))?;
            if !pairings.contains_key(connection) && pairings.len() >= MAX_PAIRINGS {
                return Err(PairError::TooMany);
            }
            // A second start replaces the first, and the first is told.
            pairings.insert(connection.to_owned(), slot)
        };
        if let Some(replaced) = replaced {
            if let Ok(mut session) = replaced.session.lock() {
                session.end();
            }
        }
        Ok(Self::started(token, reply, deadline))
    }

    /// The next step of a pairing in flight, or the same one again when the
    /// dialog is only checking.
    pub fn pair_continue(
        &self,
        connection: &str,
        token: &str,
        input: Option<PairInput>,
    ) -> Result<PairReply, PairError> {
        let slot = self.pairing_slot(connection, token)?;
        let mut session = slot
            .session
            .try_lock()
            .map_err(|_| PairError::Refused(Error::Busy.into()))?;
        if session.expired(Instant::now()) {
            drop(session);
            self.end_pairing(connection, Some(token));
            return Err(PairError::Unknown);
        }
        if self
            .pair_package(&session.plugin)
            .ok()
            .map(|package| package.generation)
            .as_ref()
            != Some(&session.generation)
        {
            drop(session);
            self.end_pairing(connection, Some(token));
            return Ok(PairReply::failed(
                PairFailure::Unsupported,
                "The integration changed while this was being paired",
            ));
        }
        session.last_poll = Instant::now();
        // A key already in hand that only wants writing. The package is not
        // asked again, whatever the dialog sent.
        if session.pending_done.is_some() {
            let reply = self.write_done(connection, &mut session)?;
            drop(session);
            if reply.is_final() {
                self.end_pairing(connection, Some(token));
            }
            return Ok(reply);
        }
        match (&session.prompt, &input) {
            // A prompt that asked for nothing is never given anything, and a
            // code is measured against the prompt that asked for it before
            // any of this reaches the package.
            (Some(prompt), Some(input)) if !prompt.accepts(input) => {
                return Err(PairError::BadInput)
            }
            (None, Some(_)) => return Err(PairError::BadInput),
            // A dialog waiting for somebody to type may still poll, to keep
            // its countdown honest. The package has nothing to add.
            (Some(prompt), None) if prompt.is_code() => {
                return Ok(PairReply::Waiting {
                    prompt: prompt.clone(),
                    poll_after_ms: session.poll_after_ms,
                })
            }
            _ => (),
        }
        let step = match session.child.as_mut() {
            Some(child) => child.step(input),
            None => Err(Failure::from(Error::Transport)),
        };
        let reply = match step {
            Ok(step) => self.follow(connection, &mut session, step),
            Err(failure) => lost(failure),
        };
        drop(session);
        if reply.is_final() {
            self.end_pairing(connection, Some(token));
        }
        Ok(reply)
    }

    /// End a pairing, storing nothing. Idempotent: a session that is not
    /// there, or one under another token, leaves everything as it is.
    pub fn pair_cancel(&self, connection: &str, token: &str) {
        self.end_pairing(connection, Some(token));
    }

    /// Forget this connection's key.
    ///
    /// The device is not told: Couch cannot revoke what it was given, and a
    /// television that still lists Couch as paired is the device's business.
    /// What goes is Couch's copy, the line beside it, the child that was
    /// configured with it, and any pairing in flight.
    pub fn forget_credential(&self, connection: &str, plugin: &str) -> Result<(), PairError> {
        if !self.pairs(plugin) {
            return Err(PairError::DoesNotPair);
        }
        let path = self.settings_path(connection).map_err(Refusal::from)?;
        let lock = crate::api::connections::lock_for(&path);
        let deadline = Instant::now() + Duration::from_secs(2);
        let Some(_guard) = crate::api::connections::patiently(deadline, || lock.try_lock().ok())
        else {
            return Err(PairError::Refused("Integration connection is busy".into()));
        };
        self.end_pairing(connection, None);
        for path in [
            self.credential_path(connection).map_err(Refusal::from)?,
            self.pairing_path(connection).map_err(Refusal::from)?,
        ] {
            if let Err(error) = fs::remove_file(&path) {
                if error.kind() != std::io::ErrorKind::NotFound {
                    return Err(PairError::Storage(error.to_string()));
                }
            }
        }
        self.retire_endpoint(connection);
        self.forget_children(connection);
        Ok(())
    }

    fn started(session: String, step: PairReply, deadline: Instant) -> PairStarted {
        PairStarted {
            session,
            expires_in: if step.is_final() {
                0
            } else {
                deadline.saturating_duration_since(Instant::now()).as_secs()
            },
            step,
        }
    }

    fn pairings_full(&self, connection: &str) -> bool {
        self.pairings.lock().is_ok_and(|pairings| {
            !pairings.contains_key(connection) && pairings.len() >= MAX_PAIRINGS
        })
    }

    fn pairing_slot(&self, connection: &str, token: &str) -> Result<Arc<PairingSlot>, PairError> {
        self.pairings
            .lock()
            .ok()
            .and_then(|pairings| pairings.get(connection).cloned())
            .filter(|slot| !token.is_empty() && slot.token == token)
            .ok_or(PairError::Unknown)
    }

    /// Take the session out of the map and tell the package it is over.
    /// `token` names one in particular; `None` means whatever is there.
    fn end_pairing(&self, connection: &str, token: Option<&str>) {
        let slot = {
            let Ok(mut pairings) = self.pairings.lock() else {
                return;
            };
            match pairings.get(connection) {
                Some(slot) if token.is_none_or(|token| slot.token == token) => {
                    pairings.remove(connection)
                }
                _ => None,
            }
        };
        if let Some(slot) = slot {
            if let Ok(mut session) = slot.session.lock() {
                session.end();
            }
        }
    }

    /// Keep the session in step with what the package just said, and turn it
    /// into the answer the browser gets - which is where the key stops.
    fn follow(&self, connection: &str, session: &mut PairingSession, step: PairStep) -> PairReply {
        match step {
            PairStep::Waiting {
                prompt,
                poll_after_ms,
            } => {
                session.prompt = Some(prompt.clone());
                session.poll_after_ms = poll_after_ms;
                session.last_poll = Instant::now();
                PairReply::Waiting {
                    prompt,
                    poll_after_ms,
                }
            }
            PairStep::Failed { reason, message } => PairReply::Failed { reason, message },
            PairStep::Done {
                credential,
                settings,
                summary,
            } => {
                // The conversation is over whatever happens to the write, so
                // the child goes now rather than waiting on a lock.
                session.child = None;
                session.pending_done = Some((credential, settings, summary));
                match self.write_done(connection, session) {
                    Ok(reply) => reply,
                    // A key that cannot be written is not a pairing. The
                    // words go to the person; the key itself never does.
                    Err(error) => {
                        if let PairError::Storage(error) = &error {
                            eprintln!(
                                "couch-confd: connection {connection}: the pairing key was not \
                                 saved: {error}"
                            );
                        }
                        PairReply::failed(
                            PairFailure::Unsupported,
                            "Couch could not save the key this device gave it",
                        )
                    }
                }
            }
        }
    }

    /// Write what a `done` gave, under the connection's lock.
    ///
    /// The lock is waited for patiently, because the key is already in hand
    /// and asking the device again would mean pairing again. If it is still
    /// busy the result stays parked and the dialog is told to come back; the
    /// package is never asked a second time.
    fn write_done(
        &self,
        connection: &str,
        session: &mut PairingSession,
    ) -> Result<PairReply, PairError> {
        let path = self.settings_path(connection).map_err(Refusal::from)?;
        let lock = crate::api::connections::lock_for(&path);
        let deadline = Instant::now() + DONE_LOCK_WAIT;
        let Some(_guard) = crate::api::connections::patiently(deadline, || lock.try_lock().ok())
        else {
            let prompt = session
                .prompt
                .clone()
                .unwrap_or_else(PairPrompt::approve_on_device);
            return Ok(PairReply::Waiting {
                prompt,
                poll_after_ms: FINISHING_POLL_MS,
            });
        };
        // Admission validates every saved connection before activating a new
        // package; hold the selection still until the key is durable.
        let _lease = self.packages.read_lease();
        let Some((credential, corrected, summary)) = session.pending_done.take() else {
            return Err(PairError::Unknown);
        };
        let key = self.credential_path(connection).map_err(Refusal::from)?;
        if let Err(error) = save_credential(&key, &credential) {
            return Err(PairError::Storage(error.to_string()));
        }
        // Then the settings the device would rather Couch had, if it gave
        // any. The host has already measured them against the manifest.
        if let Some(corrected) = corrected {
            match session
                .manifest
                .with_defaults(corrected)
                .and_then(|settings| {
                    session.manifest.validate_settings(&settings)?;
                    Ok(settings)
                }) {
                Ok(settings) => {
                    if let Err(error) = couch_sdk::save_private(&path, &settings) {
                        eprintln!(
                            "couch-confd: connection {connection}: the settings the device \
                             corrected were not saved: {error}"
                        );
                    }
                }
                Err(code) => eprintln!(
                    "couch-confd: connection {connection}: the settings the device corrected \
                     were refused ({code}); the key was kept and they were not"
                ),
            }
        }
        let paired_at = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let record = PairingRecord {
            summary: summary.clone(),
            paired_at,
        };
        if let Err(error) = couch_sdk::save_private(
            &self.pairing_path(connection).map_err(Refusal::from)?,
            &record,
        ) {
            eprintln!(
                "couch-confd: connection {connection}: what was paired was not written down: \
                 {error}"
            );
        }
        // The next request starts a child configured with the new key.
        self.retire_endpoint(connection);
        self.forget_children(connection);
        if let Ok(mut marks) = self.paired_at.lock() {
            marks.insert(connection.to_owned(), Instant::now());
        }
        let saved = load_settings(&path).unwrap_or(None);
        Ok(PairReply::Done {
            summary,
            settings: redacted(&session.manifest, saved.as_ref()),
        })
    }
}

/// What a pairing whose package has stopped answering ends as. A pairing
/// child is never restarted: a device half way through a handshake with a
/// process that has gone is not somewhere to arrive again quietly.
fn lost(failure: Failure) -> PairReply {
    let message = failure
        .reason
        .as_ref()
        .map(|reason| reason.text().to_owned());
    match failure.code {
        Error::Transport | Error::Timeout | Error::Busy | Error::Expired => PairReply::Failed {
            reason: PairFailure::Unreachable,
            message,
        },
        Error::Unsupported | Error::Incompatible => PairReply::Failed {
            reason: PairFailure::Unsupported,
            message,
        },
        _ => PairReply::Failed {
            reason: PairFailure::Refused,
            message,
        },
    }
}

/// 128 bits from the kernel, spelt in hexadecimal: the `<session>` in the URL.
/// Never the id the package minted, which is the package's own business and
/// stays inside the host.
fn token() -> String {
    let mut random = [0; 16];
    match std::fs::File::open("/dev/urandom").and_then(|mut file| file.read_exact(&mut random)) {
        Ok(()) => random.iter().map(|byte| format!("{byte:02x}")).collect(),
        // Refusing to invent one: a guessable session is a pairing somebody
        // else can finish. An empty token matches nothing, so the attempt
        // simply cannot be continued.
        Err(error) => {
            eprintln!("couch-confd: cannot read /dev/urandom, so no pairing can start: {error}");
            String::new()
        }
    }
}

/// Write a key the way every private file here is written - atomically, mode
/// 0600 - with the connection's folder forced to 0700 first, so a key cannot
/// land in a directory somebody else can list.
fn save_credential(path: &Path, credential: &Credential) -> std::io::Result<()> {
    let parent = path.parent().ok_or(std::io::ErrorKind::InvalidInput)?;
    fs::create_dir_all(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    }
    couch_sdk::save_private(path, credential)
}

fn load_credential(path: &Path) -> Result<Option<Credential>, Error> {
    match couch_sdk::load_private::<Credential>(path) {
        Ok(credential) if credential.fits() => Ok(Some(credential)),
        Ok(_) => Err(Error::Invalid),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(Error::Invalid),
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
        let refused = check_settings(
            &directory,
            &manifest,
            &settings,
            None,
            HostPolicy::default(),
        )
        .unwrap_err();
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
            check_settings(
                &directory,
                &manifest,
                &settings,
                None,
                HostPolicy::default()
            ),
            Err(Error::Protocol.into())
        );
        let _ = fs::remove_dir_all(directory);

        let (directory, manifest) = scripted("accepted", 2, json!({"type":"ok"}));
        assert_eq!(
            check_settings(
                &directory,
                &manifest,
                &settings,
                None,
                HostPolicy::default()
            ),
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
    pub(super) fn home_with_a_package(name: &str, id: &str) -> PathBuf {
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
            .prepare_legacy("sample", json!({"host":"tv.local","port":23}), None)
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
        runtime.reap(&|_| false);
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

/// Protocol 3 (unreleased): pairing, as this daemon runs it.
///
/// No test here has a protocol 3 package and none can: the guard test above is
/// what keeps the preview out of every build in this workspace. So the one
/// step that needs one - the conversation itself - is replaceable
/// ([`Runtime::pair_with`]), exactly as a listing is, and the pairing wire is
/// tested against real subprocesses in `clients/` (`couch-plugin-echo-pair`
/// and `testing_v3::pairing`). The one true end to end, a real protocol 3
/// package paired through these routes over HTTP, is
/// `tools/tests/integrations-e2e.py --pairing`.
#[cfg(test)]
mod pairing_tests {
    use super::*;
    use std::collections::VecDeque;

    // ---- a conversation in place of a package ---------------------------

    #[derive(Default)]
    struct Conversation {
        steps: VecDeque<Result<PairStep, Failure>>,
        /// The settings it was started with, and whether a key came with them.
        started: Option<(Value, bool)>,
        /// What each continue carried, in order.
        asked: Vec<Option<PairInput>>,
        cancelled: usize,
        opened: usize,
    }
    impl Conversation {
        fn next(&mut self) -> Result<PairStep, Failure> {
            self.steps
                .pop_front()
                .unwrap_or_else(|| Err(Error::Transport.into()))
        }
    }
    type Script = Arc<Mutex<Conversation>>;

    struct Scripted(Script);
    impl PairChild for Scripted {
        fn start(
            &mut self,
            settings: Value,
            credential: Option<&Credential>,
        ) -> Result<PairStep, Failure> {
            let mut script = self.0.lock().unwrap();
            script.started = Some((settings, credential.is_some()));
            script.next()
        }
        fn step(&mut self, input: Option<PairInput>) -> Result<PairStep, Failure> {
            let mut script = self.0.lock().unwrap();
            script.asked.push(input);
            script.next()
        }
        fn cancel(&mut self) {
            self.0.lock().unwrap().cancelled += 1;
        }
    }

    fn script(runtime: &Runtime, steps: Vec<Result<PairStep, Failure>>) -> Script {
        script_for(runtime, 120, steps)
    }
    fn script_for(
        runtime: &Runtime,
        max_seconds: u16,
        steps: Vec<Result<PairStep, Failure>>,
    ) -> Script {
        let script: Script = Arc::new(Mutex::new(Conversation {
            steps: steps.into(),
            ..Default::default()
        }));
        let handle = script.clone();
        runtime.pair_with(
            settings_manifest(),
            couch_plugin::Pairing {
                required: true,
                max_seconds,
            },
            "1",
            move |_| {
                handle.lock().unwrap().opened += 1;
                Ok(Box::new(Scripted(handle.clone())) as Box<dyn PairChild>)
            },
        );
        script
    }

    /// The manifest whose settings rules apply. An ordinary one this build
    /// accepts: only the `pairing` beside it is what the switch forbids.
    fn settings_manifest() -> Manifest {
        serde_json::from_value(json!({
            "protocol_version": 1,
            "id": "sample", "label": "Sample", "version": "1.0.0", "executable": "plugin",
            "capabilities": [], "settings": [
                {"id":"host","label":"Host","kind":"text","required":true},
                {"id":"token","label":"Token","kind":"secret"},
                {"id":"port","label":"Port","kind":"integer","default":23}
            ]
        }))
        .unwrap()
    }

    fn key(value: &str) -> Credential {
        Credential::new(json!({ "key": value })).unwrap()
    }
    fn press() -> PairStep {
        PairStep::waiting(
            PairPrompt::press_button().saying("The button is on top"),
            2000,
        )
    }
    fn code() -> PairStep {
        PairStep::waiting(
            PairPrompt::enter_code(4, couch_plugin::CodeAlphabet::Digits),
            0,
        )
    }

    fn home(name: &str) -> PathBuf {
        let home = std::env::temp_dir().join(format!(
            "couch-pairing-{name}-{}-{:?}",
            std::process::id(),
            Instant::now()
        ));
        fs::create_dir_all(&home).unwrap();
        home
    }

    fn settings_of(runtime: &Runtime, connection: &str) -> Option<Value> {
        load_settings(&runtime.settings_path(connection).unwrap()).unwrap()
    }
    fn save_settings_file(runtime: &Runtime, connection: &str, settings: Value) {
        let path = runtime.settings_path(connection).unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        couch_sdk::save_private(&path, &settings).unwrap();
    }
    fn plant_key(runtime: &Runtime, connection: &str, credential: &Credential) {
        save_credential(&runtime.credential_path(connection).unwrap(), credential).unwrap();
    }
    /// Move a session's clocks back, as if the browser had gone quiet or the
    /// window had run out.
    fn age(runtime: &Runtime, connection: &str, by: Duration) {
        let pairings = runtime.pairings.lock().unwrap();
        let mut session = pairings[connection].session.lock().unwrap();
        session.deadline -= by;
        session.last_poll -= by;
    }

    // ---- the tests -------------------------------------------------------

    #[test]
    fn a_pairing_that_finishes_writes_the_key_the_settings_it_corrected_and_one_line() {
        // A real package in the store, so the settings view this ends with is
        // the one a browser reads.
        let home = super::tests::home_with_a_package("paired-view", "sample");
        let runtime = Runtime::new(home.clone());
        save_settings_file(&runtime, "tv", json!({"host": "TV.local.", "port": 23}));
        let script = script(
            &runtime,
            vec![
                Ok(press()),
                Ok(press()),
                Ok(
                    PairStep::done(key("s3cret-0001"), "Paired with the hall television")
                        .with_settings(json!({"host": "tv.local", "port": 9299})),
                ),
            ],
        );

        let started = runtime
            .pair_start("tv", "sample", json!({"host": "TV.local."}))
            .expect("the package pairs");
        assert_eq!(started.session.len(), 32, "128 bits, in hexadecimal");
        assert!(started.session.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(
            started.step,
            PairReply::Waiting {
                prompt: PairPrompt::press_button().saying("The button is on top"),
                poll_after_ms: 2000
            }
        );
        assert!(started.expires_in <= 120 && started.expires_in >= 118);
        // The settings it is pairing with are the saved ones with what the
        // person typed over them, and they are not written anywhere yet.
        assert_eq!(
            script.lock().unwrap().started.clone().unwrap().0,
            json!({"host": "TV.local.", "port": 23})
        );
        assert_eq!(
            settings_of(&runtime, "tv").unwrap(),
            json!({"host": "TV.local.", "port": 23})
        );
        assert!(!runtime.credential_path("tv").unwrap().exists());

        let token = started.session.clone();
        assert!(matches!(
            runtime.pair_continue("tv", &token, None),
            Ok(PairReply::Waiting { .. })
        ));
        let done = runtime.pair_continue("tv", &token, None).unwrap();
        let PairReply::Done { summary, settings } = &done else {
            panic!("{done:?}");
        };
        assert_eq!(summary, "Paired with the hall television");
        // The redacted view of what is saved now, and never the key.
        assert_eq!(
            settings,
            &json!({"settings": {"host": "tv.local", "port": 9299},
                    "configured": true, "secrets": []})
        );
        let rendered = serde_json::to_string(&done).unwrap();
        assert!(!rendered.contains("s3cret"), "{rendered}");
        assert!(!format!("{done:?}").contains("s3cret"));
        assert_eq!(
            rendered,
            r#"{"step":"done","summary":"Paired with the hall television","settings":{"configured":true,"secrets":[],"settings":{"host":"tv.local","port":9299}}}"#
        );

        // The key, at 0600 in a folder at 0700, beside settings the device
        // corrected and one line about what was paired.
        let stored = runtime.credential_path("tv").unwrap();
        assert_eq!(
            fs::read_to_string(&stored).unwrap(),
            r#"{"key":"s3cret-0001"}"#
        );
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&stored).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(stored.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            settings_of(&runtime, "tv").unwrap(),
            json!({"host": "tv.local", "port": 9299})
        );
        let record: Value = couch_sdk::load_private(&runtime.pairing_path("tv").unwrap()).unwrap();
        assert_eq!(record["summary"], "Paired with the hall television");
        assert!(record["paired_at"].as_u64().unwrap() > 1_750_000_000);

        // The session is over: the same token is not there any more.
        assert!(matches!(
            runtime.pair_continue("tv", &token, None),
            Err(PairError::Unknown)
        ));
        // The package was asked twice and never again for the key it gave.
        assert_eq!(script.lock().unwrap().asked.len(), 2);
        assert_eq!(script.lock().unwrap().opened, 1);

        // What a settings page now says. `paired` and the line beside it are
        // there only because this manifest declares pairing.
        let view = runtime.settings("tv", "sample").unwrap();
        assert_eq!(view["paired"], json!(true));
        assert_eq!(view["pairing"], json!({"required": true}));
        assert_eq!(view["summary"], "Paired with the hall television");
        assert!(!view.to_string().contains("s3cret"));
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn nothing_is_written_when_a_pairing_fails_is_cancelled_or_runs_out() {
        for ending in ["failed", "cancelled", "expired", "quiet"] {
            let home = home(&format!("nothing-{ending}"));
            let runtime = Runtime::new(home.clone());
            save_settings_file(&runtime, "tv", json!({"host": "tv.local", "port": 23}));
            plant_key(&runtime, "tv", &key("old-0000"));
            let before = fs::read(runtime.credential_path("tv").unwrap()).unwrap();
            let script = script(
                &runtime,
                vec![
                    Ok(press()),
                    Ok(PairStep::failed(couch_plugin::PairFailure::WrongCode)
                        .because("That was not the code")),
                ],
            );
            let started = runtime.pair_start("tv", "sample", json!({})).unwrap();
            let token = started.session.clone();
            // The key Couch already holds goes to the package, so a second
            // pairing of a set that needs the first one can be made at all.
            assert!(script.lock().unwrap().started.clone().unwrap().1);

            match ending {
                "failed" => assert_eq!(
                    runtime.pair_continue("tv", &token, None).unwrap(),
                    PairReply::Failed {
                        reason: couch_plugin::PairFailure::WrongCode,
                        message: Some("That was not the code".into())
                    }
                ),
                "cancelled" => {
                    runtime.pair_cancel("tv", &token);
                    assert_eq!(script.lock().unwrap().cancelled, 1);
                }
                "expired" => {
                    age(&runtime, "tv", Duration::from_secs(200));
                    runtime.reap(&|_| false);
                    assert_eq!(script.lock().unwrap().cancelled, 1);
                }
                _ => {
                    // The browser stopped polling a prompt that polls.
                    age(&runtime, "tv", Duration::from_secs(20));
                    runtime.reap(&|_| false);
                    assert_eq!(script.lock().unwrap().cancelled, 1);
                }
            }
            assert!(matches!(
                runtime.pair_continue("tv", &token, None),
                Err(PairError::Unknown)
            ));
            // The old key is exactly as it was, and nothing else was written.
            assert_eq!(
                fs::read(runtime.credential_path("tv").unwrap()).unwrap(),
                before,
                "{ending}"
            );
            assert!(!runtime.pairing_path("tv").unwrap().exists(), "{ending}");
            assert_eq!(
                settings_of(&runtime, "tv").unwrap(),
                json!({"host": "tv.local", "port": 23}),
                "{ending}"
            );
            let _ = fs::remove_dir_all(home);
        }
    }

    #[test]
    fn a_dialog_waiting_for_a_typed_code_is_not_a_browser_that_has_gone() {
        let home = home("quiet-code");
        let runtime = Runtime::new(home.clone());
        save_settings_file(&runtime, "tv", json!({"host": "tv.local"}));
        let script = script(&runtime, vec![Ok(code()), Ok(code())]);
        let started = runtime.pair_start("tv", "sample", json!({})).unwrap();
        let token = started.session;

        // Long past three polls, and past fifteen seconds: somebody is typing.
        age(&runtime, "tv", Duration::from_secs(60));
        runtime.reap(&|_| false);
        assert_eq!(script.lock().unwrap().cancelled, 0);
        assert!(runtime.pair_continue("tv", &token, None).is_ok());

        // Only the window itself ends it.
        age(&runtime, "tv", Duration::from_secs(200));
        runtime.reap(&|_| false);
        assert_eq!(script.lock().unwrap().cancelled, 1);
        assert!(matches!(
            runtime.pair_continue("tv", &token, None),
            Err(PairError::Unknown)
        ));
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn a_code_is_measured_against_the_prompt_that_asked_for_it_and_the_package_is_not_asked() {
        let home = home("code");
        let runtime = Runtime::new(home.clone());
        save_settings_file(&runtime, "tv", json!({"host": "tv.local"}));
        let script = script(
            &runtime,
            vec![Ok(code()), Ok(PairStep::done(key("k"), "Paired"))],
        );
        let started = runtime.pair_start("tv", "sample", json!({})).unwrap();
        let token = started.session;
        assert_eq!(
            started.step,
            PairReply::Waiting {
                prompt: PairPrompt::enter_code(4, couch_plugin::CodeAlphabet::Digits),
                poll_after_ms: 0
            }
        );

        for wrong in ["041", "04177", "04a7", ""] {
            assert!(
                matches!(
                    runtime.pair_continue("tv", &token, Some(PairInput::code(wrong))),
                    Err(PairError::BadInput)
                ),
                "{wrong}"
            );
        }
        assert!(script.lock().unwrap().asked.is_empty(), "nothing was sent");

        // A dialog may poll while somebody types; the package has nothing to
        // add and is not asked.
        assert_eq!(
            runtime.pair_continue("tv", &token, None).unwrap(),
            PairReply::Waiting {
                prompt: PairPrompt::enter_code(4, couch_plugin::CodeAlphabet::Digits),
                poll_after_ms: 0
            }
        );
        assert!(script.lock().unwrap().asked.is_empty());

        // The right shape does reach it.
        assert!(matches!(
            runtime.pair_continue("tv", &token, Some(PairInput::code("0417"))),
            Ok(PairReply::Done { .. })
        ));
        assert_eq!(
            script.lock().unwrap().asked,
            vec![Some(PairInput::code("0417"))]
        );
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn a_prompt_that_asked_for_nothing_is_never_given_anything() {
        let home = home("nothing-asked");
        let runtime = Runtime::new(home.clone());
        save_settings_file(&runtime, "tv", json!({"host": "tv.local"}));
        let script = script(&runtime, vec![Ok(press()), Ok(press())]);
        let started = runtime.pair_start("tv", "sample", json!({})).unwrap();
        assert!(matches!(
            runtime.pair_continue("tv", &started.session, Some(PairInput::code("0417"))),
            Err(PairError::BadInput)
        ));
        assert!(script.lock().unwrap().asked.is_empty());
        assert!(runtime.pair_continue("tv", &started.session, None).is_ok());
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn one_pairing_per_connection_and_eight_across_the_remote() {
        let home = home("cap");
        let runtime = Runtime::new(home.clone());
        let steps: Vec<_> = (0..30).map(|_| Ok(press())).collect();
        let script = script(&runtime, steps);
        for at in 0..MAX_PAIRINGS {
            save_settings_file(&runtime, &format!("tv{at}"), json!({"host": "tv.local"}));
            assert!(runtime
                .pair_start(&format!("tv{at}"), "sample", json!({}))
                .is_ok());
        }
        save_settings_file(&runtime, "one-too-many", json!({"host": "tv.local"}));
        assert!(matches!(
            runtime.pair_start("one-too-many", "sample", json!({})),
            Err(PairError::TooMany)
        ));
        assert_eq!(script.lock().unwrap().opened, MAX_PAIRINGS);

        // A second attempt on a connection already pairing replaces the
        // first, which is told, and the first token stops working.
        let first = runtime.pair_start("tv0", "sample", json!({})).unwrap();
        let second = runtime.pair_start("tv0", "sample", json!({})).unwrap();
        assert_ne!(first.session, second.session);
        assert_eq!(script.lock().unwrap().cancelled, 2);
        assert!(matches!(
            runtime.pair_continue("tv0", &first.session, None),
            Err(PairError::Unknown)
        ));
        assert!(runtime.pair_continue("tv0", &second.session, None).is_ok());
        // ...and the count has not moved, so the cap still holds.
        assert!(matches!(
            runtime.pair_start("one-too-many", "sample", json!({})),
            Err(PairError::TooMany)
        ));
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn a_session_is_only_ever_its_own_connections_and_a_guessed_one_is_nothing() {
        let home = home("scope");
        let runtime = Runtime::new(home.clone());
        for connection in ["tv", "other"] {
            save_settings_file(&runtime, connection, json!({"host": "tv.local"}));
        }
        script(&runtime, vec![Ok(press()), Ok(press()), Ok(press())]);
        let started = runtime.pair_start("tv", "sample", json!({})).unwrap();
        // The same token on another connection is no session at all.
        assert!(matches!(
            runtime.pair_continue("other", &started.session, None),
            Err(PairError::Unknown)
        ));
        for invented in ["", "0", &"f".repeat(32)] {
            assert!(
                matches!(
                    runtime.pair_continue("tv", invented, None),
                    Err(PairError::Unknown)
                ),
                "{invented}"
            );
        }
        // Cancelling with the wrong token leaves the pairing alone.
        runtime.pair_cancel("tv", "0");
        assert!(runtime.pair_continue("tv", &started.session, None).is_ok());
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn a_key_that_cannot_be_written_yet_is_kept_and_written_on_the_next_poll() {
        let home = home("parked");
        let runtime = Runtime::new(home.clone());
        save_settings_file(&runtime, "tv", json!({"host": "tv.local"}));
        let script = script(
            &runtime,
            vec![Ok(press()), Ok(PairStep::done(key("s3cret"), "Paired"))],
        );
        let started = runtime.pair_start("tv", "sample", json!({})).unwrap();
        let token = started.session;

        // Something else is working in this connection's folder.
        let lock = crate::api::connections::lock_for(&runtime.settings_path("tv").unwrap());
        let held = lock.lock().unwrap();
        let parked = runtime.pair_continue("tv", &token, None).unwrap();
        assert_eq!(
            parked,
            PairReply::Waiting {
                prompt: PairPrompt::press_button().saying("The button is on top"),
                poll_after_ms: FINISHING_POLL_MS
            }
        );
        assert!(!runtime.credential_path("tv").unwrap().exists());
        drop(held);

        // The next poll writes it, and the package is not asked again.
        let done = runtime.pair_continue("tv", &token, None).unwrap();
        assert!(matches!(done, PairReply::Done { .. }));
        assert_eq!(
            fs::read_to_string(runtime.credential_path("tv").unwrap()).unwrap(),
            r#"{"key":"s3cret"}"#
        );
        assert_eq!(script.lock().unwrap().asked.len(), 1);
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn a_package_that_changes_under_a_pairing_ends_it() {
        let home = home("moved");
        let runtime = Runtime::new(home.clone());
        save_settings_file(&runtime, "tv", json!({"host": "tv.local"}));
        let script = script(
            &runtime,
            vec![Ok(press()), Ok(PairStep::done(key("k"), "Paired"))],
        );
        let started = runtime.pair_start("tv", "sample", json!({})).unwrap();
        runtime.pair_generation("2");
        let reply = runtime.pair_continue("tv", &started.session, None).unwrap();
        assert_eq!(
            reply,
            PairReply::Failed {
                reason: couch_plugin::PairFailure::Unsupported,
                message: Some("The integration changed while this was being paired".into())
            }
        );
        assert!(!runtime.credential_path("tv").unwrap().exists());
        assert!(script.lock().unwrap().asked.is_empty());
        assert_eq!(script.lock().unwrap().cancelled, 1);
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn a_package_that_stops_answering_mid_pairing_is_never_started_again() {
        let home = home("dead");
        let runtime = Runtime::new(home.clone());
        save_settings_file(&runtime, "tv", json!({"host": "tv.local"}));
        let script = script(
            &runtime,
            vec![Ok(press()), Err(Failure::from(Error::Transport))],
        );
        let started = runtime.pair_start("tv", "sample", json!({})).unwrap();
        assert_eq!(
            runtime.pair_continue("tv", &started.session, None).unwrap(),
            PairReply::Failed {
                reason: couch_plugin::PairFailure::Unreachable,
                message: None
            }
        );
        // One child, and no second one: a set half way through a handshake
        // with a process that has gone is not somewhere to arrive again.
        assert_eq!(script.lock().unwrap().opened, 1);
        assert!(matches!(
            runtime.pair_continue("tv", &started.session, None),
            Err(PairError::Unknown)
        ));
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn retiring_a_connection_ends_the_pairing_it_was_having() {
        let home = home("retire");
        let runtime = Runtime::new(home.clone());
        save_settings_file(&runtime, "tv", json!({"host": "tv.local"}));
        let script = script(&runtime, vec![Ok(press()), Ok(press())]);
        let started = runtime.pair_start("tv", "sample", json!({})).unwrap();
        runtime.retire("tv");
        assert_eq!(script.lock().unwrap().cancelled, 1);
        assert!(matches!(
            runtime.pair_continue("tv", &started.session, None),
            Err(PairError::Unknown)
        ));
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn forgetting_a_pairing_removes_couchs_copy_and_says_nothing_to_the_device() {
        let home = home("forget");
        let runtime = Runtime::new(home.clone());
        save_settings_file(&runtime, "tv", json!({"host": "tv.local"}));
        let script = script(&runtime, vec![Ok(press()), Ok(press())]);
        plant_key(&runtime, "tv", &key("s3cret"));
        couch_sdk::save_private(
            &runtime.pairing_path("tv").unwrap(),
            &json!({"summary": "Paired with the hall television", "paired_at": 1_758_000_000u64}),
        )
        .unwrap();
        let started = runtime.pair_start("tv", "sample", json!({})).unwrap();

        runtime.forget_credential("tv", "sample").unwrap();
        assert!(!runtime.credential_path("tv").unwrap().exists());
        assert!(!runtime.pairing_path("tv").unwrap().exists());
        // The pairing in flight goes with it, and the settings stay.
        assert!(matches!(
            runtime.pair_continue("tv", &started.session, None),
            Err(PairError::Unknown)
        ));
        assert_eq!(script.lock().unwrap().cancelled, 1);
        assert_eq!(
            settings_of(&runtime, "tv").unwrap(),
            json!({"host": "tv.local"})
        );
        // Nothing was said to the device, and doing it again is no error.
        assert_eq!(script.lock().unwrap().asked.len(), 0);
        runtime.forget_credential("tv", "sample").unwrap();
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn a_key_is_never_in_what_a_page_reads_or_in_what_a_backup_holds() {
        let home = super::tests::home_with_a_package("secret", "sample");
        let runtime = Runtime::new(home.clone());
        save_settings_file(
            &runtime,
            "tv",
            json!({"host": "tv.local", "token": "typed"}),
        );
        script(
            &runtime,
            vec![Ok(PairStep::done(key("s3cret-0001"), "Paired"))],
        );
        let started = runtime.pair_start("tv", "sample", json!({})).unwrap();
        assert!(matches!(started.step, PairReply::Done { .. }));
        // Every reply this daemon makes about the connection, rendered.
        for rendered in [
            serde_json::to_string(&started.step).unwrap(),
            runtime.settings("tv", "sample").unwrap().to_string(),
            format!("{:?}", started.step),
        ] {
            assert!(!rendered.contains("s3cret"), "{rendered}");
            assert!(!rendered.contains("typed"), "{rendered}");
        }
        // The configuration this daemon saves, and the export beside it, are
        // built from config.json alone - which nothing above has touched.
        let store = crate::store::Store::open(home.join("config.json")).unwrap();
        let config = serde_json::to_string(store.config()).unwrap();
        assert!(!config.contains("s3cret"));
        assert!(!config.contains("plugin-credential"));
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn a_paired_connections_configuration_is_the_same_bytes_as_an_unpaired_ones() {
        let home = home("bytes");
        let runtime = Runtime::new(home.clone());
        // A configuration with the packaged connection in it, saved as the
        // daemon saves one.
        let mut store = crate::store::Store::open(home.join("config.json")).unwrap();
        store
            .mutate(None, |config| {
                config.connections.push(couch_model::Connection {
                    id: "tv".into(),
                    name: "TV".into(),
                    provider: couch_model::Provider::Plugin {
                        id: "sample".into(),
                        label: "Sample".into(),
                        capabilities: Vec::new(),
                        supports_inputs: false,
                        presentation: Vec::new(),
                        actions: Vec::new(),
                        children: Vec::new(),
                    },
                });
            })
            .unwrap();
        drop(store);
        let unpaired = fs::read(home.join("config.json")).unwrap();

        save_settings_file(&runtime, "tv", json!({"host": "tv.local"}));
        script(
            &runtime,
            vec![Ok(PairStep::done(key("s3cret"), "Paired with the TV"))],
        );
        assert!(matches!(
            runtime.pair_start("tv", "sample", json!({})).unwrap().step,
            PairReply::Done { .. }
        ));
        assert!(runtime.credential_path("tv").unwrap().exists());
        // Pairing adds nothing to config.json, which is what makes rolling
        // back to a Couch that knows nothing about it need no new rule.
        assert_eq!(fs::read(home.join("config.json")).unwrap(), unpaired);
        // ...and a Couch reading it afterwards sees exactly what it saw.
        let again = crate::store::Store::open(home.join("config.json")).unwrap();
        assert_eq!(again.config().connections.len(), 1);

        // The recovery export beside it is built from the configuration and
        // from nothing else, so a key is not in that either. Saving again is
        // what writes one.
        let mut store = crate::store::Store::open(home.join("config.json")).unwrap();
        store
            .mutate(None, |config| {
                config.connections[0].name = "Television".into()
            })
            .unwrap();
        drop(store);
        let exports: Vec<PathBuf> = fs::read_dir(&home)
            .unwrap()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.contains("integration-recovery"))
            })
            .collect();
        assert!(!exports.is_empty(), "an export was written");
        for export in exports {
            let written = fs::read_to_string(&export).unwrap();
            assert!(!written.contains("s3cret"), "{}", export.display());
            assert!(
                !written.contains("plugin-credential"),
                "{}",
                export.display()
            );
        }
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn neither_the_panel_nor_a_browser_can_pair_through_the_request_gate() {
        let home = home("gate");
        let runtime = Runtime::new(home.clone());
        save_settings_file(&runtime, "tv", json!({"host": "tv.local"}));
        script(&runtime, vec![Ok(press())]);
        // `execute` is the whole of what `plugin.sock` and the HTTP command
        // routes reach. Pairing is a conversation with a child of its own and
        // has never been one of the four things they may send.
        for request in [
            Request::pair_start(json!({"host": "tv.local"}), None),
            Request::pair_continue("p1", None),
            Request::pair_cancel("p1"),
            Request::configure(json!({"host": "tv.local"})),
        ] {
            assert_eq!(
                runtime.execute("tv", "sample", None, request),
                Err(Error::Unsupported.into())
            );
        }
        assert!(runtime.pairings.lock().unwrap().is_empty());
        let _ = fs::remove_dir_all(home);
    }
}

/// Protocol 3 (unreleased): `keep_alive`, and the one thing it changes.
///
/// No manifest a shipped build accepts may declare it (`Manifest::validate`
/// refuses it below protocol 3, and no protocol 3 manifest validates with the
/// switch off), so the flag is set on the entry here rather than read from a
/// package. Everything else - real children of a real package in a real store,
/// the reaper, the cap and what it is measured against - is the live code.
#[cfg(test)]
mod keep_alive_tests {
    use super::*;

    /// Start a child for each of these connections of the one package.
    fn warm(runtime: &Runtime, connections: &[String]) {
        for connection in connections {
            let path = runtime.settings_path(connection).unwrap();
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            couch_sdk::save_private(&path, &json!({"host": "tv.local", "port": 23})).unwrap();
            // What the child answers is not what this is about: the request
            // is what starts one and puts it in the registry.
            let _ = runtime.execute(connection, "sample", None, Request::status());
            assert!(
                runtime.endpoints.lock().unwrap().contains_key(connection),
                "{connection}"
            );
        }
        assert_eq!(runtime.endpoints.lock().unwrap().len(), connections.len());
    }

    /// Make every child look older than the idle retain, say which packages
    /// asked to stay, and space out when each was last used.
    fn idle(runtime: &Runtime, keep_alive: bool) {
        let mut endpoints = runtime.endpoints.lock().unwrap();
        for (older, entry) in endpoints.values_mut().enumerate() {
            entry.keep_alive = keep_alive;
            entry.used -= IDLE + Duration::from_secs(1 + older as u64);
        }
    }

    fn live(runtime: &Runtime) -> Vec<String> {
        let mut names: Vec<String> = runtime.endpoints.lock().unwrap().keys().cloned().collect();
        names.sort();
        names
    }

    #[test]
    fn keep_alive_holds_a_child_only_while_a_device_points_at_it_and_only_eight_of_them() {
        let home = super::tests::home_with_a_package("keep-alive", "sample");
        let runtime = Runtime::new(home.clone());

        // What happens today, and what still happens to a package that has
        // not asked: an idle child goes, whatever refers to its connection.
        let pair: Vec<String> = ["a", "b"].iter().map(|n| (*n).to_owned()).collect();
        warm(&runtime, &pair);
        idle(&runtime, false);
        runtime.reap(&|_| true);
        assert!(
            live(&runtime).is_empty(),
            "keep_alive is the only exemption"
        );

        // A package that asked, for a connection nothing points at any more.
        warm(&runtime, &pair);
        idle(&runtime, true);
        runtime.reap(&|_| false);
        assert!(live(&runtime).is_empty(), "nothing refers to it");

        // Nine that asked and are all in use: the eight most recently used
        // stay, and the one nobody has touched for longest is reaped as any
        // other idle child is.
        let many: Vec<String> = (0..=MAX_KEEP_ALIVE).map(|at| format!("tv{at}")).collect();
        warm(&runtime, &many);
        idle(&runtime, true);
        let oldest = {
            let endpoints = runtime.endpoints.lock().unwrap();
            endpoints
                .iter()
                .min_by_key(|(_, entry)| entry.used)
                .map(|(connection, _)| connection.clone())
                .unwrap()
        };
        runtime.reap(&|_| true);
        let kept = live(&runtime);
        assert_eq!(kept.len(), MAX_KEEP_ALIVE);
        assert!(
            !kept.contains(&oldest),
            "{oldest} was the least recently used"
        );

        // They keep no state of their own: the moment the last device stops
        // pointing at them they are ordinary idle children again.
        runtime.reap(&|_| false);
        assert!(live(&runtime).is_empty());
        for connection in many {
            runtime.retire(&connection);
        }
        let _ = fs::remove_dir_all(home);
    }
}

/// Protocol 3 (unreleased): handing a departed built-in's stored key to its
/// package.
///
/// No row does this yet - Denon never stored a key - so the hook is driven
/// here by a row of the shape a built-in that did keep one would have.
#[cfg(test)]
mod legacy_credential_tests {
    use super::*;
    use couch_model::{LegacyBuiltin, LegacySetting, Provider};

    fn carried(provider: &Provider) -> Option<Vec<(&'static str, LegacySetting)>> {
        match provider {
            Provider::Hue => Some(vec![
                ("host", LegacySetting::Text("bridge.local".into())),
                ("port", LegacySetting::Integer(443)),
            ]),
            _ => None,
        }
    }
    fn mapped(stored: &Value) -> Option<Value> {
        let key = stored.get("application_key")?.as_str()?;
        (!key.is_empty()).then(|| json!({ "key": key }))
    }
    fn row() -> LegacyBuiltin {
        LegacyBuiltin {
            kind: "hue",
            package: "sample",
            name: "Hue",
            settings: carried,
            credential_file: Some("hue-connection.json"),
            credential: Some(mapped),
        }
    }

    #[test]
    fn a_converted_connection_keeps_its_key_and_the_built_in_file_it_came_from() {
        let home = super::tests::home_with_a_package("legacy-key", "sample");
        let runtime = Runtime::new(home.clone());
        let row = row();
        let folder = home.join("connections/bridge");
        fs::create_dir_all(&folder).unwrap();
        let built_in = folder.join("hue-connection.json");
        couch_sdk::save_private(
            &built_in,
            &json!({"url": "https://bridge.local/", "application_key": "abc123"}),
        )
        .unwrap();

        let credential = runtime
            .legacy_credential("bridge", &row)
            .expect("the built-in's key, in the shape its package takes");
        assert_eq!(
            credential,
            Credential::new(json!({"key": "abc123"})).unwrap()
        );
        // A row that hands nothing over, and a file that is not there.
        assert!(runtime
            .legacy_credential("bridge", LegacyBuiltin::for_kind("denon").unwrap())
            .is_none());
        assert!(runtime.legacy_credential("nothing-here", &row).is_none());

        // The package checks the carried-over settings with the key in hand.
        // This one is a protocol 1 package, so the gate strips the key and
        // the child receives today's exact configure - which is the point:
        // it refuses an unknown field, and it does not refuse this.
        let prepared = runtime
            .prepare_legacy(
                "sample",
                json!({"host": "bridge.local", "port": 443}),
                Some(credential.clone()),
            )
            .expect("the package accepts the carried-over settings");
        runtime
            .adopt_legacy("bridge", &prepared, |manifest| {
                assert_eq!(manifest.id, "sample");
                Ok(())
            })
            .unwrap();

        // The key is written before the settings, so a package that cannot
        // work without one is never left converted and unpaired.
        let key = runtime.credential_path("bridge").unwrap();
        let settings = runtime.settings_path("bridge").unwrap();
        assert_eq!(fs::read_to_string(&key).unwrap(), r#"{"key":"abc123"}"#);
        assert!(
            fs::metadata(&key).unwrap().modified().unwrap()
                <= fs::metadata(&settings).unwrap().modified().unwrap()
        );
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&key).unwrap().permissions().mode() & 0o777,
            0o600
        );
        // The built-in's own file stays where it is: a Couch rolled back to
        // one that still has the built-in client has to find its pairing.
        assert_eq!(
            fs::read_to_string(&built_in).unwrap(),
            r#"{"application_key":"abc123","url":"https://bridge.local/"}"#
        );
        // A conversion that hands nothing over writes no key at all.
        fs::remove_file(&key).unwrap();
        let prepared = runtime
            .prepare_legacy("sample", json!({"host": "bridge.local", "port": 443}), None)
            .unwrap();
        runtime
            .adopt_legacy("bridge", &prepared, |_| Ok(()))
            .unwrap();
        assert!(!key.exists());
        let _ = fs::remove_dir_all(home);
    }
}
