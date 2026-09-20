use crate::{
    protocol::Envelope, read_frame, write_frame, Error, Failure, Manifest, Request, Response,
    Result, NEXT_PROTOCOL_VERSION,
};
use couch_sdk::{
    children::valid_cursor,
    couch_model::{commands::Function, valid_resource, ChildComponent, PluginChildKind},
    ActionKind, KeyPhase, PluginActionSchema, TypedAction, MAX_PAGE,
};
use std::{
    collections::HashSet,
    io::{Read, Write},
    os::{
        fd::OwnedFd,
        unix::{
            fs::{MetadataExt, PermissionsExt},
            net::UnixStream,
            process::CommandExt,
        },
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        mpsc::{self, SyncSender, TrySendError},
        Arc,
    },
    time::{Duration, Instant},
};

pub const QUEUE_CAPACITY: usize = 8;
pub const QUEUE_TTL: Duration = Duration::from_millis(750);
/// Protocol 3. The most children one connection may offer, the most pages it
/// may take to list them, and how long the whole listing has. A package that
/// breaks any of them is answering nonsense and is retired.
pub const MAX_CHILDREN: usize = 1024;
pub const MAX_CHILD_PAGES: usize = 64;
pub const CHILD_LISTING_DEADLINE: Duration = Duration::from_secs(10);
// A Denon mute toggle can include four status queries and one confirmed write.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(12);
pub const STARTUP_TIMEOUT: Duration = Duration::from_secs(3);

// The HA100's Android-derived ARMv7 kernel enables
// CONFIG_ANDROID_PARANOID_NETWORK. Its inet_create() accepts a normal TCP/UDP
// socket only for AID_INET (3003) or CAP_NET_RAW. Give plugins the narrow
// group, never the capability. Keep this target-specific: desktop Linux does
// not use that Android kernel policy.
#[cfg(all(target_os = "linux", target_arch = "arm", target_env = "musl"))]
const DEFAULT_SUPPLEMENTARY_GIDS: &[libc::gid_t] = &[3003];
#[cfg(not(all(target_os = "linux", target_arch = "arm", target_env = "musl")))]
const DEFAULT_SUPPLEMENTARY_GIDS: &[libc::gid_t] = &[];

/// The production policy drops root before exec on Linux. Unprivileged host
/// development inherits its uid. This is privilege separation, not a sandbox:
/// integrations still share a uid and can access the LAN.
#[derive(Clone, Copy, Debug)]
pub struct HostPolicy {
    pub uid: u32,
    pub gid: u32,
    supplementary_gids: &'static [libc::gid_t],
}
impl Default for HostPolicy {
    fn default() -> Self {
        Self {
            uid: 65534,
            gid: 65534,
            supplementary_gids: DEFAULT_SUPPLEMENTARY_GIDS,
        }
    }
}
impl HostPolicy {
    /// The complete supplementary set applied while dropping privileges.
    ///
    /// Couch's ARMv7 musl target carries Android's `AID_INET` (3003) so an
    /// unprivileged plugin can create ordinary Internet sockets on the HA100.
    /// Other targets receive no supplementary groups.
    pub const fn supplementary_gids(self) -> &'static [libc::gid_t] {
        self.supplementary_gids
    }
}

struct DeadlineStream<'a> {
    stream: &'a mut UnixStream,
    deadline: Instant,
}
impl DeadlineStream<'_> {
    fn remaining(&self) -> std::io::Result<Duration> {
        let left = self.deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            Err(std::io::ErrorKind::TimedOut.into())
        } else {
            Ok(left)
        }
    }
}
impl Read for DeadlineStream<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        self.stream.set_read_timeout(Some(self.remaining()?))?;
        self.stream.read(bytes)
    }
}
impl Write for DeadlineStream<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.stream.set_write_timeout(Some(self.remaining()?))?;
        self.stream.write(bytes)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.stream.flush()
    }
}

