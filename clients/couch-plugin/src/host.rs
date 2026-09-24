use crate::{
    protocol::{Envelope, ReplyEnvelope},
    read_frame, write_frame, Error, Failure, Manifest, Request, Response, Result,
    MAX_CAMERA_SECONDS, MAX_SNAPSHOT_BYTES, MAX_SNAPSHOT_CHUNK_BASE64, MAX_SNAPSHOT_CHUNK_BYTES,
    NEXT_PROTOCOL_VERSION,
};
use base64::Engine;
use couch_sdk::{
    children::valid_cursor,
    couch_model::{commands::Function, valid_resource, ChildComponent, PluginChildKind},
    pairing::valid_session,
    ActionKind, Credential, KeyPhase, PairInput, PairPrompt, PairStep, PluginActionSchema,
    TypedAction, CAMERA_PROTOCOL_VERSION, MAX_PAGE,
};
use std::{
    collections::HashSet,
    io::{Read, Write},
    os::{
        fd::{AsRawFd, OwnedFd},
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

/// The pairing conversation one host has in flight, if it has one.
///
/// A host owns at most one, because a package owns at most one: the daemon
/// runs pairing in a child of its own so the connection that is already paired
/// keeps serving on its old key. It is what makes "the session the host holds"
/// a thing the gate can check, and it carries the last prompt so a code the
/// person typed is measured against the prompt that asked for it before any
/// I/O.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PairSession {
    session: String,
    prompt: Option<PairPrompt>,
}

pub struct Host {
    child: Child,
    stream: UnixStream,
    media: Option<UnixStream>,
    manifest: Manifest,
    timeout: Duration,
    next_id: u64,
    alive: bool,
    pairing: Option<PairSession>,
    camera: Option<CameraView>,
}

struct CameraView {
    resource: String,
    deadline: Instant,
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
        let (media, child_media): (Option<UnixStream>, Option<OwnedFd>) =
            if manifest.protocol_version >= CAMERA_PROTOCOL_VERSION {
                let (host, child) = UnixStream::pair()?;
                (Some(host), Some(child.into()))
            } else {
                (None, None)
            };
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
                if let Some(media) = child_media.as_ref() {
                    let source = media.as_raw_fd();
                    if source == 3 {
                        let flags = libc::fcntl(3, libc::F_GETFD);
                        if flags < 0 || libc::fcntl(3, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0
                        {
                            return Err(std::io::Error::last_os_error());
                        }
                    } else if libc::dup2(source, 3) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                }
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
        // In particular, close the parent's copy of the package media end so
        // fd 3 EOF means the package really closed or exited.
        drop(command);
        let mut host = Self {
            child,
            stream,
            media,
            manifest: manifest.clone(),
            timeout,
            next_id: 0,
            alive: true,
            pairing: None,
            camera: None,
        };
        match host.request(Request::Hello {
            protocol_version: manifest.protocol_version,
        })? {
            Response::Hello { manifest: actual } if actual == *manifest => (),
            _ => return Err(Error::Incompatible),
        }
        // A package that stores a key has to have closed its own `/proc` entry
        // first, which only it can do (`execve` puts the flag back). Checked
        // once the child is up and has answered, because that is the first
        // moment `serve` has run. Only root can read the answer: unprivileged
        // hosts - a developer's machine, CI, every host test - share the
        // running user with their children, where the ownership says nothing,
        // and skip it. Packages published before that SDK stay dumpable and
        // are unaffected: they declare no pairing.
        if manifest.pairing.is_some() && is_non_dumpable(host.pid()) == Some(false) {
            return Err(Error::Incompatible);
        }
        Ok(host)
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
        self.configure_with(settings, None)
    }
    /// [`Host::configure`] with the key Couch holds for this connection. The
    /// gate strips it for any package that may not be told one, so this is
    /// safe to call whatever the package is.
    pub fn configure_with(
        &mut self,
        settings: serde_json::Value,
        credential: Option<&Credential>,
    ) -> Result<()> {
        match self.request(Request::configure_with(settings, credential))? {
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
        self.request_child_full(kind, request)
            .map(|(response, _)| response)
    }
    /// [`Host::request_detailed`], keeping a key the device rotated under us.
    ///
    /// This is the only way a rotated key is surfaced: every other signature
    /// drops it, so no existing caller can store one by accident and no log
    /// line can carry one. The caller is the daemon, which writes it under the
    /// connection's lock before returning the body.
    pub fn request_full(
        &mut self,
        request: Request,
    ) -> std::result::Result<(Response, Option<Credential>), Failure> {
        self.request_child_full(None, request)
    }
    /// [`Host::request_full`] for a request that names a child.
    pub fn request_child_full(
        &mut self,
        kind: Option<&str>,
        request: Request,
    ) -> std::result::Result<(Response, Option<Credential>), Failure> {
        if !self.alive {
            return Err(Error::Transport.into());
        }
        let request = admit(&self.manifest, kind, self.pairing.as_ref(), request)?;
        match &request {
            Request::CameraOpen { .. } if self.camera.is_some() => return Err(Error::Busy.into()),
            Request::CameraOpen { .. } => {
                if let Err(error) = self.camera_channel_idle() {
                    self.terminate();
                    return Err(error.into());
                }
            }
            Request::CameraClose { resource }
                if self
                    .camera
                    .as_ref()
                    .is_none_or(|view| view.resource != *resource) =>
            {
                return Err(Error::Invalid.into());
            }
            _ => (),
        }
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
            let reply: ReplyEnvelope = read_frame(&mut stream)?;
            if reply.id != self.next_id {
                return Err(Error::Protocol);
            }
            accept_reply(&self.manifest, &request, &reply)?;
            Ok(reply)
        })();
        // Protocol/transport failures retire the stream: never consume a late
        // reply as the next request's response and never replay this request.
        if result.is_err() {
            self.terminate();
        }
        let reply = result?;
        self.follow(&request, &reply.body);
        match reply.body {
            Response::Error { code, reason } => Err(Failure { code, reason }),
            response => Ok((response, reply.store_credential)),
        }
    }

    /// Read one bounded Annex-B record from the protocol-4 media socket.
    /// `None` is the package's clean terminal record. A malformed record,
    /// closed descriptor or missed view deadline retires the package so bytes
    /// from one session can never be mistaken for the next.
    pub fn read_camera_record(&mut self) -> Result<Option<Vec<u8>>> {
        let deadline = self.camera.as_ref().ok_or(Error::Invalid)?.deadline;
        let result = {
            let media = self.media.as_mut().ok_or(Error::Unsupported)?;
            let deadline = core::cmp::min(deadline, Instant::now() + self.timeout);
            couch_sdk::camera::read_h264_record(&mut DeadlineStream {
                stream: media,
                deadline,
            })
        };
        match result {
            Ok(Some(record)) => Ok(Some(record)),
            Ok(None) => match self.camera_channel_idle() {
                Ok(()) => {
                    self.camera = None;
                    Ok(None)
                }
                Err(error) => {
                    self.terminate();
                    Err(error)
                }
            },
            Err(error) => {
                let error = match error {
                    couch_sdk::camera::CameraWireError::Protocol => Error::Protocol,
                    couch_sdk::camera::CameraWireError::Io(
                        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock,
                    ) => Error::Timeout,
                    couch_sdk::camera::CameraWireError::Io(_) => Error::Transport,
                };
                self.terminate();
                Err(error)
            }
        }
    }

    fn camera_channel_idle(&self) -> Result<()> {
        let media = self.media.as_ref().ok_or(Error::Unsupported)?;
        let mut byte = 0_u8;
        let read = unsafe {
            libc::recv(
                media.as_raw_fd(),
                (&mut byte as *mut u8).cast(),
                1,
                libc::MSG_PEEK | libc::MSG_DONTWAIT,
            )
        };
        if read > 0 {
            Err(Error::Protocol)
        } else if read == 0 {
            Err(Error::Transport)
        } else {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::WouldBlock {
                Ok(())
            } else {
                Err(error.into())
            }
        }
    }

    /// Keep the pairing conversation's state in step with what just happened.
    /// The reply has already been accepted, so a session here is one the gate
    /// agreed to.
    fn follow(&mut self, request: &Request, response: &Response) {
        match (request, response) {
            (
                Request::PairStart { .. } | Request::PairContinue { .. },
                Response::Pairing { session, step },
            ) => {
                if step.is_final() {
                    self.pairing = None;
                } else {
                    let prompt = match step {
                        PairStep::Waiting { prompt, .. } => Some(prompt.clone()),
                        _ => None,
                    };
                    self.pairing = Some(PairSession {
                        session: session.clone(),
                        prompt,
                    });
                }
            }
            // A refused start never opened one; a refused step leaves the one
            // in flight alone, so the browser can poll again.
            (Request::PairStart { .. }, _) | (Request::PairCancel { .. }, _) => {
                self.pairing = None;
            }
            (Request::CameraOpen { resource }, Response::CameraOpen { seconds, .. }) => {
                self.camera = Some(CameraView {
                    resource: resource.clone(),
                    deadline: Instant::now() + Duration::from_secs(u64::from(*seconds)),
                });
            }
            (Request::CameraClose { .. }, Response::Ok) => self.camera = None,
            _ => (),
        }
    }

    /// The pairing session this host is in the middle of, if any.
    pub fn pair_session(&self) -> Option<&str> {
        self.pairing.as_ref().map(|state| state.session.as_str())
    }
    /// Protocol 3: begin a pairing conversation. The session the package
    /// names comes back with the first step and is remembered here, so every
    /// step after it is checked against it before any I/O.
    pub fn pair_start(
        &mut self,
        settings: serde_json::Value,
        credential: Option<&Credential>,
    ) -> std::result::Result<(String, PairStep), Failure> {
        match self.request_detailed(Request::pair_start(settings, credential))? {
            Response::Pairing { session, step } => Ok((session, step)),
            _ => Err(Error::Protocol.into()),
        }
    }
    /// Protocol 3: the next step of the conversation this host holds.
    pub fn pair_continue(
        &mut self,
        input: Option<PairInput>,
    ) -> std::result::Result<PairStep, Failure> {
        let session = self
            .pair_session()
            .ok_or(Failure::from(Error::Invalid))?
            .to_owned();
        match self.request_detailed(Request::pair_continue(session, input))? {
            Response::Pairing { step, .. } => Ok(step),
            _ => Err(Error::Protocol.into()),
        }
    }
    /// Protocol 3: end it, storing nothing. Idempotent for the caller: with no
    /// session in flight there is nothing to cancel.
    pub fn pair_cancel(&mut self) -> std::result::Result<(), Failure> {
        let Some(session) = self.pair_session().map(str::to_owned) else {
            return Ok(());
        };
        match self.request_detailed(Request::pair_cancel(session))? {
            Response::Ok => Ok(()),
            _ => Err(Error::Protocol.into()),
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
            self.camera = None;
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
    pairing: Option<&PairSession>,
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
    // A key is stripped rather than refused, the same way and for the same
    // reason as a phase: a package rolled back from protocol 3 to 2 with a
    // credential file beside it is still configured, with today's bytes. This
    // is the only place a credential can leave the host, so it is also what
    // keeps `credential` out of a published package's frames.
    if let Request::Configure { credential, .. } = &mut request {
        if !manifest.pairs() {
            *credential = None;
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
        Request::Configure {
            settings,
            credential,
        } => {
            manifest.validate_settings(settings)?;
            // A key that does not fit cannot have come from a package: it is
            // the daemon's own file, and a request that would be refused is
            // better refused here than after a round trip.
            if credential.as_ref().is_some_and(|key| !key.fits()) {
                return Err(Error::Invalid);
            }
        }
        Request::Action { action, .. } => manifest.validate_action(*action)?,
        // Pairing. `requires` has already refused every package below protocol
        // 3; this is the rest of it.
        Request::PairStart {
            settings,
            credential,
        } => {
            if !manifest.pairs() {
                return Err(Error::Unsupported);
            }
            manifest.validate_settings(settings)?;
            if credential.as_ref().is_some_and(|key| !key.fits()) {
                return Err(Error::Invalid);
            }
        }
        Request::PairContinue { session, input } => {
            if !manifest.pairs() {
                return Err(Error::Unsupported);
            }
            let held = admit_session(pairing, session)?;
            match input {
                // A code is measured against the prompt that asked for it, so
                // a mistyped one costs no round trip and a prompt that asked
                // for nothing is never given anything.
                Some(input) => {
                    if !input.is_well_formed()
                        || !held.prompt.as_ref().is_some_and(|p| p.accepts(input))
                    {
                        return Err(Error::Invalid);
                    }
                }
                // Polling a prompt that is waiting for typed input would tell
                // the package nothing it does not already know.
                None => {
                    if held.prompt.as_ref().is_some_and(PairPrompt::is_code) {
                        return Err(Error::Invalid);
                    }
                }
            }
        }
        Request::PairCancel { session } => {
            if !manifest.pairs() {
                return Err(Error::Unsupported);
            }
            admit_session(pairing, session)?;
        }
        _ => (),
    }
    Ok(request)
}

/// The session a step names has to be the one this host is holding. A caller
/// that invents one, or that carries on after a conversation ended, is asking
/// about something that does not exist.
fn admit_session<'a>(pairing: Option<&'a PairSession>, session: &str) -> Result<&'a PairSession> {
    if !valid_session(session) {
        return Err(Error::Invalid);
    }
    match pairing {
        Some(held) if held.session == session => Ok(held),
        _ => Err(Error::Invalid),
    }
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
        Request::CameraSnapshot { offset, .. } => {
            if *offset as usize >= MAX_SNAPSHOT_BYTES {
                return Err(Error::Invalid);
            }
            if kind.component != ChildComponent::Camera {
                return Err(Error::Unsupported);
            }
        }
        Request::CameraOpen { .. } | Request::CameraClose { .. } => {
            if kind.component != ChildComponent::Camera {
                return Err(Error::Unsupported);
            }
        }
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

/// The gate, after I/O, over the whole reply: the body and the one thing a
/// package may say beside it. This is what the host calls; [`accept`] is the
/// body's half of it.
pub(crate) fn accept_reply(
    manifest: &Manifest,
    request: &Request,
    reply: &ReplyEnvelope,
) -> Result<()> {
    accept(manifest, request, &reply.body)?;
    accept_credential(manifest, request, reply.store_credential.as_ref())
}

/// A key the package says the device rotated.
///
/// Only a protocol 3 package that declares `pairing` may send one at all, only
/// on the answer to an ordinary request - never a handshake, a configure or a
/// pairing step, each of which has its own way of saying what it means - and
/// only within the limit. Everything else is a broken package.
fn accept_credential(
    manifest: &Manifest,
    request: &Request,
    credential: Option<&Credential>,
) -> Result<()> {
    let Some(credential) = credential else {
        return Ok(());
    };
    if !manifest.pairs() || !credential.fits() {
        return Err(Error::Protocol);
    }
    match request {
        Request::Command { .. }
        | Request::Action { .. }
        | Request::Status { .. }
        | Request::Inputs
        | Request::Children { .. }
        | Request::CameraSnapshot { .. }
        | Request::CameraOpen { .. }
        | Request::CameraClose { .. } => Ok(()),
        Request::Hello { .. }
        | Request::Configure { .. }
        | Request::PairStart { .. }
        | Request::PairContinue { .. }
        | Request::PairCancel { .. } => Err(Error::Protocol),
    }
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
            Response::Children { .. } | Response::Pairing { .. } => true,
            Response::Status { status } => status.is_child_state(),
            _ => false,
        }
    {
        return Err(Error::Protocol);
    }
    if version < CAMERA_PROTOCOL_VERSION
        && matches!(
            response,
            Response::CameraSnapshot { .. } | Response::CameraOpen { .. }
        )
    {
        return Err(Error::Protocol);
    }
    // A pairing step from a package that declared no pairing, and one whose
    // settings the manifest would not accept. The rest of a step's bounds are
    // in `validate_response`, which needs no manifest.
    if let Response::Pairing { step, .. } = response {
        if !manifest.pairs() {
            return Err(Error::Protocol);
        }
        if let PairStep::Done {
            settings: Some(settings),
            ..
        } = step
        {
            manifest
                .validate_settings(settings)
                .map_err(|_| Error::Protocol)?;
        }
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
    if matches!(
        request,
        Request::CameraSnapshot { .. } | Request::CameraOpen { .. } | Request::CameraClose { .. }
    ) {
        return CAMERA_PROTOCOL_VERSION;
    }
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
        // Pairing is protocol 3 entire. Whether the package also declared
        // `pairing` is asked next, in `admit`.
        Request::PairStart { .. } | Request::PairContinue { .. } | Request::PairCancel { .. } => {
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
        // Protocol 3, pairing. The session a step names has to be the one the
        // step belongs to: the package chooses it on the first step and has no
        // say after that. Everything a step carries with it is bounded by the
        // step itself; only `Done.settings` needs the manifest, and `accept`
        // asks it.
        (Request::PairStart { .. }, Response::Pairing { session, step })
            if valid_session(session) && step.is_well_formed() =>
        {
            Ok(())
        }
        (Request::PairContinue { session: asked, .. }, Response::Pairing { session, step })
            if session == asked && step.is_well_formed() =>
        {
            Ok(())
        }
        (Request::PairCancel { .. }, Response::Ok) => Ok(()),
        (
            Request::CameraSnapshot {
                offset: requested, ..
            },
            Response::CameraSnapshot {
                data,
                offset,
                total,
            },
        ) => {
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(data)
                .map_err(|_| Error::Protocol)?;
            let end = (*offset as usize)
                .checked_add(decoded.len())
                .ok_or(Error::Protocol)?;
            if offset != requested
                || !(4..=MAX_SNAPSHOT_BYTES).contains(&(*total as usize))
                || data.len() > MAX_SNAPSHOT_CHUNK_BASE64
                || decoded.is_empty()
                || decoded.len() > MAX_SNAPSHOT_CHUNK_BYTES
                || end > *total as usize
                || (*offset == 0 && !decoded.starts_with(&[0xff, 0xd8]))
                || (end == *total as usize && !decoded.ends_with(&[0xff, 0xd9]))
            {
                return Err(Error::Protocol);
            }
            Ok(())
        }
        (Request::CameraOpen { .. }, Response::CameraOpen { seconds, .. })
            if (1..=MAX_CAMERA_SECONDS).contains(seconds) =>
        {
            Ok(())
        }
        (Request::CameraClose { .. }, Response::Ok) => Ok(()),
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

enum PendingWork {
    Request {
        request: Request,
        /// The kind of child the request names, if it names one. Carried here
        /// rather than on the wire: the gate on the worker's side needs it,
        /// the package never does.
        kind: Option<String>,
    },
    /// Read one record from the camera side channel already opened on this
    /// exact child. A read never starts a replacement process: a replacement
    /// has no open view and bytes from it would belong to no request.
    CameraRecord,
}

enum PendingReply {
    Response(Response, Option<Credential>),
    CameraRecord(Option<Vec<u8>>),
}

struct Pending {
    work: PendingWork,
    queued: Instant,
    reply: SyncSender<std::result::Result<PendingReply, Failure>>,
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
        Self::start_paired(package_dir, manifest, settings, None, timeout, policy)
    }
    /// [`Endpoint::start_as`] for a connection Couch holds a key for. The key
    /// is configured into every child of this endpoint, including the
    /// replacement started after a failure, and is stripped by the gate for
    /// any package that may not be told one.
    pub fn start_paired(
        package_dir: &Path,
        manifest: Manifest,
        settings: serde_json::Value,
        credential: Option<&Credential>,
        timeout: Duration,
        policy: HostPolicy,
    ) -> Result<Self> {
        let package_dir: PathBuf = package_dir.into();
        let settings = manifest.with_defaults(settings)?;
        let credential = credential.cloned();
        let mut host = Host::spawn_with_policy(&package_dir, &manifest, STARTUP_TIMEOUT, policy)?;
        host.configure_with(settings.clone(), credential.as_ref())?;
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
                    let may_restart = matches!(&pending.work, PendingWork::Request { .. });
                    if !host.is_alive() && !may_restart {
                        let _ = pending.reply.send(Err(Error::Transport.into()));
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
                            h.configure_with(settings.clone(), credential.as_ref())?;
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
                    let result = match pending.work {
                        PendingWork::Request { request, kind } => host
                            .request_child_full(kind.as_deref(), request)
                            .map(|(response, credential)| {
                                PendingReply::Response(response, credential)
                            }),
                        PendingWork::CameraRecord => host
                            .read_camera_record()
                            .map(PendingReply::CameraRecord)
                            .map_err(Failure::from),
                    };
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
        self.request_child_full(kind, request)
            .map(|(response, _)| response)
    }
    /// [`Endpoint::request_detailed`], keeping a key the device rotated. See
    /// [`Host::request_full`]: every other signature drops it.
    pub fn request_full(
        &self,
        request: Request,
    ) -> std::result::Result<(Response, Option<Credential>), Failure> {
        self.request_child_full(None, request)
    }
    /// [`Endpoint::request_full`] for a request that names a child.
    pub fn request_child_full(
        &self,
        kind: Option<&str>,
        request: Request,
    ) -> std::result::Result<(Response, Option<Credential>), Failure> {
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
                work: PendingWork::Request {
                    request,
                    kind: kind.map(str::to_owned),
                },
                queued: Instant::now(),
                reply,
            }) {
            Ok(()) => (),
            Err(TrySendError::Full(_)) => return Err(Error::Busy.into()),
            Err(TrySendError::Disconnected(_)) => return Err(Error::Transport.into()),
        }
        match receiver
            .recv()
            .map_err(|_| Failure::from(Error::Transport))??
        {
            PendingReply::Response(response, credential) => Ok((response, credential)),
            PendingReply::CameraRecord(_) => Err(Error::Protocol.into()),
        }
    }

    /// Read one bounded record from the live camera view previously opened on
    /// this endpoint. Reads share the endpoint's single owner with control
    /// requests, so package stdout and the media descriptor can never be
    /// consumed by different host instances.
    pub fn read_camera_record(&self) -> std::result::Result<Option<Vec<u8>>, Failure> {
        let (reply, receiver) = mpsc::sync_channel(1);
        match self
            .inner
            .sender
            .as_ref()
            .ok_or(Failure::from(Error::Transport))?
            .try_send(Pending {
                work: PendingWork::CameraRecord,
                queued: Instant::now(),
                reply,
            }) {
            Ok(()) => (),
            Err(TrySendError::Full(_)) => return Err(Error::Busy.into()),
            Err(TrySendError::Disconnected(_)) => return Err(Error::Transport.into()),
        }
        match receiver
            .recv()
            .map_err(|_| Failure::from(Error::Transport))??
        {
            PendingReply::CameraRecord(record) => Ok(record),
            PendingReply::Response(_, _) => Err(Error::Protocol.into()),
        }
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

/// One live camera stream relayed by the local Couch daemon. The opening
/// response uses the ordinary bounded JSON frame; everything after it is the
/// protocol-4 H264 record codec. Dropping either handle closes the local
/// socket, which makes the daemon close the package view.
pub struct LocalCamera {
    stream: UnixStream,
    deadline: Instant,
}

pub struct LocalCameraInterrupt {
    stream: UnixStream,
}

impl LocalCameraInterrupt {
    pub fn cancel(&self) {
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
    }
}

impl LocalCamera {
    pub fn open(
        socket: &Path,
        connection_id: &str,
        resource: &str,
        timeout: Duration,
    ) -> std::result::Result<Self, Failure> {
        if connection_id.is_empty()
            || connection_id.len() > 128
            || connection_id.chars().any(char::is_control)
            || !valid_resource(resource)
            || timeout.is_zero()
        {
            return Err(Error::Invalid.into());
        }
        let request = Request::camera_open(resource);
        let mut stream = UnixStream::connect(socket).map_err(Error::from)?;
        let response = {
            let mut bounded = DeadlineStream {
                stream: &mut stream,
                deadline: Instant::now() + timeout,
            };
            write_frame(
                &mut bounded,
                &LocalRequest {
                    connection_id: connection_id.into(),
                    request: request.clone(),
                },
            )?;
            read_frame(&mut bounded)?
        };
        validate_response(&request, &response)?;
        match response {
            Response::CameraOpen {
                codec: crate::CameraCodec::H264AnnexB,
                seconds,
            } => Ok(Self {
                stream,
                deadline: Instant::now() + Duration::from_secs(u64::from(seconds)),
            }),
            Response::Error {
                reason: Some(reason),
                ..
            } if !reason.is_well_formed() => Err(Error::Protocol.into()),
            Response::Error { code, reason } => Err(Failure { code, reason }),
            _ => Err(Error::Protocol.into()),
        }
    }

    pub fn interrupter(&self) -> Result<LocalCameraInterrupt> {
        Ok(LocalCameraInterrupt {
            stream: self.stream.try_clone()?,
        })
    }

    pub fn next_record(&mut self) -> Result<Option<Vec<u8>>> {
        couch_sdk::camera::read_h264_record(&mut DeadlineStream {
            stream: &mut self.stream,
            deadline: self.deadline,
        })
        .map_err(|error| match error {
            couch_sdk::camera::CameraWireError::Protocol => Error::Protocol,
            couch_sdk::camera::CameraWireError::Io(
                std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock,
            ) => Error::Timeout,
            couch_sdk::camera::CameraWireError::Io(_) => Error::Transport,
        })
    }
}

impl Drop for LocalCamera {
    fn drop(&mut self) {
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
    }
}

#[cfg(test)]
mod tests {
    use super::{accept, admit};
    use crate::{
        CameraCodec, Capability, Child, ChildComponent, Error, LightState, LightTraits, Manifest,
        PluginActionSchema, PluginChildKind, Request, Response, Status, TypedAction,
        MAX_CAMERA_SECONDS, MAX_SNAPSHOT_BYTES, MAX_SNAPSHOT_CHUNK_BASE64,
        MAX_SNAPSHOT_CHUNK_BYTES,
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

    fn camera_bridge() -> Manifest {
        let mut manifest = manifest(4);
        manifest.children = vec![PluginChildKind {
            kind: "camera".into(),
            label: "Camera".into(),
            device_kind: DeviceKind::Camera,
            component: ChildComponent::Camera,
            capabilities: vec![],
            actions: vec![],
        }];
        manifest
    }

    #[test]
    fn protocol_4_camera_controls_are_bounded_and_remain_switched_off() {
        use base64::Engine;

        let camera = camera_bridge();
        let open = Request::camera_open("front-yard");
        let close = Request::camera_close("front-yard");
        let snapshot = Request::camera_snapshot("front-yard", 0);
        for request in [&open, &close, &snapshot] {
            assert_eq!(super::requires(request), 4);
            assert_eq!(
                admit(&manifest(3), Some("camera"), None, request.clone()),
                Err(Error::Unsupported),
                "the shipping protocol-3 host performs no I/O"
            );
            assert_eq!(
                admit(&camera, Some("camera"), None, request.clone()),
                Ok(request.clone())
            );
        }
        assert_eq!(
            admit(
                &camera,
                Some("camera"),
                None,
                Request::camera_snapshot("front-yard", MAX_SNAPSHOT_BYTES as u32)
            ),
            Err(Error::Invalid)
        );
        assert_eq!(
            admit(&bridge(), Some("light"), None, open.clone()),
            Err(Error::Unsupported)
        );

        let jpeg = [0xff, 0xd8, 1, 2, 0xff, 0xd9];
        let data = base64::engine::general_purpose::STANDARD.encode(jpeg);
        assert_eq!(
            accept(
                &camera,
                &snapshot,
                &Response::CameraSnapshot {
                    data: data.clone(),
                    offset: 0,
                    total: jpeg.len() as u32,
                }
            ),
            Ok(())
        );
        assert_eq!(
            accept(
                &camera,
                &open,
                &Response::CameraOpen {
                    codec: CameraCodec::H264AnnexB,
                    seconds: MAX_CAMERA_SECONDS,
                }
            ),
            Ok(())
        );
        assert_eq!(accept(&camera, &close, &Response::Ok), Ok(()));

        let mut largest = vec![0; MAX_SNAPSHOT_CHUNK_BYTES];
        largest[..2].copy_from_slice(&[0xff, 0xd8]);
        largest[MAX_SNAPSHOT_CHUNK_BYTES - 2..].copy_from_slice(&[0xff, 0xd9]);
        let largest = base64::engine::general_purpose::STANDARD.encode(largest);
        assert_eq!(largest.len(), MAX_SNAPSHOT_CHUNK_BASE64);
        let largest_response = Response::CameraSnapshot {
            data: largest,
            offset: 0,
            total: MAX_SNAPSHOT_CHUNK_BYTES as u32,
        };
        assert_eq!(accept(&camera, &snapshot, &largest_response), Ok(()));
        assert!(crate::write_frame(&mut Vec::new(), &largest_response).is_ok());

        for response in [
            Response::CameraSnapshot {
                data: "not base64".into(),
                offset: 0,
                total: jpeg.len() as u32,
            },
            Response::CameraSnapshot {
                data: data.clone(),
                offset: 1,
                total: jpeg.len() as u32 + 1,
            },
            Response::CameraSnapshot {
                data,
                offset: 0,
                total: MAX_SNAPSHOT_BYTES as u32 + 1,
            },
        ] {
            assert_eq!(accept(&camera, &snapshot, &response), Err(Error::Protocol));
        }
        assert_eq!(
            accept(
                &camera,
                &open,
                &Response::CameraOpen {
                    codec: CameraCodec::H264AnnexB,
                    seconds: 0,
                }
            ),
            Err(Error::Protocol)
        );
        assert_eq!(
            accept(
                &manifest(3),
                &snapshot,
                &Response::CameraSnapshot {
                    data: base64::engine::general_purpose::STANDARD.encode(jpeg),
                    offset: 0,
                    total: jpeg.len() as u32,
                }
            ),
            Err(Error::Protocol),
            "a protocol-3 package cannot answer with protocol-4 vocabulary"
        );
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
        let sent = |kind, request| admit(&manifest, Some(kind), None, request);
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
            admit(&dimmable, None, None, Request::command("dim:30")),
            Ok(Request::command("dim:30"))
        );
        assert_eq!(
            admit(&manifest, None, None, Request::command("dim:30")),
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
                admit(&manifest, kind, None, request.clone()).err(),
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
                admit(&manifest, Some("light"), None, request.clone()).is_ok(),
                "{request:?}"
            );
        }
        // A listing is refused outright by a package that offers no children.
        let childless = super::tests::manifest(3);
        assert_eq!(
            admit(&childless, Some("light"), None, Request::children(None)),
            Err(Error::Unsupported)
        );
        assert_eq!(
            admit(
                &manifest,
                None,
                None,
                Request::children(Some("a//b".into()))
            ),
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
