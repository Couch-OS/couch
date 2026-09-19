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
//! reason), whenever the Integrations page refreshes or installs, and when
//! someone presses "Try again". Until it succeeds the connection reads "Needs
//! the Denon package" everywhere, with the reason kept here for the web UI.
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
/// A package operation somebody else started is not a failure; look again soon.
const BUSY_RETRY: Duration = Duration::from_secs(5);
const OPERATION_DEADLINE: Duration = Duration::from_secs(600);

type Installer = Box<dyn Fn(&LegacyBuiltin) -> Result<(), String> + Send>;

#[derive(Default)]
struct State {
    running: bool,
    failures: u32,
    /// When the next automatic attempt may start; `None` is "now".
    retry_at: Option<Instant>,
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
            if state.running || state.retry_at.is_some_and(|at| now < at) {
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
        }
        let outcome = self.convert_legacy_connections();
        let mut state = self.legacy.state();
        state.running = false;
        match outcome {
            Attempt::Busy => state.retry_at = Some(now + BUSY_RETRY),
            Attempt::Done(reasons) if reasons.is_empty() => *state = State::default(),
            Attempt::Done(reasons) => {
                let wait = BACKOFF[(state.failures as usize).min(BACKOFF.len() - 1)];
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
                    Err(Install::Failed(reason)) => {
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
                    reasons.insert(id.to_string(), reason);
                }
            }
        }
        Attempt::Done(reasons)
    }

    /// The same checks, settings hand-over and single config write the package
    /// connection form goes through, for a connection that already exists.
    fn convert_legacy_connection(&self, id: &Id, row: &LegacyBuiltin) -> Result<(), String> {
        // Config readers and writers wait here through verification and the
        // commit, so nothing edits the connection between the two.
        let mut store = self.store.lock().unwrap_or_else(|e| e.into_inner());
        let mut next = store.config().clone();
        let Some(provider) = next.connection(id).map(|c| c.provider.clone()) else {
            return Ok(());
        };
        let Some(settings) = settings_value(row, &provider) else {
            return Ok(());
        };
        let revision = store.revision();
        self.plugins
            .adopt_legacy(id.as_str(), row.package, settings, |manifest| {
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
            return installer(row).map_err(Install::Failed);
        }
        // Exactly what the Integrations page does: refresh the signed indexes,
        // then install the package from the repository that offers it.
        let refreshed = self.package_operation("refresh", None);
        if let Err(Install::Busy) = refreshed {
            return Err(Install::Busy);
        }
        let catalog = self
            .integration_packages
            .catalog(&[])
            .map_err(|e| Install::Failed(e.to_string()))?;
        let offered: Vec<&str> = catalog["available"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|package| package["id"] == row.package)
            .filter_map(|package| package["repository"].as_str())
            .collect();
        let repository = ["official-stable", "official-preview"]
            .into_iter()
            .find(|official| offered.contains(official))
            .or(offered.first().copied());
        let Some(repository) = repository else {
            return Err(Install::Failed(match refreshed {
                Err(Install::Failed(error)) => format!(
                    "The package feed could not be read ({error}). Check the remote's internet connection."
                ),
                _ => format!("No trusted repository offers the {} package", row.name),
            }));
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
    /// Why each connection that is still waiting could not be converted.
    Done(BTreeMap<String, String>),
}

enum Install {
    Busy,
    Failed(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{assets::Assets, auth::Auth, store::Store};
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
        fn feed(&self, installer: impl Fn(&LegacyBuiltin) -> Result<(), String> + Send + 'static) {
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
        fs::write(&apk, "#!/bin/sh\nset -eu\nwhile [ $# -gt 0 ]; do\n  if [ \"$1\" = --root ]; then shift; destination=$1; fi\n  last=$1; shift\ndone\ntar -xzf \"$last\" -C \"$destination\"\n").unwrap();
        fs::set_permissions(&apk, fs::Permissions::from_mode(0o755)).unwrap();
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
            Err("the feed must not be asked for an installed package".into())
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
    fn an_unreachable_feed_leaves_it_waiting_and_is_tried_again_on_a_backoff() {
        let fixture = Fixture::new("offline", &built_in_era("avr.invalid", 23));
        let before = fs::read(fixture.home.join("config.json")).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let (seen, home) = (calls.clone(), fixture.home.clone());
        fixture.feed(move |row| {
            assert_eq!(row.package, "denon");
            // Offline twice, then the feed answers.
            if seen.fetch_add(1, Ordering::SeqCst) < 2 {
                return Err("cannot download repository index over HTTPS".into());
            }
            install_package_fixture(&home);
            Ok(())
        });
        let start = Instant::now();

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

        // Not before its time, however often the daemon looks.
        fixture
            .api
            .legacy_conversion_pass_at(start + Duration::from_secs(BACKOFF[0] - 1));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        // Then again, and the wait grows.
        let second = start + Duration::from_secs(BACKOFF[0]);
        fixture.api.legacy_conversion_pass_at(second);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        fixture
            .api
            .legacy_conversion_pass_at(second + Duration::from_secs(BACKOFF[1] - 1));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(fs::read(fixture.home.join("config.json")).unwrap(), before);

        // "Try again" does not wait for the backoff.
        assert_eq!(
            fixture
                .api
                .legacy_conversion_route("POST", &["retry"])
                .status,
            202
        );
        fixture
            .api
            .legacy_conversion_pass_at(second + Duration::from_secs(1));
        assert_eq!(calls.load(Ordering::SeqCst), 3);
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
    fn a_package_that_cannot_be_verified_converts_nothing() {
        let fixture = Fixture::new("tampered", &built_in_era("avr.invalid", 23));
        let packages = install_package_fixture(&fixture.home);
        let (directory, manifest) = packages.resolve("denon").unwrap();
        let executable = directory.join(manifest.executable);
        let mut bytes = fs::read(&executable).unwrap();
        bytes.extend_from_slice(b"\n# modified after admission\n");
        fs::write(executable, bytes).unwrap();
        fixture.feed(|_| Err("offline".into()));
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
