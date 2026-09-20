//! Connections whose built-in client left the OS, handed to their package.
//!
//! `couch_model::LEGACY_BUILTINS` names the built-ins that became packages
//! (Denon is the first). A saved connection of such a kind still loads, but
//! nothing drives it, so the daemon finishes the move by itself: install the
//! package from a trusted repository if it is missing, give the package the
//! address the connection carried, and switch the connection in place. The id
//! does not change, so rooms, activities and button maps keep working. It is
//! one way; there is no built-in client to return to.
//!
//! An attempt runs shortly after start, again on a backoff while anything is
//! still waiting (no internet on the first boot after an update is the usual
//! reason, so those first retries are quick), whenever the Integrations page
//! refreshes or installs, and when someone presses "Try again". Until it
//! succeeds the connection reads "Needs the Denon package" everywhere, with the
//! reason kept here for the web UI.
//!
//! Nobody asked for this install, so it only ever takes the package from an
//! official repository. A package the owner installed by hand, from any
//! repository they trust, is used as it is.
use super::{Api, Reply};
use couch_integrations::management::Action;
use couch_model::{Id, LegacyBuiltin, LegacySetting, Provider};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::Mutex,
    time::{Duration, Instant},
};

/// Waits after the first, second, ... failed attempt. The last one repeats.
const BACKOFF: [u64; 6] = [30, 60, 120, 300, 900, 3600];
/// The waits that come first while the feed cannot be reached. The attempt a
/// second after start usually finds Wi-Fi still coming up, and until the
/// conversion an activity stops at its receiver step.
const UNREACHABLE_FIRST: [u64; 3] = [5, 10, 20];
/// A package operation somebody else started is not a failure; look again soon.
const BUSY_RETRY: Duration = Duration::from_secs(5);
/// However often "Try again" or a refresh asks, attempts (each of which may
/// refresh the feed) start at least this far apart.
const MIN_INTERVAL: Duration = Duration::from_secs(5);
const OPERATION_DEADLINE: Duration = Duration::from_secs(600);
/// The only repositories an install nobody asked for may use, in order.
const OFFICIAL_REPOSITORIES: [&str; 2] = ["official-stable", "official-preview"];

type Installer = Box<dyn Fn(&LegacyBuiltin) -> Result<(), Install> + Send>;

fn wait_after(failures: u32, unreachable: bool) -> u64 {
    let quick: &[u64] = if unreachable { &UNREACHABLE_FIRST } else { &[] };
    let at = failures as usize;
    quick
        .get(at)
        .or_else(|| BACKOFF.get(at - quick.len().min(at)))
        .copied()
        .unwrap_or(BACKOFF[BACKOFF.len() - 1])
}

/// The official repository to install `package` from, given the package
/// manager's catalog. A repository the owner added is never chosen here, even
/// when it is the only one that offers the package. Nor is an offer this Couch
/// cannot run (the feed's signed metadata says so before any download).
fn official_source(catalog: &Value, package: &str) -> Option<&'static str> {
    let offered: Vec<&str> = offers(catalog, package)
        .filter(|entry| entry["installable"] != false)
        .filter_map(|entry| entry["repository"].as_str())
        .collect();
    OFFICIAL_REPOSITORIES
        .into_iter()
        .find(|official| offered.contains(official))
}

/// Why the official feed's `package` cannot be installed here, when it offers
/// one that cannot: "Needs a newer Couch". The connection then waits with
/// that reason; a Couch update is what ends the wait.
fn official_obstacle(catalog: &Value, package: &str) -> Option<String> {
    OFFICIAL_REPOSITORIES.into_iter().find_map(|official| {
        offers(catalog, package)
            .find(|entry| entry["repository"] == official && entry["installable"] == false)
            .map(|entry| {
                entry["reason"]
                    .as_str()
                    .unwrap_or("It cannot be installed on this remote")
                    .to_owned()
            })
    })
}

