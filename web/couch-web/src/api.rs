//! Talking to `couch-confd`.
//!
//! Every mutating call answers with the whole config, so this module has one
//! return type and the app has one piece of state. The alternative - a local
//! copy patched from partial responses - is where an editor starts showing a
//! room that the server deleted.
//!
//! There is no base URL. The bundle is served by the daemon it talks to, so
//! same-origin relative paths are correct by construction and there is no CORS
//! to configure. `trunk serve` proxies `/api` to a local daemon for the dev
//! loop; see `Trunk.toml`.

use couch_model::Config;

thread_local! { static REVISION: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) }; }
use gloo_net::http::Request;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone)]
pub struct ApiError {
    pub message: String,
    /// The session is gone - expired, or the daemon restarted. The app drops
    /// back to the pairing screen rather than showing this as an edit failure,
    /// because "not paired" is not something dismissing a banner can fix.
    pub unauthorized: bool,
    pub stale: bool,
}

impl ApiError {
    fn new(message: impl Into<String>) -> ApiError {
        ApiError {
            message: message.into(),
            unauthorized: false,
            stale: false,
        }
    }
}

/// What the daemon says about pairing. Never includes the PIN: the digits exist
/// on the remote's screen and nowhere else.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AuthStatus {
    pub authenticated: bool,
    pub pairing: bool,
    pub expires_in: u64,
    pub tries_left: u8,
    pub disabled: bool,
}

/// A wrong PIN is an answer, not a failure: only losing the daemon is an `Err`.
pub struct PinResult {
    pub paired: bool,
    pub message: String,
}

pub async fn auth_status() -> Result<AuthStatus, ApiError> {
    json_get("/api/auth/status").await
}

/// Asks the daemon to put a PIN on the remote's screen.
pub async fn auth_challenge() -> Result<AuthStatus, ApiError> {
    let response = Request::post("/api/auth/challenge")
        .send()
        .await
        .map_err(|e| ApiError::new(format!("cannot reach the remote: {e}")))?;
    let text = response.text().await.unwrap_or_default();
    serde_json::from_str(&text)
        .map_err(|e| ApiError::new(format!("the remote sent something unreadable: {e}")))
}

pub async fn auth_verify(pin: String) -> Result<PinResult, ApiError> {
    let response = Request::post("/api/auth/verify")
        .json(&serde_json::json!({ "pin": pin }))
        .map_err(|e| ApiError::new(format!("cannot encode the request: {e}")))?
        .send()
        .await
        .map_err(|e| ApiError::new(format!("cannot reach the remote: {e}")))?;
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    let value: Value = serde_json::from_str(&text).unwrap_or_default();

    if (200..300).contains(&status) {
        return Ok(PinResult {
            paired: true,
            message: String::new(),
        });
    }
    let message = value
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or("that PIN was not accepted");
    let tries = value.get("tries_left").and_then(Value::as_u64).unwrap_or(0);
    Ok(PinResult {
        paired: false,
        message: match tries {
            0 => message.to_string(),
            1 => format!("{message} - one try left"),
            n => format!("{message} - {n} tries left"),
        },
    })
}

pub async fn log_out() -> Result<(), ApiError> {
    Request::post("/api/auth/logout")
        .send()
        .await
        .map(|_| ())
        .map_err(|e| ApiError::new(format!("cannot reach the remote: {e}")))
}

