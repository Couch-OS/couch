//! Connections: the list, and one page per connection.
//!
//! The list only says what each connection is and how much depends on it;
//! everything that can be changed about a connection lives on its own page,
//! reached by opening its card. Creating a connection lands on that page too,
//! because for most providers creating the record is the first of two steps
//! and the second (address, pairing, credentials) is only offered there.
use crate::{api, route::Route, ui, App};
use couch_model::{
    Connection, Id, Integration, PluginActionSchema, PluginCapability, PluginComponent,
    PluginStatusField, Provider, TypedAction, VolumeDb,
};
use leptos::prelude::*;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Deserialize)]
struct PluginCatalog {
    #[serde(default)]
    integrations: Vec<PluginManifest>,
}

#[derive(Clone, Debug, Deserialize)]
struct PluginManifest {
    id: String,
    label: String,
    #[serde(default)]
    capabilities: Vec<PluginCapability>,
    #[serde(default)]
    actions: Vec<PluginActionSchema>,
    #[serde(default)]
    settings: Vec<PluginSetting>,
    #[serde(default)]
    supports_inputs: bool,
    #[serde(default)]
    presentation: Vec<PluginComponent>,
    /// Protocol 3 (unreleased): how this package pairs, if it pairs at all.
    /// Absent from every manifest a shipped build accepts.
    #[serde(default)]
    pairing: Option<super::plugin_pairing::Pairing>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PluginFieldKind {
    Text,
    Secret,
    Integer,
    Boolean,
}

#[derive(Clone, Debug, Deserialize)]
struct PluginSetting {
    id: String,
    label: String,
    kind: PluginFieldKind,
    #[serde(default)]
    required: bool,
    #[serde(default)]
    default: Option<Value>,
}

pub fn screen(app: App) -> AnyView {
    let choice = RwSignal::new(String::new());
    let plugins = RwSignal::new(Vec::<PluginManifest>::new());
    let catalog_status = RwSignal::new("Loading installed integrations…".to_string());
    leptos::task::spawn_local(async move {
        match api::ha("GET", "/api/integrations", None).await {
            Ok(value) => match serde_json::from_value::<PluginCatalog>(value) {
                Ok(catalog) => {
                    catalog_status.set(if catalog.integrations.is_empty() {
                        "No external integration packages are installed.".into()
                    } else {
                        format!(
                            "{} external integration package{} installed.",
                            catalog.integrations.len(),
                            if catalog.integrations.len() == 1 {
                                ""
                            } else {
                                "s"
                            }
                        )
                    });
                    plugins.set(catalog.integrations);
                }
                Err(_) => catalog_status.set("The integration catalog was unreadable.".into()),
            },
            Err(error) => {
                if error.unauthorized {
                    app.paired.set(Some(false));
                }
                catalog_status.set(error.message);
            }
        }
    });
    // Infrared is not a connection anyone adds here; it is built into the
    // remote and configured on each device.
    let order = Memo::new(move |_| {
        app.connections.with(|all| {
            all.iter()
                .filter(|c| c.provider != Provider::Ir)
                .map(|c| c.id.clone())
                .collect::<Vec<_>>()
        })
    });
    let available: Vec<_> = [
        ("kodi", "Kodi"),
        ("core-elec", "CoreELEC"),
        ("sonos", "Sonos"),
        ("home-assistant", "Home Assistant"),
        ("android-tv", "Android / Google TV · experimental"),
        ("apple-tv", "Apple TV · experimental"),
        ("tizen", "Samsung Tizen TV · experimental"),
        ("unifi-protect", "UniFi Protect"),
        ("matter", "Matter · experimental"),
    ]
    .into_iter()
    .collect();
    view!{
        {ui::page_header(app,"Connections",None)}
        <p class="lead">"Connections tell Couch how to reach your TVs, speakers, servers and bridges. Open one to change its address, pair it or test it. Add devices and assign their infrared commands in Rooms & devices."</p>
        <p class="notice">"Using infrared? Open a device in Rooms & devices and choose Add IR commands. No infrared connection is needed."</p>
        <h2 class="section">"Saved connections" <span class="count">{move ||ui_count(order.with(Vec::len))}</span></h2>
        {move ||order.with(Vec::is_empty).then(||ui::empty("No connections yet. Add your first connection below."))}
        <div class="destination-grid"><For each=move ||order.get() key=|id|id.clone() children=move |id|card(app,id)/></div>
        <section class="creation"><h2>"Add a connection"</h2>
        <label class="field">"Connection type"<select aria-label="Connection type" prop:value=move || choice.get() on:change=move |e|choice.set(event_target_value(&e))><option value="">"Choose a type"</option>{available.into_iter().map(|(kind,label)|view!{<option value=kind>{label}</option>}).collect_view()}{move ||plugins.get().into_iter().map(|plugin|view!{<option value=format!("plugin:{}",plugin.id)>{format!("{} · installed package",plugin.label)}</option>}).collect_view()}{move ||{let installed=plugins.get();PACKAGED.iter().filter(|(id,_)|!installed.iter().any(|plugin|plugin.id==*id)).map(|(id,label)|view!{<option value=format!("package:{id}")>{format!("{label} · integration package")}</option>}).collect_view()}}</select></label>
        <p class="dim" role="status">{move ||catalog_status.get()}</p>
        {move || {let selected=choice.get();if let Some(id)=selected.strip_prefix("package:"){
            // Installed since the list was drawn (from Integrations, in another
            // tab): go straight to its form.
            let id=id.to_string();
            match plugins.get().into_iter().find(|plugin|plugin.id==id){Some(plugin)=>create_plugin(app,plugin),None=>install_first(app,&id)}
        }else if let Some(id)=selected.strip_prefix("plugin:"){plugins.get().into_iter().find(|plugin|plugin.id==id).map(|plugin|create_plugin(app,plugin)).unwrap_or_else(||view!{<p class="notice">"That integration package is no longer installed. Existing connections are retained, but a new one cannot be created."</p>}.into_any())}else{match selected.as_str(){"unifi-protect"=>create_named(app,Provider::UnifiProtect),"matter"=>create_named(app,Provider::Matter),"sonos"=>super::sonos::form(app,None),"core-elec"=>super::coreelec::form(app,None),"kodi"=>local_form(app,None,false),"home-assistant"=>create_named(app,Provider::HomeAssistant),"android-tv"=>create_named(app,Provider::AndroidTv),"apple-tv"=>create_named(app,Provider::AppleTv),"tizen"=>create_named(app,Provider::Tizen),_=>view!{<p class="dim">"Add multiple bridges, servers and TVs. Infrared is built into the remote and is configured on each device."</p>}.into_any()}}}}
        </section>
    }.into_any()
}

/// Integrations that are packages rather than part of Couch, offered in the
/// connection picker all the same so nobody has to know that to find them.
/// Installed, a package lists itself; until then its entry leads to the
/// Integrations page.
const PACKAGED: [(&str, &str); 3] = [
    ("denon", "Denon AVR"),
    ("hue", "Philips Hue"),
    ("webos", "LG webOS TV"),
];

fn install_first(app: App, id: &str) -> AnyView {
    let label = PACKAGED
        .iter()
        .find(|(package, _)| *package == id)
        .map_or(id, |(_, label)| *label)
        .to_string();
    view! {<div class="notice" role="status">
        <strong>{format!("{label} is an integration package")}</strong>
        <p>{format!("Install {label} from Integrations first. It takes a moment and needs the remote to be online; then choose it here again to enter the address.")}</p>
        <button class="primary" type="button" on:click=move |_| app.go(Route::Integrations)>"Open Integrations"</button>
    </div>}.into_any()
}

/// A Bluetooth TV connection from before per-device pairing. The daemon
/// migrates it away on its next start; until then the page says where the
/// feature went.
fn bluetooth_notes() -> AnyView {
    view!{
        <p>"Bluetooth pairing now belongs to each device: open the device in Rooms & devices and use its Bluetooth section. This connection carries nothing and is removed automatically."</p>
        <p class="dim">"Keys are standard consumer-control usages (volume, navigation, playback, power toggle). Which ones a TV honours depends on its make."</p>
    }.into_any()
}

fn ui_count(n: usize) -> String {
    super::counts(&[(n, "connection", "connections")])
}

/// The devices that reach the house through this connection, with their rooms.
///
/// Read from the device and room slices rather than the document, so adding a
/// device updates the list without the connection's page being rebuilt.
fn assigned(app: App, id: StoredValue<Id>) -> Memo<Vec<(Id, String, String)>> {
    Memo::new(move |_| {
        app.devices.with(|all| {
            all.iter()
                .filter(|(_, d)| matches!(&d.integration, Integration::Connection{connection_id,..} if id.with_value(|id| connection_id==id)))
                .map(|(room, d)| {
                    let name = app.rooms.with(|rooms| {
                        rooms
                            .iter()
                            .find(|r| &r.id == room)
                            .map(|r| r.name.clone())
                            .unwrap_or_default()
                    });
                    (room.clone(), name, d.name.clone())
                })
                .collect()
        })
    })
}

/// One line saying where a connection points, without any secret.
fn address(c: &Connection) -> String {
    match &c.provider {
        Provider::Sonos { host } => format!("{host} · Local Sonos control"),
        Provider::Kodi { host, port } | Provider::CoreElec { host, port } => {
            format!("{host}:{port} · Saved address")
        }
        Provider::LegacyDenon { host, port } => format!(
            "{host}:{port} · {}",
            c.provider
                .legacy_builtin()
                .map(|row| row.needs_package())
                .unwrap_or_default()
        ),
        Provider::Ir => "Built-in transmitter · Codes are configured per device".into(),
        _ => "Credentials are kept privately on the remote".into(),
    }
}

/// A saved connection in the list: what it is, what depends on it, and a way in.
fn card(app: App, id: Id) -> AnyView {
    let connection = app.connection(id.clone());
    let used = assigned(app, StoredValue::new(id.clone()));
    let route = Route::Connection(id);
    view! { <button class="destination" on:click=move |_| app.go(route.clone())>
        <strong>{move ||connection.get().map(|c|c.name)}</strong>
        <span>{move ||connection.get().map(|c|format!("{} · {}", c.provider.label(), super::counts(&[(used.with(Vec::len), "assigned device", "assigned devices")])))}</span>
        <span>{move ||connection.get().map(|c|address(&c))}</span>
        <span class="destination-action">"Open →"</span>
    </button> }.into_any()
}

/// One connection's own page: what it is, what uses it, its settings, and
/// the way to remove it.
pub fn detail(app: App, id: Id) -> AnyView {
    let connection = app.connection(id.clone());
    view! {
        <Show
            when=move || connection.with(Option::is_some)
            fallback=move || super::gone(app, "That connection has been removed.")
        >
            {page(app, id.clone())}
        </Show>
    }
    .into_any()
}

fn page(app: App, id: Id) -> AnyView {
    let connection = app.connection(id.clone());
    let key = StoredValue::new(id);
    let used = assigned(app, key);
    let Some(c) = connection.get_untracked() else {
        return ().into_any();
    };
    // Built once, from the record as it stands. Every provider form seeds its
    // own drafts from it and keeps them across a write, which is the point: a
    // rejected save must not lose what was typed and a pairing under way must
    // not be torn down. A connection never changes provider, so this is safe.
    let label = c.provider.label().to_string();
    // What the remote stored for a connection goes with it: nobody should have
    // to wonder whether a token outlived the thing it was for. The one
    // exception is the remote's own Matter identity, which cannot be reissued.
    let (removal_note, removal_confirm) = match c.provider {
        Provider::Matter => (
            "Remove its assigned devices first. The remote's Matter keys for this connection are kept on the remote, not removed. To release a paired device, use Forget device above before removing the connection.",
            "Confirm: remove connection",
        ),
        _ => (
            "Remove its assigned devices first. Removing the connection also removes everything the remote saved for it: its pairing, keys and passwords. To use it again you will pair or sign in again.",
            "Confirm: remove it and its saved keys",
        ),
    };
    let settings = match c.provider {
        Provider::Sonos { .. } => view!{{titled("Connection",super::sonos::form(app,Some(c.clone())))}{super::sonos::controls(app,c.id.to_string())}}.into_any(),
        Provider::CoreElec { .. } => view!{{titled("Connection",super::coreelec::form(app,Some(c.clone())))}{super::kodi::setup(app,&c)}{super::coreelec::setup(app,&c)}}.into_any(),
        Provider::Kodi { .. } => view!{{titled("Connection",local_form(app, Some(c.clone()), false))}{super::kodi::setup(app, &c)}}.into_any(),
        Provider::LegacyDenon { .. } | Provider::LegacyHue | Provider::LegacyWebOs => super::integration_migrations::connection_notice(
            app,
            c.id.to_string(),
            c.provider
                .legacy_builtin()
                .map(|row| row.name.to_string())
                .unwrap_or_default(),
        ),
        Provider::Ir => titled("Connection", local_form(app, Some(c.clone()), true)),
        Provider::UnifiProtect => super::protect::setup(app, &c),
        Provider::Matter => super::matter::setup(app, &c),
        Provider::Plugin { .. } => plugin_setup(app, &c),
        Provider::HomeAssistant => super::home_assistant::setup(app, &c),
        Provider::AndroidTv | Provider::AppleTv => titled(label.clone(), super::streaming_tv::setup(app, &c)),
        Provider::Tizen => super::tizen::setup(app, &c),
        Provider::BluetoothTv => titled(label.clone(), bluetooth_notes()),
    };
    view!{
        {ui::page_header(app, move ||connection.get().map(|c|c.name), Some(Route::Connections))}
        <p class="lead">{move ||connection.get().map(|c|format!("{} · {}", label, address(&c)))}</p>

        <div class="connection-settings">{settings}</div>

        <section class="card">
            <h2>"Assigned devices" <span class="count">{move ||super::counts(&[(used.with(Vec::len), "device", "devices")])}</span></h2>
            {move ||used.with(Vec::is_empty).then(|| view!{<p class="dim">"No device uses this connection yet. Add one in Rooms & devices and choose From connection."</p>})}
            <ul class="rows">{move ||used.get().into_iter().map(|(room_id, room, device)| {
                let route = Route::Room(room_id);
                view!{<li class="row"><button class="row-main" on:click=move |_| app.go(route.clone())>
                    <span class="row-title">{device}</span><span class="row-sub">{room}</span>
                </button></li>}
            }).collect_view()}</ul>
        </section>

        <section class="card danger-zone">
            <h2>"Remove connection"</h2>
            <p class="dim">{removal_note}</p>
            {ui::confirm_button("Remove connection",removal_confirm,move ||app.run_then(api::delete(format!("/api/connections/{}", key.get_value())), move |_| app.go(Route::Connections)))}
        </section>
    }.into_any()
}

/// A settings form on the connection page, in its own card under a heading.
fn titled(heading: impl Into<String>, body: AnyView) -> AnyView {
    let heading = heading.into();
    view! {<section class="card"><h2>{heading}</h2>{body}</section>}.into_any()
}

/// Create a connection, then open its page.
///
/// The response is the whole configuration; the new record is the one whose id
/// was not there when the form was drawn.
pub(super) fn create(app: App, body: Value) {
    let known: Vec<Id> = app
        .config
        .get_untracked()
        .map(|c| c.connections.iter().map(|c| c.id.clone()).collect())
        .unwrap_or_default();
    app.run_then(api::post("/api/connections", body), move |config| {
        if let Some(fresh) = config.connections.iter().find(|c| !known.contains(&c.id)) {
            app.go(Route::Connection(fresh.id.clone()));
        }
    });
}

fn local_form(app: App, existing: Option<Connection>, infrared: bool) -> AnyView {
    let name = RwSignal::new(
        existing
            .as_ref()
            .map(|c| c.name.clone())
            .unwrap_or(if infrared {
                "Infrared".into()
            } else {
                String::new()
            }),
    );
    let (initial_host, initial_port) = match existing.as_ref().map(|c| &c.provider) {
        Some(Provider::Kodi { host, port }) => (host.clone(), port.to_string()),
        _ => (String::new(), "9090".into()),
    };
    let host = RwSignal::new(initial_host);
    let port = RwSignal::new(initial_port);
    let error = RwSignal::new(String::new());
    view!{<form on:submit=move |e|{e.prevent_default();let name=name.get_untracked().trim().to_string();if name.is_empty(){error.set("Enter a connection name".into());return}
        let provider=if infrared{Provider::Ir}else{let Ok(port)=port.get_untracked().parse::<u16>()else{error.set("Enter a TCP port from 1 to 65535".into());return};if port==0 || host.get_untracked().trim().is_empty(){error.set("Enter a hostname and a TCP port from 1 to 65535".into());return}Provider::Kodi{host:host.get_untracked().trim().into(),port}};
        let body=json!({"name":name,"provider":provider});match &existing{Some(c)=>app.run(api::put(format!("/api/connections/{}",c.id),body)),None=>create(app,body)}
    }>
        {field("Connection name",name,"Living room Kodi")}
        {(!infrared).then(||view!{<p class="dim">"Enable remote control in Kodi. Saving an address does not test connectivity."</p>{field("Hostname or IP address",host,"kodi.local")}{field("TCP port",port,"9090")}})}
        {infrared.then(||view!{<p class="notice">"Use the remote’s built-in infrared transmitter. In Rooms & devices, choose a brand and model from the library or import your own codes, then assign commands. Sending requires a working IR driver; learning is not available."</p>})}
        <p role="alert">{move ||error.get()}</p><button class="primary" type="submit">"Save connection"</button>
    </form>}.into_any()
}
pub(super) fn field(
    label: &'static str,
    value: RwSignal<String>,
    placeholder: &'static str,
) -> AnyView {
    view!{<label class="field">{label}<input type="text" prop:value=move ||value.get() placeholder=placeholder on:input=move |e|value.set(event_target_value(&e))/></label>}.into_any()
}

pub(super) fn label(c: &Connection) -> String {
    if c.name == c.provider.label() {
        c.name.clone()
    } else {
        format!("{} · {}", c.name, c.provider.label())
    }
}

fn create_named(app: App, provider: Provider) -> AnyView {
    let name = RwSignal::new(String::new());
    view!{<form on:submit=move |e|{e.prevent_default();let name=name.get_untracked().trim().to_string();if !name.is_empty(){create(app,json!({"name":name,"provider":provider}));}}>
    {field("Connection name",name,"Living room TV / Upstairs bridge")}
    <p class="dim">"Create a named connection. Its page opens next, where you enter its address and pair it."</p>
    <button type="submit" class="primary">"Create connection"</button></form>}.into_any()
}

fn create_plugin(app: App, manifest: PluginManifest) -> AnyView {
    let name = RwSignal::new(manifest.label.clone());
    let provider = Provider::Plugin {
        id: manifest.id,
        label: manifest.label,
        capabilities: manifest.capabilities,
        actions: manifest.actions,
        supports_inputs: manifest.supports_inputs,
        presentation: manifest.presentation,
        children: vec![],
    };
    view!{<form on:submit=move |event|{event.prevent_default();let name=name.get_untracked().trim().to_string();if !name.is_empty(){create(app,json!({"name":name,"provider":provider}));}}>
        {field("Connection name",name,"Living room integration")}
        <p class="dim">"Create the connection, then enter its private settings on the next page. The package runs through Couch’s restricted integration host."</p>
        <button type="submit" class="primary">"Create connection"</button>
    </form>}.into_any()
}

fn plugin_setup(app: App, connection: &Connection) -> AnyView {
    let Provider::Plugin {
        id: package_id,
        label,
        capabilities,
        actions,
        supports_inputs,
        presentation,
        ..
    } = &connection.provider
    else {
        return ().into_any();
    };
    let package_id = package_id.clone();
    let cached_label = label.clone();
    let missing_label = cached_label.clone();
    let cached_capabilities = capabilities.clone();
    let cached_actions = actions.clone();
    let cached_inputs = *supports_inputs;
    let cached_presentation = presentation.clone();
    let connection_id = connection.id.to_string();
    let base = StoredValue::new(format!("/api/connections/{connection_id}/plugin"));
    let manifest = RwSignal::new(None::<PluginManifest>);
    let values = RwSignal::new(BTreeMap::<String, Value>::new());
    let saved_secrets = RwSignal::new(BTreeSet::<String>::new());
    let clear_secrets = RwSignal::new(BTreeSet::<String>::new());
    let configured = RwSignal::new(false);
    let busy = RwSignal::new(true);
    let message = RwSignal::new("Loading integration settings…".to_string());
    // Protocol 3 (unreleased): what this browser knows about the connection's
    // pairing, shared with the device panels and with the dialog at the root.
    let pairing = expect_context::<super::plugin_pairing::State>();
    let settings_connection = connection_id.clone();
    leptos::task::spawn_local(async move {
        let catalog = api::ha("GET", "/api/integrations", None)
            .await
            .ok()
            .and_then(|value| serde_json::from_value::<PluginCatalog>(value).ok());
        let installed = catalog.and_then(|catalog| {
            catalog
                .integrations
                .into_iter()
                .find(|item| item.id == package_id)
        });
        let Some(installed) = installed else {
            message.set(format!(
                "The {missing_label} package is not installed. This connection and its button mappings are retained. Reinstall the package to edit private settings or send commands."
            ));
            busy.set(false);
            return;
        };
        super::plugin_pairing::declares(pairing, &settings_connection, installed.pairing);
        match api::ha("GET", &format!("{}/settings", base.get_value()), None).await {
            Ok(redacted) => {
                let mut loaded = BTreeMap::new();
                for field in &installed.settings {
                    if let Some(default) = &field.default {
                        loaded.insert(field.id.clone(), default.clone());
                    }
                }
                values.set(loaded);
                adopt_plugin_settings(&redacted, values, saved_secrets, configured);
                super::plugin_pairing::from_settings(pairing, &settings_connection, &redacted);
                message.set(settings_line(configured.get_untracked()));
                manifest.set(Some(installed));
            }
            Err(error) => {
                if error.unauthorized {
                    app.paired.set(Some(false));
                }
                super::plugin_pairing::noticed(pairing, &settings_connection, &error);
                message.set(error.message);
            }
        }
        busy.set(false);
    });

    // A pairing that finished, or one that was forgotten, changes what the
    // remote holds for this connection. Read it again rather than showing
    // what this page happened to have: the package may have corrected the
    // settings on its way through.
    let refresh_connection = connection_id.clone();
    Effect::new(move |seen: Option<u32>| {
        let now = pairing.refreshed.get();
        if seen.is_some_and(|seen| seen != now) {
            let connection = refresh_connection.clone();
            leptos::task::spawn_local(async move {
                if let Ok(redacted) =
                    api::ha("GET", &format!("{}/settings", base.get_value()), None).await
                {
                    adopt_plugin_settings(&redacted, values, saved_secrets, configured);
                    super::plugin_pairing::from_settings(pairing, &connection, &redacted);
                    message.set(settings_line(configured.get_untracked()));
                }
            });
        }
        now
    });

    let cached = PluginManifest {
        id: String::new(),
        label: cached_label,
        capabilities: cached_capabilities,
        actions: cached_actions,
        settings: Vec::new(),
        supports_inputs: cached_inputs,
        presentation: cached_presentation,
        pairing: None,
    };
    let form_connection = connection_id.clone();
    let card_connection = connection_id.clone();
    view! {
        {super::plugin_pairing::banner(app,pairing,&connection_id,base.get_value())}
        <section class="card">
            <h2>"Integration settings"</h2>
            <p class="dim">"Settings are stored in the remote’s private connection store and never appear in the home configuration."</p>
            <p role="status">{move ||message.get()}</p>
            {move || manifest.get().map(|installed|plugin_form(app,pairing,form_connection.clone(),base,installed,values,saved_secrets,clear_secrets,configured,busy,message))}
            {super::plugin_pairing::settings_card(app,pairing,&card_connection,base.get_value())}
        </section>
        {plugin_controls(app,pairing,connection_id,cached,manifest,busy)}
    }.into_any()
}

/// Take a redacted settings view as what the form is showing. Never a
/// replacement: a secret is not in the view, and what is typed for one must
/// survive a read.
fn adopt_plugin_settings(
    view: &Value,
    values: RwSignal<BTreeMap<String, Value>>,
    saved_secrets: RwSignal<BTreeSet<String>>,
    configured: RwSignal<bool>,
) {
    if let Some(settings) = view["settings"].as_object() {
        values.update(|all| {
            all.extend(
                settings
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone())),
            );
        });
    }
    saved_secrets.set(
        view["secrets"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
    );
    configured.set(view["configured"].as_bool().unwrap_or(false));
}

