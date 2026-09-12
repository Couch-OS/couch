//! Where in the app we are, kept in the browser's URL.
//!
//! Hand-rolled rather than `leptos_router`. The small route set is explicit and the
//! whole router is one enum, two string functions and a `popstate` listener;
//! the crate would add a matcher, a nested-route tree and a set of macros to
//! do the same thing, on a bundle that is downloaded over the remote's own
//! WiFi.
//!
//! It is a real URL rather than app-internal state because the thing this runs
//! on is a phone, and on a phone Back means "up one level". Without history
//! entries, Back leaves the app entirely from the first screen a user drills
//! into.

use couch_model::Id;
use leptos::prelude::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    Overview,
    Rooms,
    Connections,
    Settings,
    Updates,
    Areas,
    Area(Id),
    Room(Id),
    Scene(Id),
    Activities,
    Activity(Id),
    NotFound,
}

impl Route {
    pub fn from_path(path: &str) -> Route {
        let parts: Vec<&str> = path
            .trim_matches('/')
            .split('/')
            .filter(|s| !s.is_empty())
            .collect();
        match parts.as_slice() {
            [] => Route::Overview,
            ["overview"] => Route::Overview,
            ["rooms"] => Route::Rooms,
            ["connections"] => Route::Connections,
            ["settings"] => Route::Settings,
            ["updates"] => Route::Updates,
            ["areas"] => Route::Areas,
            ["areas", id] => Route::Area(Id::new(*id)),
            ["rooms", id] => Route::Room(Id::new(*id)),
            ["scenes"] => Route::Rooms,
            ["scenes", id] => Route::Scene(Id::new(*id)),
            ["activities"] => Route::Activities,
            ["activities", id] => Route::Activity(Id::new(*id)),
            _ => Route::NotFound,
        }
    }

    pub fn path(&self) -> String {
        match self {
            Route::Overview => "/".to_string(),
            Route::Rooms => "/rooms".to_string(),
            Route::Connections => "/connections".to_string(),
            Route::Settings => "/settings".to_string(),
            Route::Updates => "/updates".to_string(),
            Route::Areas | Route::NotFound => "/areas".to_string(),
            Route::Area(id) => format!("/areas/{id}"),
            Route::Room(id) => format!("/rooms/{id}"),
            Route::Scene(id) => format!("/scenes/{id}"),
            Route::Activities => "/activities".to_string(),
            Route::Activity(id) => format!("/activities/{id}"),
        }
    }

    /// Which navigation destination this screen belongs to.
    pub fn tab(&self) -> Route {
        match self {
            Route::Overview => Route::Overview,
            Route::Rooms | Route::Room(_) => Route::Rooms,
            Route::Connections => Route::Connections,
            Route::Settings => Route::Settings,
            Route::Updates => Route::Updates,
            Route::Scene(_) => Route::Rooms,
            Route::Activities | Route::Activity(_) => Route::Activities,
            _ => Route::Areas,
        }
    }
}

/// The current route, and the only way to change it.
#[derive(Clone, Copy)]
pub struct Router {
    pub current: RwSignal<Route>,
}

impl Router {
    /// Reads the URL the page was opened on, and follows Back and Forward.
    pub fn install() -> Router {
        let current = RwSignal::new(Route::from_path(&current_path()));
        let router = Router { current };

        // Leptos cleans this up with the owner, which for the root component is
        // the lifetime of the page.
        let handle = window_event_listener(leptos::ev::popstate, move |_| {
            current.set(Route::from_path(&current_path()));
        });
        on_cleanup(move || handle.remove());

        router
    }

    pub fn go(&self, route: Route) {
        if self.current.get_untracked() == route {
            return;
        }
        let path = route.path();
        if let Some(history) = window().history().ok() {
            let _ = history.push_state_with_url(&wasm_bindgen::JsValue::NULL, "", Some(&path));
        }
        self.current.set(route);
    }
}

fn current_path() -> String {
    window()
        .location()
        .pathname()
        .unwrap_or_else(|_| "/".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn navigation_round_trips_and_keeps_old_bookmarks() {
        for route in [
            Route::Overview,
            Route::Rooms,
            Route::Connections,
            Route::Areas,
            Route::Area(Id::new("upstairs")),
            Route::Room(Id::new("study")),
            Route::Scene(Id::new("night")),
            Route::Activities,
            Route::Activity(Id::new("tv")),
        ] {
            assert_eq!(Route::from_path(&route.path()), route);
        }
        assert_eq!(Route::Room(Id::new("study")).tab(), Route::Rooms);
        assert_eq!(Route::from_path("/scenes"), Route::Rooms);
        assert_eq!(Route::Scene(Id::new("night")).tab(), Route::Rooms);
        assert_eq!(Route::from_path("/unknown"), Route::NotFound);
    }
}
