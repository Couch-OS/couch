use crate::{
    protocol::{Envelope, ReplyEnvelope},
    read_frame, write_frame, CameraCodec, Error, Failure, Manifest, Request, Response, Result,
    MAX_CAMERA_SECONDS, MAX_SNAPSHOT_BYTES, MAX_SNAPSHOT_CHUNK_BYTES, NEXT_PROTOCOL_VERSION,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use couch_sdk::{ClientSettings, Credential, DeviceClient, KeyPhase, PairFlow};
use std::{
    fs::File,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread::JoinHandle,
};

struct SnapshotCache {
    resource: String,
    bytes: Vec<u8>,
}

struct RunningCamera {
    resource: String,
    cancel: Arc<dyn Fn() + Send + Sync>,
    closing: Arc<AtomicBool>,
    worker: Option<JoinHandle<Result<()>>>,
}

impl RunningCamera {
    fn close(mut self) -> Result<()> {
        self.closing.store(true, Ordering::Release);
        (self.cancel)();
        self.join()
    }

    fn join(&mut self) -> Result<()> {
        self.worker
            .take()
            .ok_or(Error::Protocol)?
            .join()
            .map_err(|_| Error::Transport)?
    }
}

impl Drop for RunningCamera {
    fn drop(&mut self) {
        self.closing.store(true, Ordering::Release);
        (self.cancel)();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// Run an SDK integration over stdin/stdout. Configure validates settings and
/// clears prior state; the first actual device operation connects lazily.
///
/// What goes on the wire follows the manifest's protocol version, not the SDK
/// the package was built with. A protocol 1 or 2 package built with this SDK
/// sends the bytes it always sent: a reason its client attaches is dropped,
/// `Unpaired` leaves as `rejected`, and its client is only ever told of taps.
/// Protocol 3 adds the detailed failures, key phases, children, and pairing
/// frames described below while preserving the older wire shapes.
pub fn serve<C: DeviceClient>(manifest: Manifest) -> Result<()> {
    hide_from_other_users();
    manifest.validate()?;
    let expected: Vec<_> = C::capabilities()
        .iter()
        .map(|(id, label)| crate::Capability {
            id: (*id).into(),
            label: (*label).into(),
        })
        .collect();
    if manifest.id != C::KIND
        || manifest.capabilities != expected
        || manifest.actions != C::actions()
        // Protocol 3: the kinds of child the manifest declares and the ones
        // this client answers for have to be the same list, for the same
        // reason the capabilities do.
        || manifest.children != C::child_kinds()
    {
        return Err(Error::Invalid);
    }
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut input = stdin.lock();
    let mut output = stdout.lock();
    let mut hello = false;
    let mut settings: Option<C::Settings> = None;
    let mut credential: Option<Credential> = None;
    let mut client: Option<C> = None;
    let explains = manifest.protocol_version >= NEXT_PROTOCOL_VERSION;
    // Only a package that declared pairing takes a key, starts a flow, or ever
    // writes one back. The host refuses all three from anyone else, so this is
    // the child's half of the same rule.
    let pairs = manifest.pairs();
    // At most one conversation, numbered so a stale step from a dialog that
    // was already replaced names a session this package no longer has.
    let mut flow: Option<(String, Box<dyn PairFlow>)> = None;
    let mut snapshot: Option<SnapshotCache> = None;
    let mut camera: Option<RunningCamera> = None;
    let mut sessions: u64 = 0;
    let mut last_id = 0;
    loop {
        let envelope: Envelope<Request> = read_frame(&mut input)?;
        if envelope.id <= last_id {
            return Err(Error::Protocol);
        }
        last_id = envelope.id;
        // A key the device rotated, taken after the request it was discovered
        // by and never on a handshake, a configure or a pairing step.
        let mut rotated: Option<Credential> = None;
        let ordinary = !matches!(
            envelope.body,
            Request::Hello { .. } | Request::Configure { .. }
        ) && !envelope.body.is_pairing();
        let response = (|| -> std::result::Result<Response, Failure> {
            match envelope.body {
                Request::Hello { protocol_version } => {
                    if hello || protocol_version != manifest.protocol_version {
                        return Err(Error::Incompatible.into());
                    }
                    hello = true;
                    Ok(Response::Hello {
                        manifest: manifest.clone(),
                    })
                }
                _ if !hello => Err(Error::Incompatible.into()),
                Request::Configure {
                    settings: value,
                    credential: key,
                } => {
                    if let Some(running) = camera.take() {
                        running.close()?;
                    }
                    snapshot = None;
                    if key.is_some() && !pairs {
                        return Err(Error::Unsupported.into());
                    }
                    let value = manifest.with_defaults(value)?;
                    let parsed: C::Settings =
                        serde_json::from_value(value).map_err(|_| Error::Invalid)?;
                    parsed.validate()?;
                    settings = Some(parsed);
                    credential = key;
                    client = None;
                    Ok(Response::Ok)
                }
                // Pairing needs a handshake and nothing else: a connection
                // becomes usable by being paired, so there may be no settings
                // yet and never a client.
                Request::PairStart {
                    settings: value,
                    credential: existing,
                } => {
                    if !pairs {
                        return Err(Error::Unsupported.into());
                    }
                    let value = manifest.with_defaults(value)?;
                    let parsed: C::Settings =
                        serde_json::from_value(value).map_err(|_| Error::Invalid)?;
                    parsed.validate()?;
                    // A second start replaces the first, whatever it was
                    // doing: the host only ever has one dialog open.
                    if let Some((_, mut previous)) = flow.take() {
                        previous.cancel();
                    }
                    let mut started = C::pair_start(&parsed, existing.as_ref())?;
                    let step = started.step(None)?;
                    sessions += 1;
                    let session = format!("p{sessions}");
                    if !step.is_final() {
                        flow = Some((session.clone(), started));
                    }
                    Ok(Response::Pairing { session, step })
                }
                Request::PairContinue { session, input } => {
                    if !pairs {
                        return Err(Error::Unsupported.into());
                    }
                    // A step for a conversation this package is not having.
                    if flow.as_ref().is_none_or(|(held, _)| *held != session) {
                        return Err(Error::Invalid.into());
                    }
                    let (_, running) = flow.as_mut().ok_or(Error::Invalid)?;
                    let step = running.step(input)?;
                    if step.is_final() {
                        flow = None;
                    }
                    Ok(Response::Pairing { session, step })
                }
                Request::PairCancel { session } => {
                    if !pairs {
                        return Err(Error::Unsupported.into());
                    }
                    if flow.as_ref().is_none_or(|(held, _)| *held != session) {
                        return Err(Error::Invalid.into());
                    }
                    if let Some((_, mut cancelled)) = flow.take() {
                        cancelled.cancel();
                    }
                    Ok(Response::Ok)
                }
                request => {
                    // A child of this connection is protocol 3 vocabulary. A
                    // package built with this SDK but serving an older
                    // manifest never answers to one, whoever asks.
                    let child = request.resource().map(str::to_owned);
                    if !explains && (child.is_some() || matches!(request, Request::Children { .. }))
                    {
                        return Err(Error::Unsupported.into());
                    }
                    if child
                        .as_deref()
                        .is_some_and(|id| !couch_sdk::couch_model::valid_resource(id))
                    {
                        return Err(Error::Invalid.into());
                    }
                    // Refuse undeclared input/commands before opening a socket.
                    // A button the package names itself (`x:`) passes only
                    // if this manifest declares it, like any other. A child's
                    // command is declared by its kind, which the host has
                    // already checked; here it only has to be a function.
                    let mut function = None;
                    if let Request::Command { function: id, .. } = &request {
                        let parsed = couch_sdk::couch_model::commands::Function::parse(id)
                            .ok_or(Error::Unsupported)?;
                        if child.is_none() && (!manifest.supports(id) || !C::supports(&parsed)) {
                            return Err(Error::Unsupported.into());
                        }
                        function = Some(parsed);
                    }
                    if let Request::Action { action, .. } = &request {
                        if child.is_none() {
                            manifest.validate_action(*action)?;
                            C::validate_action(*action)?;
                        } else if !action.is_valid() {
                            return Err(Error::Invalid.into());
                        }
                    }
                    if matches!(request, Request::Inputs) && !manifest.supports_inputs {
                        return Err(Error::Unsupported.into());
                    }
                    if matches!(request, Request::Children { .. }) && manifest.children.is_empty() {
                        return Err(Error::Unsupported.into());
                    }
                    if manifest.protocol_version < couch_sdk::CAMERA_PROTOCOL_VERSION
                        && matches!(
                            request,
                            Request::CameraSnapshot { .. }
                                | Request::CameraOpen { .. }
                                | Request::CameraClose { .. }
                        )
                    {
                        return Err(Error::Unsupported.into());
                    }
                    if client.is_none() {
                        client = Some(C::connect_with(
                            settings.as_ref().ok_or(Error::Invalid)?,
                            credential.as_ref(),
                        )?);
                    }
                    let client_ref = client.as_mut().ok_or(Error::Transport)?;
                    let shape = |status: couch_sdk::Status| shaped(&manifest, status);
                    let result = match (request, child) {
                        (
                            Request::Command {
                                function: id,
                                phase,
                                ..
                            },
                            None,
                        ) => {
                            // The host never sends an older package a phase.
                            let phase = if explains { phase } else { KeyPhase::Tap };
                            client_ref.command_phased(&id, phase).map(|()| Response::Ok)
                        }
                        (Request::Command { phase, .. }, Some(resource)) => client_ref
                            .child_command(
                                &resource,
                                function.as_ref().ok_or(couch_sdk::Error::Unsupported)?,
                                phase,
                            )
                            .map(|status| answered(status.map(shape))),
                        (Request::Action { action, .. }, None) => {
                            client_ref.action(action).map(|()| Response::Ok)
                        }
                        (Request::Action { action, .. }, Some(resource)) => client_ref
                            .child_action(&resource, action)
                            .map(|status| answered(status.map(shape))),
                        (Request::Status { .. }, None) => {
                            client_ref.status().map(|status| Response::Status {
                                status: shape(status),
                            })
                        }
                        (Request::Status { .. }, Some(resource)) => client_ref
                            .child_status(&resource)
                            .map(|status| Response::Status {
                                status: shape(status),
                            }),
                        (Request::Inputs, _) => client_ref
                            .inputs()
                            .map(|inputs| Response::Inputs { inputs }),
                        (Request::Children { cursor }, _) => client_ref
                            .children(cursor.as_deref())
                            .map(|page| Response::Children {
                                children: page.children,
                                next: page.next,
                            }),
                        (Request::CameraSnapshot { resource, offset }, Some(_)) => {
                            camera_snapshot(client_ref, &mut snapshot, resource, offset)
                        }
                        (Request::CameraOpen { resource }, Some(_)) => {
                            if camera.is_some() {
                                Err(couch_sdk::Error::Invalid)
                            } else {
                                let view = client_ref.camera_open(&resource)?;
                                let (stream, cancel, seconds) = view.into_parts();
                                if seconds > MAX_CAMERA_SECONDS {
                                    Err(couch_sdk::Error::Invalid)
                                } else {
                                    let file = camera_file().map_err(couch_sdk::Error::from)?;
                                    camera = Some(start_camera(resource, stream, cancel, file));
                                    Ok(Response::CameraOpen {
                                        codec: CameraCodec::H264AnnexB,
                                        seconds,
                                    })
                                }
                            }
                        }
                        (Request::CameraClose { resource }, Some(_)) => {
                            if camera.as_ref().is_none_or(|open| open.resource != resource) {
                                Err(couch_sdk::Error::Invalid)
                            } else {
                                let running = camera.take().ok_or(couch_sdk::Error::Invalid)?;
                                running.close().map_err(|error| match error {
                                    Error::Protocol => couch_sdk::Error::Protocol,
                                    Error::Timeout => couch_sdk::Error::Timeout,
                                    _ => couch_sdk::Error::Transport,
                                })?;
                                Ok(Response::Ok)
                            }
                        }
                        _ => unreachable!(),
                    };
                    if result.as_ref().is_err_and(|error| {
                        matches!(
                            error.code(),
                            couch_sdk::Error::Transport
                                | couch_sdk::Error::Timeout
                                | couch_sdk::Error::Protocol
                        )
                    }) {
                        client = None;
                    }
                    result.map_err(Into::into)
                }
            }
        })()
        .unwrap_or_else(|failure| refusal(&manifest, failure));
        // A key the device rotated under us, asked for once the request it
        // was discovered by is answered. Only on an ordinary request, and only
        // from a package that declared pairing: the host refuses it anywhere
        // else and would retire this child for sending it.
        if pairs && ordinary {
            if let Some(client) = client.as_mut() {
                rotated = client.take_credential().filter(Credential::fits);
                if rotated.is_some() {
                    // A rotation replaces the key this child was configured
                    // with, so the next connect uses it whether or not the
                    // daemon managed to write it.
                    credential = rotated.clone();
                }
            }
        }
        write_frame(
            &mut output,
            &ReplyEnvelope {
                id: envelope.id,
                body: response,
                store_credential: rotated,
            },
        )?;
    }
}

fn camera_snapshot<C: DeviceClient>(
    client: &mut C,
    cache: &mut Option<SnapshotCache>,
    resource: String,
    offset: u32,
) -> couch_sdk::Result<Response> {
    if offset == 0 {
        let bytes = client.camera_snapshot(&resource)?;
        if !(4..=MAX_SNAPSHOT_BYTES).contains(&bytes.len())
            || !bytes.starts_with(&[0xff, 0xd8])
            || !bytes.ends_with(&[0xff, 0xd9])
        {
            return Err(couch_sdk::Error::Protocol);
        }
        *cache = Some(SnapshotCache {
            resource: resource.clone(),
            bytes,
        });
    }
    let held = cache.as_ref().ok_or(couch_sdk::Error::Invalid)?;
    let offset = offset as usize;
    if held.resource != resource || offset >= held.bytes.len() {
        return Err(couch_sdk::Error::Invalid);
    }
    let end = offset
        .saturating_add(MAX_SNAPSHOT_CHUNK_BYTES)
        .min(held.bytes.len());
    Ok(Response::CameraSnapshot {
        data: STANDARD.encode(&held.bytes[offset..end]),
        offset: offset as u32,
        total: held.bytes.len() as u32,
    })
}

fn start_camera(
    resource: String,
    mut stream: Box<dyn couch_sdk::CameraStream>,
    cancel: Arc<dyn Fn() + Send + Sync>,
    mut file: File,
) -> RunningCamera {
    let closing = Arc::new(AtomicBool::new(false));
    let worker_closing = closing.clone();
    let worker = std::thread::spawn(move || {
        let result = loop {
            if worker_closing.load(Ordering::Acquire) {
                break Ok(());
            }
            match stream.next_h264() {
                Ok(bytes) => {
                    if let Err(error) = couch_sdk::write_h264_record(&mut file, &bytes) {
                        break Err(camera_wire_error(error));
                    }
                }
                Err(_) if worker_closing.load(Ordering::Acquire) => break Ok(()),
                Err(error) => break Err(error.into()),
            }
        };
        match couch_sdk::write_camera_end(&mut file) {
            Ok(()) => result,
            Err(error) => Err(camera_wire_error(error)),
        }
    });
    RunningCamera {
        resource,
        cancel,
        closing,
        worker: Some(worker),
    }
}

fn camera_wire_error(error: couch_sdk::CameraWireError) -> Error {
    match error {
        couch_sdk::CameraWireError::Io(_) => Error::Transport,
        couch_sdk::CameraWireError::Protocol => Error::Protocol,
    }
}

#[cfg(unix)]
fn camera_file() -> std::io::Result<File> {
    use std::os::fd::FromRawFd;
    let fd = unsafe { libc::dup(3) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

#[cfg(not(unix))]
fn camera_file() -> std::io::Result<File> {
    Err(std::io::ErrorKind::Unsupported.into())
}

/// Give up being dumpable, which hands `/proc/<pid>` to root and closes the
/// last way one package's user could read another's memory, open file list or
/// environment.
///
/// It has to happen here, in the child, and not in the host's `pre_exec`:
/// `execve` puts the flag back to 1 for a program the new user can read, which
/// every package slot is. Each installed package has its own user, so this is
/// what makes that separation real rather than nominal.
///
/// Best effort, and deliberately not an error: a package that cannot set it is
/// still a working package, and a package whose stored key must be protected
/// is checked by the host instead. Packages published before this SDK do not
/// call it at all.
fn hide_from_other_users() {
    #[cfg(target_os = "linux")]
    unsafe {
        libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0);
    }
}

/// A status, as a package of this manifest's protocol may report it: a
/// protocol 1 adapter never mentions decibels, and only protocol 3 knows what
/// a lamp, a blind or a thermostat is.
fn shaped(manifest: &Manifest, mut status: couch_sdk::Status) -> couch_sdk::Status {
    if manifest.protocol_version == 1 {
        status.volume_db = None;
    }
    if manifest.protocol_version < NEXT_PROTOCOL_VERSION {
        status.light = None;
        status.cover = None;
        status.climate = None;
    }
    status
}

/// A write that was acknowledged with the child's state, or plainly.
fn answered(status: Option<couch_sdk::Status>) -> Response {
    match status {
        Some(status) => Response::Status { status },
        None => Response::Ok,
    }
}

/// The error a package of this manifest's protocol puts on the wire.
pub(crate) fn refusal(manifest: &Manifest, mut failure: Failure) -> Response {
    if manifest.protocol_version < NEXT_PROTOCOL_VERSION {
        // Exactly the error a protocol 1 or 2 package has always sent.
        failure.reason = None;
        if failure.code == Error::Unpaired {
            failure.code = Error::Rejected;
        }
    } else if failure
        .reason
        .as_ref()
        .is_some_and(|reason| !manifest.accepts_reason(reason))
    {
        // The host would retire this package over a reason it cannot show.
        // The code alone is still the truth.
        failure.reason = None;
    }
    Response::error(failure)
}

#[cfg(all(test, unix))]
mod camera_tests {
    use super::*;
    use couch_sdk::{couch_model::commands::Function, CameraStream, Capability, ClientSettings};
    use std::{
        os::fd::{FromRawFd, IntoRawFd},
        os::unix::net::UnixStream,
        sync::atomic::AtomicUsize,
        time::{Duration, Instant},
    };

    #[derive(serde::Deserialize, serde::Serialize)]
    struct Settings;
    impl ClientSettings for Settings {
        const FILE_PREFIX: &'static str = "camera-test";
        fn validate(&self) -> couch_sdk::Result<()> {
            Ok(())
        }
    }

    struct Client {
        jpeg: Vec<u8>,
    }
    impl DeviceClient for Client {
        type Settings = Settings;
        const KIND: &'static str = "camera-test";
        const LABEL: &'static str = "Camera test";
        fn capabilities() -> &'static [Capability] {
            &[]
        }
        fn connect(_: &Settings) -> couch_sdk::Result<Self> {
            unreachable!()
        }
        fn execute(&mut self, _: &Function) -> couch_sdk::Result<()> {
            Err(couch_sdk::Error::Unsupported)
        }
        fn camera_snapshot(&mut self, _: &str) -> couch_sdk::Result<Vec<u8>> {
            Ok(self.jpeg.clone())
        }
    }

    #[test]
    fn snapshot_bytes_are_validated_cached_and_chunked_by_the_sdk() {
        let mut jpeg = vec![7; MAX_SNAPSHOT_CHUNK_BYTES + 4];
        jpeg[..2].copy_from_slice(&[0xff, 0xd8]);
        let end = jpeg.len();
        jpeg[end - 2..].copy_from_slice(&[0xff, 0xd9]);
        let mut client = Client { jpeg: jpeg.clone() };
        let mut cache = None;
        let first = camera_snapshot(&mut client, &mut cache, "front".into(), 0).unwrap();
        let Response::CameraSnapshot {
            data,
            offset,
            total,
        } = first
        else {
            panic!("snapshot response")
        };
        assert_eq!(offset, 0);
        assert_eq!(total as usize, jpeg.len());
        assert_eq!(
            STANDARD.decode(data).unwrap(),
            jpeg[..MAX_SNAPSHOT_CHUNK_BYTES]
        );
        let second = camera_snapshot(
            &mut client,
            &mut cache,
            "front".into(),
            MAX_SNAPSHOT_CHUNK_BYTES as u32,
        )
        .unwrap();
        let Response::CameraSnapshot { data, .. } = second else {
            panic!("snapshot response")
        };
        assert_eq!(
            STANDARD.decode(data).unwrap(),
            jpeg[MAX_SNAPSHOT_CHUNK_BYTES..]
        );
        assert_eq!(
            camera_snapshot(&mut client, &mut cache, "other".into(), 4),
            Err(couch_sdk::Error::Invalid)
        );
    }

    struct BlockingStream {
        cancelled: Arc<AtomicBool>,
        calls: Arc<AtomicUsize>,
    }
    impl CameraStream for BlockingStream {
        fn next_h264(&mut self) -> couch_sdk::Result<Vec<u8>> {
            if self.calls.fetch_add(1, Ordering::AcqRel) == 0 {
                return Ok(vec![0, 0, 0, 1, 0x65, 0x88]);
            }
            while !self.cancelled.load(Ordering::Acquire) {
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(couch_sdk::Error::Transport)
        }
    }

    #[test]
    fn live_worker_frames_h264_and_close_interrupts_the_source() {
        let (package, mut host) = UnixStream::pair().unwrap();
        host.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
        let file = unsafe { File::from_raw_fd(package.into_raw_fd()) };
        let cancelled = Arc::new(AtomicBool::new(false));
        let calls = Arc::new(AtomicUsize::new(0));
        let source = BlockingStream {
            cancelled: cancelled.clone(),
            calls: calls.clone(),
        };
        let mut running = start_camera(
            "front".into(),
            Box::new(source),
            Arc::new(move || cancelled.store(true, Ordering::Release)),
            file,
        );
        assert_eq!(
            couch_sdk::read_h264_record(&mut host).unwrap(),
            Some(vec![0, 0, 0, 1, 0x65, 0x88])
        );
        let deadline = Instant::now() + Duration::from_secs(1);
        while calls.load(Ordering::Acquire) < 2 && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(calls.load(Ordering::Acquire) >= 2);
        running.closing.store(true, Ordering::Release);
        (running.cancel)();
        running.join().unwrap();
        assert_eq!(couch_sdk::read_h264_record(&mut host).unwrap(), None);
    }
}