fn settings_line(configured: bool) -> String {
    if configured {
        "Private settings are configured.".into()
    } else {
        "Enter the required settings to configure this integration.".into()
    }
}

#[allow(clippy::too_many_arguments)]
fn plugin_form(
    app: App,
    pairing: super::plugin_pairing::State,
    connection: String,
    base: StoredValue<String>,
    manifest: PluginManifest,
    values: RwSignal<BTreeMap<String, Value>>,
    saved_secrets: RwSignal<BTreeSet<String>>,
    clear_secrets: RwSignal<BTreeSet<String>>,
    configured: RwSignal<bool>,
    busy: RwSignal<bool>,
    message: RwSignal<String>,
) -> AnyView {
    let fields = manifest.settings.clone();
    let submit_fields = fields.clone();
    let pair_fields = fields.clone();
    // The setting a package blamed for refusing to save, and its words.
    let blamed = RwSignal::new(None::<(String, String)>);
    let pairs = manifest.pairing.is_some();
    let pair_connection = connection.clone();
    let label_connection = connection;
    view! {<form on:submit=move |event|{
        event.prevent_default();
        save_plugin_settings(app,base,submit_fields.clone(),values,saved_secrets,clear_secrets,configured,busy,message,blamed,None);
    }>
        {fields.into_iter().map(|setting|plugin_field(setting,values,saved_secrets,clear_secrets,busy,blamed)).collect_view()}
        <div class="actions settings-actions">
            <button type="submit" class="primary" disabled=move ||busy.get()>"Save private settings"</button>
            // Protocol 3 (unreleased): the daemon validates the settings a
            // pairing is started with but does not save them, so what is
            // typed is saved first and the pairing starts on what the
            // connection then has.
            {pairs.then(||{
                let fields=pair_fields.clone();
                let connection=pair_connection.clone();
                view!{<button type="button" class="ghost" disabled=move ||busy.get() on:click=move |_|{
                    let fields=fields.clone();
                    let connection=connection.clone();
                    let started_fields=fields.clone();
                    save_plugin_settings(app,base,fields,values,saved_secrets,clear_secrets,configured,busy,message,blamed,Some(Box::new(move ||{
                        let fields=started_fields.clone();
                        super::plugin_pairing::begin(app,pairing,connection.clone(),base.get_value(),move |error|{
                            let (field,text)=refused_setting(&error,&fields);
                            blamed.set(field);
                            message.set(text);
                        });
                    })));
                }>{move ||super::plugin_pairing::pair_label(pairing,&label_connection)}</button>}
            })}
        </div>
    </form>}.into_any()
}

