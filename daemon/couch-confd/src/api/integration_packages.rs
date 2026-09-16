use super::{parse, Api, Reply};
use couch_integrations::management::{Action, NewRepository};
use serde::Deserialize;
use serde_json::json;

impl Api {
    pub(super) fn integration_package_route(
        &self,
        method: &str,
        path: &[&str],
        body: &[u8],
    ) -> Reply {
        let manager = &self.integration_packages;
        let result = match (method, path) {
            ("GET", ["recovery"]) => {
                return self.with(|store| {
                    Reply::json(200, &json!({"recovery":store.integration_recovery()}))
                })
            }
            ("GET", ["recovery", "config"]) => {
                return self.with(|store| match store.integration_recovery_config() {
                    Ok(Some(config)) => Reply::json(200, &config),
                    Ok(None) => {
                        Reply::error(404, "No confirmed integration configuration is available")
                    }
                    Err(error) => Reply::error(500, error.to_string()),
                })
            }
            ("GET", ["catalog"]) => {
                let configured = self.with(|store| {
                    store
                        .config()
                        .connections
                        .iter()
                        .filter_map(|connection| match &connection.provider {
                            couch_model::Provider::Plugin { id, .. } => Some(id.clone()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                });
                manager.catalog(&configured)
            }
            ("GET", ["operations", id]) => {
                return match manager.operation(id) {
                    Some(operation) => Reply::json(200, &operation),
                    None => Reply::error(
                        404,
                        "Operation is no longer available; refresh installed packages",
                    ),
                }
            }
            ("POST", ["repositories"]) => {
                let input: NewRepository = match parse(body) {
                    Ok(input) => input,
                    Err(reply) => return reply,
                };
                manager.stage_repository(input).map(|repository| {
                    json!({"pending_confirmation":{
                        "id":repository.id,"name":repository.name,"url":repository.url,
                        "fingerprint":repository.fingerprint,"algorithm":"SHA-256 (PEM)"
                    }})
                })
            }
            ("POST", ["repositories", id, "confirm"]) => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Confirmation {
                    fingerprint: String,
                }
                let input: Confirmation = match parse(body) {
                    Ok(input) => input,
                    Err(reply) => return reply,
                };
                manager
                    .confirm_repository(id, &input.fingerprint)
                    .map(|repository| json!({"repository":repository}))
            }
            ("DELETE", ["repositories", id]) => manager
                .remove_repository(id)
                .map(|_| json!({"removed":true})),
            ("POST", [operation @ ("refresh" | "install" | "update" | "remove" | "rollback")]) => {
                let action = if *operation == "refresh" {
                    None
                } else {
                    let action: Action = match parse(body) {
                        Ok(input) => input,
                        Err(reply) => return reply,
                    };
                    Some(action)
                };
                return match manager.start(operation, action) {
                    Ok(id) => Reply::json(202, &json!({"operation_id":id})),
                    Err(error) => {
                        Reply::error(if error.is_busy() { 409 } else { 400 }, error.to_string())
                    }
                };
            }
            _ => return Reply::error(404, "Unknown package management operation"),
        };
        match result {
            Ok(value) => Reply::json(200, &value),
            Err(error) => Reply::error(if error.is_busy() { 409 } else { 400 }, error.to_string()),
        }
    }
}
