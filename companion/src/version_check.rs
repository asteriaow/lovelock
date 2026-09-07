use std::cmp::Ordering;
use std::io::Read;
use std::sync::LazyLock;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::Duration;

use crate::bridge_listener::ModVersionObservation;
use egui::Context;
use reqwest::blocking::{Client, Response};
use semver::Version;
use serde::Deserialize;

pub const LATEST_RELEASE_URL: &str =
    "https://api.github.com/repos/VolcanoCookies/deadlockshock/releases/latest";
pub const COMPANION_RELEASE_URL: &str =
    "https://github.com/VolcanoCookies/deadlockshock/releases/latest";
pub const MOD_RELEASE_URL: &str = "https://gamebanana.com/mods/700758";
pub const USER_AGENT: &str = concat!("Lovelock-Companion/", env!("CARGO_PKG_VERSION"));
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(8);
const MAX_RESPONSE_BYTES: usize = 64 * 1024;

#[derive(Debug, Deserialize)]
struct LatestReleaseResponse {
    tag_name: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum VersionCheckState {
    #[default]
    Checking,
    Current { latest: Version },
    UpdateAvailable { latest: Version },
    Unavailable { reason: String },
}

#[derive(Debug, thiserror::Error)]
pub enum VersionCheckError {
    #[error("request failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("response body read failed: {0}")]
    Read(#[from] std::io::Error),
    #[error("latest release returned HTTP status {0}")]
    HttpStatus(reqwest::StatusCode),
    #[error("latest release response exceeded {MAX_RESPONSE_BYTES} bytes")]
    OversizedResponse,
    #[error("latest release response was malformed: {0}")]
    Malformed(#[from] serde_json::Error),
    #[error("latest release tag is invalid: {0}")]
    InvalidTag(String),
    #[error("latest release response omitted tag_name")]
    MissingTag,
    #[error("could not construct HTTP client: {0}")]
    Client(String),
}

impl VersionCheckError {
    fn category(&self) -> &'static str {
        match self {
            // `is_connect` is checked first: on some platforms a failure to
            // reach the host (refused / unroutable) also reports `is_timeout`
            // when it happens during the connect phase, and "connect" is the
            // more useful label for it than "timeout".
            Self::Request(error) if error.is_connect() => "connect",
            Self::Request(error) if error.is_timeout() => "timeout",
            Self::Request(_) => "request",
            Self::Read(_) => "body_read",
            Self::HttpStatus(_) => "http_status",
            Self::OversizedResponse => "oversized_response",
            Self::Malformed(_) => "malformed_response",
            Self::InvalidTag(_) => "invalid_tag",
            Self::MissingTag => "missing_tag",
            Self::Client(_) => "client",
        }
    }
}

pub fn normalize_release_tag(tag: &str) -> Result<Version, VersionCheckError> {
    let Some(version) = tag.strip_prefix('v') else {
        return Err(VersionCheckError::InvalidTag(tag.to_owned()));
    };
    if version.is_empty() || version.len() > 128 {
        return Err(VersionCheckError::InvalidTag(tag.to_owned()));
    }
    Version::parse(version).map_err(|_| VersionCheckError::InvalidTag(tag.to_owned()))
}

fn read_bounded_body(mut response: Response) -> Result<Vec<u8>, VersionCheckError> {
    let mut body = Vec::new();
    response
        .by_ref()
        .take((MAX_RESPONSE_BYTES + 1) as u64)
        .read_to_end(&mut body)?;
    if body.len() > MAX_RESPONSE_BYTES {
        return Err(VersionCheckError::OversizedResponse);
    }
    Ok(body)
}

pub fn check_latest_release_with(
    client: &Client,
    endpoint: &str,
) -> Result<Version, VersionCheckError> {
    let response = client
        .get(endpoint)
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .send()?;
    if !response.status().is_success() {
        return Err(VersionCheckError::HttpStatus(response.status()));
    }
    let body = read_bounded_body(response)?;
    let release: LatestReleaseResponse = serde_json::from_slice(&body)?;
    let tag = release.tag_name.ok_or(VersionCheckError::MissingTag)?;
    normalize_release_tag(&tag)
}

/// The compiled-in package version, parsed once. `draw` reads it every frame.
static APP_VERSION: LazyLock<Version> = LazyLock::new(|| {
    Version::parse(env!("CARGO_PKG_VERSION")).expect("Cargo package version must be valid semver")
});

pub fn app_version() -> Version {
    APP_VERSION.clone()
}

fn precedence_cmp(left: &Version, right: &Version) -> Ordering {
    left.cmp_precedence(right)
}

fn is_older(left: &Version, right: &Version) -> bool {
    precedence_cmp(left, right).is_lt()
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WarningSelection {
    pub target: Option<Version>,
    pub companion_outdated: Option<Version>,
    pub mod_outdated: Option<(Version, Version)>,
    pub mod_legacy: bool,
    pub mod_invalid: bool,
}

pub fn select_warnings(
    app: &Version,
    mod_observation: &ModVersionObservation,
    remote: Option<&Version>,
) -> WarningSelection {
    // Parsed once: `draw` calls this every frame, and the raw string is also
    // needed below to tell "reported but unparseable" from "not reported".
    let reported_parse = match mod_observation {
        ModVersionObservation::Reported(version) => Some(Version::parse(version)),
        _ => None,
    };
    let reported_unparseable = matches!(reported_parse, Some(Err(_)));
    let observed = reported_parse.and_then(Result::ok);
    let target = [Some(app), observed.as_ref(), remote]
        .into_iter()
        .flatten()
        .max_by(|left, right| precedence_cmp(left, right))
        .cloned();
    let companion_outdated = target
        .as_ref()
        .filter(|target| is_older(app, target))
        .cloned();
    let mod_outdated = observed.as_ref().and_then(|version| {
        target
            .as_ref()
            .filter(|target| is_older(version, target))
            .map(|target| (version.clone(), target.clone()))
    });
    WarningSelection {
        target,
        companion_outdated,
        mod_outdated,
        mod_legacy: matches!(mod_observation, ModVersionObservation::Legacy),
        mod_invalid: matches!(mod_observation, ModVersionObservation::Invalid)
            || reported_unparseable,
    }
}

struct WorkerResult {
    result: Result<Version, VersionCheckError>,
}

pub struct VersionCheckOwner {
    endpoint: String,
    client: Option<Client>,
    completion: Option<Receiver<WorkerResult>>,
    pub state: VersionCheckState,
}

impl VersionCheckOwner {
    pub fn new(context: &Context) -> Self {
        let client = Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|error| VersionCheckError::Client(error.to_string()));
        let mut owner = Self::with_client(LATEST_RELEASE_URL, client.ok());
        owner.start(context.clone());
        owner
    }

    pub fn with_client(endpoint: impl Into<String>, client: Option<Client>) -> Self {
        Self {
            endpoint: endpoint.into(),
            client,
            completion: None,
            state: VersionCheckState::Checking,
        }
    }

    pub fn start(&mut self, context: Context) -> bool {
        if self.completion.is_some() {
            return false;
        }
        self.state = VersionCheckState::Checking;
        let endpoint = self.endpoint.clone();
        let client = self.client.clone();
        let (sender, receiver) = mpsc::channel();
        self.completion = Some(receiver);
        match thread::Builder::new()
            .name("deadlock-version-check".to_owned())
            .spawn(move || {
                let result = client
                    .ok_or_else(|| VersionCheckError::Client("client unavailable".to_owned()))
                    .and_then(|client| check_latest_release_with(&client, &endpoint));
                let _ = sender.send(WorkerResult { result });
                context.request_repaint();
            }) {
            Ok(_) => true,
            Err(error) => {
                log::error!(
                    target: "companion::version_check",
                    "version_check_worker_spawn_failed error={error:?}"
                );
                self.completion = None;
                self.state = VersionCheckState::Unavailable {
                    reason: error.to_string(),
                };
                false
            }
        }
    }

    pub fn poll(&mut self) -> bool {
        let Some(receiver) = self.completion.as_ref() else {
            return false;
        };
        let result = match receiver.try_recv() {
            Ok(result) => result.result,
            Err(TryRecvError::Empty) => return false,
            Err(TryRecvError::Disconnected) => Err(VersionCheckError::Client(
                "worker channel disconnected".to_owned(),
            )),
        };
        self.completion = None;
        self.state = match result {
            Ok(latest) if is_older(&app_version(), &latest) => {
                VersionCheckState::UpdateAvailable { latest }
            }
            Ok(latest) => VersionCheckState::Current { latest },
            Err(error) => {
                let category = error.category();
                log::warn!(
                    target: "companion::version_check",
                    "version_check_failed error_kind={category}"
                );
                VersionCheckState::Unavailable {
                    reason: category.to_owned(),
                }
            }
        };
        true
    }

    pub fn is_checking(&self) -> bool {
        self.completion.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
    use std::time::Instant;

    fn version(value: &str) -> Version {
        Version::parse(value).expect("valid test version")
    }

    fn test_client(timeout: Duration) -> Client {
        Client::builder()
            .connect_timeout(timeout)
            .timeout(timeout)
            .build()
            .expect("test client")
    }

    fn serve_once(status: &str, body: Vec<u8>, delay: Duration) -> (String, Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock server");
        let endpoint = format!("http://{}", listener.local_addr().expect("mock address"));
        let (request_sender, request_receiver) = mpsc::channel();
        let status = status.to_owned();
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept mock request");
            let mut request = [0_u8; 8192];
            let read = stream.read(&mut request).expect("read mock request");
            let _ = request_sender.send(String::from_utf8_lossy(&request[..read]).into_owned());
            thread::sleep(delay);
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.write_all(&body);
        });
        (endpoint, request_receiver)
    }