/// Save what the form is showing.
///
/// `then` runs only when the remote accepted it: starting a pairing with
/// settings the package has already refused would ask a device for something
/// nobody typed.
#[allow(clippy::too_many_arguments)]
fn save_plugin_settings(
    app: App,
    base: StoredValue<String>,
    fields: Vec<PluginSetting>,
    values: RwSignal<BTreeMap<String, Value>>,
    saved_secrets: RwSignal<BTreeSet<String>>,
    clear_secrets: RwSignal<BTreeSet<String>>,
    configured: RwSignal<bool>,
    busy: RwSignal<bool>,
    message: RwSignal<String>,
    blamed: RwSignal<Option<(String, String)>>,
    then: Option<Box<dyn Fn()>>,
) {
    if busy.get_untracked() {
        return;
    }
    blamed.set(None);
    let current = values.get_untracked();
    let cleared = clear_secrets.get_untracked();
    let mut settings = serde_json::Map::new();
    for field in &fields {
        if field.kind == PluginFieldKind::Secret {
            if cleared.contains(&field.id) {
                settings.insert(field.id.clone(), Value::Null);
            } else if let Some(value) = current
                .get(&field.id)
                .filter(|value| value.as_str().is_some_and(|text| !text.is_empty()))
            {
                settings.insert(field.id.clone(), value.clone());
            }
        } else if let Some(value) = current.get(&field.id) {
            settings.insert(field.id.clone(), value.clone());
        }
    }
    busy.set(true);
    message.set("Saving private settings…".into());
    leptos::task::spawn_local(async move {
        match api::ha(
            "POST",
            &format!("{}/settings", base.get_value()),
            Some(Value::Object(settings)),
        )
        .await
        {
            Ok(redacted) => {
                saved_secrets.set(
                    redacted["secrets"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect(),
                );
                clear_secrets.set(BTreeSet::new());
                configured.set(redacted["configured"].as_bool().unwrap_or(true));
                values.update(|all| {
                    for field in &fields {
                        if field.kind == PluginFieldKind::Secret {
                            all.remove(&field.id);
                        }
                    }
                });
                message.set("Private settings saved.".into());
                busy.set(false);
                if let Some(then) = then {
                    then();
                }
                return;
            }
            Err(error) => {
                if error.unauthorized {
                    app.paired.set(Some(false));
                }
                let (field, text) = refused_setting(&error, &fields);
                blamed.set(field);
                message.set(text);
            }
        }
        busy.set(false);
    });
}

/// Where a refused save is shown. A reason naming a setting of this form marks
/// that setting and puts the package's words beside it, and the form's own
/// line says which one to look at; anything else is the form's line alone, as
/// it always was (`error` already holds a `message` reason's words).
fn refused_setting(
    error: &api::ApiError,
    fields: &[PluginSetting],
) -> (Option<(String, String)>, String) {
    if let Some(api::Reason::InvalidSetting { field, text }) = &error.reason {
        if let Some(setting) = fields.iter().find(|setting| &setting.id == field) {
            return (
                Some((field.clone(), text.clone())),
                format!("Not saved. Check {}.", setting.label),
            );
        }
    }
    (None, error.message.clone())
}

fn plugin_field(
    setting: PluginSetting,
    values: RwSignal<BTreeMap<String, Value>>,
    saved_secrets: RwSignal<BTreeSet<String>>,
    clear_secrets: RwSignal<BTreeSet<String>>,
    busy: RwSignal<bool>,
    blamed: RwSignal<Option<(String, String)>>,
) -> AnyView {
    let id = setting.id.clone();
    let input_id = id.clone();
    // The package's words about this setting, from the last refused save.
    // Editing the setting takes them away: they were about the old value.
    let blame_id = id.clone();
    let refusal = Memo::new(move |_| {
        blamed.with(|blamed| {
            blamed
                .as_ref()
                .filter(|(field, _)| *field == blame_id)
                .map(|(_, text)| text.clone())
        })
    });
    let edited_id = id.clone();
    let edited = move || {
        if blamed.with_untracked(|blamed| {
            blamed
                .as_ref()
                .is_some_and(|(field, _)| *field == edited_id)
        }) {
            blamed.set(None);
        }
    };
    let edited_toggle = edited.clone();
    let describes = format!("plugin-setting-{id}-error");
    let described = describes.clone();
    let complaint = move || {
        let describes = describes.clone();
        refusal
            .get()
            .map(|text| view! {<p class="field-error" role="alert" id=describes>{text}</p>})
    };
    let label = if setting.required {
        format!("{} · required", setting.label)
    } else {
        setting.label
    };
    match setting.kind {
        PluginFieldKind::Boolean => view! {<div class:field-refused=move ||refusal.get().is_some()>
            <label class="field checkbox-field"><input type="checkbox" aria-invalid=move ||refusal.get().map(|_|"true") aria-describedby=move ||refusal.get().map(|_|described.clone()) checked=move ||values.with(|all|all.get(&id).and_then(Value::as_bool).unwrap_or(false)) disabled=move ||busy.get() on:change=move |event|{values.update(|all|{all.insert(input_id.clone(),Value::Bool(event_target_checked(&event)));});edited_toggle();}/><span>{label}</span></label>
            {complaint}
        </div>}.into_any(),
        kind => {
            let id_for_value=id.clone();
            let id_for_input=id.clone();
            let saved_id=id.clone();
            let clear_id=id.clone();
            let clear_change=id.clone();
            let input_type=if kind==PluginFieldKind::Secret{"password"}else if kind==PluginFieldKind::Integer{"number"}else{"text"};
            view! {<div class:field-refused=move ||refusal.get().is_some()>
                <label class="field">{label}<input type=input_type aria-invalid=move ||refusal.get().map(|_|"true") aria-describedby=move ||refusal.get().map(|_|described.clone()) autocomplete=if kind==PluginFieldKind::Secret{"new-password"}else{"off"} required=setting.required && kind!=PluginFieldKind::Secret prop:value=move ||values.with(|all|all.get(&id_for_value).map(|value|value.as_str().map(str::to_owned).unwrap_or_else(||value.to_string())).unwrap_or_default()) placeholder=move ||if kind==PluginFieldKind::Secret&&saved_secrets.with(|all|all.contains(&saved_id)){"Saved secret · leave blank to keep"}else{""} on:input=move |event|{let text=event_target_value(&event);let value=if kind==PluginFieldKind::Integer{text.parse::<i64>().map(Value::from).unwrap_or(Value::String(text))}else{Value::String(text)};values.update(|all|{all.insert(id_for_input.clone(),value);});clear_secrets.update(|all|{all.remove(&clear_id);});edited();}/></label>
                {complaint}
                {(kind==PluginFieldKind::Secret).then(||view!{<label class="field checkbox-field"><input type="checkbox" checked=move ||clear_secrets.with(|all|all.contains(&id)) disabled=move ||busy.get() on:change=move |event|clear_secrets.update(|all|{if event_target_checked(&event){all.insert(clear_change.clone());}else{all.remove(&clear_change);}})/><span>"Clear the saved value"</span></label>})}
            </div>}.into_any()
        }
    }
}

fn plugin_controls(
    app: App,
    pairing: super::plugin_pairing::State,
    connection_id: String,
    cached: PluginManifest,
    installed: RwSignal<Option<PluginManifest>>,
    settings_busy: RwSignal<bool>,
) -> AnyView {
    let live_busy = RwSignal::new(false);
    let result = RwSignal::new(String::new());
    let status = RwSignal::new(Value::Null);
    let inputs = RwSignal::new(Vec::<(String, String)>::new());
    let base = StoredValue::new(format!("/api/connections/{connection_id}/plugin"));
    let noticed = StoredValue::new(connection_id.clone());
    let call = move |method: &'static str, body: Option<Value>| {
        if live_busy.get_untracked() || settings_busy.get_untracked() {
            return;
        }
        live_busy.set(true);
        leptos::task::spawn_local(async move {
            match api::ha(
                if matches!(method, "action" | "typed-action") {
                    "POST"
                } else {
                    "GET"
                },
                &format!("{}/{method}", base.get_value()),
                body,
            )
            .await
            {
                Ok(value) => result.set(if matches!(method, "action" | "typed-action") {
                    // A command is sent once. A failed readback must never retry
                    // the command or pretend its requested value was observed.
                    match api::ha("GET", &format!("{}/status", base.get_value()), None).await {
                        Ok(value) => {
                            status.set(value);
                            "Command sent. Status refreshed.".into()
                        }
                        Err(error) => {
                            status.set(Value::Null);
                            if error.unauthorized {
                                app.paired.set(Some(false));
                            }
                            super::plugin_pairing::noticed(pairing, &noticed.get_value(), &error);
                            format!("Command sent. Status unavailable: {}", error.message)
                        }
                    }
                } else if method == "inputs" {
                    let choices = value
                        .as_array()
                        .or_else(|| value["inputs"].as_array())
                        .into_iter()
                        .flatten()
                        .filter_map(|item| {
                            Some((
                                item["id"].as_str()?.to_owned(),
                                item["name"]
                                    .as_str()
                                    .unwrap_or(item["id"].as_str()?)
                                    .to_owned(),
                            ))
                        })
                        .collect::<Vec<_>>();
                    let message = if choices.is_empty() {
                        "No selectable inputs reported.".into()
                    } else {
                        format!(
                            "{} input{} available.",
                            choices.len(),
                            if choices.len() == 1 { "" } else { "s" }
                        )
                    };
                    inputs.set(choices);
                    message
                } else {
                    status.set(value);
                    "Status refreshed.".into()
                }),
                Err(error) => {
                    if error.unauthorized {
                        app.paired.set(Some(false));
                    }
                    super::plugin_pairing::noticed(pairing, &noticed.get_value(), &error);
                    result.set(error.message);
                }
            }
            live_busy.set(false);
        });
    };
    view!{<section class="card"><h2>"Integration controls"</h2><p class="dim">"Test only commands declared by the installed package. Couch sends them through its local integration host."</p>
        <p role="status">{move ||result.get()}</p><div class="actions">
        <button class="ghost" disabled=move ||live_busy.get()||settings_busy.get()||installed.get().is_none() on:click=move |_|call("status",None)>"Refresh status"</button>
        </div>
        {move ||{
            let source=installed.get().unwrap_or_else(||cached.clone());
            let components=if source.presentation.is_empty(){vec![PluginComponent::CommandGroup{title:"Commands".into(),commands:source.capabilities.iter().map(|capability|capability.id.clone()).collect()}]}else{source.presentation.clone()};
            components.into_iter().map(|component|match component {
                PluginComponent::VolumeDbControl{label} => plugin_volume_control(label, source.actions.clone(), status, move ||live_busy.get()||settings_busy.get()||installed.get().is_none(), move |action|call("typed-action",Some(json!(action)))),
                PluginComponent::CommandGroup{title,commands}=>{
                    let capabilities=source.capabilities.clone();
                    view!{<section class="integration-component"><h3>{title}</h3><div class="actions">{commands.into_iter().filter_map(|command|capabilities.iter().find(|capability|capability.id==command).map(|capability|(command,capability.label.clone()))).map(|(command,label)|view!{<button class="ghost" disabled=move ||live_busy.get()||settings_busy.get()||installed.get().is_none() on:click=move |_|call("action",Some(json!({"command":command})))>{label}</button>}).collect_view()}</div></section>}.into_any()
                }
                PluginComponent::StatusText{label,field}=>view!{<div class="integration-component integration-reading"><span>{label}</span><strong>{move ||plugin_status_text(&status.get(),field)}</strong></div>}.into_any(),
                PluginComponent::Toggle{label,state:on_field,on,off}=>view!{<section class="integration-component"><h3>{label}</h3><button class="ghost" disabled=move ||live_busy.get()||settings_busy.get()||installed.get().is_none()||plugin_status_bool(&status.get(),on_field).is_none() on:click=move |_|{if let Some(enabled)=plugin_status_bool(&status.get_untracked(),on_field){let command=if enabled{off.clone()}else{on.clone()};call("action",Some(json!({"command":command})));}}>{move ||match plugin_status_bool(&status.get(),on_field){Some(true)=>"Turn off",Some(false)=>"Turn on",None=>"Status unavailable"}}</button></section>}.into_any(),
                PluginComponent::InputSelector{label}=>view!{<section class="integration-component"><h3>{label}</h3><div class="actions"><button class="ghost" disabled=move ||live_busy.get()||settings_busy.get()||installed.get().is_none() on:click=move |_|call("inputs",None)>"Refresh inputs"</button><select aria-label="Integration input" disabled=move ||live_busy.get()||inputs.with(Vec::is_empty) on:change=move |event|{let id=event_target_value(&event);if !id.is_empty(){call("action",Some(json!({"command":format!("input:{id}")})));}}><option value="">"Choose an input"</option>{move ||inputs.get().into_iter().map(|(id,name)|view!{<option value=id>{name}</option>}).collect_view()}</select></div></section>}.into_any(),
                // Protocol 3 (unreleased): no manifest this build accepts can
                // declare one, and the controls come with the web step.
                PluginComponent::Light{..}|PluginComponent::Cover{..}|PluginComponent::Climate{..}=>().into_any(),
            }).collect_view()
        }}
        </section>}.into_any()
}

