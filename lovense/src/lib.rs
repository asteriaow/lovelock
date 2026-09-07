//! Synchronous client for the Lovense Standard API in local/LAN mode.
//!
//! This talks directly to the Lovense Connect / Lovense Remote app running on
//! the same LAN (typically the same PC, via the `127-0-0-1.lovense.club`
//! loopback hostname Lovense provisions a certificate for). It never touches
//! Lovense's cloud "Remote API" and needs no developer application/approval:
//! enabling "Game Mode" / developer mode inside the Lovense Remote app is
//! enough. See https://github.com/lovense/Standard_solutions for the wire
//! format this client implements.

use std::fmt;
use std::time::Duration;

use reqwest::blocking::Client;
use serde::Deserialize;
use serde_json::{Value, json};
use thiserror::Error;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const MIN_STRENGTH: u8 = 0;
const MAX_STRENGTH: u8 = 20;
const MIN_DURATION_SECS: u32 = 1;
const MAX_DURATION_SECS: u32 = 600;

/// Where to reach the local Lovense Connect/Remote HTTP server.
///
/// The default matches the PC build of Lovense Remote with Game Mode
/// enabled. Mobile Remote apps advertise their own `domain`/`httpPort` (shown
/// on the Game Mode screen after scanning its QR code) which the caller
/// should use instead if the toy is paired to a phone rather than the PC.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Connection {
    domain: String,
    http_port: u16,
}
impl Default for Connection {
    fn default() -> Self {
        Self {
            domain: "127-0-0-1.lovense.club".to_owned(),
            http_port: 20010,
        }
    }
}
impl Connection {
    pub fn new(domain: impl Into<String>, http_port: u16) -> Self {
        Self {
            domain: domain.into().trim().to_owned(),
            http_port,
        }
    }
    pub fn present(&self) -> bool {
        !self.domain.trim().is_empty() && self.http_port != 0
    }
    pub fn domain(&self) -> &str {
        &self.domain
    }
    pub fn http_port(&self) -> u16 {
        self.http_port
    }
    fn base_url(&self) -> String {
        format!("http://{}:{}", self.domain.trim(), self.http_port)
    }
}

/// A toy reported by the connected Lovense app.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Toy {
    pub id: String,
    pub name: String,
    pub status_connected: bool,
    pub battery: Option<i64>,
}

#[derive(Debug, Error, Clone, Eq, PartialEq)]
pub enum Error {
    #[error("Lovense connection domain must not be empty")]
    EmptyDomain,
    #[error("could not reach the Lovense app: {0}")]
    Transport(String),
    #[error("Lovense app returned HTTP status {0}")]
    HttpStatus(u16),
    #[error("invalid Lovense response for {operation}")]
    Decode { operation: &'static str },
    #[error("Lovense command was rejected: {message}")]
    CommandRejected { message: String },
    #[error("no toy is connected to the Lovense app")]
    NoToysAvailable,
    #[error("requested toy is not connected to the Lovense app")]
    ToyNotFound,
    #[error("vibration strength must be between 0 and 20")]
    InvalidStrength,
    #[error("vibration duration must be between 1 and 600 seconds")]
    InvalidDuration,
}

pub struct LovenseClient {
    http: Client,
    connection: Connection,
}

impl fmt::Debug for LovenseClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LovenseClient")
            .field("connection", &self.connection)
            .finish()
    }
}

impl LovenseClient {
    /// Connects and immediately verifies the app is reachable by listing toys.
    pub fn connect(connection: Connection) -> Result<Self, Error> {
        if !connection.present() {
            return Err(Error::EmptyDomain);
        }
        let http = Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(user_agent())
            .build()
            .map_err(|error| Error::Transport(error.to_string()))?;
        let client = Self { http, connection };
        client.list_toys()?;
        Ok(client)
    }

