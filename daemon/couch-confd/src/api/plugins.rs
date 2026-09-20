use super::{parse, Api, Reply};
use couch_model::{ChildComponent, Id, Integration, PluginChildKind, Provider, SceneResource};
use couch_plugin::{Error, Failure, LocalRequest, Request, Response};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    fs,
    os::unix::{
        fs::{FileTypeExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    },
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

impl Api {
    pub(super) fn refresh_plugin_metadata(&self) {
        let ids = self.with(|s| {
            s.config()
                .connections
                .iter()
                .filter_map(|c| match &c.provider {
                    Provider::Plugin { id, .. } => Some(id.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
        });
        let manifests = self.plugins.changed_manifests(&ids);
        if manifests.is_empty() {
            return;
        }
        let mut store = self.store.lock().unwrap_or_else(|e| e.into_inner());
        let mut next = store.config().clone();
        let referenced = kinds_in_use(&next);
        for connection in &mut next.connections {
            if let Provider::Plugin {
                id,
                label,
                capabilities,
                supports_inputs,
                presentation,
                actions,
                children,
            } = &mut connection.provider
            {
                let Some(manifest) = manifests.iter().find(|manifest| &manifest.id == id) else {
                    continue;
                };
                *children =
                    kept_child_kinds(&manifest.children, children, &connection.id, &referenced);
                *label = manifest.label.clone();
                // Preserve old bindings when a package removes a capability.
                // The live manifest, not this display cache, gates execution.
                for capability in &manifest.capabilities {
                    if let Some(old) = capabilities.iter_mut().find(|old| old.id == capability.id) {
                        old.label = capability.label.clone();
                    } else {
                        capabilities.push(couch_model::PluginCapability {
                            id: capability.id.clone(),
                            label: capability.label.clone(),
                        });
                    }
                }
                *supports_inputs |= manifest.supports_inputs;
                *presentation = manifest.presentation.clone();
                *actions = manifest.actions.clone();
            }
        }
        if next.connections != store.config().connections {
            let _ = store.mutate(None, |cfg| cfg.connections = next.connections);
        }
    }
    pub(super) fn integration_route(&self, method: &str, path: &[&str], body: &[u8]) -> Reply {
        match (method, path) {
            ("GET", []) => match self.plugins.catalog() {
                Ok(integrations) => Reply::json(200, &json!({"integrations":integrations})),
                Err(_) => Reply::error(503, "Cannot read the installed integration catalog"),
            },
            _ => self.integration_package_route(method, path, body),
        }
    }

    fn plugin_id(&self, connection: &str) -> Option<String> {
        self.with(
            |s| match &s.config().connection(&Id::new(connection))?.provider {
                Provider::Plugin { id, .. } => Some(id.clone()),
                _ => None,
            },
        )
    }

    /// The kinds of child a packaged connection declares, as its saved
    /// snapshot has them. Empty for every other provider, and for every
    /// package a shipped build can run.
    pub(super) fn child_kinds(&self, connection: &Id) -> Vec<PluginChildKind> {
        self.with(
            |s| match s.config().connection(connection).map(|c| &c.provider) {
                Some(Provider::Plugin { children, .. }) => children.clone(),
                _ => Vec::new(),
            },
        )
    }

    /// Which kind of child this resource is, for a request aimed at one child
    /// of `connection`.
    ///
    /// The saved configuration answers first, so a device that is already in a
    /// room needs no listing and works with the cache cold. The cache answers
    /// for a child nothing has been made from yet, which is how the browser
    /// can try a lamp before adding it.
    ///
    /// Both are scoped to this connection and to nothing else. The connection
    /// id alone selects the package process (`Runtime::execute` keys its
    /// endpoints and its settings file on it), so a resource borrowed from
    /// another connection's device finds no kind here and is refused before
    /// any I/O; it can never be carried to another connection's package.
    pub(super) fn child_kind(&self, connection: &str, resource: &str) -> Option<String> {
        let id = Id::new(connection);
        self.with(|s| {
            let config = s.config();
            for (_, device) in config.devices() {
                let Some((connection_id, resource_id, Some(child))) = parts(&device.integration)
                else {
                    continue;
                };
                if connection_id == &id && resource_id == resource {
                    return Some(child.kind.clone());
                }
            }
            config.scenes.iter().find_map(|scene| {
                scene
                    .resource
                    .as_ref()
                    .filter(|r| r.connection_id == id && r.resource_id == resource)
                    .map(|r| r.kind.clone())
            })
        })
        .or_else(|| self.plugins.cached_child_kind(connection, resource))
    }

    fn plugin_request(&self, connection: &str, request: Request) -> Result<Response, Failure> {
        let id = self.plugin_id(connection).ok_or(Error::Invalid)?;
        // The panel's socket carries the resource inside the request, exactly
        // as HTTP does; the kind is the daemon's to derive, and never comes
        // from the caller.
        let kind = match request.resource() {
            None => None,
            Some(resource) => Some(
                self.child_kind(connection, resource)
                    .ok_or(Error::Invalid)?,
            ),
        };
        self.plugins
            .execute(connection, &id, kind.as_deref(), request)
    }

    /// The children of a connection, healing whatever a rollback stripped
    /// whenever the package is asked afresh.
    fn listing(
        &self,
        connection: &str,
        plugin: &str,
        refresh: bool,
    ) -> Result<crate::plugins::Children, Failure> {
        let listing = self.plugins.children(connection, plugin, refresh)?;
        if listing.fresh {
            self.heal_children(connection, &listing.children);
        }
        Ok(listing)
    }

    /// `GET …/plugin/children` and `POST …/plugin/children/refresh`.
    fn children_route(&self, connection: &str, plugin: &str, refresh: bool) -> Reply {
        let id = Id::new(connection);
        let kinds = self.child_kinds(&id);
        if kinds.is_empty() {
            return Reply::error(404, NO_CHILDREN);
        }
        let listing = match self.listing(connection, plugin, refresh) {
            Ok(listing) => listing,
            Err(failure) => return refused(status_for(failure.code), &failure),
        };
        // After healing, so a device that has just got its snapshot back is
        // reported the way it will be read from now on.
        let saved = self.saved_children(&id);
        let children: Vec<Value> = listing
            .children
            .iter()
            .map(|child| {
                let mut value = serde_json::to_value(child).unwrap_or_else(|_| json!({}));
                value["assigned"] = saved
                    .iter()
                    .find(|entry| entry.resource == child.id)
                    .map_or(Value::Null, |entry| entry.at.clone());
                value
            })
            .collect();
        // A child that has stopped being listed is never deleted and never
        // altered: the device, its keys and its scenes stay exactly as they
        // are. It is named here so the page can badge it.
        let missing: Vec<Value> = saved
            .iter()
            .filter(|entry| !listing.children.iter().any(|c| c.id == entry.resource))
            .map(|entry| {
                json!({"id": entry.resource, "kind": entry.kind, "name": entry.name,
                       "assigned": entry.at})
            })
            .collect();
        Reply::json(
            200,
            &json!({"kinds": kinds, "children": children, "missing": missing,
                    "fetched_ms": listing.age.as_millis() as u64}),
        )
    }

    /// Every device and package scene made from this connection's children.
    fn saved_children(&self, connection: &Id) -> Vec<Assigned> {
        self.with(|s| {
            let config = s.config();
            let mut saved = Vec::new();
            for (room, device) in config.devices() {
                let Some((connection_id, resource_id, child)) = parts(&device.integration) else {
                    continue;
                };
                if connection_id != connection || resource_id.is_empty() {
                    continue;
                }
                saved.push(Assigned {
                    resource: resource_id.to_owned(),
                    kind: child.map(|child| child.kind.clone()),
                    name: device.name.clone(),
                    at: json!({"room": room.id, "device": device.id}),
                });
            }
            for scene in &config.scenes {
                let Some(resource) = scene
                    .resource
                    .as_ref()
                    .filter(|r| &r.connection_id == connection)
                else {
                    continue;
                };
                saved.push(Assigned {
                    resource: resource.resource_id.clone(),
                    kind: Some(resource.kind.clone()),
                    name: scene.name.clone(),
                    at: json!({"scene": scene.id}),
                });
            }
            saved
        })
    }

    /// A Couch without children strips a device's snapshot on its way out and
    /// keeps the connection and the resource, so the next Couch that can read
    /// a listing knows what the device was. This puts the snapshot back.
    ///
    /// Only devices of the connection just listed are looked at, so a
    /// connection that offers no children - which is every connection in a
    /// shipped build - is never edited by this.
    fn heal_children(&self, connection: &str, children: &[couch_sdk::Child]) {
        let id = Id::new(connection);
        let kinds = self.child_kinds(&id);
        if kinds.is_empty() {
            return;
        }
        let mut store = self.store.lock().unwrap_or_else(|e| e.into_inner());
        let mut next = store.config().clone();
        for room in &mut next.rooms {
            for device in &mut room.devices {
                let (connection_id, resource_id, child) = match &mut device.integration {
                    Integration::Connection {
                        connection_id,
                        resource_id,
                        child,
                    }
                    | Integration::Plugin {
                        connection_id,
                        resource_id,
                        child,
                        ..
                    } => (connection_id, resource_id, child),
                    _ => continue,
                };
                if connection_id != &id || child.is_some() || resource_id.is_empty() {
                    continue;
                }
                let Some(listed) = children.iter().find(|c| &c.id == resource_id) else {
                    continue;
                };
                let Some(kind) = kinds.iter().find(|kind| kind.kind == listed.kind) else {
                    continue;
                };
                if kind.component == ChildComponent::Scene {
                    continue;
                }
                *child = Some(listed.snapshot());
                device.kind = kind.device_kind;
            }
        }
        if next.rooms != store.config().rooms {
            let _ = store.mutate(None, |config| config.rooms = next.rooms);
        }
    }

    /// Protocol 3 (unreleased). Fill in what a device that is one child of a
    /// packaged connection is, from the connection's own listing.
    ///
    /// The snapshot decides which commands validate and how the row is drawn,
    /// so it is never the browser's to describe: whatever the request carried
    /// under `child` is thrown away here, before anything else happens, and
    /// what comes back is what the package says. The browser sends
    /// `{"via":"connection","connection_id":…,"resource_id":…}` and a name.
    ///
    /// The device kind comes back with it, and the caller forces the device to
    /// be it: a lamp cannot be saved as a television.
    ///
    /// An edit that leaves the connection and the resource where they were
    /// keeps the snapshot already saved and asks nothing, so renaming a lamp
    /// works with its bridge unplugged. Anything else - a new device, a device
    /// moved to another child - reads the connection's children, from the
    /// cache when it is fresh.
    pub(super) fn stamp_device(
        &self,
        mut integration: Integration,
        saved: Option<&Integration>,
    ) -> Result<(Integration, Option<couch_model::DeviceKind>), Reply> {
        let (connection_id, resource_id, described) = match &mut integration {
            Integration::Connection {
                connection_id,
                resource_id,
                child,
            }
            | Integration::Plugin {
                connection_id,
                resource_id,
                child,
                ..
            } => (connection_id.clone(), resource_id.clone(), child.take()),
            _ => return Ok((integration, None)),
        };
        let kinds = self.child_kinds(&connection_id);
        // A connection that offers no children has no child to describe, and a
        // device of one that does but names no resource is the connection
        // itself. Neither is ever touched here - which is what keeps every
        // shipped build's devices exactly as they are.
        if kinds.is_empty() || resource_id.is_empty() {
            return Ok((integration, None));
        }
        // An edit that does not move the device to another child is a rename,
        // and a rename asks the package nothing at all: the device keeps what
        // it was saved as, whether that is a snapshot or - for one a rollback
        // stripped, which the next listing heals - nothing.
        let unchanged = saved.and_then(parts).and_then(|(was, had, child)| {
            (was == &connection_id && had == resource_id).then_some(child)
        });
        if let Some(snapshot) = unchanged {
            let device_kind = snapshot.and_then(|snapshot| {
                kinds
                    .iter()
                    .find(|kind| kind.kind == snapshot.kind)
                    .map(|kind| kind.device_kind)
            });
            set_child(&mut integration, snapshot.cloned());
            return Ok((integration, device_kind));
        }
        // What the request said this child is has been read by nothing.
        let _ = described;
        let plugin = match self.plugin_id(connection_id.as_str()) {
            Some(plugin) => plugin,
            None => return Err(Reply::error(400, NO_CHILDREN)),
        };
        let listed = match self.child_of(connection_id.as_str(), &plugin, &resource_id) {
            Ok(listed) => listed,
            Err(reply) => return Err(reply),
        };
        let Some(kind) = kinds.iter().find(|kind| kind.kind == listed.kind) else {
            return Err(Reply::error(400, UNKNOWN_CHILD));
        };
        if kind.component == ChildComponent::Scene {
            return Err(Reply::error(
                400,
                "A package scene belongs to a room's scenes, not to its devices",
            ));
        }
        set_child(&mut integration, Some(listed.snapshot()));
        Ok((integration, Some(kind.device_kind)))
    }

    /// The same for a package scene: the browser sends
    /// `{"connection_id":…,"resource_id":…}` and the daemon says which scene
    /// kind that is. A `kind` in the request is not read.
    pub(super) fn stamp_scene(
        &self,
        asked: Option<(Id, String)>,
        saved: Option<&SceneResource>,
    ) -> Result<Option<SceneResource>, Reply> {
        let Some((connection_id, resource_id)) = asked else {
            return Ok(None);
        };
        if let Some(saved) = saved.filter(|saved| {
            saved.connection_id == connection_id && saved.resource_id == resource_id
        }) {
            return Ok(Some(saved.clone()));
        }
        let kinds = self.child_kinds(&connection_id);
        if kinds.is_empty() {
            return Err(Reply::error(400, NO_CHILDREN));
        }
        let Some(plugin) = self.plugin_id(connection_id.as_str()) else {
            return Err(Reply::error(400, NO_CHILDREN));
        };
        let listed = self.child_of(connection_id.as_str(), &plugin, &resource_id)?;
        let scene = kinds
            .iter()
            .any(|kind| kind.kind == listed.kind && kind.component == ChildComponent::Scene);
        if !scene {
            return Err(Reply::error(
                400,
                "Choose a scene this integration offers; a package scene cannot include device steps or a Hue scene",
            ));
        }
        Ok(Some(SceneResource {
            connection_id,
            resource_id,
            kind: listed.kind,
        }))
    }

    /// One child of a connection, from the cache if it is fresh and from the
    /// package otherwise.
    fn child_of(
        &self,
        connection: &str,
        plugin: &str,
        resource: &str,
    ) -> Result<couch_sdk::Child, Reply> {
        if let Some(child) = self.plugins.cached_child(connection, resource) {
            return Ok(child);
        }
        match self.listing(connection, plugin, false) {
            Ok(listing) => listing
                .children
                .into_iter()
                .find(|child| child.id == resource)
                .ok_or_else(|| Reply::error(400, UNKNOWN_CHILD)),
            Err(failure) => Err(refused(status_for(failure.code), &failure)),
        }
    }

    pub(super) fn plugin_route(
        &self,
        method: &str,
        connection: &str,
        path: &[&str],
        body: &[u8],
    ) -> Reply {
        let Some(id) = self.plugin_id(connection) else {
            return Reply::error(404, "Integration connection not found");
        };
        if path == ["settings"] {
            let result = match method {
                "GET" => self
                    .plugins
                    .settings(connection, &id)
                    .map_err(crate::plugins::Refusal::from),
                "POST" | "PUT" => {
                    let value = match parse(body) {
                        Ok(value) => value,
                        Err(reply) => return reply,
                    };
                    self.plugins.save_settings(connection, &id, value)
                }
                _ => return Reply::error(405, "Use GET or POST for integration settings"),
            };
            return match result {
                Ok(value) => Reply::json(200, &value),
                Err(refusal) => match &refusal.failure {
                    Some(failure) => refused(400, failure),
                    None => Reply::error(400, refusal.text),
                },
            };
        }
        // Protocol 3 (unreleased). One child of this connection. The router
        // has no query strings, so the child's id is the path between
        // `children` and the verb, which is always the last segment; empty
        // segments never reach here, and the resource grammar refuses `.` and
        // `..`, so an id can never climb out of its connection.
        if let ["children", rest @ ..] = path {
            return match (method, rest) {
                ("GET", []) => self.children_route(connection, &id, false),
                ("POST", ["refresh"]) => self.children_route(connection, &id, true),
                _ => match rest.split_last() {
                    Some((verb, segments)) if !segments.is_empty() => {
                        self.child_route(method, connection, &id, &segments.join("/"), verb, body)
                    }
                    _ => Reply::error(404, "Unknown integration operation"),
                },
            };
        }
        let request = match (method, path) {
            ("GET", ["status"]) => Request::status(),
            ("GET", ["inputs"]) => Request::Inputs,
            ("POST", ["typed-action"]) => {
                let action: couch_model::TypedAction = match parse(body) {
                    Ok(value) => value,
                    Err(reply) => return reply,
                };
                Request::action(action)
            }
            ("POST", ["action"]) => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Action {
                    command: String,
                }
                let input: Action = match parse(body) {
                    Ok(value) => value,
                    Err(reply) => return reply,
                };
                Request::command(input.command)
            }
            _ => return Reply::error(404, "Unknown integration operation"),
        };
        answered(self.plugin_request(connection, request))
    }

    /// `…/plugin/children/<id…>/{status,action,typed-action}`.
    fn child_route(
        &self,
        method: &str,
        connection: &str,
        plugin: &str,
        resource: &str,
        verb: &str,
        body: &[u8],
    ) -> Reply {
        if self.child_kinds(&Id::new(connection)).is_empty() {
            return Reply::error(404, NO_CHILDREN);
        }
        if !couch_model::valid_resource(resource) {
            return Reply::error(404, UNKNOWN_CHILD);
        }
        let Some(kind) = self.child_kind(connection, resource) else {
            return Reply::error(404, UNKNOWN_CHILD);
        };
        let request = match (method, verb) {
            ("GET", "status") => Request::status(),
            ("POST", "typed-action") => {
                let action: couch_model::TypedAction = match parse(body) {
                    Ok(value) => value,
                    Err(reply) => return reply,
                };
                Request::action(action)
            }
            ("POST", "action") => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Action {
                    command: String,
                }
                let input: Action = match parse(body) {
                    Ok(value) => value,
                    Err(reply) => return reply,
                };
                Request::command(input.command)
            }
            _ => return Reply::error(404, "Unknown integration operation"),
        };
        answered(
            self.plugins
                .execute(connection, plugin, Some(&kind), request.at(resource)),
        )
    }

    /// Same-uid, owner-only socket. GUI commands pass through the exact same
    /// endpoint registry and capability gate as authenticated HTTP commands.
    pub fn serve_plugins(self: &Arc<Self>) -> std::io::Result<()> {
        let path = self.with(|s| s.path().with_file_name("plugin.sock"));
        if let Ok(metadata) = fs::symlink_metadata(&path) {
            if !metadata.file_type().is_socket() {
                return Err(std::io::ErrorKind::AlreadyExists.into());
            }
            if UnixStream::connect(&path).is_ok() {
                return Err(std::io::ErrorKind::AddrInUse.into());
            }
            fs::remove_file(&path)?;
        }
        let listener = UnixListener::bind(path)?;
        fs::set_permissions(
            self.with(|s| s.path().with_file_name("plugin.sock")),
            fs::Permissions::from_mode(0o600),
        )?;
        let api = self.clone();
        let active = Arc::new(AtomicUsize::new(0));
        std::thread::Builder::new()
            .name("integration-socket".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    let Ok(mut stream) = stream else { continue };
                    if !same_uid(&stream) || active.load(Ordering::SeqCst) >= 8 {
                        continue;
                    }
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
                    let api = api.clone();
                    let count = active.clone();
                    count.fetch_add(1, Ordering::SeqCst);
                    let spawned = std::thread::Builder::new()
                        .name("integration-request".into())
                        .spawn(move || {
                            relay(&mut stream, |request| {
                                api.plugin_request(&request.connection_id, request.request)
                            });
                            count.fetch_sub(1, Ordering::SeqCst);
                        });
                    if spawned.is_err() {
                        active.fetch_sub(1, Ordering::SeqCst);
                    }
                }
            })?;
        let weak = Arc::downgrade(self);
        std::thread::Builder::new()
            .name("integration-reaper".into())
            .spawn(move || loop {
                std::thread::sleep(Duration::from_secs(10));
                let Some(api) = weak.upgrade() else { break };
                api.plugins.reap();
            })?;
        Ok(())
    }
}

