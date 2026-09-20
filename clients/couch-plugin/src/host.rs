use crate::{
    protocol::Envelope, read_frame, write_frame, Error, Failure, Manifest, Request, Response,
    Result, NEXT_PROTOCOL_VERSION,
};
use couch_sdk::{couch_model::commands::Function, KeyPhase};
use std::{
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
/// an integration can still reach the LAN, and it can still see that other
/// processes exist.
///
/// The default is the one every package shared before each installed package
/// had a user of its own. It is still what an out-of-tree caller and every
/// test gets; the daemon and the package store hand [`HostPolicy::for_package`]
/// the identity the store allocated for that package.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
    /// One installed package's own user and group, with exactly the
    /// supplementary groups the shared default had: nothing a package could do
    /// before becomes impossible because it stopped being 65534.
    ///
    /// The identity itself comes from the package store, which allocates it
    /// once per package id and never reuses it. Nothing on disk is owned by
    /// these ids, so an older Couch that knows nothing about them still runs
    /// every package.
    pub const fn for_package(uid: u32, gid: u32) -> Self {
        Self {
            uid,
            gid,
            supplementary_gids: DEFAULT_SUPPLEMENTARY_GIDS,
        }
    }
    /// The complete supplementary set applied while dropping privileges.
    ///
    /// Couch's ARMv7 musl target carries Android's `AID_INET` (3003) so an
    /// unprivileged plugin can create ordinary Internet sockets on the HA100.
    /// Other targets receive no supplementary groups.
    pub const fn supplementary_gids(self) -> &'static [libc::gid_t] {
        self.supplementary_gids
    }
}

/// Whether a child has made itself undumpable, as the kernel reports it:
/// `/proc/<pid>/environ` belongs to root rather than to the process's own user.
///
/// `None` when that cannot be read as an answer - anywhere but Linux, or when
/// this process is not root, where a child shares this process's user and the
/// ownership says nothing. Nothing decides anything on this yet; it is how the
/// host will check a package that stores a pairing key.
pub fn is_non_dumpable(pid: u32) -> Option<bool> {
    #[cfg(target_os = "linux")]
    {
        if unsafe { libc::geteuid() } != 0 {
            return None;
        }
        let owner = std::fs::metadata(format!("/proc/{pid}/environ"))
            .ok()?
            .uid();
        Some(owner == 0)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        None
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
                // No core file, ever: a crashing package would otherwise write
                // whatever it held in memory to a file under its own user.
                // Lowering a limit needs no privilege, so this is done before
                // the drop and survives execve.
                let no_core = libc::rlimit {
                    rlim_cur: 0,
                    rlim_max: 0,
                };
                if libc::setrlimit(libc::RLIMIT_CORE, &no_core) != 0 {
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
        match self.request(Request::Action { action })? {
            Response::Ok => Ok(()),
            _ => Err(Error::Protocol),
        }
    }
    pub fn status(&mut self) -> Result<couch_sdk::Status> {
        match self.request(Request::Status)? {
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
        if !self.alive {
            return Err(Error::Transport.into());
        }
        let request = admit(&self.manifest, request)?;
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
pub(crate) fn admit(manifest: &Manifest, mut request: Request) -> Result<Request> {
    let version = manifest.protocol_version;
    // A phase the package cannot read is dropped rather than refused: the key
    // still works, as a tap, and the bytes are the ones a protocol 1 or 2
    // package has always been sent.
    if let Request::Command { phase, .. } = &mut request {
        if version < NEXT_PROTOCOL_VERSION {
            *phase = KeyPhase::Tap;
        }
    }
    if requires(&request) > version {
        return Err(Error::Unsupported);
    }
    match &request {
        Request::Command { function, .. } if !manifest.supports(function) => {
            return Err(Error::Unsupported)
        }
        Request::Inputs if !manifest.supports_inputs => return Err(Error::Unsupported),
        Request::Configure { settings } => manifest.validate_settings(settings)?,
        Request::Action { action } => manifest.validate_action(*action)?,
        _ => (),
    }
    Ok(request)
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
    match request {
        Request::Action { .. } => 2,
        Request::Command { function, .. }
            if matches!(Function::parse(function), Some(Function::Custom(_))) =>
        {
            NEXT_PROTOCOL_VERSION
        }
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
        (Request::Status, Response::Status { status })
            if status.volume.is_none_or(|v| v <= 100)
                && status.volume_db.is_none_or(|v| v.is_valid())
                && [&status.input, &status.title].iter().all(|s| {
                    s.as_ref()
                        .is_none_or(|v| v.len() <= 4096 && !v.chars().any(char::is_control))
                }) =>
        {
            Ok(())
        }
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
        _ => Err(Error::Protocol),
    }
}

struct Pending {
    request: Request,
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
        Self::start_as(
            package_dir,
            manifest,
            settings,
            timeout,
            HostPolicy::default(),
        )
    }
    /// The endpoint an installed package gets: every child of it, including
    /// the replacement started after a failure, runs under this policy.
    pub fn start_as(
        package_dir: &Path,
        manifest: Manifest,
        settings: serde_json::Value,
        timeout: Duration,
        policy: HostPolicy,
    ) -> Result<Self> {
        let package_dir: PathBuf = package_dir.into();
        let settings = manifest.with_defaults(settings)?;
        let mut host = Host::spawn_with_policy(&package_dir, &manifest, STARTUP_TIMEOUT, policy)?;
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
                        let replacement = Host::spawn_with_policy(
                            &package_dir,
                            &manifest,
                            STARTUP_TIMEOUT,
                            policy,
                        )
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
                    let result = host.request_detailed(pending.request);
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