async fn json_get<T: for<'de> Deserialize<'de>>(path: &str) -> Result<T, ApiError> {
    let response = Request::get(path)
        .send()
        .await
        .map_err(|e| ApiError::new(format!("cannot reach the remote: {e}")))?;
    let text = response
        .text()
        .await
        .map_err(|e| ApiError::new(format!("truncated response: {e}")))?;
    serde_json::from_str(&text)
        .map_err(|e| ApiError::new(format!("the remote sent something unreadable: {e}")))
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

pub async fn load() -> Result<Config, ApiError> {
    let response = Request::get("/api/config")
        .send()
        .await
        .map_err(|e| ApiError::new(format!("cannot reach the remote: {e}")))?;
    parse(response).await
}

// Paths and bodies are taken by value rather than by reference. These futures
// are handed to `spawn_local`, so they have to be `'static`; borrowing a
// `format!` temporary from the call site would not survive the first await.
pub async fn post(path: impl Into<String>, body: impl Serialize) -> Result<Config, ApiError> {
    let path = path.into();
    send(Request::post(&path), Some(body)).await
}

pub async fn put(path: impl Into<String>, body: impl Serialize) -> Result<Config, ApiError> {
    let path = path.into();
    send(Request::put(&path), Some(body)).await
}

pub async fn delete(path: impl Into<String>) -> Result<Config, ApiError> {
    let path = path.into();
    send(Request::delete(&path), None::<()>).await
}

async fn send(
    builder: gloo_net::http::RequestBuilder,
    body: Option<impl Serialize>,
) -> Result<Config, ApiError> {
    let builder = match REVISION.with(|revision| revision.get()) {
        Some(revision) => builder.header("If-Match", &revision.to_string()),
        None => builder,
    };
    let request = match body {
        Some(value) => builder
            .json(&value)
            .map_err(|e| ApiError::new(format!("cannot encode the request: {e}")))?,
        None => builder
            .build()
            .map_err(|e| ApiError::new(format!("cannot build the request: {e}")))?,
    };
    let response = request
        .send()
        .await
        .map_err(|e| ApiError::new(format!("cannot reach the remote: {e}")))?;
    parse(response).await
}

async fn parse(response: gloo_net::http::Response) -> Result<Config, ApiError> {
    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|e| ApiError::new(format!("truncated response: {e}")))?;

    if (200..300).contains(&status) {
        let config: Config = serde_json::from_str(&text)
            .map_err(|e| ApiError::new(format!("the remote sent something unreadable: {e}")))?;
        REVISION.with(|revision| revision.set(Some(config.revision)));
        return Ok(config);
    }

    // The daemon's errors carry a message and, for a rejected edit, the list of
    // things wrong with it. Surface those verbatim: they name a field, which is
    // the only thing that helps a user fix it.
    let unauthorized = status == 401;
    Err(ApiError {
        unauthorized,
        stale: status == 409 || status == 404,
        ..ApiError::new(match serde_json::from_str::<Value>(&text) {
            Ok(value) => {
                let message = value
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("the remote refused the change")
                    .to_string();
                match value.get("problems").and_then(Value::as_array) {
                    Some(problems) if !problems.is_empty() => {
                        let detail = problems
                            .iter()
                            .filter_map(|p| {
                                Some(format!(
                                    "{}: {}",
                                    p.get("at")?.as_str()?,
                                    p.get("message")?.as_str()?
                                ))
                            })
                            .collect::<Vec<_>>()
                            .join("; ");
                        format!("{message} ({detail})")
                    }
                    _ => message,
                }
            }
            Err(_) => format!("the remote answered {status}"),
        })
    })
}

/// Integration operations return live state, not a replacement house config.
pub async fn ha(method: &str, path: &str, body: Option<Value>) -> Result<Value, ApiError> {
    let builder = match method {
        "DELETE" => Request::delete(path),
        "PUT" => Request::put(path),
        "POST" => Request::post(path),
        _ => Request::get(path),
    };
    let response = if let Some(body) = body {
        builder
            .json(&body)
            .map_err(|_| ApiError::new("Cannot encode the request"))?
            .send()
            .await
    } else {
        builder.send().await
    }
    .map_err(|_| ApiError::new("Cannot reach the remote"))?;
    let status = response.status();
    let value: Value = response
        .json()
        .await
        .map_err(|_| ApiError::new("The remote returned an invalid response"))?;
    if !(200..300).contains(&status) {
        return Err(ApiError {
            message: value["error"].as_str().unwrap_or("Operation failed").into(),
            unauthorized: status == 401,
            stale: false,
        });
    }
    Ok(value)
}