/// What every route under `…/plugin/children` says when the connection's
/// package offers no children, which is every package a shipped build runs.
const NO_CHILDREN: &str = "This integration does not list devices";
const UNKNOWN_CHILD: &str = "This integration does not offer that device";

/// One device or package scene made from a child of a connection.
struct Assigned {
    resource: String,
    kind: Option<String>,
    name: String,
    /// `{"room":…,"device":…}` or `{"scene":…}`.
    at: Value,
}

/// The connection, resource and snapshot of a device that refers to a
/// connection, in either saved form. Everything else refers to none.
fn parts(integration: &Integration) -> Option<(&Id, &str, Option<&couch_model::ChildSnapshot>)> {
    match integration {
        Integration::Connection {
            connection_id,
            resource_id,
            child,
        }
        | Integration::Plugin {
            connection_id,
            resource_id,
            child,
            ..
        } => Some((connection_id, resource_id, child.as_ref())),
        _ => None,
    }
}

fn set_child(integration: &mut Integration, snapshot: Option<couch_model::ChildSnapshot>) {
    if let Integration::Connection { child, .. } | Integration::Plugin { child, .. } = integration {
        *child = snapshot;
    }
}

/// The kinds of child a connection's snapshot holds after its package has
/// been updated: the manifest's, and any the manifest has dropped that a saved
/// device or scene still says it is.
///
/// A saved snapshot is validated against this list, so a kind forgotten while
/// a device still refers to it would stop the daemon starting over a device
/// nobody had touched. The kept kind says what that device can be told; the
/// package's own manifest, not this display cache, still gates execution, and
/// the package will refuse a child it no longer has.
fn kept_child_kinds(
    manifest: &[PluginChildKind],
    saved: &[PluginChildKind],
    connection: &Id,
    referenced: &std::collections::HashSet<(Id, String)>,
) -> Vec<PluginChildKind> {
    let mut kinds = manifest.to_vec();
    for kind in saved {
        if kinds.iter().any(|next| next.kind == kind.kind) {
            continue;
        }
        if referenced.contains(&(connection.clone(), kind.kind.clone())) {
            kinds.push(kind.clone());
        }
    }
    kinds
}