    pub fn list_toys(&self) -> Result<Vec<Toy>, Error> {
        let response = self.send(&json!({ "command": "GetToys" }))?;
        log::debug!(target: "lovense", "get_toys_response body={response}");
        let toys_json = response
            .get("data")
            .and_then(|data| data.get("toys"))
            .and_then(Value::as_str)
            .ok_or(Error::Decode {
                operation: "toy listing",
            })?;
        let toys: Value = serde_json::from_str(toys_json).map_err(|_| Error::Decode {
            operation: "toy listing",
        })?;
        let toys = toys.as_object().ok_or(Error::Decode {
            operation: "toy listing",
        })?;
        let reported = toys.len();
        let decoded: Vec<Toy> = toys
            .values()
            .filter_map(|value| serde_json::from_value::<WireToy>(value.clone()).ok())
            .map(WireToy::into_toy)
            .collect();
        if decoded.len() != reported {
            log::warn!(
                target: "lovense",
                "get_toys_partial_decode reported={reported} decoded={} raw={toys_json}",
                decoded.len()
            );
        } else {
            log::debug!(target: "lovense", "get_toys_decoded count={}", decoded.len());
        }
        Ok(decoded)
    }

    /// Drives every function of `toy` (or of every connected toy when `toy` is
    /// `None`) at `strength` for `duration_secs`.
    ///
    /// The action is `All:<strength>`, not `Vibrate:<strength>`: the Standard
    /// API rejects `Vibrate` on toys that expose no vibration function (a
    /// suction-only or rotation-only toy, say), whereas `All` maps the level
    /// onto whatever functions the toy actually has. The method name stays
    /// `vibrate` because that is how every caller thinks of it.
    pub fn vibrate(
        &self,
        toy: Option<&Toy>,
        strength: u8,
        duration_secs: u32,
    ) -> Result<(), Error> {
        validate_strength(strength)?;
        validate_duration(duration_secs)?;
        self.send(&function_command(
            &format!("All:{strength}"),
            Some(duration_secs),
            toy,
        ))?;
        Ok(())
    }

    /// Drives every function of `toy` (or of every connected toy when `toy` is
    /// `None`) at a fixed `strength` with no time limit, so the toy holds that
    /// level until the next command or a [`stop`](Self::stop). Used to keep a
    /// resting baseline between timed trigger effects; the Lovense app treats
    /// an omitted `timeSec` as "run until told otherwise". Sends `All`, for
    /// the reason described on [`vibrate`](Self::vibrate).
    pub fn vibrate_steady(&self, toy: Option<&Toy>, strength: u8) -> Result<(), Error> {
        validate_strength(strength)?;
        self.send(&function_command(&format!("All:{strength}"), None, toy))?;
        Ok(())
    }

    /// Stops all running functions on `toy`, or every connected toy when `toy` is `None`.
    pub fn stop(&self, toy: Option<&Toy>) -> Result<(), Error> {
        self.send(&function_command("Stop", Some(0), toy))?;
        Ok(())
    }

    fn send(&self, body: &Value) -> Result<Value, Error> {
        let response = self
            .http
            .post(format!("{}/command", self.connection.base_url()))
            .json(body)
            .send()
            .map_err(|error| Error::Transport(error.to_string()))?;
        let status = response.status();
        if !status.is_success() {
            return Err(Error::HttpStatus(status.as_u16()));
        }
        let value: Value = response.json().map_err(|_| Error::Decode {
            operation: "command response",
        })?;
        let code = value.get("code").and_then(Value::as_i64);
        if code.is_some_and(|code| code != 200) {
            let detail = value
                .get("message")
                .or_else(|| value.get("type"))
                .and_then(Value::as_str)
                .unwrap_or("unknown error");
            let message = match code {
                Some(code) => format!("{detail} (code {code})"),
                None => detail.to_owned(),
            };
            return Err(Error::CommandRejected { message });
        }
        Ok(value)
    }
}

fn function_command(action: &str, time_sec: Option<u32>, toy: Option<&Toy>) -> Value {
    let mut command = json!({
        "command": "Function",
        "action": action,
        "apiVer": 1,
    });
    if let (Some(time_sec), Some(object)) = (time_sec, command.as_object_mut()) {
        object.insert("timeSec".to_owned(), Value::from(time_sec));
    }
    set_toy(&mut command, toy);
    command
}

fn set_toy(command: &mut Value, toy: Option<&Toy>) {
    if let (Some(toy), Some(object)) = (toy, command.as_object_mut()) {
        object.insert("toy".to_owned(), Value::String(toy.id.clone()));
    }
}

fn validate_strength(strength: u8) -> Result<(), Error> {
    (MIN_STRENGTH..=MAX_STRENGTH)
        .contains(&strength)
        .then_some(())
        .ok_or(Error::InvalidStrength)
}

