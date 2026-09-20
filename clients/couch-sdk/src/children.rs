//! Protocol 3 (unreleased): the children one connection offers.
//!
//! A bridge is one connection and many devices. It lists them in pages, and
//! every command, typed action and status read may then name one of them with
//! a `resource`. A child is nothing but its id, the kind of child it is (a
//! kind the package declared in its manifest), a name, an optional room hint,
//! and the traits of whichever built-in control its kind is drawn with.
//!
//! The kind is never on the wire in a request: the host derives it from the
//! saved configuration and uses it only to decide what it is willing to send.
//!
//! Nothing here is reachable with the protocol 3 switch off.

use couch_model::{ChildSnapshot, ClimateTraits, CoverTraits, LightTraits};
use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// The most children one page may carry. A worst-case child is about 1.2 KB,
/// so a full page fits a 64 KiB frame with room to spare.
pub const MAX_PAGE: usize = 32;
/// ...and the most bytes a page of them may serialize to, which is what
/// [`ChildPage::fill`] stops at first when the names are long.
pub const MAX_PAGE_BYTES: usize = 48 * 1024;
/// A cursor is spelt like a resource: the same alphabet, at most 128 bytes.
pub const MAX_CURSOR: usize = 128;
/// A child's name and its room hint are shown to a person, never interpreted.
pub const MAX_CHILD_LABEL: usize = 128;

/// Whether a cursor is one the host is willing to hand back.
pub fn valid_cursor(cursor: &str) -> bool {
    cursor.len() <= MAX_CURSOR && couch_model::valid_resource(cursor)
}

fn shown(text: &str) -> bool {
    !text.is_empty() && text.len() <= MAX_CHILD_LABEL && !text.chars().any(char::is_control)
}

/// One device behind a connection, as its package lists it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Child {
    /// How the package names it: 1 to 128 bytes of `[A-Za-z0-9._/+-]` in
    /// segments that never climb out ([`couch_model::valid_resource`]).
    pub id: String,
    /// One of the kinds the manifest declares.
    pub kind: String,
    /// What the person calls it.
    pub name: String,
    /// The room the bridge itself puts it in, if it has one. Only ever a hint
    /// for the person choosing: Couch never assigns a device by itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub room_hint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub light: Option<LightTraits>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover: Option<CoverTraits>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub climate: Option<ClimateTraits>,
}

impl Child {
    pub fn new(id: impl Into<String>, kind: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            kind: kind.into(),
            name: name.into(),
            room_hint: None,
            light: None,
            cover: None,
            climate: None,
        }
    }
    pub fn in_room(mut self, hint: impl Into<String>) -> Self {
        self.room_hint = Some(hint.into());
        self
    }
    pub fn with_light(mut self, traits: LightTraits) -> Self {
        self.light = Some(traits);
        self
    }
    pub fn with_cover(mut self, traits: CoverTraits) -> Self {
        self.cover = Some(traits);
        self
    }
    pub fn with_climate(mut self, traits: ClimateTraits) -> Self {
        self.climate = Some(traits);
        self
    }

    /// What is saved with the room device this child becomes.
    pub fn snapshot(&self) -> ChildSnapshot {
        ChildSnapshot {
            kind: self.kind.clone(),
            light: self.light,
            cover: self.cover,
            climate: self.climate.clone(),
        }
    }

    /// Everything that can be checked without the package's manifest: the id
    /// and kind grammars, names fit to show, and traits within the bounds that
    /// hold for every device. Whether the kind is declared, and whether the
    /// traits are those of that kind's control, is the host's to say.
    pub fn is_well_formed(&self) -> bool {
        couch_model::valid_resource(&self.id)
            && couch_model::domain::valid_kind_id(&self.kind)
            && shown(&self.name)
            && self.room_hint.as_deref().is_none_or(shown)
            && self.light.is_none_or(|traits| traits.is_valid())
            && self.climate.as_ref().is_none_or(|traits| traits.is_valid())
    }
}

/// One answer to a listing request: some children, and where to carry on.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChildPage {
    pub children: Vec<Child>,
    /// The cursor for the next page, absent on the last one. A page with a
    /// cursor is never empty.
    pub next: Option<String>,
}

impl ChildPage {
    /// The last page of a listing.
    pub fn last(children: Vec<Child>) -> Self {
        Self {
            children,
            next: None,
        }
    }