fn offers<'a>(catalog: &'a Value, package: &'a str) -> impl Iterator<Item = &'a Value> {
    catalog["available"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(move |entry| entry["id"] == package)
}

#[derive(Default)]
struct State {
    running: bool,
    failures: u32,
    /// When the next automatic attempt may start; `None` is "now".
    retry_at: Option<Instant>,
    /// When the last attempt started, which `MIN_INTERVAL` counts from.
    last_attempt: Option<Instant>,
    /// Why each connection is still waiting, by connection id.
    reasons: BTreeMap<String, String>,
}

#[derive(Default)]
pub(crate) struct LegacyConversion {
    state: Mutex<State>,
    /// Tests stand in for the feed; the daemon always installs through the
    /// package manager.
    installer: Mutex<Option<Installer>>,
}

impl LegacyConversion {
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Make the next pass attempt now rather than when its backoff ends.
    pub(crate) fn kick(&self) {
        self.state().retry_at = None;
    }
}

fn settings_value(row: &LegacyBuiltin, provider: &Provider) -> Option<Value> {
    let map = row
        .settings(provider)?
        .into_iter()
        .map(|(key, value)| {
            let value = match value {
                LegacySetting::Text(text) => Value::String(text),
                LegacySetting::Integer(number) => Value::from(number),
            };
            (key.to_owned(), value)
        })
        .collect::<serde_json::Map<_, _>>();
    Some(Value::Object(map))
}

impl Api {
    pub(super) fn legacy_conversion_route(&self, method: &str, path: &[&str]) -> Reply {
        match (method, path) {
            ("GET", []) => {
                let now = Instant::now();
                let connections: Vec<(Id, String, &'static LegacyBuiltin)> = self.with(|store| {
                    store
                        .config()
                        .legacy_connections()
                        .map(|(connection, row)| {
                            (connection.id.clone(), connection.name.clone(), row)
                        })
                        .collect()
                });
                let state = self.legacy.state();
                let retry = state
                    .retry_at
                    .filter(|_| !connections.is_empty())
                    .map(|at| at.saturating_duration_since(now).as_secs());
                let connections: Vec<_> = connections
                    .into_iter()
                    .map(|(id, name, row)| {
                        json!({
                            "id": id, "name": name,
                            "kind": row.kind, "package": row.package,
                            "package_name": row.name,
                            "message": row.needs_package(),
                            "reason": state.reasons.get(id.as_str()),
                        })
                    })
                    .collect();
                Reply::json(
                    200,
                    &json!({
                        "connections": connections,
                        "working": state.running,
                        "retry_in_seconds": retry,
                    }),
                )
            }
            ("POST", ["retry"]) => {
                self.legacy.kick();
                Reply::json(202, &json!({"retrying": true}))
            }
            _ => Reply::error(404, "Unknown legacy connection operation"),
        }
    }

    /// One look at whether anything waits for a package, and one attempt if it
    /// is due. Blocking (an install downloads); the daemon gives it a thread
    /// of its own and calls it once a second.
    pub fn legacy_conversion_pass(&self) {
        self.legacy_conversion_pass_at(Instant::now());
    }

    fn legacy_conversion_pass_at(&self, now: Instant) {
        let waiting = self.with(|store| store.config().legacy_connections().next().is_some());
        {
            let mut state = self.legacy.state();
            if !waiting {
                *state = State::default();
                return;
            }
            if state.running
                || state.retry_at.is_some_and(|at| now < at)
                || state.last_attempt.is_some_and(|at| now < at + MIN_INTERVAL)
            {
                return;
            }
            // Somebody is refreshing or installing by hand. Their operation
            // owns the package manager; go right after it.
            if self
                .integration_packages
                .current_operation()
                .is_some_and(|operation| operation.state == "running")
            {
                return;
            }
            state.running = true;
            state.last_attempt = Some(now);
        }
        let outcome = self.convert_legacy_connections();
        let mut state = self.legacy.state();
        state.running = false;
        match outcome {
            Attempt::Busy => state.retry_at = Some(now + BUSY_RETRY),
            Attempt::Done { reasons, .. } if reasons.is_empty() => *state = State::default(),
            Attempt::Done {
                reasons,
                unreachable,
            } => {
                let wait = wait_after(state.failures, unreachable);
                state.failures = state.failures.saturating_add(1);
                state.retry_at = Some(now + Duration::from_secs(wait));
                for (id, reason) in &reasons {
                    if state.reasons.get(id) != Some(reason) {
                        eprintln!("couch-confd: connection {id} still needs its package: {reason}");
                    }
                }
                state.reasons = reasons;
            }
        }
    }

    fn convert_legacy_connections(&self) -> Attempt {
        let waiting: Vec<(Id, &'static LegacyBuiltin)> = self.with(|store| {
            store
                .config()
                .legacy_connections()
                .map(|(connection, row)| (connection.id.clone(), row))
                .collect()
        });
        let mut reasons = BTreeMap::new();
        let mut unavailable = BTreeMap::<&str, String>::new();
        // Whether the feed being out of reach is all that went wrong.
        let (mut unreachable, mut other) = (false, false);
        for (id, row) in waiting {
            if let Some(reason) = unavailable.get(row.package) {
                reasons.insert(id.to_string(), reason.clone());
                continue;
            }
            if self.plugins.manifest(row.package).is_err() {
                match self.install_legacy_package(row) {
                    Ok(()) => println!(
                        "couch-confd: installed the {} package for a saved connection",
                        row.name
                    ),
                    Err(Install::Busy) => return Attempt::Busy,
                    Err(failure) => {
                        let reason = match failure {
                            Install::Unreachable(reason) => {
                                unreachable = true;
                                reason
                            }
                            Install::Failed(reason) => {
                                other = true;
                                reason
                            }
                            Install::Busy => unreachable!("handled above"),
                        };
                        unavailable.insert(row.package, reason.clone());
                        reasons.insert(id.to_string(), reason);
                        continue;
                    }
                }
            }
            match self.convert_legacy_connection(&id, row) {
                Ok(()) => println!(
                    "couch-confd: connection {id} now uses the {} package",
                    row.name
                ),
                Err(reason) => {
                    other = true;
                    reasons.insert(id.to_string(), reason);
                }
            }
        }
        Attempt::Done {
            reasons,
            unreachable: unreachable && !other,
        }
    }

    /// The same checks, settings hand-over and single config write the package
    /// connection form goes through, for a connection that already exists.
    fn convert_legacy_connection(&self, id: &Id, row: &LegacyBuiltin) -> Result<(), String> {
        let snapshot = self.with(|store| store.config().connection(id).map(|c| c.provider.clone()));
        let Some(provider) = snapshot else {
            return Ok(());
        };
        let Some(settings) = settings_value(row, &provider) else {
            return Ok(());
        };
        // Protocol 3 (unreleased): the key the built-in kept, mapped to the
        // shape its package takes. `None` for every row there is today, and
        // the old file is left where it is either way.
        let credential = self.plugins.legacy_credential(id.as_str(), row);
        // The slow part, with the configuration unlocked: start the package
        // and let it check the carried-over address (it contacts no device).
        let prepared = self
            .plugins
            .prepare_legacy(row.package, settings, credential)?;
        // Config readers and writers wait from here to the commit, so nothing
        // edits the connection in between. What was checked above has to be
        // what is converted: an address edited meanwhile goes round again.
        let mut store = self.store.lock().unwrap_or_else(|e| e.into_inner());
        let mut next = store.config().clone();
        match next.connection(id).map(|c| &c.provider) {
            None => return Ok(()),
            Some(current) if *current != provider => {
                return Err("The connection changed while its package was being prepared".into())
            }
            Some(_) => {}
        }
        let revision = store.revision();
        self.plugins
            .adopt_legacy(id.as_str(), &prepared, |manifest| {
                next.convert_legacy(
                    id,
                    Provider::Plugin {
                        id: manifest.id.clone(),
                        label: manifest.label.clone(),
                        capabilities: manifest
                            .capabilities
                            .iter()
                            .map(|c| couch_model::PluginCapability {
                                id: c.id.clone(),
                                label: c.label.clone(),
                            })
                            .collect(),
                        supports_inputs: manifest.supports_inputs,
                        presentation: manifest.presentation.clone(),
                        actions: manifest.actions.clone(),
                        children: manifest.children.clone(),
                    },
                )?;
                store
                    .mutate(Some(revision), |config| *config = next)
                    .map_err(|e| e.to_string())
            })
    }

    fn install_legacy_package(&self, row: &LegacyBuiltin) -> Result<(), Install> {
        if let Some(installer) = self
            .legacy
            .installer
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            return installer(row);
        }
        // What the Integrations page does - refresh the signed indexes, then
        // install - except that the owner chose nothing here, so only an
        // official repository will do.
        let refreshed = self.package_operation("refresh", None);
        if let Err(Install::Busy) = refreshed {
            return Err(Install::Busy);
        }
        let catalog = self
            .integration_packages
            .catalog(&[])
            .map_err(|e| Install::Failed(e.to_string()))?;
        let Some(repository) = official_source(&catalog, row.package) else {
            if let Some(reason) = official_obstacle(&catalog, row.package) {
                return Err(Install::Failed(format!(
                    "The {} package cannot be installed yet: {reason}. Update Couch on this remote and the connection converts by itself",
                    row.name
                )));
            }
            return Err(match refreshed {
                Err(Install::Failed(error) | Install::Unreachable(error)) => {
                    Install::Unreachable(format!(
                    "The package feed could not be read ({error}). Check the remote's internet connection."
                ))
                }
                _ => Install::Failed(format!(
                    "The official package feed does not offer the {} package yet",
                    row.name
                )),
            });
        };
        self.package_operation(
            "install",
            Some(Action {
                id: row.package.into(),
                repository: Some(repository.into()),
                preserve_connection_config: true,
            }),
        )
    }

    fn package_operation(&self, kind: &str, action: Option<Action>) -> Result<(), Install> {
        let manager = &self.integration_packages;
        let id = manager.start(kind, action).map_err(|error| {
            if error.is_busy() {
                Install::Busy
            } else {
                Install::Failed(error.to_string())
            }
        })?;
        let deadline = Instant::now() + OPERATION_DEADLINE;
        loop {
            std::thread::sleep(Duration::from_millis(200));
            match manager.operation(&id) {
                Some(operation) if operation.state == "running" => {
                    if Instant::now() > deadline {
                        return Err(Install::Failed(
                            "The package operation did not finish".into(),
                        ));
                    }
                }
                Some(operation) if operation.state == "succeeded" => return Ok(()),
                Some(operation) => return Err(Install::Failed(operation.message)),
                None => return Err(Install::Busy),
            }
        }
    }
}