/// Every (connection, kind of child) a saved device or scene still refers to.
fn kinds_in_use(config: &couch_model::Config) -> std::collections::HashSet<(Id, String)> {
    let mut used = std::collections::HashSet::new();
    for (_, device) in config.devices() {
        if let Some((connection_id, _, Some(child))) = parts(&device.integration) {
            used.insert((connection_id.clone(), child.kind.clone()));
        }
    }
    for scene in &config.scenes {
        if let Some(resource) = &scene.resource {
            used.insert((resource.connection_id.clone(), resource.kind.clone()));
        }
    }
    used
}

/// What an integration request answers with, over HTTP.
fn answered(result: Result<Response, Failure>) -> Reply {
    match result {
        Ok(Response::Ok) => Reply::json(200, &json!({"accepted":true})),
        Ok(Response::Status { status }) => Reply::json(200, &status),
        Ok(Response::Inputs { inputs }) => Reply::json(200, &inputs),
        Ok(_) => Reply::error(502, "Invalid integration reply"),
        Err(failure) => refused(status_for(failure.code), &failure),
    }
}

/// The HTTP status for a refused integration request. `unpaired` is a conflict
/// with the state of the device, which only pairing again resolves; it is not
/// a bad gateway.
fn status_for(code: Error) -> u16 {
    match code {
        Error::Invalid | Error::Unsupported => 400,
        Error::Unpaired => 409,
        Error::Busy | Error::Expired => 503,
        _ => 502,
    }
}

