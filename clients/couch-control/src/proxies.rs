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