enum Attempt {
    Busy,
    Done {
        /// Why each connection that is still waiting could not be converted.
        reasons: BTreeMap<String, String>,
        /// Nothing went wrong except that the feed could not be reached.
        unreachable: bool,
    },
}

enum Install {
    Busy,
    /// The feed could not be read: the network is not up, or not there.
    Unreachable(String),
    Failed(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        assets::Assets,
        auth::Auth,
        plugins::{write_executable, FIXTURE_APK},
        store::Store,
    };
    use couch_model::{Config, StoredConfig};
    use std::{
        fs,
        path::{Path, PathBuf},
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
    };

    const MANIFEST: &str = include_str!("../../tests/fixtures/denon-0.2.1-plugin.json");

    /// A house saved by a release that still had the built-in client: a named
    /// receiver, a device on it, an activity that uses it and a key bound to it.
    fn built_in_era(host: &str, port: u16) -> Value {
        json!({
            "schema_version": 1, "revision": 4,
            "connections": [{"id":"receiver","name":"Receiver","provider":{"kind":"denon","host":host,"port":port}}],
            "areas": [{"id":"home","name":"Home","rooms":["den"],"activities":["movie"]}],
            "rooms": [{"id":"den","name":"Den","devices":[
                {"id":"avr","name":"AVR","kind":"speaker","integration":{"via":"connection","connection_id":"receiver"}}]}],
            "activities": [{"id":"movie","name":"Movie","room":"den","source":"avr",
                "steps":[{"device":"avr","command":"input:SAT/CBL"}],
                "buttons":[{"button":"red","action":{"device":"avr","command":"volume-up"}}]}]
        })
    }

