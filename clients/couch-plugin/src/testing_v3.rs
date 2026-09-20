//! Admission checks for a package whose connection has children.
//!
//! Protocol 3 is unreleased, so this is a second file rather than more cases
//! in [`crate::testing`]: that one is digest-pinned as part of the release
//! evidence, and nothing in the protocol 3 train may move it. It becomes part
//! of `testing` at the step that switches protocol 3 on.
//!
//! What it asserts is the part a bridge gets wrong: a listing that ends, ids
//! that mean the same thing the second time they are asked for, a resource
//! that does not exist being refused rather than answered for something else,
//! a write whose acknowledgement agrees with the next read, and a child of one
//! kind never answering for another.
//!
//! ```no_run
//! # use couch_plugin::{testing::{Adapter, FakeDevice}, testing_v3::{self, ChildrenCase}, TypedAction};
//! # fn adapter() -> Adapter<'static> { unimplemented!() }
//! # fn device() -> Box<dyn FakeDevice> { unimplemented!() }
//! testing_v3::children(
//!     adapter(),
//!     ChildrenCase {
//!         device: device(),
//!         expect: 79,
//!         kind: "light",
//!         write: TypedAction::SetLight { on: Some(true), brightness: Some(40), mirek: None, xy: None },
//!         unknown: "no-such-lamp",
//!     },
//! );
//! ```

use crate::{
    list_children,
    testing::{Adapter, FakeDevice, Package},
    Child, Endpoint, Error, Host, Request, Response, Status, TypedAction, MAX_PAGE,
};
use std::time::Duration;

/// One listing, one write and one read, against a real package subprocess.
pub struct ChildrenCase {
    /// The fake bridge. One case needs one, and it is running by the time it
    /// gets here: unlike [`crate::testing::failure`] nothing here wants a
    /// second device with a fresh log.
    pub device: Box<dyn FakeDevice>,
    /// How many children it offers. The case knows; the harness does not.
    pub expect: usize,
    /// The kind whose first child the write is aimed at.
    pub kind: &'static str,
    /// A typed action that child accepts. Only the case can choose it: what
    /// one lamp takes is a trait of that lamp.
    pub write: TypedAction,
    /// A resource no child of this bridge has.
    pub unknown: &'static str,
}

fn ask<'a>(
    endpoint: &'a Endpoint,
    kind: &'a str,
) -> impl Fn(Request) -> Result<Response, Error> + 'a {
    move |request| {
        endpoint
            .request_child_detailed(Some(kind), request)
            .map_err(|failure| failure.code)
    }
}

fn status(response: Response) -> Status {
    match response {
        Response::Status { status } => status,
        other => panic!("expected the child's status, got {other:?}"),
    }
}

