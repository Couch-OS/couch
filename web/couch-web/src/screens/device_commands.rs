//! Activity pickers combine network capabilities with exact saved IR assignments.
use crate::api;
use couch_model::{Config, Device, Integration};
use leptos::prelude::*;
use std::collections::BTreeMap;
type Rows = Vec<(String, String)>;
#[derive(Clone)]
enum Loaded {
    Loading,
    Ready(Vec<String>),
    Failed,
}
#[derive(Clone, Copy)]
pub(super) struct Commands {
    sets: RwSignal<BTreeMap<String, Loaded>>,
}
impl Commands {
    pub fn new(config: &Config) -> Self {
        let initial: BTreeMap<String, Loaded> = config
            .devices()
            .filter(|(_, d)| d.effective_ir_codeset(config).is_some())
            .map(|(_, d)| (d.id.to_string(), Loaded::Loading))
            .collect();
        let sets = RwSignal::new(initial);
        for (room, device) in config
            .devices()
            .filter(|(_, d)| d.effective_ir_codeset(config).is_some())
        {
            let path = format!("/api/rooms/{}/devices/{}/ir", room.id, device.id);
            let id = device.id.to_string();
            leptos::task::spawn_local(async move {
                let result = match api::ha("GET", &path, None).await {
                    Ok(v) if v["error"].is_null() => v["commands"]
                        .as_array()
                        .map(|rows| {
                            Loaded::Ready(
                                rows.iter()
                                    .filter_map(|r| r["name"].as_str().map(str::to_owned))
                                    .collect(),
                            )
                        })
                        .unwrap_or(Loaded::Failed),
                    _ => Loaded::Failed,
                };
                sets.try_update(|all| {
                    all.insert(id, result);
                });
            });
        }
        Self { sets }
    }
    pub fn choices(self, config: &Config, device: &Device, dynamic: Rows) -> Rows {
        let names = match self.sets.get().get(device.id.as_str()) {
            Some(Loaded::Ready(names)) => names.clone(),
            _ => Vec::new(),
        };
        merge(config, device, &names, dynamic)
    }
    pub fn status(self) -> String {
        let sets = self.sets.get();
        if sets.values().any(|s| matches!(s, Loaded::Failed)) {
            "Some IR commands could not be loaded. Saved mappings are retained; reopen this editor to retry.".into()
        } else if sets.values().any(|s| matches!(s, Loaded::Loading)) {
            "Loading assigned IR commands…".into()
        } else {
            String::new()
        }
    }
}
/// Value-carrying functions cannot be catalog rows: the picker has to collect a
/// number first. Returns the `<kind>` prefix and its label for each level this
/// device's integration accepts, so a caller renders one control per entry.
pub fn levels(config: &Config, device: &Device) -> Vec<(&'static str, &'static str)> {
    config
        .resolve_integration(&device.integration)
        .map(|integration| couch_model::buttons::levels(&integration))
        .unwrap_or_default()
}
/// A saved `dim:30` has no catalog row to name it, so spell the value out.
pub fn value_label(command: &str) -> Option<String> {
    [
        ("dim:", "Brightness"),
        ("volume:", "Volume"),
        ("position:", "Position"),
    ]
    .into_iter()
    .find_map(|(prefix, name)| {
        command
            .strip_prefix(prefix)
            .map(|value| format!("{name} · {value}%"))
    })
}
fn merge(config: &Config, device: &Device, names: &[String], dynamic: Rows) -> Rows {
    let integration = config.resolve_integration(&device.integration);
    let mut rows = integration
        .as_ref()
        .filter(|i| !matches!(i, Integration::Ir { .. }))
        .map(|i| couch_model::buttons::function_choices(i))
        .unwrap_or_default();
    for row in dynamic {
        if !rows.iter().any(|(id, _)| id == &row.0) {
            rows.push(row);
        }
    }
    if device.effective_ir_codeset(config).is_some() {
        for &(id, label) in couch_model::buttons::functions(&Integration::Ir {
            codeset: String::new(),
        }) {
            if !names.iter().any(|name| name.eq_ignore_ascii_case(id)) {
                continue;
            }
            if let Some((_, existing)) = rows.iter_mut().find(|(candidate, _)| candidate == id) {
                *existing = format!("{label} · IR override");
            } else {
                rows.push((id.into(), format!("{label} · IR")));
            }
        }
    }
    rows
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ir_only_lists_assignments_and_never_infers_discrete_power_from_toggle() {
        let config = Config::default();
        let mut device = Device::new("tv".into(), "TV", couch_model::DeviceKind::Tv);
        device.ir = Some(couch_model::DeviceIr {
            codeset: "tv".into(),
        });
        let rows = merge(
            &config,
            &device,
            &["toggle".into(), "volume-up".into()],
            vec![],
        );
        assert_eq!(rows.len(), 2);
        assert!(!rows.iter().any(|r| r.0 == "power-on" || r.0 == "power-off"));
        assert!(merge(&config, &device, &[], vec![]).is_empty());
    }
    #[test]
    fn overrides_replace_duplicate_network_rows_and_keep_dynamic_apps() {
        let config = Config::default();
        let mut device = Device::new("tv".into(), "TV", couch_model::DeviceKind::Tv)
            .with_integration(Integration::WebOs);
        device.ir = Some(couch_model::DeviceIr {
            codeset: "tv".into(),
        });
        let rows = merge(
            &config,
            &device,
            &["volume-up".into()],
            vec![("app:netflix".into(), "Netflix".into())],
        );
        assert_eq!(rows.iter().filter(|r| r.0 == "volume-up").count(), 1);
        assert!(rows
            .iter()
            .any(|r| r.0 == "volume-up" && r.1.contains("IR override")));
        assert!(rows.iter().any(|r| r.0 == "app:netflix"));
        assert!(rows.iter().any(|r| r.0 == "up" && !r.1.contains("IR")));
    }
    #[test]
    fn level_controls_are_offered_only_where_the_client_sets_one() {
        let config = Config::default();
        let hue = Device::new("lamp".into(), "Lamp", couch_model::DeviceKind::Light)
            .with_integration(Integration::Hue {
                light_id: "id".into(),
            });
        assert_eq!(levels(&config, &hue), vec![("dim", "Brightness")]);
        let blind = Device::new("blind".into(), "Blind", couch_model::DeviceKind::Blind)
            .with_integration(Integration::HomeAssistant {
                entity_id: "cover.office".into(),
            });
        assert_eq!(levels(&config, &blind), vec![("position", "Position")]);
        let tv = Device::new("tv".into(), "TV", couch_model::DeviceKind::Tv)
            .with_integration(Integration::AppleTv);
        assert!(levels(&config, &tv).is_empty());
        assert_eq!(value_label("dim:30").as_deref(), Some("Brightness · 30%"));
        assert_eq!(value_label("play"), None);
    }
}
