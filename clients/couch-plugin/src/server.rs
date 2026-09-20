use crate::{
    protocol::Envelope, read_frame, write_frame, Error, Failure, Manifest, Request, Response,
    Result, NEXT_PROTOCOL_VERSION,
};
use couch_sdk::{ClientSettings, DeviceClient, KeyPhase};

/// Run an SDK integration over stdin/stdout. Configure validates settings and
/// clears prior state; the first actual device operation connects lazily.
///
/// What goes on the wire follows the manifest's protocol version, not the SDK
/// the package was built with. A protocol 1 or 2 package built with this SDK
/// sends the bytes it always sent: a reason its client attaches is dropped,
/// `Unpaired` leaves as `rejected`, and its client is only ever told of taps.
/// A protocol 3 manifest is refused here, as it is by the host, unless the
/// `protocol-3-preview` feature is on.
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
    {
        return Err(Error::Invalid);
    }
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut input = stdin.lock();
    let mut output = stdout.lock();
    let mut hello = false;
    let mut settings: Option<C::Settings> = None;
    let mut client: Option<C> = None;
    let explains = manifest.protocol_version >= NEXT_PROTOCOL_VERSION;
    let mut last_id = 0;
    loop {
        let envelope: Envelope<Request> = read_frame(&mut input)?;
        if envelope.id <= last_id {
            return Err(Error::Protocol);
        }
        last_id = envelope.id;
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
                Request::Configure { settings: value } => {
                    let value = manifest.with_defaults(value)?;
                    let parsed: C::Settings =
                        serde_json::from_value(value).map_err(|_| Error::Invalid)?;
                    parsed.validate()?;
                    settings = Some(parsed);
                    client = None;
                    Ok(Response::Ok)
                }
                request => {
                    // Refuse undeclared input/commands before opening a socket.
                    // A button the package names itself (`x:`) passes only
                    // if this manifest declares it, like any other.
                    if let Request::Command { function, .. } = &request {
                        let parsed = couch_sdk::couch_model::commands::Function::parse(function)
                            .ok_or(Error::Unsupported)?;
                        if !manifest.supports(function) || !C::supports(&parsed) {
                            return Err(Error::Unsupported.into());
                        }
                    }
                    if let Request::Action { action } = &request {
                        manifest.validate_action(*action)?;
                        C::validate_action(*action)?;
                    }
                    if matches!(request, Request::Inputs) && !manifest.supports_inputs {
                        return Err(Error::Unsupported.into());
                    }
                    if client.is_none() {
                        client = Some(C::connect(settings.as_ref().ok_or(Error::Invalid)?)?);
                    }
                    let client_ref = client.as_mut().ok_or(Error::Transport)?;
                    let result = match request {
                        Request::Command { function, phase } => {
                            // The host never sends an older package a phase.
                            let phase = if explains { phase } else { KeyPhase::Tap };
                            client_ref
                                .command_phased(&function, phase)
                                .map(|()| Response::Ok)
                        }
                        Request::Action { action } => {
                            client_ref.action(action).map(|()| Response::Ok)
                        }
                        Request::Status => client_ref.status().map(|mut status| {
                            // A v1 SDK adapter retains its exact wire contract.
                            if manifest.protocol_version == 1 {
                                status.volume_db = None;
                            }
                            Response::Status { status }
                        }),
                        Request::Inputs => client_ref
                            .inputs()
                            .map(|inputs| Response::Inputs { inputs }),
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
        write_frame(
            &mut output,
            &Envelope {
                id: envelope.id,
                body: response,
            },
        )?;
    }
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