/// `error` is the sentence every client already shows. `code` and, from a
/// protocol 3 package, `reason` sit beside it for a client that can do better:
/// mark the setting a reason names, or offer pairing for `unpaired`.
fn refused(status: u16, failure: &Failure) -> Reply {
    let mut body = json!({"error": failure.to_string(), "code": failure.code});
    if let Some(reason) = &failure.reason {
        body["reason"] = json!(reason);
    }
    Reply::json(status, &body)
}

/// One request from the panel, answered on its own stream. A refusal goes back
/// whole: the code and, when a protocol 3 package gave one, its reason.
fn relay(stream: &mut UnixStream, execute: impl FnOnce(LocalRequest) -> Result<Response, Failure>) {
    let response =
        match couch_plugin::read_frame_timeout::<LocalRequest>(stream, Duration::from_secs(2)) {
            Ok(request) => execute(request).unwrap_or_else(Response::error),
            Err(code) => Response::error(code),
        };
    let _ = couch_plugin::write_frame_timeout(stream, &response, Duration::from_secs(2));
}

fn same_uid(stream: &UnixStream) -> bool {
    use std::os::fd::AsRawFd;
    #[cfg(target_os = "linux")]
    unsafe {
        let mut peer: libc::ucred = std::mem::zeroed();
        let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut peer as *mut libc::ucred).cast(),
            &mut length,
        ) == 0
            && peer.uid == libc::geteuid()
    }
    #[cfg(any(target_os = "macos", target_os = "freebsd"))]
    unsafe {
        let mut uid = 0;
        let mut gid = 0;
        libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) == 0 && uid == libc::geteuid()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "freebsd")))]
    {
        let _ = stream;
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use couch_plugin::Reason;
    use serde_json::Value;

    fn body(reply: &Reply) -> Value {
        serde_json::from_slice(&reply.body).unwrap()
    }

    #[test]
    fn a_refusal_over_http_keeps_its_sentence_and_gains_a_code_and_a_reason() {
        // Every code a package can send today: the status and the sentence are
        // the ones they were, and `code` is new beside them.
        for (code, status, word) in [
            (Error::Invalid, 400, "invalid"),
            (Error::Unsupported, 400, "unsupported"),
            (Error::Busy, 503, "busy"),
            (Error::Expired, 503, "expired"),
            (Error::Incompatible, 502, "incompatible"),
            (Error::Protocol, 502, "protocol"),
            (Error::Transport, 502, "transport"),
            (Error::Timeout, 502, "timeout"),
            (Error::Rejected, 502, "rejected"),
        ] {
            let reply = refused(status_for(code), &code.into());
            assert_eq!(reply.status, status, "{word}");
            assert_eq!(
                body(&reply),
                json!({"error": code.to_string(), "code": word}),
                "{word}"
            );
        }
        // Protocol 3, which no shipped build lets a package speak.
        let unpaired = Failure {
            code: Error::Unpaired,
            reason: Some(Reason::Message {
                text: "Pair this TV again".into(),
            }),
        };
        let reply = refused(status_for(unpaired.code), &unpaired);
        assert_eq!(reply.status, 409);
        assert_eq!(
            body(&reply),
            json!({"error":"Pair this TV again","code":"unpaired",
                "reason":{"kind":"message","text":"Pair this TV again"}})
        );
        let port = Failure {
            code: Error::Invalid,
            reason: Some(Reason::InvalidSetting {
                field: "port".into(),
                text: "The port must not be 0".into(),
            }),
        };
        assert_eq!(
            body(&refused(400, &port)),
            json!({"error":"The port must not be 0","code":"invalid",
                "reason":{"kind":"invalid_setting","field":"port","text":"The port must not be 0"}})
        );
    }

    #[test]
    fn the_panel_is_told_the_reason_and_a_refusal_without_one_is_the_frame_it_always_was() {
        let directory = std::env::temp_dir().join(format!("couch-relay-{}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        let socket = directory.join("p.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let locked = Failure {
            code: Error::Rejected,
            reason: Some(Reason::Message {
                text: "The TV is locked".into(),
            }),
        };
        let answers = [Err(locked.clone()), Err(Error::Unsupported.into())];
        let server = std::thread::spawn(move || {
            // Answered streams stay open until the end: macOS refuses to set a
            // deadline on a socket whose peer has already gone, which the
            // asking side does before it reads.
            let mut answered = Vec::new();
            for answer in answers {
                let (mut stream, _) = listener.accept().unwrap();
                relay(&mut stream, |request| {
                    assert_eq!(request.connection_id, "tv");
                    // How the key was pressed arrives with it; the host, not
                    // this socket, decides whether the package is told.
                    assert!(matches!(
                        request.request,
                        Request::Command { ref function, phase: couch_plugin::KeyPhase::Repeat, .. }
                            if function == "power-off"
                    ));
                    answer
                });
                answered.push(stream);
            }
            // The last one by hand, to see the bytes the panel is sent.
            let (mut stream, _) = listener.accept().unwrap();
            relay(&mut stream, |_| Err(Error::Unsupported.into()));
        });
        let ask = || {
            couch_plugin::local_request_detailed(
                &socket,
                "tv",
                Request::key("power-off", couch_plugin::KeyPhase::Repeat),
                Duration::from_secs(2),
            )
        };
        assert_eq!(ask(), Err(locked));
        assert_eq!(ask(), Err(Error::Unsupported.into()));
        let mut stream = UnixStream::connect(&socket).unwrap();
        couch_plugin::write_frame(
            &mut stream,
            &LocalRequest {
                connection_id: "tv".into(),
                request: Request::command("power-off"),
            },
        )
        .unwrap();
        let frame: Value = couch_plugin::read_frame(&mut stream).unwrap();
        assert_eq!(
            frame.to_string(),
            r#"{"code":"unsupported","type":"error"}"#
        );
        server.join().unwrap();
        let _ = fs::remove_dir_all(directory);
    }
}

/// Protocol 3 (unreleased): the daemon's side of the children of a connection
/// - the routes, what a device that is one saves, and how it gets that back.
///
/// The listing itself is replaced here (`Runtime::list_with`): `daemon` can
/// never run a protocol 3 package, which is the point of the guard test in
/// `plugins.rs`, and every way a real bridge can answer nonsense is tested
/// against the real thing in `clients/`. The configurations below are ones no
/// shipped build can come to hold, because no manifest it accepts may declare
/// a kind of child.
#[cfg(test)]
mod children_tests {
    use super::*;
    use crate::{assets::Assets, auth::Auth, store::Store};
    use couch_model::{ChildSnapshot, Config, CoverTraits, DeviceKind, LightTraits};
    use couch_sdk::Child;
    use std::path::PathBuf;

    fn lamp(id: &str, name: &str) -> Child {
        Child::new(id, "light", name)
            .in_room("Study")
            .with_light(LightTraits {
                dimmable: true,
                ..Default::default()
            })
    }
    fn bridge_children() -> Vec<Child> {
        vec![
            lamp("lamp/1", "Desk"),
            lamp("lamp/2", "Reading"),
            Child::new("scene/1", "scene", "Relax"),
        ]
    }
    fn blind_children() -> Vec<Child> {
        vec![
            Child::new("lamp/1", "blind", "Kitchen blind").with_cover(CoverTraits {
                position: true,
                stop: false,
            }),
        ]
    }

    /// Two packaged connections that offer children, one that offers none,
    /// and a listing in place of the packages.
    fn fixture(name: &str) -> (PathBuf, Api) {
        let dir = std::env::temp_dir().join(format!(
            "couch-api-children-{name}-{}-{:?}",
            std::process::id(),
            std::time::Instant::now()
        ));
        fs::create_dir_all(&dir).unwrap();
        let mut config = serde_json::to_value(Config::seed()).unwrap();
        config["connections"] = json!([
            {"id": "bridge", "name": "Bridge", "provider": {
                "kind": "plugin", "id": "echo", "label": "Echo", "children": [
                    {"kind": "light", "label": "Light", "device_kind": "light",
                     "component": "light", "capabilities": [{"id": "toggle", "label": "Toggle"}],
                     "actions": [{"action": "set_light"}]},
                    {"kind": "scene", "label": "Scene", "device_kind": "other",
                     "component": "scene", "capabilities": [{"id": "on", "label": "On"}]}]}},
            {"id": "blinds", "name": "Blinds", "provider": {
                "kind": "plugin", "id": "shades", "label": "Shades", "children": [
                    {"kind": "blind", "label": "Blind", "device_kind": "blind",
                     "component": "cover", "capabilities": [{"id": "toggle", "label": "Toggle"}],
                     "actions": [{"action": "set_cover"}]}]}},
            {"id": "plain", "name": "Receiver", "provider": {
                "kind": "plugin", "id": "denon", "label": "Denon"}}
        ]);
        // A lamp of the bridge, and a blind the other connection calls by the
        // same name. Nothing may carry one connection's name to the other.
        config["rooms"][0]["devices"][4]["integration"] = json!({"via": "connection",
            "connection_id": "bridge", "resource_id": "lamp/1",
            "child": {"kind": "light", "light": {"dimmable": true}}});
        config["rooms"][2]["devices"][2]["integration"] = json!({"via": "connection",
            "connection_id": "blinds", "resource_id": "lamp/1",
            "child": {"kind": "blind", "cover": {"position": true}}});
        let config: Config = serde_json::from_value(config).unwrap();
        config.validate().unwrap();
        fs::write(
            dir.join("config.json"),
            serde_json::to_vec(&couch_model::StoredConfig::new(&config)).unwrap(),
        )
        .unwrap();
        let api = Api::new(
            Store::open(dir.join("config.json")).unwrap(),
            Assets::embedded(),
            Arc::new(Auth::new(dir.join("pin"), true)),
        );
        api.plugins
            .list_with(Box::new(|connection| match connection {
                "bridge" => Ok(bridge_children()),
                "blinds" => Ok(blind_children()),
                _ => Err(Error::Unsupported.into()),
            }));
        (dir, api)
    }

    fn body(reply: &Reply) -> Value {
        serde_json::from_slice(&reply.body).unwrap()
    }
    fn device(api: &Api, id: &str) -> Value {
        api.with(|s| {
            let config = s.config();
            let found = config.devices().find(|(_, d)| d.id.as_str() == id);
            serde_json::to_value(found.expect("device").1).unwrap()
        })
    }
    fn create(api: &Api, room: &str, value: Value) -> Reply {
        api.create_device(value.to_string().as_bytes(), None, room)
    }

    #[test]
    fn a_device_is_described_by_its_package_and_never_by_the_request() {
        let (dir, api) = fixture("stamp");
        // Everything the request says about the child is thrown away: the
        // snapshot decides which keys validate and how the row is drawn, so
        // it is the package's to give. The browser sends the connection, the
        // resource and a name.
        let asked = json!({"name": "Desk lamp", "kind": "tv", "integration": {
            "via": "connection", "connection_id": "bridge", "resource_id": "lamp/1",
            "child": {"kind": "scene", "light": {"dimmable": true, "color": true}}}});
        let reply = create(&api, "kitchen", asked);
        assert_eq!(reply.status, 200, "{}", body(&reply));
        let created = reply.created.clone().unwrap();
        let saved = device(&api, &created);
        assert_eq!(
            saved["integration"],
            json!({"via": "connection", "connection_id": "bridge", "resource_id": "lamp/1",
                   "child": {"kind": "light", "light": {"dimmable": true}}})
        );
        // ...and a lamp cannot be saved as a television.
        assert_eq!(saved["kind"], "light");

        // A child the connection does not offer, and one that is a scene.
        for resource in ["lamp/9", "scene/1"] {
            let reply = create(
                &api,
                "kitchen",
                json!({"name": "Nope", "integration": {"via": "connection",
                    "connection_id": "bridge", "resource_id": resource}}),
            );
            assert_eq!(reply.status, 400, "{resource}");
        }
        // The same device on a connection that offers no children at all is
        // saved exactly as it always was, with no snapshot and no listing.
        let reply = create(
            &api,
            "kitchen",
            json!({"name": "Zone 2", "kind": "speaker", "integration": {"via": "connection",
                "connection_id": "plain", "resource_id": "zone2",
                "child": {"kind": "light", "light": {"dimmable": true}}}}),
        );
        assert_eq!(reply.status, 200);
        let zone = device(&api, &reply.created.clone().unwrap());
        assert_eq!(zone["kind"], "speaker");
        assert!(zone["integration"].get("child").is_none());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_rename_keeps_the_snapshot_with_the_package_stopped_and_moving_reads_it_again() {
        let (dir, api) = fixture("rename");
        let saved = device(&api, "living-lamp")["integration"].clone();
        api.plugins
            .list_with(Box::new(|_| Err(Error::Transport.into())));

        let mut edit = device(&api, "living-lamp");
        edit["name"] = "Reading lamp".into();
        let reply = api.replace_device(
            edit.to_string().as_bytes(),
            None,
            "living-room",
            "living-lamp",
        );
        assert_eq!(reply.status, 200, "{}", body(&reply));
        assert_eq!(device(&api, "living-lamp")["name"], "Reading lamp");
        assert_eq!(device(&api, "living-lamp")["integration"], saved);

        // Nor can an edit change the snapshot by sending a different one back.
        let mut edit = device(&api, "living-lamp");
        edit["integration"]["child"]["light"]["color"] = true.into();
        edit["kind"] = "tv".into();
        assert_eq!(
            api.replace_device(
                edit.to_string().as_bytes(),
                None,
                "living-room",
                "living-lamp"
            )
            .status,
            200
        );
        assert_eq!(device(&api, "living-lamp")["integration"], saved);
        assert_eq!(device(&api, "living-lamp")["kind"], "light");

        // A device a rollback stripped has no snapshot yet. Renaming it is
        // still a rename: it is not refused because the bridge is down, and
        // the next listing is what heals it.
        {
            let mut store = api.store.lock().unwrap();
            store
                .mutate(None, |config| {
                    config.rooms[0].devices[4].integration = serde_json::from_value(
                        json!({"via": "connection", "connection_id": "bridge",
                               "resource_id": "lamp/1"}),
                    )
                    .unwrap();
                })
                .unwrap();
        }
        let mut edit = device(&api, "living-lamp");
        edit["name"] = "Corner lamp".into();
        assert_eq!(
            api.replace_device(
                edit.to_string().as_bytes(),
                None,
                "living-room",
                "living-lamp"
            )
            .status,
            200
        );
        assert!(device(&api, "living-lamp")["integration"]
            .get("child")
            .is_none());
        {
            let mut store = api.store.lock().unwrap();
            store
                .mutate(None, |config| {
                    config.rooms[0].devices[4].integration =
                        serde_json::from_value(saved.clone()).unwrap();
                })
                .unwrap();
        }

        // Pointing the device at another child is not a rename: the package
        // is asked, and it is not there.
        let mut edit = device(&api, "living-lamp");
        edit["integration"]["resource_id"] = "lamp/2".into();
        let reply = api.replace_device(
            edit.to_string().as_bytes(),
            None,
            "living-room",
            "living-lamp",
        );
        assert_eq!(reply.status, 502);
        assert_eq!(body(&reply)["code"], "transport");
        assert_eq!(device(&api, "living-lamp")["integration"], saved);

        // With the bridge back, it is read again.
        api.plugins.list_with(Box::new(|_| Ok(bridge_children())));
        let mut edit = device(&api, "living-lamp");
        edit["integration"]["resource_id"] = "lamp/2".into();
        assert_eq!(
            api.replace_device(
                edit.to_string().as_bytes(),
                None,
                "living-room",
                "living-lamp"
            )
            .status,
            200
        );
        assert_eq!(
            device(&api, "living-lamp")["integration"]["resource_id"],
            "lamp/2"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_device_a_rollback_stripped_heals_on_the_next_listing_and_nothing_else_is_touched() {
        let (dir, api) = fixture("heal");
        // What a Couch without children leaves behind: the connection and the
        // resource, no snapshot. Beside it, a device of a connection that
        // offers no children, which must never be touched by this.
        let stripped = json!({"via": "connection", "connection_id": "bridge",
            "resource_id": "lamp/2"});
        let plain = json!({"via": "connection", "connection_id": "plain", "resource_id": "zone1"});
        {
            let mut store = api.store.lock().unwrap();
            store
                .mutate(None, |config| {
                    let room = config.room_mut(&Id::new("kitchen")).unwrap();
                    room.devices[1].integration = serde_json::from_value(stripped).unwrap();
                    room.devices[1].kind = DeviceKind::Other;
                    room.devices[0].integration = serde_json::from_value(plain.clone()).unwrap();
                })
                .unwrap();
        }
        let reply = api.plugin_route("GET", "bridge", &["children"], b"");
        assert_eq!(reply.status, 200, "{}", body(&reply));
        let healed = device(&api, "kitchen-hue");
        assert_eq!(
            healed["integration"],
            json!({"via": "connection", "connection_id": "bridge", "resource_id": "lamp/2",
                   "child": {"kind": "light", "light": {"dimmable": true}}})
        );
        assert_eq!(healed["kind"], "light");
        assert_eq!(device(&api, "kitchen-sonos")["integration"], plain);
        // It is reported as assigned from the same listing that healed it.
        let listed = body(&reply);
        let entry = listed["children"]
            .as_array()
            .unwrap()
            .iter()
            .find(|child| child["id"] == "lamp/2")
            .unwrap()
            .clone();
        assert_eq!(
            entry["assigned"],
            json!({"room": "kitchen", "device": "kitchen-hue"})
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_child_that_stops_being_listed_is_named_and_never_deleted() {
        let (dir, api) = fixture("missing");
        let before = device(&api, "living-lamp");
        api.plugins
            .list_with(Box::new(|_| Ok(vec![lamp("lamp/2", "Reading")])));
        let reply = api.plugin_route("POST", "bridge", &["children", "refresh"], b"");
        assert_eq!(reply.status, 200, "{}", body(&reply));
        let listed = body(&reply);
        assert!(!listed["children"]
            .as_array()
            .unwrap()
            .iter()
            .any(|child| child["id"] == "lamp/1"));
        assert_eq!(
            listed["missing"],
            json!([{"id": "lamp/1", "kind": "light", "name": "Corner lamp",
                    "assigned": {"room": "living-room", "device": "living-lamp"}}])
        );
        // The device, its keys and its scenes stay exactly as they are.
        assert_eq!(device(&api, "living-lamp"), before);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn the_listing_route_says_what_is_offered_what_is_taken_and_how_old_it_is() {
        let (dir, api) = fixture("routes");
        let reply = api.plugin_route("GET", "bridge", &["children"], b"");
        assert_eq!(reply.status, 200);
        let listed = body(&reply);
        assert_eq!(listed["kinds"].as_array().unwrap().len(), 2);
        assert_eq!(listed["kinds"][0]["label"], "Light");
        assert_eq!(listed["kinds"][0]["device_kind"], "light");
        assert_eq!(listed["kinds"][0]["component"], "light");
        assert_eq!(
            listed["children"][0],
            json!({"id": "lamp/1", "kind": "light", "name": "Desk", "room_hint": "Study",
                   "light": {"dimmable": true},
                   "assigned": {"room": "living-room", "device": "living-lamp"}})
        );
        assert_eq!(listed["children"][1]["assigned"], Value::Null);
        assert_eq!(listed["missing"], json!([]));
        assert_eq!(listed["fetched_ms"], 0);
        // Read again, it is the same listing and says how old it is.
        assert_eq!(
            body(&api.plugin_route("GET", "bridge", &["children"], b""))["children"],
            listed["children"]
        );

        // A connection whose package offers no children has no such routes.
        for path in [vec!["children"], vec!["children", "lamp", "1", "status"]] {
            let reply = api.plugin_route("GET", "plain", &path, b"");
            assert_eq!(reply.status, 404);
            assert_eq!(body(&reply)["error"], NO_CHILDREN);
        }
        // Nor does a connection that is not a package's at all.
        assert_eq!(
            api.plugin_route("GET", "living-room", &["children"], b"")
                .status,
            404
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_resource_never_reaches_another_connections_package() {
        let (dir, api) = fixture("scope");
        // Both connections have a child called `lamp/1`, and they are not the
        // same thing. The kind comes from the configuration of the connection
        // the request named and from nowhere else; the connection id alone
        // selects the package process and its private settings, so a resource
        // borrowed from another connection has nothing to travel on.
        assert_eq!(api.child_kind("bridge", "lamp/1").as_deref(), Some("light"));
        assert_eq!(api.child_kind("blinds", "lamp/1").as_deref(), Some("blind"));
        // The bridge's scene is nothing to the blinds, so the request is
        // refused before the blinds' package is started at all.
        assert_eq!(api.child_kind("bridge", "scene/1"), None);
        let reply = api.plugin_route("GET", "blinds", &["children", "scene", "1", "status"], b"");
        assert_eq!(reply.status, 404);
        assert_eq!(body(&reply)["error"], UNKNOWN_CHILD);
        // Listing the bridge does not teach the blinds anything.
        assert_eq!(
            api.plugin_route("GET", "bridge", &["children"], b"").status,
            200
        );
        assert_eq!(
            api.child_kind("bridge", "scene/1").as_deref(),
            Some("scene")
        );
        assert_eq!(api.child_kind("blinds", "scene/1"), None);
        assert_eq!(
            api.plugin_route("GET", "blinds", &["children", "scene", "1", "status"], b"")
                .status,
            404
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_child_can_be_tried_before_it_is_added_and_an_unknown_one_never_starts_a_package() {
        let (dir, api) = fixture("try");
        let status = |api: &Api, path: &[&str]| api.plugin_route("GET", "bridge", path, b"");
        // Nothing has been made from `lamp/2`, and nothing has listed yet.
        assert_eq!(
            status(&api, &["children", "lamp", "2", "status"]).status,
            404
        );
        assert_eq!(
            api.plugin_route("GET", "bridge", &["children"], b"").status,
            200
        );
        // Now the listing knows it, and the request gets as far as the
        // package - which no `daemon` test has, so it is refused there.
        let reply = status(&api, &["children", "lamp", "2", "status"]);
        assert_eq!(reply.status, 400);
        assert_eq!(body(&reply)["code"], "invalid");
        // A device that is saved needs no listing at all.
        let reply = status(&api, &["children", "lamp", "1", "status"]);
        assert_eq!(reply.status, 400);
        assert_eq!(body(&reply)["code"], "invalid");

        // An id that would climb out of its connection is no id.
        for path in [
            vec!["children", "..", "x", "status"],
            vec!["children", ".", "status"],
        ] {
            assert_eq!(status(&api, &path).status, 404, "{path:?}");
        }
        // The verb is always the last segment, and there are only three.
        for (method, path) in [
            ("GET", vec!["children", "lamp", "1", "nonsense"]),
            ("POST", vec!["children", "lamp", "1", "status"]),
            ("GET", vec!["children", "lamp", "1", "action"]),
            ("GET", vec!["children", "refresh"]),
            ("POST", vec!["children"]),
        ] {
            let reply = api.plugin_route(method, "bridge", &path, b"");
            assert_eq!(reply.status, 404, "{method} {path:?}");
        }
        // A body that is not a command, and one that is.
        let reply = api.plugin_route(
            "POST",
            "bridge",
            &["children", "lamp", "1", "action"],
            br#"{"command":"toggle","extra":1}"#,
        );
        assert_eq!(reply.status, 400);
        for (verb, body_bytes) in [
            ("action", &br#"{"command":"toggle"}"#[..]),
            (
                "typed-action",
                &br#"{"action":"set_light","brightness":30}"#[..],
            ),
        ] {
            let reply = api.plugin_route(
                "POST",
                "bridge",
                &["children", "lamp", "1", verb],
                body_bytes,
            );
            assert_eq!(reply.status, 400, "{verb}");
            assert_eq!(body(&reply)["code"], "invalid", "{verb}");
        }
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_package_scene_takes_its_kind_from_the_package_too() {
        let (dir, api) = fixture("scenes");
        let asked = json!({"name": "Relax", "rooms": ["living-room"],
            "resource": {"connection_id": "bridge", "resource_id": "scene/1", "kind": "light"}});
        let reply = api.create_scene(asked.to_string().as_bytes(), None);
        assert_eq!(reply.status, 200, "{}", body(&reply));
        let id = Id::new(reply.created.clone().unwrap());
        let saved = api.with(|s| s.config().scene(&id).unwrap().resource.clone());
        // The kind in the request was not read: this is the scene kind the
        // package declares, which is the only thing `on` can be sent to.
        assert_eq!(
            serde_json::to_value(&saved).unwrap(),
            json!({"connection_id": "bridge", "resource_id": "scene/1", "kind": "scene"})
        );
        // A lamp is not a scene, and a connection that offers nothing has no
        // scene to name.
        for resource in [
            json!({"connection_id": "bridge", "resource_id": "lamp/1"}),
            json!({"connection_id": "bridge", "resource_id": "scene/9"}),
            json!({"connection_id": "plain", "resource_id": "scene/1"}),
        ] {
            let asked = json!({"name": "No", "rooms": [], "resource": resource});
            assert_eq!(
                api.create_scene(asked.to_string().as_bytes(), None).status,
                400,
                "{asked}"
            );
        }
        // Renaming it with the package stopped keeps what it is.
        api.plugins
            .list_with(Box::new(|_| Err(Error::Transport.into())));
        let edit = json!({"name": "Unwind", "rooms": ["living-room"],
            "resource": {"connection_id": "bridge", "resource_id": "scene/1", "kind": "scene"}});
        assert_eq!(
            api.replace_scene(edit.to_string().as_bytes(), None, id.as_str())
                .status,
            200
        );
        assert_eq!(
            api.with(|s| s.config().scene(&id).unwrap().resource.clone()),
            saved
        );
        // A scene with no resource is the scene it always was.
        let plain = json!({"name": "Evening", "rooms": ["living-room"]});
        let reply = api.create_scene(plain.to_string().as_bytes(), None);
        assert_eq!(reply.status, 200);
        assert_eq!(
            api.with(|s| s
                .config()
                .scene(&Id::new(reply.created.clone().unwrap()))
                .unwrap()
                .resource
                .clone()),
            None
        );
        let _ = fs::remove_dir_all(dir);
    }

    /// A package update that drops a kind must not make a device that still
    /// says it is one fail validation, which would stop the daemon.
    #[test]
    fn a_kind_a_package_update_drops_is_kept_while_anything_still_refers_to_it() {
        let (dir, api) = fixture("kinds");
        let bridge = Id::new("bridge");
        let saved = api.child_kinds(&bridge);
        assert_eq!(saved.len(), 2);
        let referenced = api.with(|s| kinds_in_use(s.config()));
        assert!(referenced.contains(&(bridge.clone(), "light".to_owned())));
        assert!(referenced.contains(&(Id::new("blinds"), "blind".to_owned())));

        // The update declares neither kind. The lamp in the living room still
        // says it is a `light`, so that kind stays; nothing is a `scene`, so
        // that one goes.
        let kept = kept_child_kinds(&[], &saved, &bridge, &referenced);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].kind, "light");
        // The update's own version of a kind wins over the saved one.
        let mut renamed = saved[0].clone();
        renamed.label = "Lamp".into();
        let kept = kept_child_kinds(&[renamed], &saved, &bridge, &referenced);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].label, "Lamp");
        // A package scene counts as a reference too.
        {
            let mut store = api.store.lock().unwrap();
            store
                .mutate(None, |config| {
                    config.scenes[0].steps.clear();
                    config.scenes[0].hue = None;
                    config.scenes[0].resource = Some(SceneResource {
                        connection_id: bridge.clone(),
                        resource_id: "scene/1".into(),
                        kind: "scene".into(),
                    });
                })
                .unwrap();
        }
        let referenced = api.with(|s| kinds_in_use(s.config()));
        assert_eq!(kept_child_kinds(&[], &saved, &bridge, &referenced).len(), 2);
        // ...and only for its own connection.
        assert!(kept_child_kinds(&[], &saved, &Id::new("blinds"), &referenced).is_empty());
        let _ = fs::remove_dir_all(dir);
    }

    /// Nothing above is reachable in a shipped build. The switch is off, so
    /// no manifest that declares a kind of child is accepted, so no saved
    /// connection can declare one.
    #[test]
    fn with_the_switch_off_no_connection_lists_anything() {
        assert_eq!(
            couch_plugin::accepted_protocol_version(),
            couch_plugin::PROTOCOL_VERSION
        );
        let dir = std::env::temp_dir().join(format!(
            "couch-api-children-off-{}-{:?}",
            std::process::id(),
            std::time::Instant::now()
        ));
        fs::create_dir_all(&dir).unwrap();
        let mut config = serde_json::to_value(Config::seed()).unwrap();
        config["connections"] = json!([{"id": "plain", "name": "Receiver", "provider": {
            "kind": "plugin", "id": "denon", "label": "Denon"}}]);
        let config: Config = serde_json::from_value(config).unwrap();
        fs::write(
            dir.join("config.json"),
            serde_json::to_vec(&couch_model::StoredConfig::new(&config)).unwrap(),
        )
        .unwrap();
        let api = Api::new(
            Store::open(dir.join("config.json")).unwrap(),
            Assets::embedded(),
            Arc::new(Auth::new(dir.join("pin"), true)),
        );
        assert!(api.child_kinds(&Id::new("plain")).is_empty());
        assert_eq!(
            api.plugin_route("GET", "plain", &["children"], b"").status,
            404
        );
        // A device of it is saved as it always was: no listing, no snapshot,
        // and the kind the request asked for.
        let reply = create(
            &api,
            "kitchen",
            json!({"name": "Zone 2", "kind": "speaker", "integration": {"via": "connection",
                "connection_id": "plain", "resource_id": "zone2"}}),
        );
        assert_eq!(reply.status, 200);
        let saved = device(&api, &reply.created.clone().unwrap());
        assert_eq!(saved["kind"], "speaker");
        assert_eq!(
            saved["integration"],
            json!({"via": "connection", "connection_id": "plain", "resource_id": "zone2"})
        );
        // And a snapshot in the request is dropped rather than saved.
        let described: ChildSnapshot =
            serde_json::from_value(json!({"kind": "light", "light": {"dimmable": true}})).unwrap();
        let Ok((integration, kind)) = api.stamp_device(
            Integration::Connection {
                connection_id: Id::new("plain"),
                resource_id: "zone2".into(),
                child: Some(described),
            },
            None,
        ) else {
            panic!("a connection that offers no children never refuses a device");
        };
        assert_eq!(kind, None);
        assert_eq!(parts(&integration).unwrap().2, None);
        let _ = fs::remove_dir_all(dir);
    }
}