    struct Fixture {
        api: Api,
        home: PathBuf,
    }
    impl Fixture {
        fn new(name: &str, config: &Value) -> Self {
            let home = std::env::temp_dir().join(format!(
                "couch-legacy-conversion-{name}-{}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&home);
            fs::create_dir_all(&home).unwrap();
            fs::write(
                home.join("config.json"),
                serde_json::to_vec(config).unwrap(),
            )
            .unwrap();
            let api = Api::new(
                Store::open(home.join("config.json")).unwrap(),
                Assets::embedded(),
                Arc::new(Auth::new(home.join("pin"), true)),
            );
            Self { api, home }
        }
        fn status(&self) -> Value {
            let reply = self.api.legacy_conversion_route("GET", &[]);
            assert_eq!(reply.status, 200);
            serde_json::from_slice(&reply.body).unwrap()
        }
        fn config(&self) -> Config {
            self.api.with(|store| store.config().clone())
        }
        /// The feed, as far as the converter is concerned.
        fn feed(&self, installer: impl Fn(&LegacyBuiltin) -> Result<(), Install> + Send + 'static) {
            *self.api.legacy.installer.lock().unwrap() = Some(Box::new(installer));
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.home);
        }
    }

    /// Real archive audit, immutable selection, hash revalidation and host
    /// handshake; APK extraction/signature checking is a test fixture.
    /// Real signature admission remains covered by the package smoke test.
    fn install_package_fixture(home: &Path) -> couch_integrations::Store {
        use std::os::unix::fs::PermissionsExt;
        let manifest: Value = serde_json::from_str(MANIFEST).unwrap();
        let payload = home.join("payload");
        let directory = payload.join("usr/lib/couch/integrations/denon");
        fs::create_dir_all(directory.join("bin")).unwrap();
        fs::write(
            directory.join("manifest.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        let mut script = String::from("#!/bin/sh\n");
        for value in [
            json!({"id":1,"body":{"type":"hello","manifest":manifest}}),
            json!({"id":2,"body":{"type":"ok"}}),
        ] {
            let mut frame = Vec::new();
            couch_plugin::write_frame(&mut frame, &value).unwrap();
            let bytes: String = frame.iter().map(|byte| format!("\\{byte:03o}")).collect();
            script.push_str(&format!("printf '{bytes}'\n"));
        }
        script.push_str("sleep 5\n");
        let executable = directory.join("bin/couch-plugin-denon");
        fs::write(&executable, script).unwrap();
        fs::set_permissions(executable, fs::Permissions::from_mode(0o755)).unwrap();
        let package = home.join("denon-fixture.apk");
        assert!(std::process::Command::new("tar")
            .args(["-czf"])
            .arg(&package)
            .arg("-C")
            .arg(payload)
            .args([
                "usr/lib/couch/integrations/denon/manifest.json",
                "usr/lib/couch/integrations/denon/bin/couch-plugin-denon"
            ])
            .status()
            .unwrap()
            .success());
        let apk = home.join("fixture-apk");
        write_executable(&apk, FIXTURE_APK);
        let packages = couch_integrations::Store::new(home.join("integrations")).with_apk(apk);
        packages.install(&package).unwrap();
        packages
    }

    #[test]
    fn a_saved_built_in_connection_loads_and_says_what_it_needs() {
        let fixture = Fixture::new("loads", &built_in_era("avr.invalid", 23));
        let status = fixture.status();
        assert_eq!(status["working"], false);
        assert_eq!(status["connections"][0]["id"], "receiver");
        assert_eq!(status["connections"][0]["package"], "denon");
        assert_eq!(
            status["connections"][0]["message"],
            "Needs the Denon package"
        );
        assert_eq!(status["connections"][0]["reason"], Value::Null);
        // Reading it is all the API offers: there is no built-in client left
        // behind it, and nobody can add another of its kind.
        assert_eq!(
            fixture
                .api
                .connection_route("GET", &["receiver", "denon", "status"], &[], None)
                .status,
            404
        );
        let created = fixture.api.connection_route(
            "POST",
            &[],
            &serde_json::to_vec(
                &json!({"name":"Another","provider":{"kind":"denon","host":"x.invalid","port":23}}),
            )
            .unwrap(),
            None,
        );
        assert_eq!(created.status, 400);
        assert!(String::from_utf8_lossy(&created.body).contains("integration package"));
        assert_eq!(fixture.config().connections.len(), 1);
    }

    #[test]
    fn with_the_package_installed_the_connection_converts_in_place() {
        use std::os::unix::fs::PermissionsExt;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let fixture = Fixture::new("installed", &built_in_era("127.0.0.1", port));
        install_package_fixture(&fixture.home);
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = calls.clone();
        fixture.feed(move |_| {
            seen.fetch_add(1, Ordering::SeqCst);
            Err(Install::Failed(
                "the feed must not be asked for an installed package".into(),
            ))
        });
        let before = fixture.config();

        fixture.api.legacy_conversion_pass();

        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let after = fixture.config();
        assert_eq!(after.revision, before.revision + 1);
        assert_eq!(after.rooms, before.rooms);
        assert_eq!(after.activities, before.activities);
        assert_eq!(after.areas, before.areas);
        assert_eq!(after.connections.len(), 1);
        assert_eq!(after.connections[0].id, before.connections[0].id);
        assert_eq!(after.connections[0].name, "Receiver");
        let Provider::Plugin {
            id,
            label,
            supports_inputs,
            actions,
            ..
        } = &after.connections[0].provider
        else {
            panic!("still {:?}", after.connections[0].provider);
        };
        assert_eq!((id.as_str(), label.as_str()), ("denon", "Denon AVR"));
        assert!(*supports_inputs && !actions.is_empty());
        // The room's device and the activity's commands resolve through it.
        after.validate().unwrap();
        let device = &after.rooms[0].devices[0];
        assert!(matches!(
            after.resolve_integration(&device.integration),
            Some(couch_model::Integration::Plugin { connection_id, .. })
                if connection_id == Id::new("receiver")
        ));
        for command in ["input:SAT/CBL", "volume-up"] {
            assert!(couch_model::commands::Function::parse(command)
                .unwrap()
                .supports_device(device, &after));
        }
        // The address moved into the package's private settings, and the
        // receiver itself was never contacted.
        let path = couch_sdk::connection_file(&fixture.home, "receiver", "plugin").unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&fs::read(&path).unwrap()).unwrap(),
            json!({"host":"127.0.0.1","port":port})
        );
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        assert_eq!(fixture.status()["connections"], json!([]));
        // It survives a restart as an ordinary package connection, and an
        // older runtime reading the same file is not shown a half-built one.
        let reopened = Store::open(fixture.home.join("config.json")).unwrap();
        assert_eq!(reopened.config(), &after);
        let outer: Config =
            serde_json::from_slice(&fs::read(fixture.home.join("config.json")).unwrap()).unwrap();
        outer.validate().unwrap();
        assert!(outer.connections.is_empty());
        // Nothing left to do: another pass writes nothing.
        fixture.api.legacy_conversion_pass();
        assert_eq!(fixture.config().revision, after.revision);
    }