fn validate_duration(duration_secs: u32) -> Result<(), Error> {
    (MIN_DURATION_SECS..=MAX_DURATION_SECS)
        .contains(&duration_secs)
        .then_some(())
        .ok_or(Error::InvalidDuration)
}

fn user_agent() -> String {
    format!("deadlockshock-companion-lovense/{}", env!("CARGO_PKG_VERSION"))
}

#[derive(Deserialize)]
struct WireToy {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default, rename = "nickName")]
    nick_name: Option<String>,
    /// Documented as a string ("1"/"0"), but observed in the wild as a bare
    /// JSON number (1/0) depending on app version, so this accepts either.
    #[serde(default, deserialize_with = "deserialize_flexible_string_opt")]
    status: Option<String>,
    #[serde(default)]
    battery: Option<i64>,
}

fn deserialize_flexible_string_opt<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value: Option<Value> = Option::deserialize(deserializer)?;
    Ok(value.and_then(|value| match value {
        Value::String(text) => Some(text),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        _ => None,
    }))
}

impl WireToy {
    fn into_toy(self) -> Toy {
        Toy {
            name: self
                .nick_name
                .filter(|name| !name.is_empty())
                .or(self.name)
                .unwrap_or_else(|| self.id.clone()),
            status_connected: self.status.as_deref() == Some("1"),
            battery: self.battery,
            id: self.id,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_connection_targets_pc_remote_game_mode() {
        let connection = Connection::default();
        assert_eq!(connection.base_url(), "http://127-0-0-1.lovense.club:20010");
    }

    #[test]
    fn empty_domain_is_rejected_before_any_request() {
        assert_eq!(
            LovenseClient::connect(Connection::new("  ", 20010)).unwrap_err(),
            Error::EmptyDomain
        );
    }

    #[test]
    fn steady_vibrate_command_omits_time_limit() {
        let command = function_command("Vibrate:12", None, None);
        assert_eq!(command["action"], "Vibrate:12");
        assert!(command.get("timeSec").is_none());
    }

    #[test]
    fn timed_vibrate_command_carries_time_limit() {
        let command = function_command("Vibrate:5", Some(8), None);
        assert_eq!(command["timeSec"], 8);
    }

    #[test]
    fn stop_command_is_a_zero_length_function() {
        let command = function_command("Stop", Some(0), None);
        assert_eq!(command["action"], "Stop");
        assert_eq!(command["timeSec"], 0);
    }

    #[test]
    fn strength_bounds_are_enforced() {
        assert!(validate_strength(0).is_ok());
        assert!(validate_strength(20).is_ok());
        assert!(validate_strength(21).is_err());
    }

    #[test]
    fn duration_bounds_are_enforced() {
        assert!(validate_duration(1).is_ok());
        assert!(validate_duration(600).is_ok());
        assert!(validate_duration(0).is_err());
        assert!(validate_duration(601).is_err());
    }

    #[test]
    fn wire_toy_prefers_nickname_and_decodes_connected_status() {
        let toy: WireToy = serde_json::from_value(json!({
            "id": "fc9f37e96593",
            "name": "nora",
            "nickName": "my toy",
            "status": "1",
            "battery": 87
        }))
        .unwrap();
        let toy = toy.into_toy();
        assert_eq!(toy.name, "my toy");
        assert!(toy.status_connected);
        assert_eq!(toy.battery, Some(87));
    }

    #[test]
    fn wire_toy_accepts_numeric_status_from_real_world_app_responses() {
        // Lovense's docs show `"status": "1"` as a string, but some app
        // versions report a bare JSON number instead; both must decode.
        let toy: WireToy = serde_json::from_value(json!({
            "id": "881a144613cf",
            "name": "tenera",
            "nickName": "",
            "status": 1,
            "battery": 100
        }))
        .unwrap();
        let toy = toy.into_toy();
        assert_eq!(toy.name, "tenera");
        assert!(toy.status_connected);
        assert_eq!(toy.battery, Some(100));

        let disconnected: WireToy = serde_json::from_value(json!({
            "id": "881a144613cf",
            "status": 0
        }))
        .unwrap();
        assert!(!disconnected.into_toy().status_connected);
    }

    #[test]
    fn wire_toy_falls_back_to_id_when_unnamed() {
        let toy: WireToy = serde_json::from_value(json!({ "id": "abc", "status": "0" })).unwrap();
        let toy = toy.into_toy();
        assert_eq!(toy.name, "abc");
        assert!(!toy.status_connected);
    }
}