/// Lists every child twice, writes to one and reads it back, and proves a
/// resource that is unknown or of another kind is refused.
pub fn children(adapter: Adapter<'_>, case: ChildrenCase) {
    let package = Package::new(adapter);
    assert!(
        !package.manifest.children.is_empty(),
        "a package with children declares their kinds in its manifest"
    );
    let endpoint = package.endpoint(case.device.settings(), Duration::from_secs(5));

    let listed = |endpoint: &Endpoint| -> (Vec<Child>, usize) {
        let mut pages = 0;
        let mut request = |r| {
            pages += 1;
            endpoint.request_detailed(r)
        };
        let children = list_children(&mut request).expect("the bridge lists its children");
        (children, pages)
    };
    let (first, pages) = listed(&endpoint);
    assert_eq!(
        first.len(),
        case.expect,
        "the bridge listed the wrong number"
    );
    assert!(
        first.iter().all(|child| package
            .manifest
            .children
            .iter()
            .any(|kind| kind.kind == child.kind)),
        "a child of a kind the manifest does not declare"
    );
    // The listing ended, and it ended after as many pages as that many
    // children need: a bridge that answered everything in one oversized page
    // would never have got here.
    assert_eq!(
        pages,
        case.expect.div_ceil(MAX_PAGE).max(1),
        "the listing took the wrong number of pages"
    );
    // Asked again, the same children, in the same order, under the same ids:
    // an id the browser saved yesterday has to mean the same lamp today.
    let (again, _) = listed(&endpoint);
    assert_eq!(
        first.iter().map(|c| &c.id).collect::<Vec<_>>(),
        again.iter().map(|c| &c.id).collect::<Vec<_>>(),
        "the listing is not stable"
    );
    assert_eq!(first, again, "a child changed between two listings");

    let target = first
        .iter()
        .find(|child| child.kind == case.kind)
        .unwrap_or_else(|| panic!("no child of kind {}", case.kind));
    let ask_kind = ask(&endpoint, case.kind);

    // A resource the bridge has never heard of is refused. It must never be
    // answered for something else.
    assert!(
        ask_kind(Request::status().at(case.unknown)).is_err(),
        "an unknown resource was answered"
    );

    // The acknowledgement of a write is the state the child is in, and the
    // next read agrees with it.
    let written = ask_kind(Request::action(case.write).at(&target.id))
        .expect("a declared action on a child of its own kind");
    let read = status(ask_kind(Request::status().at(&target.id)).expect("a child's status"));
    match written {
        Response::Status { status } => assert_eq!(
            status, read,
            "the acknowledged state is not the state the child is in"
        ),
        Response::Ok => (),
        other => panic!("a write was answered with {other:?}"),
    }

    // A kind that does not declare this action is never asked to perform it,
    // whichever child is named: the gate decides from the kind the
    // configuration holds, before any I/O. That covers the child of that kind
    // and the right child named under the wrong kind, because the kind is the
    // host's word and not the package's.
    let mut tried = 0;
    for kind in &package.manifest.children {
        if kind.kind == case.kind
            || kind
                .actions
                .iter()
                .any(|schema| schema.kind() == case.write.kind())
        {
            continue;
        }
        tried += 1;
        for id in first
            .iter()
            .find(|child| child.kind == kind.kind)
            .map(|other| other.id.clone())
            .into_iter()
            .chain([target.id.clone()])
        {
            assert_eq!(
                endpoint
                    .request_child_detailed(Some(&kind.kind), Request::action(case.write).at(&id))
                    .map_err(|failure| failure.code),
                Err(Error::Unsupported),
                "{} answered an action only {} declares",
                kind.kind,
                case.kind
            );
        }
    }
    assert!(
        tried > 0,
        "a case wants at least one kind that cannot take its action"
    );
    // Without a kind at all nothing is sent.
    assert_eq!(
        endpoint
            .request_detailed(Request::status().at(&target.id))
            .map_err(|failure| failure.code),
        Err(Error::Invalid),
        "a resource with no kind must not reach the package"
    );
}

// ---------------------------------------------------------------------------
// Pairing.
// ---------------------------------------------------------------------------

use crate::{
    host::accept, Credential, Error as WireError, PairFailure, PairInput, PairPrompt, PairStep,
    Response as R, MAX_CODE_LENGTH, MAX_PAIR_TEXT, MAX_POLL_MS, MIN_POLL_MS,
};

/// The most steps one conversation may take before the harness gives up. A
/// real dialog is bounded by a deadline; this is bounded by patience.
const MAX_STEPS: usize = 40;

/// One pairing conversation, and how it ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PairingScenario {
    /// It works: the package hands over a key.
    Paired,
    /// The device says no.
    Refused,
    /// The device's own window closes first.
    TimedOut,
    /// The person closes the dialog.
    Cancelled,
}

impl PairingScenario {
    pub const ALL: [Self; 4] = [Self::Paired, Self::Refused, Self::TimedOut, Self::Cancelled];
}

/// What a package with pairing has to survive.
///
/// The harness never sleeps between polls: the delay a step asks for is the
/// browser's business and the daemon's, and a package must be able to answer
/// the next poll whenever it arrives.
pub struct PairingCase {
    /// A fresh fake device, arranged to end one scenario the way that
    /// scenario says. `None` for a scenario this package genuinely cannot be
    /// made to reach; the harness then prints what it did not check rather
    /// than passing quietly.
    pub device: fn(PairingScenario) -> Option<Box<dyn FakeDevice>>,
    /// The settings that address it, for that scenario.
    pub settings: fn(&dyn FakeDevice, PairingScenario) -> serde_json::Value,
    /// What to type when the package asks for a code, in the scenario that
    /// pairs. Whatever a code prompt asks for in another scenario, this is
    /// still what is typed, so a case that wants a wrong code answers with a
    /// wrong one there.
    pub code: &'static str,
    /// An ordinary request the harness makes once the key is stored, to prove
    /// the key is never echoed back in an answer.
    pub after: Request,
}

fn shown(text: &str) -> bool {
    text.len() <= MAX_PAIR_TEXT && !text.chars().any(char::is_control)
}