fn plugin_status_bool(status: &Value, field: PluginStatusField) -> Option<bool> {
    match field {
        PluginStatusField::On => status["on"].as_bool(),
        PluginStatusField::Playing => status["playing"].as_bool(),
        PluginStatusField::Muted => status["muted"].as_bool(),
        _ => None,
    }
}

fn db_number(tenths: i16) -> String {
    format!(
        "{}{:.1}",
        if tenths < 0 { "-" } else { "" },
        f32::from(tenths).abs() / 10.0
    )
}

fn parse_db_target(text: &str, schema: PluginActionSchema) -> Option<TypedAction> {
    let text = text.trim();
    let (negative, digits) = text
        .strip_prefix('-')
        .map(|s| (true, s))
        .unwrap_or((false, text));
    let (whole, fraction) = digits.split_once('.').unwrap_or((digits, ""));
    if whole.is_empty()
        || !whole.bytes().all(|b| b.is_ascii_digit())
        || fraction.len() > 1
        || !fraction.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let tenths =
        whole
            .parse::<i32>()
            .ok()?
            .checked_mul(10)?
            .checked_add(if fraction.is_empty() {
                0
            } else {
                fraction.parse::<i32>().ok()?
            })?;
    let tenths = i16::try_from(if negative { -tenths } else { tenths }).ok()?;
    let action = TypedAction::SetVolumeDb { tenths };
    schema.accepts(action).then_some(action)
}

