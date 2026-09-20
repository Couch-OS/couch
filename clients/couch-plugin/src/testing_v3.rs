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
    Child, Endpoint, Error, Request, Response, Status, TypedAction, MAX_PAGE,
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