    fn wait_for_completion(owner: &mut VersionCheckOwner) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while !owner.poll() {
            assert!(Instant::now() < deadline, "version-check worker timed out");
            thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn strict_tag_normalization_and_semver_precedence() {
        assert_eq!(normalize_release_tag("v0.2.0").unwrap(), version("0.2.0"));
        assert!(normalize_release_tag("0.2.0").is_err());
        assert!(normalize_release_tag("vv0.2.0").is_err());
        assert!(normalize_release_tag("v0.2").is_err());
        assert!(is_older(&version("1.0.0-alpha"), &version("1.0.0")));
        assert_eq!(
            precedence_cmp(&version("1.0.0+build.1"), &version("1.0.0+build.2")),
            Ordering::Equal
        );
    }

    #[test]
    fn warning_selection_uses_newest_known_precedence() {
        let app_010 = version("0.1.0");
        let app_020 = version("0.2.0");
        let mod_010 = ModVersionObservation::Reported("0.1.0".to_owned());
        let mod_020 = ModVersionObservation::Reported("0.2.0".to_owned());

        let both_old = select_warnings(&app_010, &mod_010, Some(&version("0.2.0")));
        assert_eq!(both_old.companion_outdated, Some(version("0.2.0")));
        assert_eq!(
            both_old.mod_outdated,
            Some((version("0.1.0"), version("0.2.0")))
        );

        let mod_old = select_warnings(&app_020, &mod_010, None);
        assert!(mod_old.companion_outdated.is_none());
        assert!(mod_old.mod_outdated.is_some());

        let app_old = select_warnings(&app_010, &mod_020, None);
        assert!(app_old.companion_outdated.is_some());
        assert!(app_old.mod_outdated.is_none());

        let equal = select_warnings(&app_020, &mod_020, Some(&version("0.2.0")));
        assert!(equal.companion_outdated.is_none());
        assert!(equal.mod_outdated.is_none());

        let local_newer = select_warnings(&version("0.3.0"), &mod_020, Some(&version("0.2.0")));
        assert!(local_newer.companion_outdated.is_none());
        assert!(local_newer.mod_outdated.is_some());
    }

    #[test]
    fn warning_selection_handles_prerelease_build_legacy_invalid_and_unknown() {
        let prerelease = select_warnings(
            &version("1.0.0-alpha"),
            &ModVersionObservation::Unknown,
            Some(&version("1.0.0")),
        );
        assert!(prerelease.companion_outdated.is_some());

        let build_only = select_warnings(
            &version("1.0.0+app"),
            &ModVersionObservation::Reported("1.0.0+mod".to_owned()),
            Some(&version("1.0.0+release")),
        );
        assert!(build_only.companion_outdated.is_none());
        assert!(build_only.mod_outdated.is_none());

        let legacy = select_warnings(&version("1.0.0"), &ModVersionObservation::Legacy, None);
        assert!(legacy.mod_legacy);
        assert!(!legacy.mod_invalid);

        let invalid = select_warnings(&version("1.0.0"), &ModVersionObservation::Invalid, None);
        assert!(invalid.mod_invalid);
        assert!(!invalid.mod_legacy);

        let unknown = select_warnings(&version("1.0.0"), &ModVersionObservation::Unknown, None);
        assert!(!unknown.mod_legacy);
        assert!(!unknown.mod_invalid);
        assert!(unknown.mod_outdated.is_none());
    }

    #[test]
    fn github_response_success_and_required_headers() {
        let (endpoint, request) = serve_once(
            "200 OK",
            br#"{"tag_name":"v0.2.0"}"#.to_vec(),
            Duration::ZERO,
        );
        assert_eq!(
            check_latest_release_with(&test_client(Duration::from_secs(1)), &endpoint).unwrap(),
            version("0.2.0")
        );
        let request = request
            .recv_timeout(Duration::from_secs(1))
            .expect("captured request")
            .to_ascii_lowercase();
        assert!(request.contains(&format!("user-agent: {}", USER_AGENT.to_ascii_lowercase())));
        assert!(request.contains("accept: application/vnd.github+json"));
    }

    #[test]
    fn github_response_rejects_status_oversize_and_bad_payloads() {
        let cases = [
            (
                "503 Service Unavailable",
                br#"{"tag_name":"v0.2.0"}"#.to_vec(),
                "http_status",
            ),
            (
                "200 OK",
                vec![b'x'; MAX_RESPONSE_BYTES + 1],
                "oversized_response",
            ),
            ("200 OK", b"{bad".to_vec(), "malformed_response"),
            ("200 OK", b"{}".to_vec(), "missing_tag"),
            ("200 OK", br#"{"tag_name":"0.2.0"}"#.to_vec(), "invalid_tag"),
        ];
        for (status, body, category) in cases {
            let (endpoint, _) = serve_once(status, body, Duration::ZERO);
            let error = check_latest_release_with(&test_client(Duration::from_secs(1)), &endpoint)
                .expect_err("response must fail");
            assert_eq!(error.category(), category);
        }
    }

    #[test]
    fn github_request_reports_connect_and_timeout_failures() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("reserve unused port");
        let endpoint = format!("http://{}", listener.local_addr().expect("unused address"));
        drop(listener);
        // A short connect timeout with a generous overall deadline, so a host we
        // cannot reach fails in the connect phase. Whether the OS answers with
        // "refused" (fast) or drops the SYN (the connect timeout fires) is
        // platform dependent, but both are connection failures, not request
        // timeouts.
        let connect_client = Client::builder()
            .connect_timeout(Duration::from_millis(100))
            .timeout(Duration::from_secs(5))
            .build()
            .expect("connect test client");
        let connect = check_latest_release_with(&connect_client, &endpoint)
            .expect_err("connection must fail");
        assert_eq!(connect.category(), "connect");

        let (endpoint, _) = serve_once(
            "200 OK",
            br#"{"tag_name":"v0.2.0"}"#.to_vec(),
            Duration::from_millis(100),
        );
        let timeout = check_latest_release_with(&test_client(Duration::from_millis(20)), &endpoint)
            .expect_err("request must time out");
        assert_eq!(timeout.category(), "timeout");
    }

    #[test]
    fn worker_completes_repaints_suppresses_concurrency_and_retries() {
        let context = Context::default();
        let repaint_count = Arc::new(AtomicUsize::new(0));
        let callback_count = Arc::clone(&repaint_count);
        context.set_request_repaint_callback(move |_| {
            callback_count.fetch_add(1, AtomicOrdering::Relaxed);
        });

        let (endpoint, _) = serve_once(
            "200 OK",
            br#"{"tag_name":"v0.2.0"}"#.to_vec(),
            Duration::ZERO,
        );
        let mut owner =
            VersionCheckOwner::with_client(endpoint, Some(test_client(Duration::from_secs(1))));
        assert!(owner.start(context.clone()));
        assert!(!owner.start(context.clone()));
        assert!(matches!(owner.state, VersionCheckState::Checking));
        wait_for_completion(&mut owner);
        assert!(matches!(
            owner.state,
            VersionCheckState::UpdateAvailable { ref latest } if latest == &version("0.2.0")
        ));
        assert!(repaint_count.load(AtomicOrdering::Relaxed) > 0);

        let (retry_endpoint, _) = serve_once(
            "200 OK",
            br#"{"tag_name":"v0.1.0"}"#.to_vec(),
            Duration::ZERO,
        );
        owner.endpoint = retry_endpoint;
        assert!(owner.start(context));
        assert!(matches!(owner.state, VersionCheckState::Checking));
        wait_for_completion(&mut owner);
        assert!(matches!(
            owner.state,
            VersionCheckState::Current { ref latest } if latest == &version("0.1.0")
        ));
    }
}