    /// Cut one page out of everything the package has, in a fixed order.
    ///
    /// The cursor is the id of the first child of the next page, so paging is
    /// stable as long as ids are: a child added or removed in between shifts
    /// nothing else. `None` starts at the beginning; a cursor naming no child
    /// is [`Error::Invalid`], which is what the host wants for a stale
    /// browser. A page stops at [`MAX_PAGE`] children or [`MAX_PAGE_BYTES`],
    /// whichever comes first, and always carries at least one child while any
    /// are left.
    pub fn fill(all: impl IntoIterator<Item = Child>, cursor: Option<&str>) -> Result<Self> {
        let mut rest = all.into_iter().peekable();
        if let Some(cursor) = cursor {
            if !valid_cursor(cursor) {
                return Err(Error::Invalid);
            }
            loop {
                match rest.peek() {
                    Some(child) if child.id == cursor => break,
                    Some(_) => {
                        rest.next();
                    }
                    None => return Err(Error::Invalid),
                }
            }
        }
        let mut children: Vec<Child> = Vec::new();
        let mut bytes = 0;
        while let Some(child) = rest.peek() {
            if children.len() >= MAX_PAGE {
                break;
            }
            let size = serde_json::to_vec(child)
                .map(|json| json.len() + 1)
                .unwrap_or(MAX_PAGE_BYTES);
            if !children.is_empty() && bytes + size > MAX_PAGE_BYTES {
                break;
            }
            bytes += size;
            children.push(rest.next().expect("peeked"));
        }
        let next = rest.peek().map(|child| child.id.clone());
        Ok(Self { children, next })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use couch_model::LightTraits;

    fn lamps(count: usize) -> Vec<Child> {
        (0..count)
            .map(|n| Child::new(format!("lamp-{n}"), "light", format!("Lamp {n}")))
            .collect()
    }

    #[test]
    fn a_child_is_small_strict_and_says_only_what_it_is() {
        let lamp = Child::new("5f0c9a52", "light", "Desk")
            .in_room("Study")
            .with_light(LightTraits {
                dimmable: true,
                mirek: Some((153, 500)),
                color: false,
            });
        assert_eq!(
            serde_json::to_string(&lamp).unwrap(),
            r#"{"id":"5f0c9a52","kind":"light","name":"Desk","room_hint":"Study","light":{"dimmable":true,"mirek":[153,500]}}"#
        );
        assert_eq!(
            serde_json::to_string(&Child::new("a", "scene", "Evening")).unwrap(),
            r#"{"id":"a","kind":"scene","name":"Evening"}"#
        );
        assert!(serde_json::from_str::<Child>(
            r#"{"id":"a","kind":"scene","name":"Evening","icon":"moon"}"#
        )
        .is_err());
        assert!(lamp.is_well_formed());
        assert_eq!(lamp.snapshot().kind, "light");
        assert_eq!(lamp.snapshot().light, lamp.light);
        for broken in [
            Child::new("../x", "light", "Climbing"),
            Child::new("", "light", "Nameless"),
            Child::new("a", "Light", "Shouting"),
            Child::new("a", "light", ""),
            Child::new("a", "light", "Two\nlines"),
            Child::new("a", "light", "a".repeat(MAX_CHILD_LABEL + 1)),
            Child::new("a", "light", "Hinted").in_room("a".repeat(MAX_CHILD_LABEL + 1)),
            Child::new("a", "light", "Bad range").with_light(LightTraits {
                dimmable: true,
                mirek: Some((500, 153)),
                color: false,
            }),
        ] {
            assert!(!broken.is_well_formed(), "{broken:?}");
        }
    }

    #[test]
    fn a_page_stops_at_thirty_two_and_its_cursor_is_the_next_child() {
        let all = lamps(70);
        let first = ChildPage::fill(all.clone(), None).unwrap();
        assert_eq!(first.children.len(), MAX_PAGE);
        assert_eq!(first.next.as_deref(), Some("lamp-32"));
        let second = ChildPage::fill(all.clone(), first.next.as_deref()).unwrap();
        assert_eq!(second.children.len(), MAX_PAGE);
        assert_eq!(second.children[0].id, "lamp-32");
        assert_eq!(second.next.as_deref(), Some("lamp-64"));
        let third = ChildPage::fill(all.clone(), second.next.as_deref()).unwrap();
        assert_eq!(third.children.len(), 6);
        assert_eq!(third.next, None);

        assert_eq!(
            ChildPage::fill(Vec::new(), None).unwrap(),
            ChildPage::last(Vec::new())
        );
        assert_eq!(
            ChildPage::fill(all.clone(), Some("lamp-999")),
            Err(Error::Invalid)
        );
        assert_eq!(ChildPage::fill(all, Some("../x")), Err(Error::Invalid));
    }

    #[test]
    fn a_page_of_long_names_stops_at_forty_eight_kilobytes_with_one_child_at_least() {
        let wordy: Vec<Child> = (0..MAX_PAGE)
            .map(|n| {
                Child::new(format!("lamp-{n}"), "light", "n".repeat(MAX_CHILD_LABEL))
                    .in_room("r".repeat(MAX_CHILD_LABEL))
            })
            .collect();
        let page = ChildPage::fill(wordy.clone(), None).unwrap();
        assert!(page.children.len() <= MAX_PAGE);
        assert!(
            serde_json::to_vec(&page.children).unwrap().len() <= MAX_PAGE_BYTES,
            "a page must fit the frame"
        );
        // One enormous child is still a page of one, never an empty page with
        // a cursor: a listing that returned nothing but a cursor never ends.
        let huge = vec![
            Child::new("a", "light", "a".repeat(MAX_CHILD_LABEL)),
            Child::new("b", "light", "b".repeat(MAX_CHILD_LABEL)),
        ];
        let page = ChildPage::fill(huge, None).unwrap();
        assert!(!page.children.is_empty());
    }
}
