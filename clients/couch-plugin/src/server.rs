use crate::{
    protocol::{Envelope, ReplyEnvelope},
    read_frame, write_frame, Error, Failure, Manifest, Request, Response, Result,
    NEXT_PROTOCOL_VERSION,
};
use couch_sdk::{ClientSettings, Credential, DeviceClient, KeyPhase, PairFlow};

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