/// Every bound a step carries, asserted by the harness rather than trusted:
/// the host refuses a step that breaks one, and a package that has to be
/// retired to be stopped is a package that will one day be retired on a
/// person's sofa.
fn bounded(step: &PairStep, manifest: &crate::Manifest) {
    assert!(step.is_well_formed(), "a step outside its bounds: {step:?}");
    match step {
        PairStep::Waiting {
            prompt,
            poll_after_ms,
        } => {
            if let Some(message) = prompt.message() {
                assert!(shown(message), "a prompt line too long or unprintable");
            }
            if let PairPrompt::EnterCode { length, .. } = prompt {
                assert!(
                    (1..=MAX_CODE_LENGTH).contains(length),
                    "a code of {length} characters"
                );
            }
            match *poll_after_ms {
                0 => assert!(
                    prompt.is_code(),
                    "only a code prompt may ask to be polled at once"
                ),
                ms => assert!(
                    (MIN_POLL_MS..=MAX_POLL_MS).contains(&ms),
                    "a poll of {ms} ms"
                ),
            }
        }
        PairStep::Done {
            credential,
            settings,
            summary,
        } => {
            assert!(shown(summary), "a summary too long or unprintable");
            assert!(credential.fits(), "a key Couch will not store");
            if let Some(settings) = settings {
                assert_eq!(
                    manifest.validate_settings(settings),
                    Ok(()),
                    "a Done whose settings the manifest refuses"
                );
            }
        }
        PairStep::Failed { message, .. } => {
            if let Some(message) = message {
                assert!(shown(message), "a failure line too long or unprintable");
            }
        }
    }
}

/// Drive one conversation to its end, asserting every step on the way.
fn converse(
    host: &mut Host,
    manifest: &crate::Manifest,
    settings: serde_json::Value,
    code: &str,
) -> (String, PairStep) {
    let (session, mut step) = host
        .pair_start(settings, None)
        .expect("a package that declares pairing answers a start");
    assert!(
        couch_sdk::valid_session(&session),
        "not a session id: {session}"
    );
    bounded(&step, manifest);
    for _ in 0..MAX_STEPS {
        if step.is_final() {
            return (session, step);
        }
        assert_eq!(
            host.pair_session(),
            Some(session.as_str()),
            "the session changed under the host"
        );
        let input = match &step {
            PairStep::Waiting { prompt, .. } if prompt.is_code() => {
                Some(PairInput::code(code.to_owned()))
            }
            _ => None,
        };
        step = host
            .pair_continue(input)
            .expect("a package answers every step of its own conversation");
        bounded(&step, manifest);
    }
    panic!("the conversation never ended: {step:?}");
}

