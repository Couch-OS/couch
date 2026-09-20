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
                        client = Some(C::connect(settings.as_ref().ok_or(Error::Invalid)?)?);
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
        write_frame(
            &mut output,
            &Envelope {
                id: envelope.id,
                body: response,
            },
        )?;
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