    #[test]
    fn an_unreachable_feed_leaves_it_waiting_and_is_tried_again_quickly_at_first() {
        let fixture = Fixture::new("offline", &built_in_era("avr.invalid", 23));
        let before = fs::read(fixture.home.join("config.json")).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let (seen, home) = (calls.clone(), fixture.home.clone());
        fixture.feed(move |row| {
            assert_eq!(row.package, "denon");
            // Wi-Fi is still coming up for the first three looks.
            if seen.fetch_add(1, Ordering::SeqCst) < 3 {
                return Err(Install::Unreachable(
                    "cannot download repository index over HTTPS".into(),
                ));
            }
            install_package_fixture(&home);
            Ok(())
        });
        let start = Instant::now();
        let at = |seconds: u64| start + Duration::from_secs(seconds);

        fixture.api.legacy_conversion_pass_at(start);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let status = fixture.status();
        assert_eq!(status["working"], false);
        assert_eq!(
            status["connections"][0]["message"],
            "Needs the Denon package"
        );
        assert_eq!(
            status["connections"][0]["reason"],
            "cannot download repository index over HTTPS"
        );
        assert!(status["retry_in_seconds"].as_u64().is_some());
        assert_eq!(fs::read(fixture.home.join("config.json")).unwrap(), before);
        assert!(!fixture.home.join("connections/receiver").exists());

        // 5 s, then 10 s, then 20 s: not before its time, however often the
        // daemon looks, and then again.
        fixture.api.legacy_conversion_pass_at(at(4));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        fixture.api.legacy_conversion_pass_at(at(5));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        fixture.api.legacy_conversion_pass_at(at(14));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        fixture.api.legacy_conversion_pass_at(at(15));
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        assert_eq!(fs::read(fixture.home.join("config.json")).unwrap(), before);
        fixture.api.legacy_conversion_pass_at(at(34));
        assert_eq!(calls.load(Ordering::SeqCst), 3);

        // The feed answers on the next look; the connection converts.
        fixture.api.legacy_conversion_pass_at(at(35));
        assert_eq!(calls.load(Ordering::SeqCst), 4);
        let after = fixture.config();
        assert!(matches!(
            &after.connections[0].provider,
            Provider::Plugin { id, .. } if id == "denon"
        ));
        assert_eq!(after.connections[0].id, Id::new("receiver"));
        let status = fixture.status();
        assert_eq!(status["connections"], json!([]));
        assert_eq!(status["retry_in_seconds"], Value::Null);
    }

