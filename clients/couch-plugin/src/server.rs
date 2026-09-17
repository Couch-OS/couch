use crate::{
    protocol::Envelope, read_frame, write_frame, Error, Manifest, Request, Response, Result,
};
use couch_sdk::{ClientSettings, DeviceClient};

/// Run an SDK integration over stdin/stdout. Configure validates settings and
/// clears prior state; the first actual device operation connects lazily.
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
    let mut last_id = 0;
    loop {
        let envelope: Envelope<Request> = read_frame(&mut input)?;
        if envelope.id <= last_id {
            return Err(Error::Protocol);
        }
        last_id = envelope.id;
        let response = (|| -> Result<Response> {
            match envelope.body {
                Request::Hello { protocol_version } => {
                    if hello || protocol_version != manifest.protocol_version {
                        return Err(Error::Incompatible);
                    }
                    hello = true;
                    Ok(Response::Hello {
                        manifest: manifest.clone(),
                    })
                }
                _ if !hello => Err(Error::Incompatible),
                Request::Configure { settings: value } => {
                    let value = manifest.with_defaults(value)?;
                    let parsed: C::Settings =
                        serde_json::from_value(value).map_err(|_| Error::Invalid)?;
                    parsed.validate().map_err(Error::from)?;
                    settings = Some(parsed);
                    client = None;
                    Ok(Response::Ok)
                }
                request => {
                    // Refuse undeclared input/commands before opening a socket.
                    if let Request::Command { function } = &request {
                        let parsed = couch_sdk::couch_model::commands::Function::parse(function)
                            .ok_or(Error::Unsupported)?;
                        if !manifest.supports(function) || !C::supports(&parsed) {
                            return Err(Error::Unsupported);
                        }
                    }
                    if let Request::Action { action } = &request {
                        manifest.validate_action(*action)?;
                        C::validate_action(*action)?;
                    }
                    if matches!(request, Request::Inputs) && !manifest.supports_inputs {
                        return Err(Error::Unsupported);
                    }
                    if client.is_none() {
                        client = Some(C::connect(settings.as_ref().ok_or(Error::Invalid)?)?);
                    }
                    let client_ref = client.as_mut().ok_or(Error::Transport)?;
                    let result = match request {
                        Request::Command { function } => {
                            client_ref.command(&function).map(|()| Response::Ok)
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
                    if matches!(
                        result,
                        Err(couch_sdk::Error::Transport
                            | couch_sdk::Error::Timeout
                            | couch_sdk::Error::Protocol)
                    ) {
                        client = None;
                    }
                    result.map_err(Into::into)
                }
            }
        })()
        .unwrap_or_else(|code| Response::Error { code });
        write_frame(
            &mut output,
            &Envelope {
                id: envelope.id,
                body: response,
            },
        )?;
    }
}