fn plugin_volume_control(
    label: String,
    actions: Vec<PluginActionSchema>,
    status: RwSignal<Value>,
    disabled: impl Fn() -> bool + Copy + Send + Sync + 'static,
    send: impl Fn(TypedAction) + Copy + Send + Sync + 'static,
) -> AnyView {
    let draft = RwSignal::new(String::new());
    let Some(
        schema @ PluginActionSchema::SetVolumeDb {
            min_tenths,
            max_tenths,
            step_tenths,
        },
    ) = actions
        .into_iter()
        .find(|s| s.kind() == couch_model::ActionKind::SetVolumeDb && s.is_valid())
    else {
        return view!{<section class="integration-component"><h3>{label}</h3><p>"Volume control unavailable."</p></section>}.into_any();
    };
    view!{<section class="integration-component"><h3>{label}</h3>
        <p>"Current volume: "<strong>{move ||plugin_status_text(&status.get(),PluginStatusField::VolumeDb)}</strong></p>
        <form on:submit=move |event|{event.prevent_default();if !disabled(){if let Some(action)=parse_db_target(&draft.get_untracked(),schema){send(action);}}}>
            <label class="field">"Target volume (dB)"<input type="number" min=db_number(min_tenths) max=db_number(max_tenths) step=db_number(step_tenths as i16) required=true placeholder="Choose a target" disabled=disabled prop:value=move ||draft.get() on:input=move |event|draft.set(event_target_value(&event))/></label>
            <p class="dim">{format!("{} to {} dB, in {} dB steps. Changes apply when you select Set volume.",db_number(min_tenths),db_number(max_tenths),db_number(step_tenths as i16))}</p>
            <button class="primary" type="submit" disabled=move ||disabled()||parse_db_target(&draft.get(),schema).is_none()>"Set volume"</button>
        </form>
    </section>}.into_any()
}