    #[test]
    fn the_waits_are_quick_only_while_the_feed_is_out_of_reach() {
        let unreachable: Vec<u64> = (0..11).map(|n| wait_after(n, true)).collect();
        assert_eq!(
            unreachable,
            [5, 10, 20, 30, 60, 120, 300, 900, 3600, 3600, 3600]
        );
        let other: Vec<u64> = (0..8).map(|n| wait_after(n, false)).collect();
        assert_eq!(other, [30, 60, 120, 300, 900, 3600, 3600, 3600]);
        assert_eq!(wait_after(u32::MAX, true), 3600);
    }

    #[test]
    fn try_again_skips_the_backoff_but_cannot_hammer_the_feed() {
        let fixture = Fixture::new("try-again", &built_in_era("avr.invalid", 23));
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = calls.clone();
        fixture.feed(move |_| {
            seen.fetch_add(1, Ordering::SeqCst);
            // Not a network problem, so the long ladder: 30 s to the next.
            Err(Install::Failed(
                "The official package feed does not offer the Denon package yet".into(),
            ))
        });
        let start = Instant::now();
        let at = |millis: u64| start + Duration::from_millis(millis);
        let retry = || {
            assert_eq!(
                fixture
                    .api
                    .legacy_conversion_route("POST", &["retry"])
                    .status,
                202
            );
        };
        fixture.api.legacy_conversion_pass_at(start);
        fixture.api.legacy_conversion_pass_at(at(29_000));
        assert_eq!(calls.load(Ordering::SeqCst), 1, "its backoff is 30 s");
        // A client that posts as fast as it can gets one attempt per 5 s.
        for millis in (29_000..41_000).step_by(250) {
            retry();
            fixture.api.legacy_conversion_pass_at(at(millis));
        }
        // 29.0 s (the first post), 34.0 s and 39.0 s.
        assert_eq!(calls.load(Ordering::SeqCst), 4);
        // The last post is still owed an attempt, 5 s after the one before;
        // left alone after that, it is back on its ladder.
        fixture.api.legacy_conversion_pass_at(at(43_900));
        assert_eq!(calls.load(Ordering::SeqCst), 4);
        fixture.api.legacy_conversion_pass_at(at(44_000));
        assert_eq!(calls.load(Ordering::SeqCst), 5);
        fixture.api.legacy_conversion_pass_at(at(60_000));
        assert_eq!(calls.load(Ordering::SeqCst), 5);
    }

