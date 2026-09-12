//! Minimal MediaRemote protobuf projection. Field numbers follow pyatv schemas;
//! see docs/apple-tv-metadata.md for pinned protocol references.

#[derive(Clone, PartialEq, prost::Message)]
pub(super) struct Envelope {
    #[prost(int32, optional, tag = "1")]
    pub kind: Option<i32>,
    #[prost(string, optional, tag = "2")]
    pub identifier: Option<String>,
    #[prost(int32, optional, tag = "4")]
    pub error: Option<i32>,
    #[prost(message, optional, tag = "9")]
    pub state: Option<SetState>,
    #[prost(message, optional, tag = "20")]
    pub device: Option<DeviceInfo>,
    #[prost(message, optional, tag = "21")]
    pub updates: Option<Updates>,
    #[prost(message, optional, tag = "42")]
    pub connection: Option<ConnectionState>,
    #[prost(message, optional, tag = "50")]
    pub active_client: Option<ClientMessage>,
    #[prost(message, optional, tag = "51")]
    pub active_player: Option<PathMessage>,
    #[prost(message, optional, tag = "57")]
    pub remove_client: Option<ClientMessage>,
    #[prost(message, optional, tag = "58")]
    pub remove_player: Option<PathMessage>,
    #[prost(message, optional, tag = "59")]
    pub update_client: Option<ClientMessage>,
    #[prost(message, optional, tag = "60")]
    pub update_items: Option<ItemUpdate>,
    #[prost(string, optional, tag = "85")]
    pub unique_id: Option<String>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(super) struct SetState {
    #[prost(message, optional, tag = "3")]
    pub queue: Option<Queue>,
    #[prost(int32, optional, tag = "6")]
    pub playback_state: Option<i32>,
    #[prost(message, optional, tag = "9")]
    pub path: Option<PlayerPath>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(super) struct Queue {
    #[prost(int32, optional, tag = "1")]
    pub location: Option<i32>,
    #[prost(message, repeated, tag = "2")]
    pub items: Vec<Item>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(super) struct Item {
    #[prost(string, optional, tag = "1")]
    pub id: Option<String>,
    #[prost(message, optional, tag = "2")]
    pub metadata: Option<Metadata>,
    #[prost(bytes, optional, tag = "3")]
    pub artwork: Option<Vec<u8>>,
    #[prost(int32, optional, tag = "13")]
    pub artwork_width: Option<i32>,
    #[prost(int32, optional, tag = "14")]
    pub artwork_height: Option<i32>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(super) struct Metadata {
    #[prost(string, optional, tag = "1")]
    pub title: Option<String>,
    #[prost(string, optional, tag = "2")]
    pub subtitle: Option<String>,
    #[prost(string, optional, tag = "6")]
    pub album: Option<String>,
    #[prost(string, optional, tag = "7")]
    pub artist: Option<String>,
    #[prost(string, optional, tag = "8")]
    pub album_artist: Option<String>,
    #[prost(int32, optional, tag = "10")]
    pub season: Option<i32>,
    #[prost(int32, optional, tag = "11")]
    pub episode: Option<i32>,
    #[prost(double, optional, tag = "14")]
    pub duration: Option<f64>,
    #[prost(bool, optional, tag = "19")]
    pub artwork_available: Option<bool>,
    #[prost(string, optional, tag = "31")]
    pub artwork_mime: Option<String>,
    #[prost(double, optional, tag = "35")]
    pub elapsed: Option<f64>,
    #[prost(string, optional, tag = "36")]
    pub genre: Option<String>,
    #[prost(bool, optional, tag = "37")]
    pub live: Option<bool>,
    #[prost(float, optional, tag = "39")]
    pub rate: Option<f32>,
    #[prost(string, optional, tag = "63")]
    pub series: Option<String>,
    #[prost(int32, optional, tag = "64")]
    pub media_type: Option<i32>,
    #[prost(string, optional, tag = "70")]
    pub artwork_url: Option<String>,
    #[prost(double, optional, tag = "74")]
    pub elapsed_timestamp: Option<f64>,
    #[prost(string, optional, tag = "80")]
    pub artwork_id: Option<String>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(super) struct PlayerPath {
    #[prost(message, optional, tag = "2")]
    pub client: Option<ClientInfo>,
    #[prost(message, optional, tag = "3")]
    pub player: Option<PlayerInfo>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(super) struct ClientInfo {
    #[prost(string, optional, tag = "2")]
    pub bundle: Option<String>,
    #[prost(string, optional, tag = "7")]
    pub name: Option<String>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(super) struct PlayerInfo {
    #[prost(string, optional, tag = "1")]
    pub id: Option<String>,
    #[prost(string, optional, tag = "2")]
    pub name: Option<String>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(super) struct ClientMessage {
    #[prost(message, optional, tag = "1")]
    pub client: Option<ClientInfo>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(super) struct PathMessage {
    #[prost(message, optional, tag = "1")]
    pub path: Option<PlayerPath>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(super) struct ItemUpdate {
    #[prost(message, repeated, tag = "1")]
    pub items: Vec<Item>,
    #[prost(message, optional, tag = "2")]
    pub path: Option<PlayerPath>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(super) struct DeviceInfo {
    #[prost(string, optional, tag = "1")]
    pub id: Option<String>,
    #[prost(string, optional, tag = "2")]
    pub name: Option<String>,
    #[prost(string, optional, tag = "3")]
    pub model: Option<String>,
    #[prost(string, optional, tag = "4")]
    pub build: Option<String>,
    #[prost(string, optional, tag = "5")]
    pub bundle: Option<String>,
    #[prost(string, optional, tag = "6")]
    pub version: Option<String>,
    #[prost(int32, optional, tag = "7")]
    pub protocol: Option<i32>,
    #[prost(uint32, optional, tag = "8")]
    pub last_message: Option<u32>,
    #[prost(bool, optional, tag = "9")]
    pub system_pairing: Option<bool>,
    #[prost(bool, optional, tag = "10")]
    pub allows_pairing: Option<bool>,
    #[prost(string, optional, tag = "12")]
    pub media_app: Option<String>,
    #[prost(bool, optional, tag = "13")]
    pub acl: Option<bool>,
    #[prost(bool, optional, tag = "14")]
    pub shared_queue: Option<bool>,
    #[prost(bool, optional, tag = "15")]
    pub extended_motion: Option<bool>,
    #[prost(uint32, optional, tag = "17")]
    pub queue_version: Option<u32>,
    #[prost(int32, optional, tag = "21")]
    pub device_class: Option<i32>,
    #[prost(uint32, optional, tag = "22")]
    pub logical_count: Option<u32>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(super) struct Updates {
    #[prost(bool, optional, tag = "1")]
    pub artwork: Option<bool>,
    #[prost(bool, optional, tag = "2")]
    pub now_playing: Option<bool>,
    #[prost(bool, optional, tag = "3")]
    pub volume: Option<bool>,
    #[prost(bool, optional, tag = "4")]
    pub keyboard: Option<bool>,
    #[prost(bool, optional, tag = "5")]
    pub output: Option<bool>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(super) struct ConnectionState {
    #[prost(int32, optional, tag = "1")]
    pub state: Option<i32>,
}
