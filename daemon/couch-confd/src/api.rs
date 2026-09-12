//! The REST surface, and the router in front of it.
//!
//! Two conventions run through all of it, and they are what keep the frontend
//! small:
//!
//! * **Every mutating response is the whole config.** A config for a house is a
//!   few kilobytes; a client that re-reads it after each edit cannot drift out
//!   of step with the server, and the alternative - patching a local copy from
//!   a partial response - is where "the room disappeared until I reloaded" bugs
//!   come from. Creates additionally answer with `X-Couch-Created`, because
//!   that id is the one thing the client cannot work out for itself.
//!
//! * **Every response carries `X-Couch-Revision`**, and every mutation accepts
//!   `If-Match`. Two phones open on the same page is the collision that
//!   actually happens here.
//!
//! Everything under `/api` needs a paired session, except the pairing endpoints
//! themselves and a stripped-down `health`. Pairing is a PIN on the remote's
//! screen - see `auth`. The assets are served unguarded, because the page has to
//! load in order to ask for the PIN, and the page on its own says nothing about
//! anybody's house.

mod ha;
mod protect;
mod kodi;
mod remote;
mod hue;
mod webos;
mod streaming_tv;
mod updates;
mod coreelec;
mod sonos;
mod connections;
mod denon;
mod ir;
mod device_ir;

use std::io::Read;
use std::sync::{Arc, Mutex};