    #[test]
    fn an_install_nobody_asked_for_only_comes_from_an_official_repository() {
        let offer = |repositories: &[&str]| {
            json!({"available": repositories.iter().map(|repository| json!({
                "id":"denon","name":"Denon AVR","version":"9.9.9","repository":repository,
            })).chain([json!({"id":"echo","repository":"official-stable"})]).collect::<Vec<_>>()})
        };
        // The owner trusts a repository of their own that offers `denon`.
        // That is theirs to install from, by hand; it is never picked here.
        assert_eq!(official_source(&offer(&["living-room"]), "denon"), None);
        assert_eq!(
            official_source(&offer(&["living-room", "official-preview"]), "denon"),
            Some("official-preview")
        );
        assert_eq!(
            official_source(
                &offer(&["official-preview", "living-room", "official-stable"]),
                "denon"
            ),
            Some("official-stable")
        );
        // A look-alike id is not official, and nothing offered is nothing chosen.
        assert_eq!(
            official_source(&offer(&["official-previews"]), "denon"),
            None
        );
        assert_eq!(official_source(&json!({"available":[]}), "denon"), None);
        assert_eq!(official_source(&json!({}), "denon"), None);
    }

    #[test]
    fn an_official_package_this_couch_cannot_run_is_waited_for_with_its_reason() {
        let offer = |entries: &[(&str, bool)]| {
            json!({"available": entries.iter().map(|(repository, installable)| {
                let mut entry = json!({"id":"denon","version":"9.9.9","repository":repository,
                    "installable":installable});
                if !installable {
                    entry["reason"] = "Needs a newer Couch".into();
                }
                entry
            }).collect::<Vec<_>>()})
        };
        // Stable still carries a version this Couch runs; preview has moved on.
        let mixed = offer(&[("official-preview", false), ("official-stable", true)]);
        assert_eq!(official_source(&mixed, "denon"), Some("official-stable"));
        let mixed = offer(&[("official-preview", true), ("official-stable", false)]);
        assert_eq!(official_source(&mixed, "denon"), Some("official-preview"));
        // Nothing official can be installed: not chosen, and the reason is the feed's.
        let newer = offer(&[("official-preview", false), ("living-room", true)]);
        assert_eq!(official_source(&newer, "denon"), None);
        assert_eq!(
            official_obstacle(&newer, "denon").as_deref(),
            Some("Needs a newer Couch")
        );
        // An obstacle in the owner's own repository is not the official feed's.
        let theirs = offer(&[("living-room", false)]);
        assert_eq!(official_obstacle(&theirs, "denon"), None);
        assert_eq!(official_obstacle(&mixed, "kodi"), None);
        // A catalog from before feeds said so: everything is installable.
        let before = json!({"available":[{"id":"denon","repository":"official-preview"}]});
        assert_eq!(official_source(&before, "denon"), Some("official-preview"));
        assert_eq!(official_obstacle(&before, "denon"), None);
    }

    #[test]
    fn a_package_installed_by_hand_from_any_repository_is_used_as_it_is() {
        // However it got there (the owner's own repository, a sideload), an
        // installed package means the feed is never consulted.
        let fixture = Fixture::new("by-hand", &built_in_era("avr.invalid", 23));
        install_package_fixture(&fixture.home);
        fixture.feed(|_| panic!("an installed package needs no feed"));
        fixture.api.legacy_conversion_pass();
        assert!(fixture.config().legacy_connections().next().is_none());
    }

    #[test]
    fn a_package_swapped_between_the_check_and_the_commit_converts_nothing() {
        let fixture = Fixture::new("swapped", &built_in_era("avr.invalid", 23));
        let packages = install_package_fixture(&fixture.home);
        let prepared = fixture
            .api
            .plugins
            .prepare_legacy("denon", json!({"host":"avr.invalid","port":23}), None)
            .unwrap();
        packages.remove("denon").unwrap();
        let committed = std::cell::Cell::new(false);
        let result = fixture
            .api
            .plugins
            .adopt_legacy("receiver", &prepared, |_| {
                committed.set(true);
                Ok(())
            });
        assert!(result.is_err());
        assert!(!committed.get());
        assert!(
            !couch_sdk::connection_file(&fixture.home, "receiver", "plugin")
                .unwrap()
                .exists()
        );
    }

    #[test]
    fn an_inline_receiver_without_an_address_does_not_stop_the_daemon_starting() {
        for (name, host, port) in [
            ("blank", "", 23),
            ("space", " \t", 23),
            ("port", "avr.invalid", 0),
        ] {
            let mut inline = built_in_era("avr.invalid", 23);
            inline["connections"] = json!([]);
            inline["rooms"][0]["devices"][0]["integration"] =
                json!({"via":"denon","host":host,"port":port});
            let fixture = Fixture::new(&format!("inert-{name}"), &inline);
            let loaded = fixture.config();
            assert!(loaded.connections.is_empty(), "{name}");
            loaded.validate().unwrap();
            fixture.feed(|_| panic!("nothing waits for a package"));
            fixture.api.legacy_conversion_pass();
            // An edit saves, and what it saved opens again.
            fixture
                .api
                .store
                .lock()
                .unwrap()
                .mutate(None, |config| config.rooms[0].name = "Study".into())
                .unwrap();
            let reopened = Store::open(fixture.home.join("config.json")).unwrap();
            assert_eq!(reopened.config().rooms[0].name, "Study");
        }
    }