/// Pairing, against a real package subprocess: one conversation that works,
/// one refused, one that runs out of time, one the person closes, the bounds
/// of every step, and the one thing a key must never do.
pub fn pairing(adapter: Adapter<'_>, case: PairingCase) {
    let package = Package::new(adapter);
    let pairing = package
        .manifest
        .pairing
        .expect("a package that pairs says so in its manifest");
    assert!(pairing.is_valid(), "{pairing:?}");
    let mut skipped = Vec::new();

    // ---- it works, and the key it produced is the key Couch stores.
    let device =
        (case.device)(PairingScenario::Paired).expect("a package that pairs can be made to pair");
    let settings = (case.settings)(&*device, PairingScenario::Paired);
    let mut host = package.host();
    let (session, step) = converse(&mut host, &package.manifest, settings.clone(), case.code);
    let PairStep::Done {
        credential,
        settings: corrected,
        ..
    } = step
    else {
        panic!("the paired scenario did not pair: {step:?}");
    };
    // The conversation is over: the host is holding nothing, and a step that
    // names the session it just finished never leaves the host.
    assert_eq!(host.pair_session(), None);
    assert_eq!(
        host.request_detailed(Request::pair_continue(&session, None))
            .map_err(|failure| failure.code),
        Err(Error::Invalid),
        "a finished session was still answered"
    );
    assert!(host.is_alive(), "pairing cost the package its process");

    // ---- the key reaches the package, and comes back out of nothing.
    let saved = corrected.unwrap_or(settings);
    if pairing.required {
        let mut unpaired = package.host();
        unpaired
            .configure(saved.clone())
            .expect("settings alone are still valid settings");
        assert_eq!(
            unpaired
                .request_detailed(case.after.clone())
                .map_err(|failure| failure.code),
            Err(Error::Unpaired),
            "a package whose pairing is required answered without a key"
        );
    }
    let mut paired = package.host();
    paired
        .configure_with(saved, Some(&credential))
        .expect("the stored key configures");
    let (answer, rotated) = paired
        .request_full(case.after.clone())
        .expect("a paired package answers");
    // Whatever it answered, the key is not in it. A rotation travels beside
    // the reply, in `store_credential`, and is a different key.
    let body = serde_json::to_string(&answer).expect("a serializable response");
    for value in credential.get().values().filter_map(|v| v.as_str()) {
        assert!(
            !body.contains(value),
            "the key was echoed back in an answer: {body}"
        );
    }
    if let Some(rotated) = &rotated {
        assert!(rotated.fits(), "a rotated key Couch will not store");
    }

    // ---- the limit on a key, which no honest package can be asked to break.
    // The package produced a real one; this is the same key, too large, and
    // the host's own gate refusing it.
    let mut oversized = credential.get().clone();
    oversized.insert(
        "padding".into(),
        serde_json::Value::String("a".repeat(Credential::MAX_BYTES)),
    );
    let oversized: Credential =
        serde_json::from_value(serde_json::Value::Object(oversized)).expect("a credential object");
    assert!(!oversized.fits());
    assert_eq!(
        Credential::new(serde_json::Value::Object(oversized.get().clone())),
        Err(couch_sdk::Error::Invalid),
        "the constructor let an oversized key through"
    );
    let too_big = R::Pairing {
        session: session.clone(),
        step: PairStep::Done {
            credential: oversized,
            settings: None,
            summary: "Paired".into(),
        },
    };
    assert_eq!(
        accept(
            &package.manifest,
            &Request::pair_start(serde_json::json!({}), None),
            &too_big
        ),
        Err(WireError::Protocol),
        "the host accepted a key it cannot store"
    );
    // ...and the same for a step outside its bounds, which is the other half
    // of what `bounded` above asserts the package itself never sends.
    for step in [
        PairStep::Waiting {
            prompt: PairPrompt::press_button(),
            poll_after_ms: 0,
        },
        PairStep::Waiting {
            prompt: PairPrompt::press_button(),
            poll_after_ms: MAX_POLL_MS + 1,
        },
        PairStep::Failed {
            reason: PairFailure::Refused,
            message: Some("a".repeat(MAX_PAIR_TEXT + 1)),
        },
    ] {
        assert_eq!(
            accept(
                &package.manifest,
                &Request::pair_start(serde_json::json!({}), None),
                &R::Pairing {
                    session: session.clone(),
                    step: step.clone()
                }
            ),
            Err(WireError::Protocol),
            "the host accepted {step:?}"
        );
    }

    // ---- refused, and out of time. Both end the conversation with nothing
    // stored, and neither costs the package its process.
    for (scenario, expected) in [
        (PairingScenario::Refused, PairFailure::Refused),
        (PairingScenario::TimedOut, PairFailure::TimedOut),
    ] {
        let Some(device) = (case.device)(scenario) else {
            skipped.push(scenario);
            continue;
        };
        let settings = (case.settings)(&*device, scenario);
        let mut host = package.host();
        let (_, step) = converse(&mut host, &package.manifest, settings, case.code);
        match step {
            PairStep::Failed { reason, .. } => assert_eq!(reason, expected, "{scenario:?}"),
            other => panic!("{scenario:?} ended {other:?}"),
        }
        assert_eq!(host.pair_session(), None);
        assert!(host.is_alive(), "{scenario:?} cost the package its process");
    }

    // ---- the person closed the dialog.
    if let Some(device) = (case.device)(PairingScenario::Cancelled) {
        let settings = (case.settings)(&*device, PairingScenario::Cancelled);
        let mut host = package.host();
        let (session, step) = host
            .pair_start(settings, None)
            .expect("a start before a cancel");
        bounded(&step, &package.manifest);
        assert!(!step.is_final(), "nothing to cancel: {step:?}");
        host.pair_cancel().expect("a cancel is answered");
        assert_eq!(host.pair_session(), None);
        assert_eq!(
            host.request_detailed(Request::pair_continue(&session, None))
                .map_err(|failure| failure.code),
            Err(Error::Invalid),
            "a cancelled session was still answered"
        );
        assert!(host.is_alive(), "a cancel cost the package its process");
    } else {
        skipped.push(PairingScenario::Cancelled);
    }

    assert!(
        skipped.is_empty(),
        "these scenarios were not checked: {skipped:?}"
    );
}
