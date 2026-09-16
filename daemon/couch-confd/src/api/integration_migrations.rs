use super::{parse, Api, Reply};
use couch_model::{Id, Provider};
use serde::Deserialize;
use serde_json::json;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Change {
    action: Action,
    revision: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{assets::Assets, auth::Auth, store::Store};
    use couch_model::{Config, Connection, StoredConfig};
    use std::{fs, path::PathBuf, sync::Arc};

    struct Fixture {
        api: Api,
        home: PathBuf,
    }
    impl Fixture {
        fn new(name: &str, migrated: bool) -> Self {
            let home = std::env::temp_dir().join(format!(
                "couch-denon-migration-{name}-{}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&home);
            fs::create_dir_all(&home).unwrap();
            let mut config = Config::default();
            config.connections.push(Connection {
                id: Id::new("receiver"),
                name: "Receiver".into(),
                provider: Provider::Denon {
                    host: format!("{name}.invalid"),
                    port: 23,
                },
            });
            if migrated {
                config
                    .migrate_denon(
                        &Id::new("receiver"),
                        Provider::Plugin {
                            id: "denon".into(),
                            label: "Denon".into(),
                            capabilities: vec![],
                            supports_inputs: true,
                            presentation: vec![],
                        },
                    )
                    .unwrap();
            }
            fs::write(
                home.join("config.json"),
                serde_json::to_vec(&StoredConfig::new(&config)).unwrap(),
            )
            .unwrap();
            let api = Api::new(
                Store::open(home.join("config.json")).unwrap(),
                Assets::embedded(),
                Arc::new(Auth::new(home.join("pin"), true)),
            );
            Self { api, home }
        }
        fn action(&self, action: &str, revision: u64) -> Reply {
            self.api.denon_migration_route(
                "POST",
                &["receiver"],
                &serde_json::to_vec(&json!({"action":action,"revision":revision})).unwrap(),
            )
        }

        /// Real archive audit, immutable selection, hash revalidation and host
        /// handshake; APK extraction/signature checking is a test fixture.
        /// Real signature admission remains covered by the package smoke test.
        fn install_package_fixture(&self) -> couch_integrations::Store {
            use std::os::unix::fs::PermissionsExt;
            let manifest: serde_json::Value =
                serde_json::from_str(include_str!("../../../../clients/couch-denon/plugin.json"))
                    .unwrap();
            let payload = self.home.join("payload");
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
            let package = self.home.join("denon-fixture.apk");
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
            let apk = self.home.join("fixture-apk");
            fs::write(&apk, "#!/bin/sh\nset -eu\nwhile [ $# -gt 0 ]; do\n  if [ \"$1\" = --root ]; then shift; destination=$1; fi\n  last=$1; shift\ndone\ntar -xzf \"$last\" -C \"$destination\"\n").unwrap();
            fs::set_permissions(&apk, fs::Permissions::from_mode(0o755)).unwrap();
            let packages =
                couch_integrations::Store::new(self.home.join("integrations")).with_apk(apk);
            packages.install(&package).unwrap();
            packages
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.home);
        }
    }

    fn manual_denon() -> Connection {
        Connection {
            id: Id::new("manual"),
            name: "Manual package".into(),
            provider: Provider::Plugin {
                id: "denon".into(),
                label: "Denon".into(),
                capabilities: vec![],
                supports_inputs: true,
                presentation: vec![],
            },
        }
    }

    #[test]
    fn migration_rejects_a_configured_package_owner_before_preparing_settings() {
        let fixture = Fixture::new("duplicate-package", false);
        fixture
            .api
            .store
            .lock()
            .unwrap()
            .mutate(None, |config| config.connections.push(manual_denon()))
            .unwrap();
        let path = couch_sdk::connection_file(&fixture.home, "manual", "plugin").unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        couch_sdk::save_private(
            &path,
            &json!({"host":"duplicate-package.invalid","port":23}),
        )
        .unwrap();
        let before = fixture.api.with(|store| store.config().clone());
        let response = fixture.action("migrate", 1);
        assert_eq!(response.status, 409);
        assert!(String::from_utf8_lossy(&response.body).contains("already owns"));
        assert_eq!(fixture.api.with(|store| store.config().clone()), before);
        assert!(
            !couch_sdk::connection_file(&fixture.home, "receiver", "plugin")
                .unwrap()
                .exists()
        );
    }

    #[test]
    fn settings_merge_and_import_cannot_add_an_owner_for_a_migrated_target() {
        let fixture = Fixture::new("protected-target", true);
        fixture.install_package_fixture();
        let path = couch_sdk::connection_file(&fixture.home, "manual", "plugin").unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        couch_sdk::save_private(&path, &json!({"host":"protected-target.invalid","port":23}))
            .unwrap();
        let before = fixture.api.with(|store| store.config().clone());
        assert!(fixture
            .api
            .store
            .lock()
            .unwrap()
            .mutate(None, |config| config.connections.push(manual_denon()))
            .is_err());
        assert_eq!(fixture.api.with(|store| store.config().clone()), before);

        couch_sdk::save_private(&path, &json!({"host":"protected-target.invalid","port":24}))
            .unwrap();
        fixture
            .api
            .store
            .lock()
            .unwrap()
            .mutate(None, |config| config.connections.push(manual_denon()))
            .unwrap();
        let previous_settings = fs::read(&path).unwrap();
        // The patch omits host. Compare the effective merged endpoint, not
        // only fields present in the request.
        assert_eq!(
            fixture
                .api
                .plugin_route("POST", "manual", &["settings"], br#"{"port":23}"#)
                .status,
            400
        );
        assert_eq!(fs::read(&path).unwrap(), previous_settings);
        // Null removes the saved port and the manifest restores its default.
        assert_eq!(
            fixture
                .api
                .plugin_route("POST", "manual", &["settings"], br#"{"port":null}"#)
                .status,
            400
        );
        assert_eq!(fs::read(&path).unwrap(), previous_settings);
        assert_eq!(
            fixture
                .api
                .plugin_route("POST", "manual", &["settings"], br#"{"port":25}"#)
                .status,
            200
        );
        let saved: serde_json::Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        assert_eq!(saved["port"], 25);
        assert_eq!(fixture.action("restore-native", 1).status, 200);
        // Removing the receipt must not remove protection for the now-native
        // receiver. Settings continue to share the same config mutex.
        assert_eq!(
            fixture
                .api
                .plugin_route("POST", "manual", &["settings"], br#"{"port":23}"#)
                .status,
            400
        );
    }

    #[test]
    fn manual_saves_and_retained_settings_imports_cannot_duplicate_native_receivers() {
        for inline in [false, true] {
            let name = if inline {
                "inline-native-target"
            } else {
                "native-target"
            };
            let fixture = Fixture::new(name, false);
            let host = format!("{name}.invalid");
            if inline {
                fixture
                    .api
                    .store
                    .lock()
                    .unwrap()
                    .mutate(None, |config| {
                        config.connections.clear();
                        config.rooms.push(couch_model::Room {
                            id: Id::new("room"),
                            name: "Room".into(),
                            icon: None,
                            devices: vec![couch_model::Device::new(
                                Id::new("avr"),
                                "AVR",
                                couch_model::DeviceKind::Speaker,
                            )
                            .with_integration(
                                couch_model::Integration::Denon {
                                    host: host.clone(),
                                    port: 23,
                                },
                            )],
                        });
                    })
                    .unwrap();
            }
            fixture.install_package_fixture();
            let path = couch_sdk::connection_file(&fixture.home, "manual", "plugin").unwrap();
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            couch_sdk::save_private(&path, &json!({"host":host,"port":23})).unwrap();
            let before = fixture.api.with(|store| store.config().clone());
            assert!(fixture
                .api
                .store
                .lock()
                .unwrap()
                .mutate(None, |config| config.connections.push(manual_denon()))
                .is_err());
            assert_eq!(fixture.api.with(|store| store.config().clone()), before);

            fs::remove_file(&path).unwrap();
            fixture
                .api
                .store
                .lock()
                .unwrap()
                .mutate(None, |config| config.connections.push(manual_denon()))
                .unwrap();
            let request = serde_json::to_vec(&json!({"host":host})).unwrap();
            assert_eq!(
                fixture
                    .api
                    .plugin_route("POST", "manual", &["settings"], &request)
                    .status,
                400
            );
            assert!(!path.exists());
            let request = serde_json::to_vec(&json!({"host":host,"port":24})).unwrap();
            assert_eq!(
                fixture
                    .api
                    .plugin_route("POST", "manual", &["settings"], &request)
                    .status,
                200
            );

            // The reverse direction is also an import/edit: moving the native
            // target onto the package's saved endpoint must fail atomically.
            let before = fixture.api.with(|store| store.config().clone());
            assert!(fixture
                .api
                .store
                .lock()
                .unwrap()
                .mutate(None, |config| {
                    if inline {
                        config.rooms[0].devices[0].integration = couch_model::Integration::Denon {
                            host: host.clone(),
                            port: 24,
                        };
                    } else {
                        config.connections[0].provider = Provider::Denon {
                            host: host.clone(),
                            port: 24,
                        };
                    }
                })
                .is_err());
            assert_eq!(fixture.api.with(|store| store.config().clone()), before);
        }
    }

    #[test]
    fn migration_requires_verified_package_and_changes_nothing_when_missing() {
        let fixture = Fixture::new("missing", false);
        let before = fs::read(fixture.home.join("config.json")).unwrap();
        assert_eq!(fixture.action("migrate", 0).status, 409);
        assert_eq!(fs::read(fixture.home.join("config.json")).unwrap(), before);
        assert!(!fixture
            .home
            .join("connections/receiver/plugin-connection.json")
            .exists());
        assert_eq!(fixture.action("migrate", 99).status, 409);
        assert_eq!(fixture.api.with(|s| s.revision()), 0);
    }

    #[test]
    fn tampered_installed_fixture_cannot_prepare_or_convert_native_settings() {
        let fixture = Fixture::new("tampered", false);
        let packages = fixture.install_package_fixture();
        let (directory, manifest) = packages.resolve("denon").unwrap();
        let executable = directory.join(manifest.executable);
        let mut bytes = fs::read(&executable).unwrap();
        bytes.extend_from_slice(b"\n# modified after admission\n");
        fs::write(executable, bytes).unwrap();
        let before = fixture.api.with(|store| store.config().clone());
        assert_eq!(fixture.action("migrate", 0).status, 409);
        assert_eq!(fixture.api.with(|store| store.config().clone()), before);
        assert!(
            !couch_sdk::connection_file(&fixture.home, "receiver", "plugin")
                .unwrap()
                .exists()
        );
    }

    #[test]
    fn restoration_works_without_package_and_preserves_private_settings() {
        let fixture = Fixture::new("restore", true);
        let path = couch_sdk::connection_file(&fixture.home, "receiver", "plugin").unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        couch_sdk::save_private(&path, &json!({"host":"restore.invalid","port":23})).unwrap();
        let before = fs::read(&path).unwrap();
        assert_eq!(fixture.action("migrate", 0).status, 200); // already migrated
        assert_eq!(fixture.api.with(|s| s.revision()), 0);
        assert_eq!(fixture.action("restore-native", 0).status, 200);
        assert_eq!(fixture.action("restore-native", 1).status, 200); // already native
        assert_eq!(fixture.api.with(|s| s.revision()), 1);
        assert!(fixture.api.with(|s| s.config().denon_migrations.is_empty()));
        assert!(matches!(
            fixture
                .api
                .with(|s| s.config().connections[0].provider.clone()),
            Provider::Denon { port: 23, .. }
        ));
        assert_eq!(fs::read(path).unwrap(), before);
        assert_eq!(
            fixture
                .api
                .plugins
                .execute("receiver", "denon", couch_plugin::Request::Status),
            Err(couch_plugin::Error::Invalid)
        );
    }

    #[test]
    fn normal_edits_cannot_bypass_ownership_or_change_migrated_settings() {
        let fixture = Fixture::new("edits", true);
        let mut store = fixture.api.store.lock().unwrap();
        let before = store.config().clone();
        assert!(store
            .mutate(None, |c| {
                c.denon_migrations.clear();
            })
            .is_err());
        assert!(store
            .mutate(None, |c| {
                c.connections.clear();
            })
            .is_err());
        assert_eq!(store.config(), &before);
        store
            .mutate(None, |c| c.connections[0].name = "Renamed".into())
            .unwrap();
        drop(store);
        assert_eq!(
            fixture
                .api
                .plugin_route(
                    "POST",
                    "receiver",
                    &["settings"],
                    br#"{"host":"different.invalid","port":23}"#
                )
                .status,
            409
        );
    }

    #[test]
    fn failed_restore_commit_keeps_migration_and_execution_owner() {
        let fixture = Fixture::new("failed-restore", true);
        // Store's temporary-file creation fails before its atomic rename.
        fs::create_dir(fixture.home.join("config.json.tmp")).unwrap();
        let before = fixture.api.with(|s| s.config().clone());
        assert_eq!(fixture.action("restore-native", 0).status, 409);
        assert_eq!(fixture.api.with(|s| s.config().clone()), before);
        assert_eq!(
            Store::open(fixture.home.join("config.json"))
                .unwrap()
                .config(),
            &before
        );
    }

    #[test]
    fn installed_fixture_migrates_reuses_prepared_settings_and_restores_without_receiver_io() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = Fixture::new("installed", false);
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        fixture
            .api
            .store
            .lock()
            .unwrap()
            .mutate(None, |config| {
                config.connections[0].provider = Provider::Denon {
                    host: "127.0.0.1".into(),
                    port,
                };
            })
            .unwrap();
        let original = fixture.api.with(|store| store.config().clone());
        let packages = fixture.install_package_fixture();
        let path = couch_sdk::connection_file(&fixture.home, "receiver", "plugin").unwrap();

        // Preparation succeeds, but config replacement fails. The native
        // document stays authoritative and a retry can reuse the durable file.
        fs::create_dir(fixture.home.join("config.json.tmp")).unwrap();
        assert_eq!(fixture.action("migrate", 1).status, 409);
        assert_eq!(fixture.api.with(|store| store.config().clone()), original);
        let prepared = fs::read(&path).unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&prepared).unwrap(),
            json!({"host":"127.0.0.1","port":port})
        );
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        fs::remove_dir(fixture.home.join("config.json.tmp")).unwrap();

        // A recovery-confirmation rename fails after the live config rename.
        // That must remain a committed migration, with native access blocked.
        let recovery = fixture.home.join("config.integration-recovery.json");
        fs::create_dir(&recovery).unwrap();

        let reply = fixture.action("migrate", 1);
        assert_eq!(
            reply.status,
            200,
            "{}",
            String::from_utf8_lossy(&reply.body)
        );
        assert_eq!(fixture.action("migrate", 2).status, 200);
        assert_eq!(fixture.api.with(|store| store.revision()), 2);
        assert_eq!(fs::read(&path).unwrap(), prepared);
        assert!(fixture
            .home
            .join("config.integration-recovery.pending.json")
            .is_file());
        fs::remove_dir(recovery).unwrap();
        let envelope = fs::read(fixture.home.join("config.json")).unwrap();
        let mut old: Config = serde_json::from_slice(&envelope).unwrap();
        old.revision = original.revision;
        assert_eq!(old, original);
        assert!(Store::open(fixture.home.join("config.json"))
            .unwrap()
            .config()
            .migrated_denon(&Id::new("receiver"))
            .is_some());
        assert!(couch_control::Denon::connect(&couch_denon::Settings {
            host: "127.0.0.1".into(),
            port
        })
        .unwrap()
        .status()
        .is_err());
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );

        packages.remove("denon").unwrap();
        assert_eq!(fixture.action("restore-native", 2).status, 200);
        let mut restored = fixture.api.with(|store| store.config().clone());
        restored.revision = original.revision;
        assert_eq!(restored, original);
        assert_eq!(fs::read(path).unwrap(), prepared);
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Action {
    Migrate,
    RestoreNative,
}

impl Api {
    pub(super) fn denon_migration_route(&self, method: &str, path: &[&str], body: &[u8]) -> Reply {
        if method == "GET" && path.is_empty() {
            let available = self.plugins.manifest("denon").is_ok();
            return self.with(|store| {
                let config = store.config();
                let connections: Vec<_> = config.connections.iter().filter_map(|c| {
                    let state = if config.migrated_denon(&c.id).is_some() { "migrated" }
                        else if matches!(c.provider, Provider::Denon { .. }) { "native" }
                        else { return None };
                    Some(json!({"id":c.id,"name":c.name,"state":state}))
                }).collect();
                Reply::json(200, &json!({"revision":config.revision,"connections":connections,"package_available":available}))
            });
        }
        let ("POST", [id]) = (method, path) else {
            return Reply::error(404, "Unknown Denon migration operation");
        };
        let change: Change = match parse(body) {
            Ok(v) => v,
            Err(reply) => return reply,
        };
        let id = Id::new(*id);
        // Serialize config readers/writers through verification, ownership
        // transfer and commit so no newly looked-up native request can escape.
        let mut store = self.store.lock().unwrap_or_else(|e| e.into_inner());
        if change.revision != store.revision() {
            return Reply::error(409, "Configuration changed; refresh before migrating");
        }
        let mut next = store.config().clone();
        let result = match change.action {
            Action::Migrate => {
                if next.migrated_denon(&id).is_some() {
                    return Reply::json(200, &json!({"changed":false,"revision":store.revision()}));
                }
                let Some(Provider::Denon { host, port }) =
                    next.connection(&id).map(|c| &c.provider)
                else {
                    return Reply::error(400, "Choose a built-in Denon connection");
                };
                let settings = couch_denon::Settings {
                    host: host.clone(),
                    port: *port,
                };
                // Denon settings saves hold the same config mutex. This read
                // therefore stays authoritative through the ownership commit.
                for other in &next.connections {
                    if other.id == id
                        || !matches!(&other.provider, Provider::Plugin { id, .. } if id == "denon")
                    {
                        continue;
                    }
                    match self.plugins.denon_target(other.id.as_str()) {
                        Ok(Some(target))
                            if target.host == settings.host && target.port == settings.port =>
                        {
                            return Reply::error(
                                409,
                                "Another Denon package connection already owns this receiver",
                            );
                        }
                        Err(message) => return Reply::error(409, message),
                        _ => {}
                    }
                }
                self.plugins
                    .migrate_denon(id.as_str(), &settings, |manifest| {
                        next.migrate_denon(
                            &id,
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
                            },
                        )?;
                        if let Err(error) =
                            couch_control::block_denon(&settings.host, settings.port)
                        {
                            let _ = couch_control::unblock_denon(&settings.host, settings.port);
                            return Err(error.to_string());
                        }
                        if let Err(error) =
                            store.mutate_migration(Some(change.revision), |config| *config = next)
                        {
                            let _ = couch_control::unblock_denon(&settings.host, settings.port);
                            return Err(error.to_string());
                        }
                        Ok(())
                    })
            }
            Action::RestoreNative => {
                let original = next.migrated_denon(&id).cloned();
                match next.restore_native_denon(&id) {
                    Ok(false) => {
                        return Reply::json(
                            200,
                            &json!({"changed":false,"revision":store.revision()}),
                        )
                    }
                    Err(message) => return Reply::error(400, message),
                    Ok(true) => {}
                }
                let original = original.unwrap();
                self.plugins
                    .restore_denon(id.as_str(), || {
                        store
                            .mutate_migration(Some(change.revision), |config| *config = next)
                            .map_err(|e| e.to_string())?;
                        Ok(())
                    })
                    .and_then(|()| {
                        couch_control::unblock_denon(&original.host, original.port)
                            .map_err(|e| e.to_string())
                    })
            }
        };
        match result {
            Ok(()) => Reply::json(200, &json!({"changed":true,"revision":store.revision()})),
            Err(message) => Reply::error(409, message),
        }
    }
}