    #[test]
    fn a_package_that_cannot_be_verified_converts_nothing() {
        let fixture = Fixture::new("tampered", &built_in_era("avr.invalid", 23));
        let packages = install_package_fixture(&fixture.home);
        let (directory, manifest) = packages.resolve("denon").unwrap();
        let executable = directory.join(manifest.executable);
        let mut bytes = fs::read(&executable).unwrap();
        bytes.extend_from_slice(b"\n# modified after admission\n");
        fs::write(executable, bytes).unwrap();
        fixture.feed(|_| Err(Install::Unreachable("offline".into())));
        let before = fixture.config();
        fixture.api.legacy_conversion_pass();
        assert_eq!(fixture.config(), before);
        assert!(fixture.status()["connections"][0]["reason"].is_string());
        assert!(
            !couch_sdk::connection_file(&fixture.home, "receiver", "plugin")
                .unwrap()
                .exists()
        );
    }

    #[test]
    fn a_failed_config_write_leaves_the_old_connection_in_charge_until_the_retry() {
        let fixture = Fixture::new("write-fails", &built_in_era("avr.invalid", 23));
        install_package_fixture(&fixture.home);
        let before = fixture.config();
        fs::create_dir(fixture.home.join("config.json.tmp")).unwrap();
        let start = Instant::now();
        fixture.api.legacy_conversion_pass_at(start);
        assert_eq!(fixture.config(), before);
        assert!(fixture.status()["connections"][0]["reason"].is_string());
        fs::remove_dir(fixture.home.join("config.json.tmp")).unwrap();
        fixture
            .api
            .legacy_conversion_pass_at(start + Duration::from_secs(BACKOFF[0]));
        assert!(fixture.config().legacy_connections().next().is_none());
    }

    #[test]
    fn a_receiver_named_on_a_device_and_a_pilot_receipt_both_come_forward() {
        // Inline on the device, from before named connections.
        let mut inline = built_in_era("avr.invalid", 23);
        inline["connections"] = json!([]);
        inline["rooms"][0]["devices"][0]["integration"] =
            json!({"via":"denon","host":"192.0.2.7","port":23});
        let fixture = Fixture::new("inline", &inline);
        install_package_fixture(&fixture.home);
        let loaded = fixture.config();
        assert_eq!(
            loaded.connections.len(),
            1,
            "the store gave it a connection"
        );
        assert_eq!(loaded.connections[0].id, Id::new("avr"));
        fixture.api.legacy_conversion_pass();
        let after = fixture.config();
        assert!(matches!(
            &after.connections[0].provider,
            Provider::Plugin { id, .. } if id == "denon"
        ));
        assert_eq!(
            serde_json::from_slice::<Value>(
                &fs::read(couch_sdk::connection_file(&fixture.home, "avr", "plugin").unwrap())
                    .unwrap()
            )
            .unwrap(),
            json!({"host":"192.0.2.7","port":23})
        );

        // Switched by the reversible pilot: already a package connection, with
        // a receipt nothing can act on any more.
        let mut piloted: Config = serde_json::from_value(built_in_era("avr.invalid", 23)).unwrap();
        piloted.connections[0].provider = after.connections[0].provider.clone();
        piloted.denon_migrations.insert(
            Id::new("receiver"),
            couch_model::DenonMigration {
                host: "avr.invalid".into(),
                port: 23,
            },
        );
        let fixture = Fixture::new(
            "piloted",
            &serde_json::to_value(StoredConfig::new(&piloted)).unwrap(),
        );
        let loaded = fixture.config();
        assert!(loaded.denon_migrations.is_empty());
        assert_eq!(loaded.connections, piloted.connections);
        assert_eq!(fixture.status()["connections"], json!([]));
    }

    #[test]
    fn typed_http_refuses_malformed_and_out_of_range_before_device_io() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let fixture = Fixture::new(
            "typed-action",
            &built_in_era("127.0.0.1", listener.local_addr().unwrap().port()),
        );
        install_package_fixture(&fixture.home);
        fixture.api.legacy_conversion_pass();
        assert!(fixture.config().legacy_connections().next().is_none());
        for value in [
            json!({"action":"set_volume_db","tenths":-34.5}),
            json!({"action":"set_volume_db","tenths":"-345"}),
            json!({"action":"set_volume_db","tenths":-345,"arbitrary":"code"}),
            json!({"action":"set_volume_db"}),
            json!({"action":"other","tenths":-345}),
            json!({"action":"set_volume_db","tenths":-805}),
            json!({"action":"set_volume_db","tenths":-344}),
            json!({"action":"set_volume_db","tenths":185}),
        ] {
            let reply = fixture.api.plugin_route(
                "POST",
                "receiver",
                &["typed-action"],
                &serde_json::to_vec(&value).unwrap(),
            );
            assert_eq!(reply.status, 400, "{value}");
        }
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }
}