fn plugin_status_text(status: &Value, field: PluginStatusField) -> String {
    match field {
        PluginStatusField::On | PluginStatusField::Playing | PluginStatusField::Muted => {
            plugin_status_bool(status, field)
                .map(|value| if value { "On" } else { "Off" }.into())
                .unwrap_or_else(|| "—".into())
        }
        PluginStatusField::Volume => status["volume"]
            .as_u64()
            .map(|value| format!("{value}%"))
            .unwrap_or_else(|| "—".into()),
        PluginStatusField::VolumeDb => {
            match serde_json::from_value::<VolumeDb>(status["volume_db"].clone())
                .ok()
                .filter(|v| v.is_valid())
            {
                Some(VolumeDb::Reading { tenths }) => format!("{} dB", db_number(tenths)),
                Some(VolumeDb::Minimum) => "Minimum".into(),
                None => "Unavailable".into(),
            }
        }
        PluginStatusField::Input => status["input"].as_str().unwrap_or("—").into(),
        PluginStatusField::Title => status["title"].as_str().unwrap_or("—").into(),
    }
}

#[cfg(test)]
mod plugin_tests {
    use super::*;

    #[test]
    fn a_refused_save_marks_the_setting_a_package_blames_and_nothing_else() {
        let fields: Vec<PluginSetting> = serde_json::from_value(json!([
            {"id":"host","label":"Device address","kind":"text","required":true},
            {"id":"port","label":"Port","kind":"integer","default":23}
        ]))
        .unwrap();
        let refused = |message: &str, reason: Option<api::Reason>| api::ApiError {
            message: message.into(),
            reason,
            status: 400,
            code: Some("invalid".into()),
            unauthorized: false,
            stale: false,
            busy: false,
        };
        // What every daemon says today: the sentence, on the form's own line.
        assert_eq!(
            refused_setting(
                &refused("Invalid integration settings or package", None),
                &fields
            ),
            (None, "Invalid integration settings or package".into())
        );
        let port = api::Reason::InvalidSetting {
            field: "port".into(),
            text: "The port must not be 0".into(),
        };
        assert_eq!(
            refused_setting(&refused("The port must not be 0", Some(port)), &fields),
            (
                Some(("port".into(), "The port must not be 0".into())),
                "Not saved. Check Port.".into()
            )
        );
        // A setting this form does not have cannot be marked, and words about
        // no setting belong to the form.
        let elsewhere = api::Reason::InvalidSetting {
            field: "token".into(),
            text: "The token has expired".into(),
        };
        assert_eq!(
            refused_setting(&refused("The token has expired", Some(elsewhere)), &fields),
            (None, "The token has expired".into())
        );
        let message = api::Reason::Message {
            text: "Pair this TV again".into(),
        };
        assert_eq!(
            refused_setting(&refused("Pair this TV again", Some(message)), &fields),
            (None, "Pair this TV again".into())
        );
    }