pub struct Host {
    child: Child,
    stream: UnixStream,
    manifest: Manifest,
    timeout: Duration,
    next_id: u64,
    alive: bool,
}
impl Host {
    pub fn spawn(package_dir: &Path, manifest: &Manifest, timeout: Duration) -> Result<Self> {
        Self::spawn_with_policy(package_dir, manifest, timeout, HostPolicy::default())
    }
    pub fn spawn_with_policy(
        package_dir: &Path,
        manifest: &Manifest,
        timeout: Duration,
        policy: HostPolicy,
    ) -> Result<Self> {
        manifest.validate()?;
        if timeout.is_zero() || timeout > Duration::from_secs(60) {
            return Err(Error::Invalid);
        }
        let root = package_dir.canonicalize().map_err(|_| Error::Invalid)?;
        let executable = root
            .join(&manifest.executable)
            .canonicalize()
            .map_err(|_| Error::Invalid)?;
        if !executable.starts_with(&root) {
            return Err(Error::Invalid);
        }
        let metadata = executable.metadata().map_err(|_| Error::Invalid)?;
        if !metadata.is_file()
            || metadata.permissions().mode() & 0o022 != 0
            || metadata.permissions().mode() & 0o111 == 0
        {
            return Err(Error::Invalid);
        }
        // The installer owns ancestors above the package root. Check all
        // components below it so another user cannot replace the executable.
        let owner = unsafe { libc::geteuid() };
        for path in executable.ancestors().take_while(|p| p.starts_with(&root)) {
            let metadata = path.metadata().map_err(|_| Error::Invalid)?;
            if metadata.permissions().mode() & 0o022 != 0
                || (metadata.uid() != 0 && metadata.uid() != owner)
            {
                return Err(Error::Invalid);
            }
        }
        let (stream, child_stream) = UnixStream::pair()?;
        let input: OwnedFd = child_stream.try_clone()?.into();
        let output: OwnedFd = child_stream.into();
        let mut command = Command::new(executable);
        command
            .env_clear()
            .current_dir("/")
            .stdin(Stdio::from(input))
            .stdout(Stdio::from(output))
            .stderr(Stdio::null());
        // A separate process group lets a timeout retire descendants too. All
        // work in pre_exec is async-signal-safe; never allocate or lock there.
        unsafe {
            command.pre_exec(move || {
                if libc::setpgid(0, 0) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                #[cfg(target_os = "linux")]
                {
                    if libc::geteuid() == 0 {
                        if policy.uid == 0 || policy.gid == 0 {
                            return Err(std::io::ErrorKind::PermissionDenied.into());
                        }
                        if libc::setgroups(
                            policy.supplementary_gids.len(),
                            policy.supplementary_gids.as_ptr(),
                        ) != 0
                            || libc::setgid(policy.gid) != 0
                            || libc::setuid(policy.uid) != 0
                        {
                            return Err(std::io::Error::last_os_error());
                        }
                    }
                    if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                }
                #[cfg(not(target_os = "linux"))]
                let _ = policy;
                Ok(())
            });
        }
        let child = command.spawn()?;
        let mut host = Self {
            child,
            stream,
            manifest: manifest.clone(),
            timeout,
            next_id: 0,
            alive: true,
        };
        match host.request(Request::Hello {
            protocol_version: manifest.protocol_version,
        })? {
            Response::Hello { manifest: actual } if actual == *manifest => Ok(host),
            _ => Err(Error::Incompatible),
        }
    }
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }
    pub fn set_timeout(&mut self, timeout: Duration) -> Result<()> {
        if timeout.is_zero() || timeout > Duration::from_secs(60) {
            return Err(Error::Invalid);
        }
        self.timeout = timeout;
        Ok(())
    }
    pub fn is_alive(&self) -> bool {
        self.alive
    }
    pub fn pid(&self) -> u32 {
        self.child.id()
    }
    pub fn configure(&mut self, settings: serde_json::Value) -> Result<()> {
        match self.request(Request::Configure { settings })? {
            Response::Ok => Ok(()),
            _ => Err(Error::Protocol),
        }
    }
    pub fn command(&mut self, function: &str) -> Result<()> {
        match self.request(Request::command(function))? {
            Response::Ok => Ok(()),
            _ => Err(Error::Protocol),
        }
    }
    pub fn action(&mut self, action: couch_sdk::TypedAction) -> Result<()> {
        match self.request(Request::action(action))? {
            Response::Ok => Ok(()),
            _ => Err(Error::Protocol),
        }
    }
    pub fn status(&mut self) -> Result<couch_sdk::Status> {
        match self.request(Request::status())? {
            Response::Status { status } => Ok(status),
            _ => Err(Error::Protocol),
        }
    }
    pub fn inputs(&mut self) -> Result<Vec<couch_sdk::Selectable>> {
        match self.request(Request::Inputs)? {
            Response::Inputs { inputs } => Ok(inputs),
            _ => Err(Error::Protocol),
        }
    }
    /// A key press with its phase. A protocol 1 or 2 package is sent a tap.
    pub fn key(&mut self, function: &str, phase: KeyPhase) -> Result<()> {
        match self.request(Request::key(function, phase))? {
            Response::Ok => Ok(()),
            _ => Err(Error::Protocol),
        }
    }
    pub fn request(&mut self, request: Request) -> Result<Response> {
        self.request_detailed(request)
            .map_err(|failure| failure.code)
    }
    /// [`Host::request`], keeping a protocol 3 package's reason for an error.
    ///
    /// This is the one place that decides what a package of a given protocol
    /// may be sent and may answer. The version is the one in its manifest,
    /// which the handshake has already matched against the package's own.
    pub fn request_detailed(&mut self, request: Request) -> std::result::Result<Response, Failure> {
        self.request_child_detailed(None, request)
    }
    /// [`Host::request_detailed`] for a request that names a child of this
    /// connection. `kind` is the kind the configuration says that child is;
    /// it is what the gate checks the request against and is never written to
    /// the wire. A request with no `resource` passes `None`.
    pub fn request_child_detailed(
        &mut self,
        kind: Option<&str>,
        request: Request,
    ) -> std::result::Result<Response, Failure> {
        if !self.alive {
            return Err(Error::Transport.into());
        }
        let request = admit(&self.manifest, kind, request)?;
        self.next_id = self.next_id.checked_add(1).ok_or(Error::Protocol)?;
        let result = (|| {
            let mut stream = DeadlineStream {
                stream: &mut self.stream,
                deadline: Instant::now() + self.timeout,
            };
            write_frame(
                &mut stream,
                &Envelope {
                    id: self.next_id,
                    body: &request,
                },
            )?;
            let response: Envelope<Response> = read_frame(&mut stream)?;
            if response.id != self.next_id {
                return Err(Error::Protocol);
            }
            accept(&self.manifest, &request, &response.body)?;
            Ok(response.body)
        })();
        // Protocol/transport failures retire the stream: never consume a late
        // reply as the next request's response and never replay this request.
        if result.is_err() {
            self.terminate();
        }
        match result? {
            Response::Error { code, reason } => Err(Failure { code, reason }),
            response => Ok(response),
        }
    }
    /// Retire the package, killing its process group.
    ///
    /// The host does this by itself when a package answers something it must
    /// not ([`accept`]). This is for the caller that sees a violation the host
    /// cannot see in one answer: a listing that never ends, or the same child
    /// twice on two pages ([`list_children`]).
    pub fn retire(&mut self) {
        self.terminate();
    }
    fn terminate(&mut self) {
        if self.alive {
            self.alive = false;
            // Child remains unreaped until wait below, so its group id cannot
            // have been reused even if it has already exited.
            unsafe {
                libc::kill(-(self.child.id() as i32), libc::SIGKILL);
            }
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}
impl Drop for Host {
    fn drop(&mut self) {
        self.terminate();
    }
}

/// The gate, before any I/O: what a package of this manifest's protocol may be
/// sent. Returns the request as it will be written.
///
/// `kind` is the kind of child the configuration says `request`'s resource is.
/// It is the only thing here that does not come from the manifest or the
/// request itself, and it never reaches the wire: a resource is a plain string
/// to the package, which knows what its own children are.
pub(crate) fn admit(
    manifest: &Manifest,
    kind: Option<&str>,
    mut request: Request,
) -> Result<Request> {
    let version = manifest.protocol_version;
    // A phase the package cannot read is dropped rather than refused: the key
    // still works, as a tap, and the bytes are the ones a protocol 1 or 2
    // package has always been sent.
    if let Request::Command { phase, .. } = &mut request {
        if version < NEXT_PROTOCOL_VERSION {
            *phase = KeyPhase::Tap;
        }
    }
    // First, and before anything looks at a resource: a package is never sent
    // a word its protocol does not have. `requires` is 3 for every request
    // that names a child, so this is what keeps `resource` out of the bytes a
    // published package reads.
    if requires(&request) > version {
        return Err(Error::Unsupported);
    }
    if request.resource().is_some() {
        return admit_child(manifest, kind, request);
    }
    match &request {
        Request::Command { function, .. } if !manifest.supports(function) => {
            return Err(Error::Unsupported)
        }
        Request::Inputs if !manifest.supports_inputs => return Err(Error::Unsupported),
        Request::Children { cursor } => {
            if manifest.children.is_empty() {
                return Err(Error::Unsupported);
            }
            if cursor.as_deref().is_some_and(|c| !valid_cursor(c)) {
                return Err(Error::Invalid);
            }
        }
        Request::Configure { settings } => manifest.validate_settings(settings)?,
        Request::Action { action, .. } => manifest.validate_action(*action)?,
        _ => (),
    }
    Ok(request)
}

/// The rest of the gate, for a request that names a child, in order: the
/// resource grammar, the kind, the level mapping, and then what that kind
/// declares.
fn admit_child(manifest: &Manifest, kind: Option<&str>, request: Request) -> Result<Request> {
    let resource = request.resource().ok_or(Error::Invalid)?;
    if !valid_resource(resource) {
        return Err(Error::Invalid);
    }
    // Without a kind the host does not know what this child is, so it cannot
    // know what may be said to it. That is a caller's mistake, not a package's.
    let kind = manifest.child_kind(kind.ok_or(Error::Invalid)?);
    let kind = kind.ok_or(Error::Unsupported)?;
    let request = level(kind, request)?;
    match &request {
        Request::Command { function, .. } => {
            if !kind.capabilities.iter().any(|c| c.id == *function) {
                return Err(Error::Unsupported);
            }
        }
        Request::Action { action, .. } => {
            let schema =
                PluginActionSchema::find(&kind.actions, action.kind()).ok_or(Error::Unsupported)?;
            if !schema.accepts(*action) {
                return Err(Error::Invalid);
            }
        }
        Request::Status { .. } => (),
        _ => return Err(Error::Unsupported),
    }
    Ok(request)
}

/// The one place a level becomes a typed action.
///
/// `dim:30`, `position:40` and `mode:heat` are ordinary commands everywhere
/// else in Couch - in a button map, in a scene step, in the configuration file
/// - because that is what a person binds and what the file has always held.
/// A child takes them as the typed action its kind declares, and this is where
/// that happens, once, so no caller has to know which children are lamps.
/// A level to the connection itself is left alone: it is the plain command a
/// protocol 1 or 2 package has always received.
fn level(kind: &PluginChildKind, request: Request) -> Result<Request> {
    let Request::Command {
        function, resource, ..
    } = &request
    else {
        return Ok(request);
    };
    let Some(parsed) = Function::parse(function) else {
        return Ok(request);
    };
    let action = match (parsed, kind.component) {
        (Function::Dim(percent), ChildComponent::Light) => TypedAction::SetLight {
            on: None,
            brightness: Some(percent),
            mirek: None,
            xy: None,
        },
        (Function::Position(percent), ChildComponent::Cover) => {
            TypedAction::SetCover { position: percent }
        }
        (Function::Mode(name), ChildComponent::Climate) => TypedAction::SetClimate {
            target_tenths: None,
            low_tenths: None,
            high_tenths: None,
            mode: Some(couch_sdk::ClimateMode::from_name(&name).ok_or(Error::Unsupported)?),
        },
        // A level whose kind is not drawn with that control has no meaning
        // here, and no capability is ever spelt `dim:30`.
        (Function::Dim(_) | Function::Position(_) | Function::Mode(_), _) => {
            return Err(Error::Unsupported)
        }
        _ => return Ok(request),
    };
    Ok(Request::Action {
        action,
        resource: resource.clone(),
    })
}

/// The gate, after I/O: what a package of this manifest's protocol may answer.
/// Any error here retires the child.
pub(crate) fn accept(manifest: &Manifest, request: &Request, response: &Response) -> Result<()> {
    validate_response(request, response)?;
    let version = manifest.protocol_version;
    if version == 1 && matches!(response, Response::Status { status } if status.volume_db.is_some())
    {
        return Err(Error::Protocol);
    }
    // Protocol 3 words from a package that did not declare protocol 3. The
    // gate above never asks such a package to list children or names a child
    // to it, so a listing or a child's state coming back is a broken package.
    // A status as the answer to a write is caught by `validate_response`: only
    // a request that named a child may be answered with one.
    if version < NEXT_PROTOCOL_VERSION
        && match response {
            Response::Children { .. } => true,
            Response::Status { status } => status.is_child_state(),
            _ => false,
        }
    {
        return Err(Error::Protocol);
    }
    if let Response::Children { children, .. } = response {
        for child in children {
            // Well-formedness is checked without the manifest; this is the
            // part only the manifest can answer: the kind exists, and the
            // traits are the ones that kind's control has.
            let Some(kind) = manifest.child_kind(&child.kind) else {
                return Err(Error::Protocol);
            };
            if !child.snapshot().fits(kind.component) {
                return Err(Error::Protocol);
            }
        }
    }
    if let Response::Error { code, reason } = response {
        // Protocol 3 vocabulary from a package that did not declare protocol 3
        // is a broken package, not a newer one.
        if version < NEXT_PROTOCOL_VERSION && (reason.is_some() || *code == Error::Unpaired) {
            return Err(Error::Protocol);
        }
        if reason
            .as_ref()
            .is_some_and(|reason| !manifest.accepts_reason(reason))
        {
            return Err(Error::Protocol);
        }
    }
    Ok(())
}

/// The oldest manifest protocol whose package can be sent this request. A key
/// phase is not counted: the host drops it for an older package instead of
/// refusing the key.
pub fn requires(request: &Request) -> u32 {
    // Naming a child of a connection is protocol 3, whatever is being asked.
    if request.resource().is_some() {
        return NEXT_PROTOCOL_VERSION;
    }
    match request {
        // The light, cover and climate actions couch-model gained in protocol
        // 3, step T2. An older package could not declare one either
        // (`Manifest::validate`); this says so at the gate itself.
        Request::Action { action, .. } if action.kind() != ActionKind::SetVolumeDb => {
            NEXT_PROTOCOL_VERSION
        }
        Request::Action { .. } => 2,
        Request::Command { function, .. }
            if matches!(Function::parse(function), Some(Function::Custom(_))) =>
        {
            NEXT_PROTOCOL_VERSION
        }
        Request::Children { .. } => NEXT_PROTOCOL_VERSION,
        _ => 1,
    }
}

fn validate_response(request: &Request, response: &Response) -> Result<()> {
    match (request, response) {
        (_, Response::Error { .. }) => Ok(()),
        (Request::Hello { .. }, Response::Hello { manifest }) => manifest.validate(),
        (
            Request::Configure { .. } | Request::Command { .. } | Request::Action { .. },
            Response::Ok,
        ) => Ok(()),
        // Protocol 3: a write to a child may be acknowledged with the state
        // the child is in afterwards, which saves a read. `Ok` stays legal,
        // and a write with no resource may not be answered this way at all.
        (
            Request::Command {
                resource: Some(_), ..
            }
            | Request::Action {
                resource: Some(_), ..
            }
            | Request::Status { .. },
            Response::Status { status },
        ) if status.is_valid() => Ok(()),
        (Request::Inputs, Response::Inputs { inputs })
            if inputs.len() <= 128
                && inputs.iter().all(|i| {
                    couch_sdk::couch_model::commands::valid_input_id(&i.id)
                        && i.name.len() <= 256
                        && !i.name.chars().any(char::is_control)
                }) =>
        {
            Ok(())
        }
        (Request::Children { .. }, Response::Children { children, next }) => {
            let mut seen = HashSet::new();
            if children.len() > MAX_PAGE
                || (children.is_empty() && next.is_some())
                || !children
                    .iter()
                    .all(|child| child.is_well_formed() && seen.insert(&child.id))
                || next.as_deref().is_some_and(|next| !valid_cursor(next))
            {
                return Err(Error::Protocol);
            }
            Ok(())
        }
        _ => Err(Error::Protocol),
    }
}

/// Read every child of one connection, through whatever asks the package.
///
/// The caller supplies `ask` because the daemon reaches its package through an
/// [`Endpoint`], the tests through a closure, and neither should have to
/// re-implement paging. Every limit is this function's: at most
/// [`MAX_CHILDREN`] children over at most [`MAX_CHILD_PAGES`] pages, no cursor
/// twice, no id twice, and the whole listing inside
/// [`CHILD_LISTING_DEADLINE`]. A package that breaks one of them is answering
/// nonsense - a list that never ends is the same to the caller as a list that
/// is too long - so every one of them is [`Error::Protocol`], and the caller
/// retires the child.
pub fn list_children(
    ask: &mut dyn FnMut(Request) -> std::result::Result<Response, Failure>,
) -> Result<Vec<couch_sdk::Child>> {
    let deadline = Instant::now() + CHILD_LISTING_DEADLINE;
    let mut all: Vec<couch_sdk::Child> = Vec::new();
    let mut ids: HashSet<String> = HashSet::new();
    let mut cursors: HashSet<String> = HashSet::new();
    let mut cursor: Option<String> = None;
    for _ in 0..MAX_CHILD_PAGES {
        if Instant::now() >= deadline {
            return Err(Error::Protocol);
        }
        let Response::Children { children, next } =
            ask(Request::children(cursor)).map_err(|failure| failure.code)?
        else {
            return Err(Error::Protocol);
        };
        if children.len() > MAX_PAGE
            || (children.is_empty() && next.is_some())
            || all.len() + children.len() > MAX_CHILDREN
        {
            return Err(Error::Protocol);
        }
        for child in children {
            if !child.is_well_formed() || !ids.insert(child.id.clone()) {
                return Err(Error::Protocol);
            }
            all.push(child);
        }
        let Some(next) = next else {
            return Ok(all);
        };
        // A cursor already followed means the package is going round in a
        // circle, however long it takes to come back to it.
        if !valid_cursor(&next) || !cursors.insert(next.clone()) {
            return Err(Error::Protocol);
        }
        cursor = Some(next);
    }
    Err(Error::Protocol)
}

struct Pending {
    request: Request,
    /// The kind of child the request names, if it names one. Carried here
    /// rather than on the wire: the gate on the worker's side needs it, the
    /// package never does.
    kind: Option<String>,
    queued: Instant,
    reply: SyncSender<std::result::Result<Response, Failure>>,
}
/// Cloneable handle to one persistent endpoint owner and a bounded queue.
/// A failed request is never replayed. Only a subsequent explicit request may
/// launch a replacement child. Dropping every handle retires the child.
#[derive(Clone)]
pub struct Endpoint {
    inner: Arc<EndpointInner>,
}
struct EndpointInner {
    sender: Option<SyncSender<Pending>>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl Drop for EndpointInner {
    fn drop(&mut self) {
        self.sender.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
impl Endpoint {
    pub fn start(
        package_dir: &Path,
        manifest: Manifest,
        settings: serde_json::Value,
    ) -> Result<Self> {
        Self::start_with_timeout(package_dir, manifest, settings, REQUEST_TIMEOUT)
    }
    pub fn start_with_timeout(
        package_dir: &Path,
        manifest: Manifest,
        settings: serde_json::Value,
        timeout: Duration,
    ) -> Result<Self> {
        let package_dir: PathBuf = package_dir.into();
        let settings = manifest.with_defaults(settings)?;
        let mut host = Host::spawn(&package_dir, &manifest, STARTUP_TIMEOUT)?;
        host.configure(settings.clone())?;
        host.set_timeout(timeout)?;
        let (sender, receiver) = mpsc::sync_channel::<Pending>(QUEUE_CAPACITY);
        let worker = std::thread::Builder::new()
            .name(format!("integration-{}", manifest.id))
            .spawn(move || {
                while let Ok(pending) = receiver.recv() {
                    if pending.queued.elapsed() >= QUEUE_TTL {
                        let _ = pending.reply.send(Err(Error::Expired.into()));
                        continue;
                    }
                    if !host.is_alive() {
                        let replacement = Host::spawn(&package_dir, &manifest, STARTUP_TIMEOUT)
                            .and_then(|mut h| {
                                h.configure(settings.clone())?;
                                h.set_timeout(timeout)?;
                                Ok(h)
                            });
                        match replacement {
                            Ok(replacement) => host = replacement,
                            Err(error) => {
                                let _ = pending.reply.send(Err(error.into()));
                                continue;
                            }
                        }
                        if pending.queued.elapsed() >= QUEUE_TTL {
                            let _ = pending.reply.send(Err(Error::Expired.into()));
                            continue;
                        }
                    }
                    let result =
                        host.request_child_detailed(pending.kind.as_deref(), pending.request);
                    let _ = pending.reply.send(result);
                }
            })
            .map_err(|_| Error::Transport)?;
        Ok(Self {
            inner: Arc::new(EndpointInner {
                sender: Some(sender),
                worker: Some(worker),
            }),
        })
    }
    pub fn request(&self, request: Request) -> Result<Response> {
        self.request_detailed(request)
            .map_err(|failure| failure.code)
    }
    /// [`Endpoint::request`], keeping a protocol 3 package's reason.
    pub fn request_detailed(&self, request: Request) -> std::result::Result<Response, Failure> {
        self.request_child_detailed(None, request)
    }
    /// [`Endpoint::request_detailed`] for a request that names a child of this
    /// connection: `kind` is what the configuration says that child is. See
    /// [`Host::request_child_detailed`].
    pub fn request_child_detailed(
        &self,
        kind: Option<&str>,
        request: Request,
    ) -> std::result::Result<Response, Failure> {
        // Endpoint identity/settings remain fixed for the lifetime of its owner.
        if matches!(request, Request::Hello { .. } | Request::Configure { .. }) {
            return Err(Error::Unsupported.into());
        }
        let (reply, receiver) = mpsc::sync_channel(1);
        match self
            .inner
            .sender
            .as_ref()
            .ok_or(Failure::from(Error::Transport))?
            .try_send(Pending {
                request,
                kind: kind.map(str::to_owned),
                queued: Instant::now(),
                reply,
            }) {
            Ok(()) => (),
            Err(TrySendError::Full(_)) => return Err(Error::Busy.into()),
            Err(TrySendError::Disconnected(_)) => return Err(Error::Transport.into()),
        }
        receiver
            .recv()
            .map_err(|_| Failure::from(Error::Transport))?
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalRequest {
    pub connection_id: String,
    pub request: Request,
}

/// Read a bounded frame under one absolute deadline, including every partial
/// read. Useful for the daemon bridge's untrusted local clients.
pub fn read_frame_timeout<T: serde::de::DeserializeOwned>(
    stream: &mut UnixStream,
    timeout: Duration,
) -> Result<T> {
    if timeout.is_zero() {
        return Err(Error::Invalid);
    }
    read_frame(&mut DeadlineStream {
        stream,
        deadline: Instant::now() + timeout,
    })
}

pub fn write_frame_timeout<T: serde::Serialize>(
    stream: &mut UnixStream,
    value: &T,
    timeout: Duration,
) -> Result<()> {
    if timeout.is_zero() {
        return Err(Error::Invalid);
    }
    write_frame(
        &mut DeadlineStream {
            stream,
            deadline: Instant::now() + timeout,
        },
        value,
    )
}

/// The daemon bridge uses the same bounded frames, but no child envelope.
pub fn local_request(
    socket: &Path,
    connection_id: &str,
    request: Request,
    timeout: Duration,
) -> Result<Response> {
    local_request_detailed(socket, connection_id, request, timeout).map_err(|failure| failure.code)
}

/// [`local_request`], keeping the reason the daemon relayed. The frames are
/// the same ones; `LocalRequest` is unchanged.
pub fn local_request_detailed(
    socket: &Path,
    connection_id: &str,
    request: Request,
    timeout: Duration,
) -> std::result::Result<Response, Failure> {
    if connection_id.is_empty()
        || connection_id.len() > 128
        || connection_id.chars().any(char::is_control)
        || timeout.is_zero()
    {
        return Err(Error::Invalid.into());
    }
    let mut stream = UnixStream::connect(socket).map_err(Error::from)?;
    let mut stream = DeadlineStream {
        stream: &mut stream,
        deadline: Instant::now() + timeout,
    };
    write_frame(
        &mut stream,
        &LocalRequest {
            connection_id: connection_id.into(),
            request: request.clone(),
        },
    )?;
    let response = read_frame(&mut stream)?;
    validate_response(&request, &response)?;
    match response {
        Response::Error {
            reason: Some(reason),
            ..
        } if !reason.is_well_formed() => Err(Error::Protocol.into()),
        Response::Error { code, reason } => Err(Failure { code, reason }),
        response => Ok(response),
    }
}

#[cfg(test)]
mod tests {
    use super::{accept, admit};
    use crate::{
        Capability, Child, ChildComponent, Error, LightState, LightTraits, Manifest,
        PluginActionSchema, PluginChildKind, Request, Response, Status, TypedAction,
    };
    use couch_sdk::couch_model::{DeviceKind, PluginCapability};
    use serde_json::json;

    fn manifest(version: u32) -> Manifest {
        let mut manifest: Manifest = serde_json::from_value(json!({
            "protocol_version": 1, "id": "bridge", "label": "Bridge", "version": "1.0.0",
            "executable": "bin/plugin",
            "capabilities": [{"id":"power-on","label":"On"}],
            "settings": [{"id":"host","label":"Host","kind":"text","required":true}]
        }))
        .unwrap();
        manifest.protocol_version = version;
        manifest.min_core_protocol_version = version;
        if version >= 2 {
            manifest.actions = vec![PluginActionSchema::SetVolumeDb {
                min_tenths: -800,
                max_tenths: 180,
                step_tenths: 5,
            }];
        }
        manifest
    }

    fn capability(id: &str) -> PluginCapability {
        PluginCapability {
            id: id.into(),
            label: "Button".into(),
        }
    }

    fn bridge() -> Manifest {
        let mut manifest = manifest(3);
        manifest.children = vec![
            PluginChildKind {
                kind: "light".into(),
                label: "Lamp".into(),
                device_kind: DeviceKind::Light,
                component: ChildComponent::Light,
                capabilities: vec![capability("on"), capability("toggle")],
                actions: vec![PluginActionSchema::SetLight {}],
            },
            PluginChildKind {
                kind: "blind".into(),
                label: "Blind".into(),
                device_kind: DeviceKind::Blind,
                component: ChildComponent::Cover,
                capabilities: vec![capability("open")],
                actions: vec![PluginActionSchema::SetCover {}],
            },
        ];
        manifest
    }

    fn lamp(on: bool) -> Response {
        Response::Status {
            status: Status::default().with_light(LightState {
                on: Some(on),
                ..Default::default()
            }),
        }
    }

    /// A level is a command in the configuration file, in a button map and in
    /// a scene step, and a typed action on the wire. This is the one place in
    /// Couch where it changes, and it changes only for a child.
    #[test]
    fn the_gate_is_the_one_place_a_level_becomes_a_typed_action() {
        let manifest = bridge();
        let sent = |kind, request| admit(&manifest, Some(kind), request);
        assert_eq!(
            sent("light", Request::command("dim:30").at("lamp-1")),
            Ok(Request::Action {
                action: TypedAction::SetLight {
                    on: None,
                    brightness: Some(30),
                    mirek: None,
                    xy: None
                },
                resource: Some("lamp-1".into())
            })
        );
        assert_eq!(
            sent("blind", Request::command("position:40").at("blind-1")),
            Ok(Request::Action {
                action: TypedAction::SetCover { position: 40 },
                resource: Some("blind-1".into())
            })
        );
        // A level to the connection itself is untouched: it stays the plain
        // command every package has always been sent, and passes or fails on
        // the connection's own capabilities.
        let mut dimmable = manifest.clone();
        dimmable.capabilities.push(Capability {
            id: "dim:30".into(),
            label: "Dim".into(),
        });
        assert_eq!(
            admit(&dimmable, None, Request::command("dim:30")),
            Ok(Request::command("dim:30"))
        );
        assert_eq!(
            admit(&manifest, None, Request::command("dim:30")),
            Err(Error::Unsupported)
        );
    }

    /// Everything the gate refuses about a child, in the order it refuses it.
    #[test]
    fn the_gate_refuses_a_resource_a_kind_or_an_action_that_does_not_fit() {
        let manifest = bridge();
        let light = TypedAction::SetLight {
            on: Some(true),
            brightness: None,
            mirek: None,
            xy: None,
        };
        for (kind, request, expected) in [
            // A function the kind does not declare, and a level for a control
            // the kind is not drawn with.
            (
                Some("light"),
                Request::command("open").at("lamp-1"),
                Error::Unsupported,
            ),
            (
                Some("light"),
                Request::command("position:40").at("lamp-1"),
                Error::Unsupported,
            ),
            (
                Some("blind"),
                Request::command("dim:30").at("blind-1"),
                Error::Unsupported,
            ),
            // A kind the manifest never declared.
            (
                Some("ghost"),
                Request::status().at("lamp-1"),
                Error::Unsupported,
            ),
            // No kind at all: the host does not know what it is talking to.
            (None, Request::status().at("lamp-1"), Error::Invalid),
            // An id that is not a resource.
            (Some("light"), Request::status().at("a//b"), Error::Invalid),
            (Some("light"), Request::status().at("../x"), Error::Invalid),
            (
                Some("light"),
                Request::status().at("a".repeat(129)),
                Error::Invalid,
            ),
            // An action of another kind's control, and one that says nothing.
            (
                Some("light"),
                Request::action(TypedAction::SetCover { position: 40 }).at("lamp-1"),
                Error::Unsupported,
            ),
            (
                Some("light"),
                Request::action(TypedAction::SetLight {
                    on: None,
                    brightness: None,
                    mirek: None,
                    xy: None,
                })
                .at("lamp-1"),
                Error::Invalid,
            ),
            // A decibel action is the connection's, never a lamp's.
            (
                Some("light"),
                Request::action(TypedAction::SetVolumeDb { tenths: -345 }).at("lamp-1"),
                Error::Unsupported,
            ),
        ] {
            assert_eq!(
                admit(&manifest, kind, request.clone()).err(),
                Some(expected),
                "{kind:?} {request:?}"
            );
        }
        // And what it does allow.
        for request in [
            Request::status().at("lamp-1"),
            Request::command("toggle").at("lamp-1"),
            Request::command("dim:30").at("lamp-1"),
            Request::action(light).at("lamp-1"),
            Request::children(None),
            Request::children(Some("lamp-32".into())),
        ] {
            assert!(
                admit(&manifest, Some("light"), request.clone()).is_ok(),
                "{request:?}"
            );
        }
        // A listing is refused outright by a package that offers no children.
        let childless = super::tests::manifest(3);
        assert_eq!(
            admit(&childless, Some("light"), Request::children(None)),
            Err(Error::Unsupported)
        );
        assert_eq!(
            admit(&manifest, None, Request::children(Some("a//b".into()))),
            Err(Error::Invalid)
        );
    }

    /// What a package with children may answer.
    #[test]
    fn a_listing_is_only_accepted_when_every_child_is_one_of_its_own_kinds() {
        let manifest = bridge();
        let listed = |children: Vec<Child>, next: Option<String>| {
            accept(
                &manifest,
                &Request::children(None),
                &Response::Children { children, next },
            )
        };
        assert_eq!(
            listed(
                vec![
                    Child::new("lamp-1", "light", "Desk").with_light(LightTraits::default()),
                    Child::new("blind-1", "blind", "Study")
                        .with_cover(crate::CoverTraits::default()),
                ],
                None
            ),
            Ok(())
        );
        assert_eq!(
            listed(vec![Child::new("g", "ghost", "Ghost")], None),
            Err(Error::Protocol),
            "a kind the manifest never declared"
        );
        assert_eq!(
            listed(
                vec![
                    Child::new("lamp-1", "light", "Desk").with_cover(crate::CoverTraits::default())
                ],
                None
            ),
            Err(Error::Protocol),
            "a lamp with a blind's traits"
        );
        assert_eq!(
            listed(Vec::new(), Some("page-1".into())),
            Err(Error::Protocol),
            "nothing, but carry on"
        );

        // A write to a child may be acknowledged with its state; a write to
        // the connection itself may not.
        assert_eq!(
            accept(&manifest, &Request::command("on").at("lamp-1"), &lamp(true)),
            Ok(())
        );
        assert_eq!(
            accept(
                &manifest,
                &Request::action(TypedAction::SetLight {
                    on: Some(true),
                    brightness: None,
                    mirek: None,
                    xy: None
                })
                .at("lamp-1"),
                &lamp(true)
            ),
            Ok(())
        );
        assert_eq!(
            accept(&manifest, &Request::command("power-on"), &lamp(true)),
            Err(Error::Protocol)
        );
        assert_eq!(
            accept(
                &manifest,
                &Request::command("on").at("lamp-1"),
                &Response::Ok
            ),
            Ok(()),
            "a plain acknowledgement stays legal"
        );
    }

    /// A package that never said protocol 3 may not answer in it, however it
    /// was asked. The gate never asks it, so any of this is a broken package.
    #[test]
    fn a_listing_or_a_lamp_from_a_protocol_1_or_2_package_retires_it() {
        for version in [1, 2] {
            let manifest = manifest(version);
            for response in [
                Response::Children {
                    children: vec![Child::new("lamp-1", "light", "Desk")],
                    next: None,
                },
                lamp(true),
                Response::Status {
                    status: Status::default().with_cover(crate::CoverState::default()),
                },
                Response::Status {
                    status: Status::default().with_climate(crate::ClimateState::default()),
                },
            ] {
                assert_eq!(
                    accept(&manifest, &Request::status(), &response),
                    Err(Error::Protocol),
                    "protocol {version} {response:?}"
                );
            }
            // A status as the answer to a write, which only a request naming a
            // child may ever get.
            assert_eq!(
                accept(
                    &manifest,
                    &Request::command("power-on"),
                    &Response::Status {
                        status: Status::on(true)
                    }
                ),
                Err(Error::Protocol),
                "protocol {version}"
            );
        }
    }
}
