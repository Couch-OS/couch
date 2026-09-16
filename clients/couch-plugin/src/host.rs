use crate::{
    protocol::Envelope, read_frame, write_frame, Error, Manifest, Request, Response, Result,
    PROTOCOL_VERSION,
};
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

/// The production policy drops root before exec on Linux. Unprivileged host
/// development inherits its uid. This is privilege separation, not a sandbox:
/// integrations still share a uid and can access the LAN.
#[derive(Clone, Copy, Debug)]
pub struct HostPolicy {
    pub uid: u32,
    pub gid: u32,
}
impl Default for HostPolicy {
    fn default() -> Self {
        Self {
            uid: 65534,
            gid: 65534,
        }
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
                        if libc::setgroups(0, std::ptr::null()) != 0
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
            protocol_version: PROTOCOL_VERSION,
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
        match self.request(Request::Command {
            function: function.into(),
        })? {
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
    pub fn request(&mut self, request: Request) -> Result<Response> {
        if !self.alive {
            return Err(Error::Transport);
        }
        match &request {
            Request::Command { function } if !self.manifest.supports(function) => {
                return Err(Error::Unsupported)
            }
            Request::Inputs if !self.manifest.supports_inputs => return Err(Error::Unsupported),
            Request::Configure { settings } => self.manifest.validate_settings(settings)?,
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
            let response: Envelope<Response> = read_frame(&mut stream)?;
            if response.id != self.next_id {
                return Err(Error::Protocol);
            }
            validate_response(&request, &response.body)?;
            Ok(response.body)
        })();
        // Protocol/transport failures retire the stream: never consume a late
        // reply as the next request's response and never replay this request.
        if result.is_err() {
            self.terminate();
        }
        match result? {
            Response::Error { code } => Err(code),
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

fn validate_response(request: &Request, response: &Response) -> Result<()> {
    match (request, response) {
        (_, Response::Error { .. }) => Ok(()),
        (Request::Hello { .. }, Response::Hello { manifest }) => manifest.validate(),
        (Request::Configure { .. } | Request::Command { .. }, Response::Ok) => Ok(()),
        (Request::Status, Response::Status { status })
            if status.volume.is_none_or(|v| v <= 100)
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
                    !i.id.is_empty()
                        && i.id.len() <= 128
                        && !i.id.chars().any(char::is_control)
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
    reply: SyncSender<Result<Response>>,
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
                        let _ = pending.reply.send(Err(Error::Expired));
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
                                let _ = pending.reply.send(Err(error));
                                continue;
                            }
                        }
                        if pending.queued.elapsed() >= QUEUE_TTL {
                            let _ = pending.reply.send(Err(Error::Expired));
                            continue;
                        }
                    }
                    let result = host.request(pending.request);
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
        // Endpoint identity/settings remain fixed for the lifetime of its owner.
        if matches!(request, Request::Hello { .. } | Request::Configure { .. }) {
            return Err(Error::Unsupported);
        }
        let (reply, receiver) = mpsc::sync_channel(1);
        match self
            .inner
            .sender
            .as_ref()
            .ok_or(Error::Transport)?
            .try_send(Pending {
                request,
                queued: Instant::now(),
                reply,
            }) {
            Ok(()) => (),
            Err(TrySendError::Full(_)) => return Err(Error::Busy),
            Err(TrySendError::Disconnected(_)) => return Err(Error::Transport),
        }
        receiver.recv().map_err(|_| Error::Transport)?
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
    if connection_id.is_empty()
        || connection_id.len() > 128
        || connection_id.chars().any(char::is_control)
        || timeout.is_zero()
    {
        return Err(Error::Invalid);
    }
    let mut stream = UnixStream::connect(socket)?;
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
        Response::Error { code } => Err(code),
        response => Ok(response),
    }
}