    #[test]
    fn db_targets_are_exact_tenths_and_obey_declared_bounds() {
        let schema = PluginActionSchema::SetVolumeDb {
            min_tenths: -800,
            max_tenths: 180,
            step_tenths: 5,
        };
        for (text, tenths) in [
            ("-80", -800),
            ("-34.5", -345),
            ("-0.5", -5),
            ("0", 0),
            ("18.0", 180),
        ] {
            assert_eq!(
                parse_db_target(text, schema),
                Some(TypedAction::SetVolumeDb { tenths })
            );
            assert_eq!(
                parse_db_target(&db_number(tenths), schema),
                Some(TypedAction::SetVolumeDb { tenths })
            );
        }
        for text in [
            "",
            "NaN",
            "Infinity",
            "1e1",
            "-80.5",
            "18.5",
            "-34.4",
            "-34.50",
            "--1",
            "999999999999999999999",
            "2147483647",
            "0.01",
        ] {
            assert_eq!(parse_db_target(text, schema), None, "{text}");
        }
    }

    #[test]
    fn db_status_never_confuses_minimum_missing_or_percentage() {
        assert_eq!(
            plugin_status_text(
                &json!({"volume_db":{"kind":"minimum"}}),
                PluginStatusField::VolumeDb
            ),
            "Minimum"
        );
        assert_eq!(
            plugin_status_text(
                &json!({"volume_db":{"kind":"reading","tenths":-345}}),
                PluginStatusField::VolumeDb
            ),
            "-34.5 dB"
        );
        for status in [
            json!({"volume":50}),
            json!({}),
            json!({"volume_db":{"kind":"reading","tenths":301}}),
            json!({"volume_db":{"kind":"minimum","tenths":0}}),
        ] {
            assert_eq!(
                plugin_status_text(&status, PluginStatusField::VolumeDb),
                "Unavailable"
            );
        }
        assert_eq!(
            plugin_status_text(&json!({"volume":50}), PluginStatusField::Volume),
            "50%"
        );
    }
}