use couch_model::{
    Action, Activity, ActivityKind, Area, Config, Device, DeviceKind, Icon, Id, Integration, Room,
    Scene, ALL_DEVICE_KINDS, ALL_ICONS, SCHEMA_VERSION,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tiny_http::{Header, Request, Response, StatusCode};

use crate::assets::Assets;
use crate::auth::{self, Auth, Verdict};
use crate::store::{self, Store};

/// Bodies are small by construction - a whole house is a few KB - so a cap this
/// low is not a limit anyone hits, and it means a stuck or hostile client
/// cannot make the daemon allocate on a device with 1GB and no swap.
const MAX_BODY: u64 = 512 * 1024;

pub struct Api {
    store: Mutex<Store>,
    assets: Assets,
    auth: Arc<Auth>,
}

/// What a `POST` to a collection needs: everything else is defaulted and then
/// edited with a `PUT`, so the "add" button on a phone is one text field.
#[derive(Debug, Deserialize)]
struct NewNamed {
    name: String,
    #[serde(default)]
    icon: Option<Icon>,
}

#[derive(Debug, Deserialize)]
struct NewDevice {
    name: String,
    #[serde(default)]
    kind: DeviceKind,
    #[serde(default)]
    icon: Option<Icon>,
    #[serde(default)]
    integration: Option<Integration>,
    #[serde(default)]
    ir: Option<couch_model::DeviceIr>,
}

#[derive(Debug, Deserialize)]
struct NewActivity {
    #[serde(default)]
    setup: couch_model::ActivitySetup,
    name: String,
    #[serde(default)]
    kind: ActivityKind,
    room: Id,
    #[serde(default)]
    source: Option<Id>,
}

/// `POST /api/areas/{id}/rooms` either attaches an existing room or creates one
/// and attaches it, because from the phone those are the same gesture.
#[derive(Debug, Deserialize)]
struct AttachRoom {
    #[serde(default)]
    room: Option<Id>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    icon: Option<Icon>,
}

#[derive(Debug, Deserialize)]
struct AttachScene {
    #[serde(default)]
    scene: Option<Id>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    icon: Option<Icon>,
}

/// The same, for the activity strip. Creating one needs a room as well as a
/// name, because an activity that happens nowhere is not a thing.
#[derive(Debug, Deserialize)]
struct AttachActivity {
    #[serde(default)]
    activity: Option<Id>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    room: Option<Id>,
    #[serde(default)]
    kind: ActivityKind,
    #[serde(default)]
    source: Option<Id>,
}


/// An area or a room as the editor sends it back: the id comes from the path,
/// and the lists hanging off it are edited through their own endpoints, so
/// neither is required here.
///
/// Name and icon travel together rather than as two patches, so that saving
/// one cannot clear the other.
#[derive(Debug, Deserialize)]
struct NamePatch {
    name: String,
    #[serde(default)]
    icon: Option<Icon>,
}


#[derive(Debug, Serialize)]
struct ApiError {
    error: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    problems: Vec<couch_model::Problem>,
}

/// A finished response, before it is handed to tiny_http.
struct Reply {
    status: u16,
    body: Vec<u8>,
    content_type: &'static str,
    revision: Option<u64>,
    created: Option<String>,
    cache: Option<&'static str>,
    /// Overrides the body's own length, for a HEAD that carries no body.
    length: Option<usize>,
    set_cookie: Option<String>,
}


impl Reply {
    fn json(status: u16, value: &impl Serialize) -> Reply {
        Reply {
            status,
            body: serde_json::to_vec(value).unwrap_or_else(|e| {
                // Serialising our own types cannot fail in practice, but a
                // panic here would take the worker thread with it.
                format!("{{\"error\":\"cannot serialise response: {e}\"}}").into_bytes()
            }),
            content_type: "application/json",
            revision: None,
            created: None,
            cache: None,
            length: None,
            set_cookie: None,
        }
    }


    fn error(status: u16, message: impl Into<String>) -> Reply {
        Reply::json(status, &ApiError { error: message.into(), problems: Vec::new() })
    }

    fn at(mut self, revision: u64) -> Reply {
        self.revision = Some(revision);
        self
    }

    fn created(mut self, id: &Id) -> Reply {
        self.created = Some(id.to_string());
        self
    }
}

impl Api {
    pub fn new(store: Store, assets: Assets, auth: Arc<Auth>) -> Api {
        Api { store: Mutex::new(store), assets, auth }
    }

    pub fn handle(&self, mut request: Request) {
        let reply = self.route(&mut request);

        let mut headers = vec![
            Header::from_bytes(&b"Content-Type"[..], reply.content_type.as_bytes()).unwrap(),
        ];
        if let Some(rev) = reply.revision {
            headers.push(
                Header::from_bytes(&b"X-Couch-Revision"[..], rev.to_string().as_bytes()).unwrap(),
            );
        }
        if let Some(id) = &reply.created {
            if let Ok(h) = Header::from_bytes(&b"X-Couch-Created"[..], id.as_bytes()) {
                headers.push(h);
            }
        }
        if let Some(cache) = reply.cache {
            headers.push(Header::from_bytes(&b"Cache-Control"[..], cache.as_bytes()).unwrap());
        }
        if let Some(cookie) = &reply.set_cookie {
            if let Ok(h) = Header::from_bytes(&b"Set-Cookie"[..], cookie.as_bytes()) {
                headers.push(h);
            }
        }

        // The length is passed rather than left to tiny_http to chunk: these
        // bodies are always in memory already, and a plain Content-Length is
        // what caching and a `curl -i` transcript both want.
        //
        // Raising the threshold is the other half of that. tiny_http chunks
        // anything past 32KB by default, which is every response here that is
        // worth caching - the wasm is 380KB - so without this the largest
        // asset in the bundle went out with no length at all.
        let length = reply.length.unwrap_or(reply.body.len());

        let response = Response::new(
            StatusCode(reply.status),
            headers,
            std::io::Cursor::new(reply.body),
            Some(length),
            None,
        )
        .with_chunked_threshold(usize::MAX);

        // A client that hung up mid-response is normal, not an event.
        let _ = request.respond(response);
    }

    fn route(&self, request: &mut Request) -> Reply {
        let url = request.url().to_string();
        let path = url.split('?').next().unwrap_or("").to_string();
        let method = request.method().as_str().to_uppercase();

        if !path.starts_with("/api") {
            return self.serve_asset(&method, &path);
        }

        let segments: Vec<&str> = path
            .trim_matches('/')
            .split('/')
            .filter(|s| !s.is_empty())
            .collect();
        let if_match = header(request, "if-match").and_then(|v| v.trim().parse::<u64>().ok());
        let body = match read_body(request) {
            Ok(body) => body,
            Err(reply) => return reply,
        };

        // `&segments[1..]` drops the leading "api".
        let rest: Vec<&str> = segments.iter().skip(1).copied().collect();
        let cookies = header(request, "cookie").unwrap_or_default();

        // The gate. Pairing has to be reachable to pair, and health has to be
        // reachable to tell whether the daemon is up at all - so both answer
        // before this, and health answers with less when nobody is paired.
        let open = matches!(rest.as_slice(), ["auth", ..] | ["health"]);
        if !open && !self.auth.authenticated(&cookies) {
            return Reply::error(401, "not paired - open the page and enter the PIN on your remote");
        }

        if let Some(kind @ ("hue"|"ha"|"webos"))=rest.first().copied() {
            let provider=match kind {"ha"=>"home-assistant","webos"=>"web-os",_=>"hue"};
            let ids=self.with(|s|s.config().connections.iter().filter(|c|c.provider.kind()==provider).map(|c|c.id.to_string()).collect::<Vec<_>>());
            if ids.len()>1{return Reply::error(409,"Choose a specific connection");}
            if let Some(id)=ids.first(){let mut scoped=vec![id.as_str(),kind];scoped.extend_from_slice(&rest[1..]);return self.connection_route(&method,&scoped,&body,if_match);}
        }
        if rest.first() == Some(&"updates") {
            return updates::route(&method, &rest[1..], &body);
        }
        if rest.first() == Some(&"ir") {
            let directory = self.with(|s| s.path().parent().unwrap_or_else(|| std::path::Path::new(".")).join("ir"));
            return ir::route(&method, &rest[1..], &body, &directory);
        }
        if rest.first() == Some(&"connections") { return self.connection_route(&method, &rest[1..], &body, if_match); }
        if rest.first() == Some(&"webos") { return webos::route(&method, &rest[1..], &body); }
        if rest.first() == Some(&"hue") { return hue::route(&method, &rest[1..], &body); }
        if rest.first() == Some(&"ha") { return ha::route(&method, &rest[1..], &body); }

        match (method.as_str(), rest.as_slice()) {
            ("GET", ["health"]) => self.health(self.auth.authenticated(&cookies)),
            ("GET", ["auth", "status"]) => self.auth_status(&cookies),
            ("POST", ["auth", "challenge"]) => self.auth_challenge(),
            ("POST", ["auth", "verify"]) => self.auth_verify(&body),
            ("POST", ["auth", "logout"]) => self.auth_logout(&cookies),
            ("GET", ["meta"]) => self.meta(),

            ("GET", ["diagnostics", "control"]) => Reply::json(200, &couch_control::metrics()),
            ("GET", ["config"]) => self.with(|s| Reply::json(200, s.config()).at(s.revision())),
            ("GET", ["remote", "timezones"]) => Reply::json(200, &remote::timezones()),
            ("PUT", ["remote"]) => {
                let settings: couch_model::RemoteSettings = match parse(&body) {Ok(value)=>value,Err(reply)=>return reply};
                if !settings.timezone.is_empty() && !remote::timezones().contains(&settings.timezone) { return Reply::error(400,"Choose an installed IANA timezone"); }
                self.edit(if_match, move |cfg| cfg.remote=settings)
            }
            ("PUT", ["appearance"]) => {
                let appearance: couch_model::Appearance = match parse(&body) {Ok(value)=>value,Err(reply)=>return reply};
                self.edit(if_match, move |cfg| cfg.appearance=appearance)
            }
            ("PUT", ["config"]) => self.replace_config(&body, if_match),
            ("POST", ["config", "reset"]) => {
                self.edit(if_match, |cfg| *cfg = Config::seed())
            }

            ("GET", ["areas"]) => self.with(|s| Reply::json(200, &s.config().areas).at(s.revision())),
            ("POST", ["areas"]) => self.create_area(&body, if_match),
            ("GET", ["areas", id]) => self.get_one(id, |c, id| c.area(id)),
            ("PUT", ["areas", id]) => self.rename(&body, if_match, Kind::Area, id),
            ("DELETE", ["areas", id]) => {
                let id = Id::new(*id);
                self.edit_found(if_match, move |c| c.remove_area(&id).map(|_| ()))
            }
            ("PUT", ["areas", id, "rooms"]) => self.set_members(&body, if_match, id, Member::Room),

            ("POST", ["areas", id, "rooms"]) => self.attach_room(&body, if_match, id),
            ("DELETE", ["areas", id, "rooms", room]) => {
                let (id, room) = (Id::new(*id), Id::new(*room));
                self.edit_found(if_match, move |c| {
                    let area = c.area_mut(&id)?;
                    let at = area.rooms.iter().position(|r| *r == room)?;
                    area.rooms.remove(at);
                    Some(())
                })
            }
            ("PUT", ["areas", id, "scenes"]) => {
                self.set_members(&body, if_match, id, Member::Scene)
            }
            ("POST", ["areas", id, "scenes"]) => self.attach_scene(&body, if_match, id),
            ("DELETE", ["areas", id, "scenes", scene]) => {
                let (id, scene) = (Id::new(*id), Id::new(*scene));
                self.edit_found(if_match, move |c| {
                    let area = c.area_mut(&id)?;
                    let at = area.scenes.iter().position(|s| *s == scene)?;
                    area.scenes.remove(at);
                    Some(())
                })
            }
            ("PUT", ["areas", id, "activities"]) => {
                self.set_members(&body, if_match, id, Member::Activity)
            }
            ("POST", ["areas", id, "activities"]) => self.attach_activity(&body, if_match, id),
            ("DELETE", ["areas", id, "activities", activity]) => {
                let (id, activity) = (Id::new(*id), Id::new(*activity));
                self.edit_found(if_match, move |c| {
                    let area = c.area_mut(&id)?;
                    let at = area.activities.iter().position(|a| *a == activity)?;
                    area.activities.remove(at);
                    Some(())
                })
            }


            ("GET", ["rooms"]) => self.with(|s| Reply::json(200, &s.config().rooms).at(s.revision())),
            ("POST", ["rooms"]) => self.create_room(&body, if_match),
            ("GET", ["rooms", id]) => self.get_one(id, |c, id| c.room(id)),
            ("PUT", ["rooms", id]) => self.rename(&body, if_match, Kind::Room, id),
            ("DELETE", ["rooms", id]) => {
                let id = Id::new(*id);
                self.edit_found(if_match, move |c| c.remove_room(&id).map(|_| ()))
            }
            ("GET", ["rooms", id, "devices"]) => {
                let id = Id::new(*id);
                self.with(|s| match s.config().room(&id) {
                    Some(room) => Reply::json(200, &room.devices).at(s.revision()),
                    None => Reply::error(404, "no such room"),
                })
            }
            (method @ ("GET" | "PUT" | "DELETE"), ["rooms", room, "devices", device, "ir"]) => self.device_ir(method,room,device,&body,if_match),
            ("POST", ["rooms", id, "devices", "ir"]) => self.create_ir_device(id,&body,if_match),
            ("POST", ["rooms", id, "devices"]) => self.create_device(&body, if_match, id),
            ("PUT", ["rooms", id, "devices", dev]) => {
                self.replace_device(&body, if_match, id, dev)
            }
            ("DELETE", ["rooms", id, "devices", dev]) => {
                let (id, dev) = (Id::new(*id), Id::new(*dev));
                self.edit_found(if_match, move |c| c.remove_device(&id, &dev).map(|_| ()))
            }

            ("GET", ["scenes"]) => {
                self.with(|s| Reply::json(200, &s.config().scenes).at(s.revision()))
            }
            ("POST", ["scenes"]) => self.create_scene(&body, if_match),
            ("GET", ["scenes", id]) => self.get_one(id, |c, id| c.scene(id)),
            ("PUT", ["scenes", id]) => self.replace_scene(&body, if_match, id),
            ("DELETE", ["scenes", id]) => {
                let id = Id::new(*id);
                self.edit_found(if_match, move |c| c.remove_scene(&id).map(|_| ()))
            }

            ("GET", ["activities"]) => {
                self.with(|s| Reply::json(200, &s.config().activities).at(s.revision()))
            }
            ("POST", ["activities"]) => self.create_activity(&body, if_match),
            ("GET", ["activities", id]) => self.get_one(id, |c, id| c.activity(id)),
            ("PUT", ["activities", id]) => self.replace_activity(&body, if_match, id),
            ("DELETE", ["activities", id]) => {
                let id = Id::new(*id);
                self.edit_found(if_match, move |c| c.remove_activity(&id).map(|_| ()))
            }

            _ => Reply::error(404, format!("no route for {method} {path}")),
        }
    }

    // --- plumbing ---------------------------------------------------------

    fn with<T>(&self, f: impl FnOnce(&Store) -> T) -> T {
        // A poisoned lock means a worker panicked mid-edit. The store is only
        // ever replaced wholesale by a validated value, so the data behind it
        // is still sound and refusing to serve would be the worse failure.
        let store = self.store.lock().unwrap_or_else(|e| e.into_inner());
        f(&store)
    }

    /// Run an edit and answer with the resulting config, or with why not.
    fn edit<T>(&self, if_match: Option<u64>, f: impl FnOnce(&mut Config) -> T) -> Reply {
        let mut store = self.store.lock().unwrap_or_else(|e| e.into_inner());
        match store.mutate(if_match, f) {
            Ok(_) => Reply::json(200, store.config()).at(store.revision()),
            Err(e) => store_error(&e),
        }
    }

    /// The same, for edits that can fail to find what they were pointed at.
    fn edit_found(
        &self,
        if_match: Option<u64>,
        f: impl FnOnce(&mut Config) -> Option<()>,
    ) -> Reply {
        let mut store = self.store.lock().unwrap_or_else(|e| e.into_inner());
        match store.mutate(if_match, f) {
            Ok(Some(())) => Reply::json(200, store.config()).at(store.revision()),
            // The realistic cause is not a typed URL but a second phone that
            // deleted the thing already, so the message says so rather than
            // leaving a bare "not found" on a screen still showing it.
            Ok(None) => Reply::error(404, "not here any more - something else may have changed it"),
            Err(e) => store_error(&e),
        }

    }

    fn get_one<T: Serialize>(
        &self,
        id: &str,
        pick: impl for<'a> Fn(&'a Config, &'a Id) -> Option<&'a T>,
    ) -> Reply {
        let id = Id::new(id);
        self.with(|s| match pick(s.config(), &id) {
            Some(item) => Reply::json(200, item).at(s.revision()),
            None => Reply::error(404, "not found"),
        })
    }

    /// Open to anyone, so it says only what an unpaired caller needs to know:
    /// something is listening and it speaks this schema. The config path and
    /// the revision count are the shape of somebody's house, in outline, and
    /// they wait until you have paired.
    fn health(&self, paired: bool) -> Reply {
        if !paired {
            return Reply::json(
                200,
                &json!({
                    "service": "couch-confd",
                    "version": env!("CARGO_PKG_VERSION"),
                    "schema_version": SCHEMA_VERSION,
                    "authenticated": false,
                }),
            );
        }
        self.with(|s| {
            Reply::json(
                200,
                &json!({
                    "service": "couch-confd",
                    "version": env!("CARGO_PKG_VERSION"),
                    "schema_version": SCHEMA_VERSION,
                    "authenticated": true,
                    "config_path": s.path().display().to_string(),
                    "revision": s.revision(),
                    "embedded_assets": self.assets.count(),
                }),
            )
            .at(s.revision())
        })
    }

    fn auth_status(&self, cookies: &str) -> Reply {
        Reply::json(200, &status_body(&self.auth.status(cookies)))
    }

    /// Lights up the remote. Idempotent while a challenge is running, so a
    /// reload or a second tab does not change the digits somebody is reading.
    fn auth_challenge(&self) -> Reply {
        Reply::json(200, &status_body(&self.auth.challenge()))
    }

    fn auth_verify(&self, body: &[u8]) -> Reply {
        #[derive(Deserialize)]
        struct Offered {
            pin: String,
        }
        let offered: Offered = match serde_json::from_slice(body) {
            Ok(offered) => offered,
            Err(e) => return Reply::error(400, format!("expected {{\"pin\": \"1234\"}}: {e}")),
        };

        match self.auth.verify(&offered.pin) {
            Verdict::Paired(token) => {
                let mut reply = Reply::json(200, &json!({ "authenticated": true }));
                // SameSite=Strict is the CSRF defence: a page on another origin
                // cannot make the browser attach this to a request at all, so a
                // form post from a hostile tab arrives unpaired. HttpOnly keeps
                // it away from script, and there is no Secure flag because this
                // is plain HTTP on a LAN - see docs/webui.md.
                reply.set_cookie = Some(format!(
                    "{}={token}; HttpOnly; SameSite=Strict; Path=/; Max-Age={}",
                    auth::COOKIE,
                    7 * 24 * 60 * 60
                ));
                reply
            }
            Verdict::Wrong { tries_left } => Reply::json(
                401,
                &json!({ "error": "wrong PIN", "tries_left": tries_left }),
            ),
            Verdict::Expired => Reply::json(
                401,
                &json!({ "error": "that PIN has expired - ask for a new one", "tries_left": 0 }),
            ),
        }
    }

    fn auth_logout(&self, cookies: &str) -> Reply {
        self.auth.log_out(cookies);
        let mut reply = Reply::json(200, &json!({ "authenticated": false }));
        reply.set_cookie =
            Some(format!("{}=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0", auth::COOKIE));
        reply
    }

    /// The vocabularies the editor needs to build its pickers.
    ///
    /// Served rather than hardcoded in the frontend so that adding an icon is a
    /// change in one crate, not two - and so the icons offered are always the
    /// ones this daemon's model actually knows.
    fn meta(&self) -> Reply {
        Reply::json(
            200,
            &json!({
                "schema_version": SCHEMA_VERSION,
                "icons": ALL_ICONS.iter().map(|i| i.name()).collect::<Vec<_>>(),
                "device_kinds": ALL_DEVICE_KINDS.iter().map(|k| k.name()).collect::<Vec<_>>(),
                "integrations": ["none", "kodi", "home-assistant", "hue", "web-os", "denon", "sonos", "ir"],
                "activity_kinds": ["audio", "video"],
            }),
        )
    }

    // --- handlers ---------------------------------------------------------

    fn replace_config(&self, body: &[u8], if_match: Option<u64>) -> Reply {
        let incoming: Config = match serde_json::from_slice(body) {
            Ok(c) => c,
            Err(e) => return Reply::error(400, format!("not valid JSON: {e}")),
        };
        self.edit(if_match, move |cfg| {
            // The revision is the server's to hand out; a client echoing back
            // the one it read must not be able to freeze or rewind it.
            let revision = cfg.revision;
            *cfg = incoming;
            cfg.revision = revision;
        })
    }

    fn create_area(&self, body: &[u8], if_match: Option<u64>) -> Reply {
        let new: NewNamed = match parse(body) {
            Ok(v) => v,
            Err(r) => return r,
        };
        let mut created = Id::new("");
        let reply = self.edit(if_match, |cfg| {
            let id = cfg.fresh_area_id(&new.name);
            created = id.clone();
            cfg.areas.push(Area {
                id,
                name: new.name,
                icon: new.icon,
                rooms: Vec::new(),
                scenes: Vec::new(),
                activities: Vec::new(),
            });

        });
        with_created(reply, &created)
    }

    fn create_room(&self, body: &[u8], if_match: Option<u64>) -> Reply {
        let new: NewNamed = match parse(body) {
            Ok(v) => v,
            Err(r) => return r,
        };
        let mut created = Id::new("");
        let reply = self.edit(if_match, |cfg| {
            let id = cfg.fresh_room_id(&new.name);
            created = id.clone();
            cfg.rooms.push(Room { id, name: new.name, icon: new.icon, devices: Vec::new() });
        });
        with_created(reply, &created)
    }

    fn create_scene(&self, body: &[u8], if_match: Option<u64>) -> Reply {
        #[derive(Deserialize)]
        struct NewScene { name: String, #[serde(default)] icon: Option<Icon>, #[serde(default)] hue: Option<couch_model::HueScene>, #[serde(default)] rooms: Vec<Id> }
        let new: NewScene = match parse(body) { Ok(v) => v, Err(r) => return r };
        let mut created = Id::new("");
        let reply = self.edit(if_match, |cfg| {
            let id = cfg.fresh_scene_id(&new.name);
            created = id.clone();
            cfg.scenes.push(Scene { id, name: new.name, icon: new.icon, steps: Vec::new(), hue: new.hue, rooms: new.rooms });
        });
        with_created(reply, &created)
    }

    fn create_activity(&self, body: &[u8], if_match: Option<u64>) -> Reply {
        let new: NewActivity = match parse(body) {
            Ok(v) => v,
            Err(r) => return r,
        };
        let mut created = Id::new("");
        let reply = self.edit(if_match, |cfg| {
            let id = cfg.fresh_activity_id(&new.name);
            created = id.clone();
            cfg.activities.push(Activity {
                setup: new.setup,
                id,
                name: new.name,
                kind: new.kind,
                room: new.room,
                source: new.source,
                buttons: Vec::new(),
                steps: Vec::new(),
            });
        });
        with_created(reply, &created)
    }

    fn create_device(&self, body: &[u8], if_match: Option<u64>, room: &str) -> Reply {
        let new: NewDevice = match parse(body) {
            Ok(v) => v,
            Err(r) => return r,
        };
        let room = Id::new(room);
        let mut created = Id::new("");
        let reply = self.edit_found(if_match, |cfg| {
            // Device ids are unique across the home, so the id has to be
            // allocated against the whole config, not against this room.
            let id = cfg.fresh_device_id(&format!("{room}-{}", new.name));
            created = id.clone();
            let target = cfg.room_mut(&room)?;
            target.devices.push(Device {
                id,
                name: new.name,
                kind: new.kind,
                icon: new.icon,
                integration: new.integration.unwrap_or_default(),
                ir: new.ir,
            });
            Some(())
        });
        with_created(reply, &created)
    }

    fn replace_device(
        &self,
        body: &[u8],
        if_match: Option<u64>,
        room: &str,
        device: &str,
    ) -> Reply {
        let incoming: Device = match parse(body) {
            Ok(v) => v,
            Err(r) => return r,
        };
        let (room, device) = (Id::new(room), Id::new(device));
        self.edit_found(if_match, move |cfg| {
            let target = cfg.room_mut(&room)?;
            let slot = target.device_mut(&device)?;
            // The path names the device; a body carrying a different id would
            // otherwise silently move it and break every scene step pointing
            // at it.
            *slot = Device { id: device.clone(), ..incoming };
            Some(())
        })
    }

    fn rename(&self, body: &[u8], if_match: Option<u64>, kind: Kind, id: &str) -> Reply {
        let patch: NamePatch = match parse(body) {

            Ok(v) => v,
            Err(r) => return r,
        };
        let id = Id::new(id);
        self.edit_found(if_match, move |cfg| match kind {
            Kind::Area => {
                let area = cfg.area_mut(&id)?;
                area.name = patch.name;
                area.icon = patch.icon;
                Some(())
            }
            Kind::Room => {
                let room = cfg.room_mut(&id)?;
                room.name = patch.name;
                room.icon = patch.icon;
                Some(())
            }
        })

    }

    fn replace_scene(&self, body: &[u8], if_match: Option<u64>, id: &str) -> Reply {
        #[derive(Deserialize)]
        struct Body {
            name: String,
            #[serde(default)]
            icon: Option<Icon>,
            #[serde(default)]
            steps: Vec<Action>,
            #[serde(default)]
            hue: Option<couch_model::HueScene>,
            #[serde(default)]
            rooms: Vec<Id>,
        }
        let incoming: Body = match parse(body) {
            Ok(v) => v,
            Err(r) => return r,
        };
        let id = Id::new(id);
        self.edit_found(if_match, move |cfg| {
            let scene = cfg.scene_mut(&id)?;
            scene.name = incoming.name;
            scene.icon = incoming.icon;
            scene.steps = incoming.steps;
            scene.hue = incoming.hue;
            scene.rooms = incoming.rooms;
            Some(())
        })
    }

    fn replace_activity(&self, body: &[u8], if_match: Option<u64>, id: &str) -> Reply {
        #[derive(Deserialize)]
        struct Body {
            #[serde(default)]
            setup: Option<couch_model::ActivitySetup>,
            name: String,
            #[serde(default)]
            kind: ActivityKind,
            room: Id,
            #[serde(default)]
            source: Option<Id>,
            #[serde(default)]
            steps: Vec<Action>,
            #[serde(default)]
            buttons: Option<Vec<couch_model::buttons::Binding>>,
        }
        let incoming: Body = match parse(body) {
            Ok(v) => v,
            Err(r) => return r,
        };
        let id = Id::new(id);
        self.edit_found(if_match, move |cfg| {
            let act = cfg.activity_mut(&id)?;
            if let Some(setup) = incoming.setup { act.setup = setup; }
            act.name = incoming.name;
            act.kind = incoming.kind;
            act.room = incoming.room;
            act.source = incoming.source;
            act.steps = incoming.steps;
            if let Some(buttons) = incoming.buttons {act.buttons = buttons;}
            Some(())
        })
    }

    /// Replace one of an area's member lists wholesale, which is also how it
    /// is reordered: the order in the array is the order on the remote.
    fn set_members(&self, body: &[u8], if_match: Option<u64>, id: &str, which: Member) -> Reply {
        let ids: Vec<Id> = match parse(body) {
            Ok(v) => v,
            Err(r) => return r,
        };
        let id = Id::new(id);
        self.edit_found(if_match, move |cfg| {
            let area = cfg.area_mut(&id)?;
            match which {
                Member::Room => area.rooms = ids,
                Member::Scene => area.scenes = ids,
                Member::Activity => area.activities = ids,
            }
            Some(())
        })
    }


    fn attach_room(&self, body: &[u8], if_match: Option<u64>, area: &str) -> Reply {
        let req: AttachRoom = match parse(body) {
            Ok(v) => v,
            Err(r) => return r,
        };
        let area = Id::new(area);
        let mut created = Id::new("");
        let reply = self.edit_found(if_match, |cfg| {
            let room_id = match (&req.room, &req.name) {
                (Some(existing), _) => {
                    cfg.room(existing)?;
                    existing.clone()
                }
                (None, Some(name)) => {
                    let id = cfg.fresh_room_id(name);
                    created = id.clone();
                    cfg.rooms.push(Room {
                        id: id.clone(),
                        name: name.clone(),
                        icon: req.icon,
                        devices: Vec::new(),
                    });
                    id
                }
                (None, None) => return None,
            };
            let area = cfg.area_mut(&area)?;
            if !area.rooms.contains(&room_id) {
                area.rooms.push(room_id);
            }
            Some(())
        });
        with_created(reply, &created)
    }

    fn attach_scene(&self, body: &[u8], if_match: Option<u64>, area: &str) -> Reply {
        let req: AttachScene = match parse(body) {
            Ok(v) => v,
            Err(r) => return r,
        };
        let area = Id::new(area);
        let mut created = Id::new("");
        let reply = self.edit_found(if_match, |cfg| {
            let scene_id = match (&req.scene, &req.name) {
                (Some(existing), _) => {
                    cfg.scene(existing)?;
                    existing.clone()
                }
                (None, Some(name)) => {
                    let id = cfg.fresh_scene_id(name);
                    created = id.clone();
                    cfg.scenes.push(Scene {
                        id: id.clone(),
                        name: name.clone(),
                        icon: req.icon,
                        steps: Vec::new(),
                        hue: None,
                        rooms: Vec::new(),
                    });
                    id
                }
                (None, None) => return None,
            };
            let area = cfg.area_mut(&area)?;
            if !area.scenes.contains(&scene_id) {
                area.scenes.push(scene_id);
            }
            Some(())
        });
        with_created(reply, &created)
    }

    /// Put an activity on an area's strip, creating it first if asked.
    ///
    /// A create needs a room, and saying so up front is worth a branch: the
    /// generic `edit_found` failure is a 404, which for a missing `room` field
    /// would read as "no such area" and send someone looking in the wrong
    /// place.
    fn attach_activity(&self, body: &[u8], if_match: Option<u64>, area: &str) -> Reply {
        let req: AttachActivity = match parse(body) {
            Ok(v) => v,
            Err(r) => return r,
        };
        if req.activity.is_none() && (req.name.is_none() || req.room.is_none()) {
            return Reply::error(400, "an activity needs either an id, or a name and a room");
        }
        let area = Id::new(area);
        let mut created = Id::new("");
        let reply = self.edit_found(if_match, |cfg| {
            let activity_id = match (&req.activity, &req.name, &req.room) {
                (Some(existing), _, _) => {
                    cfg.activity(existing)?;
                    existing.clone()
                }
                (None, Some(name), Some(room)) => {
                    cfg.room(room)?;
                    let id = cfg.fresh_activity_id(name);
                    created = id.clone();
                    cfg.activities.push(Activity {
                        setup: Default::default(),
                        id: id.clone(),
                        name: name.clone(),
                        kind: req.kind,
                        room: room.clone(),
                        source: req.source.clone(),
                buttons: Vec::new(),
                        steps: Vec::new(),
                    });
                    id
                }
                _ => return None,
            };
            let area = cfg.area_mut(&area)?;
            if !area.activities.contains(&activity_id) {
                area.activities.push(activity_id);
            }
            Some(())
        });
        with_created(reply, &created)
    }

    fn serve_asset(&self, method: &str, path: &str) -> Reply {

        if method != "GET" && method != "HEAD" {
            return Reply::error(405, "the web root is read-only");
        }
        match self.assets.get(path) {
            Some(asset) => Reply {
                set_cookie: None,
                status: 200,
                // A HEAD still has to answer with the length the GET would
                // send, which is the only thing anybody asks HEAD for.
                length: Some(asset.body.len()),
                body: if method == "HEAD" { Vec::new() } else { asset.body },

                content_type: asset.content_type,
                revision: None,
                created: None,
                cache: Some(if asset.immutable {
                    "public, max-age=31536000, immutable"
                } else {
                    // The entry point names the fingerprinted files, so a stale
                    // one pins a stale app forever.
                    "no-cache"
                }),
            },
            None => Reply::error(404, "not found"),
        }
    }
}

/// Which collection a rename is aimed at.
///
/// Only the two whose contents are edited through separate endpoints. A scene,
/// an activity and a device are each small enough to send back whole, so their
/// `PUT` replaces rather than patches.
#[derive(Clone, Copy)]
enum Kind {
    Area,
    Room,
}


/// Which of an area's three member lists a request is about.
#[derive(Clone, Copy)]
enum Member {
    Room,
    Scene,
    Activity,
}


fn status_body(status: &auth::Status) -> serde_json::Value {
    json!({
        "authenticated": status.authenticated,
        "pairing": status.pairing,
        "expires_in": status.expires_in,
        "tries_left": status.tries_left,
        "disabled": status.disabled,
    })
}

fn with_created(mut reply: Reply, id: &Id) -> Reply {
    if reply.status == 200 && !id.is_empty() {
        reply = reply.created(id);
    }
    reply
}

fn parse<T: for<'de> Deserialize<'de>>(body: &[u8]) -> Result<T, Reply> {
    serde_json::from_slice(body).map_err(|e| Reply::error(400, format!("bad request body: {e}")))
}

fn store_error(e: &store::Error) -> Reply {
    match e {
        store::Error::Invalid(v) => Reply::json(
            422,
            &ApiError { error: "the edit would leave the config invalid".into(), problems: v.problems.clone() },
        ),
        store::Error::Stale { .. } => Reply::error(409, e.to_string()),
        store::Error::Parse(_) => Reply::error(400, e.to_string()),
        store::Error::Io(_) => Reply::error(500, e.to_string()),
    }
}

fn header(request: &Request, name: &str) -> Option<String> {
    request
        .headers()
        .iter()
        // equiv() wants a &'static str; the field's own text does not.
        .find(|h| h.field.as_str().as_str().eq_ignore_ascii_case(name))
        .map(|h| h.value.as_str().to_string())
}

fn read_body(request: &mut Request) -> Result<Vec<u8>, Reply> {
    let declared = request.body_length().unwrap_or(0) as u64;
    if declared > MAX_BODY {
        return Err(Reply::error(413, "request body too large"));
    }
    let mut body = Vec::new();
    request
        .as_reader()
        .take(MAX_BODY)
        .read_to_end(&mut body)
        .map_err(|e| Reply::error(400, format!("could not read the request body: {e}")))?;
    Ok(body)
}

#[cfg(test)]
mod activity_mapping_tests {
    use super::*;
    #[test]
    fn activity_setup_survives_legacy_update_and_invalid_edit_is_atomic() {
        use couch_model::{ActivitySetup, SequenceStep};
        let dir = std::env::temp_dir().join(format!("couch-api-setup-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.json"), serde_json::to_vec(&Config::seed()).unwrap()).unwrap();
        let api = Api::new(Store::open(dir.join("config.json")).unwrap(), Assets::embedded(), Arc::new(Auth::new(dir.join("pin"), true)));
        let mut activity = api.with(|s| s.config().activities[0].clone());
        activity.setup = ActivitySetup {
            devices: vec!["living-kodi".into()],
            keep_awake: true,
            on: vec![SequenceStep::Command { action: Action::new("living-kodi", "home") }, SequenceStep::Delay { ms: 250 }],
            off: vec![SequenceStep::Command { action: Action::new("living-kodi", "stop") }],
            ..Default::default()
        };
        assert_eq!(api.replace_activity(&serde_json::to_vec(&activity).unwrap(), None, activity.id.as_str()).status, 200);
        let mut legacy = serde_json::to_value(&activity).unwrap();
        legacy.as_object_mut().unwrap().remove("setup");
        legacy["name"] = "Renamed by older client".into();
        assert_eq!(api.replace_activity(&serde_json::to_vec(&legacy).unwrap(), None, activity.id.as_str()).status, 200);
        assert_eq!(api.with(|s| s.config().activities[0].setup.clone()), activity.setup);
        let before = std::fs::read(dir.join("config.json")).unwrap();
        activity.setup.devices.clear();
        assert_eq!(api.replace_activity(&serde_json::to_vec(&activity).unwrap(), None, activity.id.as_str()).status, 422);
        assert_eq!(std::fs::read(dir.join("config.json")).unwrap(), before);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn activity_update_persists_and_validates_physical_bindings() {
        let dir=std::env::temp_dir().join(format!("couch-api-buttons-{}",std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.json"), serde_json::to_vec(&Config::seed()).unwrap()).unwrap();
        let api=Api::new(Store::open(dir.join("config.json")).unwrap(),Assets::embedded(),Arc::new(Auth::new(dir.join("pin"),true)));
        let mut activity=api.with(|s|s.config().activities[0].clone());
        activity.buttons=vec![couch_model::buttons::Binding{button:couch_model::buttons::Button::Ok,gesture:couch_model::buttons::Gesture::Long,action:Some(Action::new("living-kodi","home"))}];
        let reply=api.replace_activity(&serde_json::to_vec(&activity).unwrap(),None,activity.id.as_str());assert_eq!(reply.status,200);
        assert_eq!(api.with(|s|s.config().activities[0].buttons.clone()),activity.buttons);
        // Older clients do not know the buttons field: a rename must preserve it.
        let mut legacy=serde_json::to_value(&activity).unwrap();legacy.as_object_mut().unwrap().remove("buttons");
        let reply=api.replace_activity(&serde_json::to_vec(&legacy).unwrap(),None,activity.id.as_str());assert_eq!(reply.status,200);
        assert_eq!(api.with(|s|s.config().activities[0].buttons.clone()),activity.buttons);
        let original=std::fs::read(dir.join("config.json")).unwrap();
        activity.buttons[0].action.as_mut().unwrap().command="bad-command".into();
        let reply=api.replace_activity(&serde_json::to_vec(&activity).unwrap(),None,activity.id.as_str());assert_eq!(reply.status,422);
        assert_eq!(std::fs::read(dir.join("config.json")).unwrap(),original);
        activity.buttons.clear();let reply=api.replace_activity(&serde_json::to_vec(&activity).unwrap(),None,activity.id.as_str());assert_eq!(reply.status,200);
        assert!(api.with(|s|s.config().activities[0].buttons.is_empty()));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
