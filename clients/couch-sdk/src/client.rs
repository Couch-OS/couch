//! The contract: what a client declares, and what it is asked to do.
//!
//! A client can be linked directly into Couch or hosted as an independent
//! subprocess through `couch-plugin`. The transport contract is the same in
//! both cases; see `docs/client-sdk.md` for the two development paths.
//! What this trait fixes is the part that was previously re-decided per client:
//! the vocabulary of commands, the point at which an unsupported command is
//! refused, and the shape of the answer.
//!
//! The vocabulary is [`couch_model::commands::Function`], because that is what
//! the configuration file persists, what the button-mapping UI offers and what
//! the GUI's executor parses. A client never sees free text except at
//! [`DeviceClient::command`], which is the boundary.

use couch_model::buttons;
use couch_model::commands::Function;
use couch_model::Integration;

use crate::{ClientSettings, Error, Result, Selectable, Status};

/// A function this client implements, as `(id, label)`.
///
/// Exactly the shape of `couch_model::buttons::functions`, so a client's
/// declaration and the catalog the web UI renders can be compared for equality
/// rather than kept in sync by hope. `id` must parse as a
/// [`Function`]; `label` is shown in the button picker.
pub type Capability = (&'static str, &'static str);

/// One integration, from the point of view of the code that talks to a device.
///
/// Implemented on the type that owns the connection. One instance owns one
/// device's transport and is never shared between threads - the broker in
/// `couch-control` is what makes that safe under a GUI and a browser at once,
/// and it assumes the client itself does no internal queuing.
pub trait DeviceClient: Sized {
    type Settings: ClientSettings;

    /// The provider slug, matching `couch_model::Provider::kind()` once the
    /// provider is registered: `"denon"`, `"web-os"`, `"kodi"`.
    const KIND: &'static str;
    /// Human name, matching `couch_model::Provider::label()`.
    const LABEL: &'static str;

    /// Every fixed function this client can perform, in the order the UI should
    /// offer them. Dynamic `input:`/`app:` functions are not listed here;
    /// declare those with [`DeviceClient::supports_input`] and
    /// [`DeviceClient::supports_app`].
    ///
    /// Declaring a function you have not implemented is the one error this
    /// harness cannot catch for you at compile time, so
    /// `couch_sdk::testing::contract_findings` checks it at test time instead.
    fn capabilities() -> &'static [Capability];

    /// Bounded typed actions; separate from persisted button command strings.
    fn actions() -> &'static [crate::PluginActionSchema] {
        &[]
    }

    /// Refuse unsupported or out-of-range values before transport I/O.
    fn validate_action(action: crate::TypedAction) -> Result<()> {
        let schema = Self::actions().first().ok_or(Error::Unsupported)?;
        if !schema.accepts(action) {
            return Err(Error::Invalid);
        }
        Ok(())
    }

    fn action(&mut self, action: crate::TypedAction) -> Result<()> {
        Self::validate_action(action)?;
        self.execute_action(action)
    }

    fn execute_action(&mut self, _action: crate::TypedAction) -> Result<()> {
        Err(Error::Unsupported)
    }

    /// Open the transport. Validate settings first; do not retry internally.
    fn connect(settings: &Self::Settings) -> Result<Self>;

    /// Perform one function. Called only after the capability gate has passed.
    ///
    /// Never retry inside this method. A lost reply does not prove a lost
    /// command, and the broker above deliberately does not retry either.
    fn execute(&mut self, function: &Function) -> Result<()>;

    /// Observe the device. [`Error::Unsupported`] if it cannot be asked.
    fn status(&mut self) -> Result<Status> {
        Err(Error::Unsupported)
    }

    /// Inputs the user may select, if the device can enumerate them.
    fn inputs(&mut self) -> Result<Vec<Selectable>> {
        Ok(Vec::new())
    }

    /// Apps the user may launch, if the device can enumerate them.
    fn apps(&mut self) -> Result<Vec<Selectable>> {
        Ok(Vec::new())
    }

    /// Whether `input:<id>` is meaningful. The default refuses every input, so
    /// a client that does not override this cannot be asked to select one.
    fn supports_input(_id: &str) -> bool {
        false
    }

    /// Whether `app:<id>` is meaningful. Defaults to refusing every app.
    fn supports_app(_id: &str) -> bool {
        false
    }

    /// The capability gate. Mirrors how `Function::supports` treats the
    /// dynamic variants: fixed functions are a lookup in the declared list,
    /// `input:`/`app:` ask the client about that specific ID.
    fn supports(function: &Function) -> bool {
        match function {
            Function::Input(id) => Self::supports_input(id),
            Function::App(id) => Self::supports_app(id),
            other => {
                let id = other.id();
                Self::capabilities().iter().any(|(name, _)| *name == id)
            }
        }
    }

    /// The persisted-string boundary: an `Action::command` out of `config.json`
    /// or a button binding arrives here.
    ///
    /// Unknown text and undeclared functions are refused before any I/O, so a
    /// stale mapping cannot make a device do something arbitrary and cannot
    /// cost a round trip to find out.
    fn command(&mut self, command: &str) -> Result<()> {
        let function = Function::parse(command).ok_or(Error::Unsupported)?;
        if !Self::supports(&function) {
            return Err(Error::Unsupported);
        }
        self.execute(&function)
    }
}

/// Compare a client's declared capabilities with the catalog `couch-model`
/// publishes for its integration.
///
/// This is the check that makes registration real. Until the `Integration`
/// variant exists in `couch-model`, `functions()` returns an empty slice and
/// this reports every declared function as missing, which is the correct
/// answer: the button picker would offer nothing.
///
/// Returns the differences, most useful first; an empty vector means the two
/// agree exactly, including order.
pub fn catalog_differences<C: DeviceClient>(integration: &Integration) -> Vec<String> {
    let declared = C::capabilities();
    let catalog = buttons::functions(integration);
    let mut findings = Vec::new();
    for (id, _) in declared {
        if !catalog.iter().any(|(name, _)| name == id) {
            findings.push(format!(
                "{} declares `{id}` but couch_model::buttons::functions does not offer it for {}",
                C::KIND,
                integration.via()
            ));
        }
    }
    for (id, _) in catalog {
        if !declared.iter().any(|(name, _)| name == id) {
            findings.push(format!(
                "couch_model::buttons::functions offers `{id}` for {} but {} does not implement it",
                integration.via(),
                C::KIND
            ));
        }
    }
    if findings.is_empty() && declared != catalog {
        findings.push(format!(
            "{} declares the same functions as {} but in a different order or with different labels",
            C::KIND,
            integration.via()
        ));
    }
    findings
}
