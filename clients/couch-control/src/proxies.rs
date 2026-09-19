use super::*;
pub struct Kodi {
    handle: Handle,
}
impl Kodi {
    pub fn tcp(host: impl Into<String>, port: u16) -> Self {
        Self {
            handle: Handle::new(Spec::Kodi {
                host: host.into(),
                port,
                http: false,
                user: String::new(),
                password: String::new(),
                timeout_ms: 2000,
            }),
        }
    }
    pub fn settings(s: &couch_kodi::settings::Settings) -> Self {
        Self {
            handle: Handle::new(Spec::Kodi {
                host: s.host.clone(),
                port: s.web_port,
                http: true,
                user: s.username.clone(),
                password: s.password.clone(),
                timeout_ms: 2000,
            }),
        }
    }
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        if let Spec::Kodi { timeout_ms, .. } = &mut self.handle.spec {
            *timeout_ms = timeout.as_millis().clamp(1, 5000) as u64;
        }
        self
    }
    pub fn call(&self, method: &str, params: Value) -> Result<Value> {
        self.handle.call(Op::KodiCall(method.into(), params))
    }
    pub fn select(&self) -> Result<()> {
        self.handle.get(Op::KodiSelect)
    }
    pub fn ping(&self) -> Result<()> {
        self.call("JSONRPC.Ping", json!({})).map(|_| ())
    }
    pub fn playback(&self) -> Result<Option<couch_kodi::playback::Playback>> {
        self.handle.get(Op::KodiPlayback)
    }
    pub fn chapters(&self, player: i64) -> Result<Option<Vec<couch_kodi::playback::Chapter>>> {
        self.handle.get(Op::KodiChapters(player))
    }
    pub fn next_notification(&self, wait: Duration) -> Result<Option<couch_kodi::Notification>> {
        if matches!(self.handle.spec, Spec::Kodi { http: true, .. }) {
            return Ok(None);
        }
        self.handle
            .get(Op::KodiNotification(wait.as_millis() as u64))
    }
    pub fn player_command(&self, player: i64, method: &str, mut params: Value) -> Result<Value> {
        params["playerid"] = json!(player);
        self.call(method, params)
    }
    pub fn volume_step(&self, delta: i64) -> Result<i64> {
        self.handle.get(Op::KodiVolumeStep(delta))
    }
    pub fn volume(&self) -> Result<couch_kodi::Volume> {
        Ok(serde_json::from_value(self.call(
            "Application.GetProperties",
            json!({"properties":["volume","muted"]}),
        )?)?)
    }
    pub fn set_volume(&self, level: i64) -> Result<i64> {
        Ok(serde_json::from_value(self.call(
            "Application.SetVolume",
            json!({"volume":level.clamp(0,100)}),
        )?)?)
    }
}
pub struct WebOs {
    handle: Handle,
}
impl WebOs {
    pub fn connect(s: &couch_webos::Settings) -> Result<Self> {
        s.validate()?;
        let c = Self {
            handle: Handle::new(Spec::WebOs(s.clone())),
        };
        c.handle.call(Op::Open)?;
        Ok(c)
    }
    pub fn request(&mut self, uri: &str, payload: Value) -> Result<Value> {
        self.handle.call(Op::TvRequest(uri.into(), payload))
    }
    pub fn toggle_mute(&mut self) -> Result<()> {
        self.handle.get(Op::TvToggleMute)
    }
    pub fn prepare_input(&mut self) -> Result<()> {
        self.handle.get(Op::TvPrepare)
    }
    pub fn button(&mut self, b: couch_webos::Button) -> Result<()> {
        self.handle.get(Op::TvButton(b))
    }
    pub fn subscribe(&mut self, uri: &str) -> Result<couch_webos::Update> {
        self.handle.get(Op::TvSubscribe(uri.into()))
    }
    pub fn unsubscribe(&mut self, id: &str) -> Result<()> {
        self.handle.get(Op::TvUnsubscribe(id.into()))
    }
    pub fn next_update(&mut self, wait: Duration) -> Result<Option<couch_webos::Update>> {
        self.handle.get(Op::TvUpdate(wait.as_millis() as u64))
    }
    pub fn volume(&mut self) -> Result<Value> {
        self.request("ssap://audio/getVolume", json!({}))
    }
    pub fn set_volume(&mut self, volume: u8) -> Result<()> {
        if volume > 100 {
            return Err(Error::Protocol);
        }
        self.request("ssap://audio/setVolume", json!({"volume":volume}))
            .map(|_| ())
    }
    pub fn volume_up(&mut self) -> Result<()> {
        self.request("ssap://audio/volumeUp", json!({})).map(|_| ())
    }
    pub fn volume_down(&mut self) -> Result<()> {
        self.request("ssap://audio/volumeDown", json!({}))
            .map(|_| ())
    }
    pub fn mute(&mut self, on: bool) -> Result<()> {
        self.request("ssap://audio/setMute", json!({"mute":on}))
            .map(|_| ())
    }
    pub fn power_state(&mut self) -> Result<Value> {
        self.request(
            "ssap://com.webos.service.tvpower/power/getPowerState",
            json!({}),
        )
    }
    pub fn power_off(&mut self) -> Result<()> {
        self.request("ssap://system/turnOff", json!({})).map(|_| ())
    }
    pub fn inputs(&mut self) -> Result<Value> {
        self.request("ssap://tv/getExternalInputList", json!({}))
    }
    pub fn select_input(&mut self, id: &str) -> Result<()> {
        valid_id(id)?;
        self.request("ssap://tv/switchInput", json!({"inputId":id}))
            .map(|_| ())
    }
    pub fn apps(&mut self) -> Result<Value> {
        self.request(
            "ssap://com.webos.applicationManager/listLaunchPoints",
            json!({}),
        )
    }
    pub fn launch_app(&mut self, id: &str) -> Result<()> {
        valid_id(id)?;
        self.request("ssap://system.launcher/launch", json!({"id":id}))
            .map(|_| ())
    }
    pub fn foreground_app(&mut self) -> Result<Value> {
        self.request(
            "ssap://com.webos.applicationManager/getForegroundAppInfo",
            json!({}),
        )
    }
    pub fn playback(&mut self, command: couch_webos::Playback) -> Result<()> {
        self.request(
            match command {
                couch_webos::Playback::Play => "ssap://media.controls/play",
                couch_webos::Playback::Pause => "ssap://media.controls/pause",
                couch_webos::Playback::Stop => "ssap://media.controls/stop",
                couch_webos::Playback::Rewind => "ssap://media.controls/rewind",
                couch_webos::Playback::FastForward => "ssap://media.controls/fastForward",
            },
            json!({}),
        )
        .map(|_| ())
    }
    pub fn channel(&mut self, up: bool) -> Result<()> {
        self.request(
            if up {
                "ssap://tv/channelUp"
            } else {
                "ssap://tv/channelDown"
            },
            json!({}),
        )
        .map(|_| ())
    }
}
fn valid_id(id: &str) -> Result<()> {
    if id.is_empty() || id.len() > 256 || id.chars().any(char::is_control) {
        Err(Error::Protocol)
    } else {
        Ok(())
    }
}
