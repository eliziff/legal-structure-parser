//! The engine's one way to the network. A request is asked again after 1 s and 4 s where no answer came or the server
//! failed or limited it (5xx, 429), or after its Retry-After where that is within a minute; a 429 holds its host until
//! the limit passes. Answers are kept for as long as each request says. The Node addon carries a request itself; the
//! page's engine, which has no socket, through its host's `send` import.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const DAY: u64 = 24 * 60 * 60 * 1_000;
const RETRY_WAITS: [u64; 2] = [1_000, 4_000];
const KEPT_BYTES: usize = 32_000_000;

/// What a server answered: its status and body.
#[derive(Clone, Serialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS), ts(rename = "RemoteReply"))]
pub struct Reply {
    pub status: u16,
    pub body: String,
}

/// A source that did not answer: it was not checked, which is not the same as not found. `transient`: asking again
/// can answer it. `retry_at`: when the source said it allows requests again; `until`: when a hold on it ends.
#[derive(Serialize, Clone, Debug)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS), ts(rename = "RemoteUnavailable", optional_fields = nullable))]
#[serde(rename_all = "camelCase")]
pub struct Unavailable {
    #[cfg_attr(feature = "binding-types", ts(type = r#""rate-limited" | "error" | "timeout" | "unreachable""#))]
    pub reason: &'static str,
    pub retry_at: Option<f64>,
    pub until: Option<f64>,
    pub message: Option<String>,
    pub transient: bool,
}

impl Unavailable {
    fn new(reason: &'static str, message: Option<String>) -> Self {
        Self { reason, retry_at: None, until: None, message, transient: matches!(reason, "timeout" | "unreachable") }
    }
}

/// Why a lookup stopped short: its source did not answer, or it failed.
pub enum Error {
    Unavailable(Unavailable),
    Failed(String),
}

impl From<String> for Error {
    fn from(error: String) -> Self { Error::Failed(error) }
}

impl From<Unavailable> for Error {
    fn from(unavailable: Unavailable) -> Self { Error::Unavailable(unavailable) }
}

/// A lookup's outcome as its host is given it: done, or its source did not answer; a failure is the op's error.
#[derive(Serialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS), ts(rename = "RemoteOutcome"))]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum Outcome<T> {
    Done { value: T },
    Unavailable(Unavailable),
}

pub fn outcome<T>(result: Result<T, Error>) -> Result<Outcome<T>, String> {
    match result {
        Ok(value) => Ok(Outcome::Done { value }),
        Err(Error::Unavailable(unavailable)) => Ok(Outcome::Unavailable(unavailable)),
        Err(Error::Failed(error)) => Err(error),
    }
}

pub fn now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.0, |since| since.as_millis() as f64)
}

struct State {
    kept: HashMap<String, (f64, Reply)>,
    kept_bytes: usize,
    /// When each host's hold ends, and whether the host named that time.
    held: HashMap<String, (f64, bool)>,
    /// When each request to a limited host was sent.
    sent: HashMap<String, Vec<f64>>,
}

static STATE: LazyLock<Mutex<State>> = LazyLock::new(|| Mutex::new(State {
    kept: HashMap::new(), kept_bytes: 0, held: HashMap::new(), sent: HashMap::new() }));

fn state() -> std::sync::MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Forgets every kept answer and hold, as a run that starts over does.
pub fn forget() {
    let mut state = state();
    state.kept.clear();
    state.kept_bytes = 0;
    state.held.clear();
    state.sent.clear();
}

/// The limits a host publishes (A2AJ's: 1,000 requests an hour and 5,000 a day for an address): a request that would
/// pass one waits as the host's own limit would hold it.
fn limits(host: &str) -> &'static [(usize, f64)] {
    match host {
        "api.a2aj.ca" => &[(1_000, 60.0 * 60_000.0), (5_000, DAY as f64)],
        _ => &[],
    }
}

fn host(url: &str) -> String {
    url.split_once("://").map_or("", |(_, rest)| rest.split(['/', '?', '#', ':']).next().unwrap_or_default()).to_ascii_lowercase()
}

fn held(until: f64, named: bool) -> Unavailable {
    Unavailable { reason: "rate-limited", retry_at: named.then_some(until), until: Some(until), message: None, transient: true }
}

/// The wait a Retry-After names, in milliseconds: its seconds, or the time to its date.
fn retry_after(value: &str) -> Option<f64> {
    let value = value.trim();
    value.parse::<f64>().ok().map(|seconds| seconds * 1_000.0)
        .or_else(|| chrono::DateTime::parse_from_rfc2822(value).ok().map(|date| date.timestamp_millis() as f64 - now()))
}

/// A posted body's type: JSON where it is an object or array, else a form's fields.
fn content_type(body: &str) -> &'static str {
    if body.starts_with(['{', '[']) { "application/json" } else { "application/x-www-form-urlencoded" }
}

/// What came of carrying a request once: an answer (its status, body and Retry-After), or none ("timeout",
/// "unreachable").
type Carried = Result<(u16, String, Option<String>), &'static str>;

#[cfg(not(target_arch = "wasm32"))]
fn carry(url: &str, accept: &str, body: Option<&str>, authorization: Option<&str>) -> Carried {
    static AGENT: LazyLock<ureq::Agent> = LazyLock::new(|| ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(15))).http_status_as_error(false).build().into());
    // ureq's GET and POST builders are different types, so the optional header is set on each.
    let response = match body {
        Some(body) => {
            let post = AGENT.post(url).header("Accept", accept).header("Content-Type", content_type(body));
            match authorization { Some(value) => post.header("Authorization", value).send(body), None => post.send(body) }
        }
        None => {
            let get = AGENT.get(url).header("Accept", accept);
            match authorization { Some(value) => get.header("Authorization", value).call(), None => get.call() }
        }
    };
    match response {
        Ok(mut response) => {
            let retry = response.headers().get("retry-after").and_then(|value| value.to_str().ok()).map(str::to_owned);
            let text = response.body_mut().with_config().limit(64 * 1024 * 1024).read_to_string().map_err(|_| "unreachable")?;
            Ok((response.status().as_u16(), text, retry))
        }
        Err(ureq::Error::Timeout(_)) => Err("timeout"),
        Err(_) => Err("unreachable"),
    }
}

#[cfg(target_arch = "wasm32")]
fn carry(url: &str, accept: &str, body: Option<&str>, authorization: Option<&str>) -> Carried {
    #[link(wasm_import_module = "authorities")]
    extern "C" {
        /// The host carries a JSON request ({url, accept, body, authorization}) and returns a frame its `authorities_alloc` made: a
        /// little-endian u32 length, then the JSON reply ({status, body, retryAfter} or {failure}).
        fn send(request: *const u8, length: usize) -> *mut u8;
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Sent { status: Option<u16>, body: Option<String>, retry_after: Option<String>, failure: Option<String> }
    let request = serde_json::json!({ "url": url, "accept": accept, "body": body, "contentType": body.map(content_type),
        "authorization": authorization }).to_string();
    let sent: Sent = unsafe {
        let frame = send(request.as_ptr(), request.len());
        let length = u32::from_le_bytes(std::slice::from_raw_parts(frame, 4).try_into().unwrap()) as usize;
        let parsed = serde_json::from_slice(std::slice::from_raw_parts(frame.add(4), length));
        drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(frame, length + 4)));
        parsed.map_err(|_| "unreachable")?
    };
    match (sent.status, sent.failure.as_deref()) {
        (Some(status), None) => Ok((status, sent.body.unwrap_or_default(), sent.retry_after)),
        (_, Some("timeout")) => Err("timeout"),
        _ => Err("unreachable"),
    }
}

/// Recorded answers (`LEGAL_SOURCE_ANSWERS`, a folder of `sha256("GET <url>").json` files `{status, body}`): read
/// first, and written for each answer that is not a failure. With `LEGAL_SOURCE_OFFLINE`, nothing is sent: an A2AJ
/// request not recorded fails, and every other address answers 404.
fn recorded(url: &str, body: Option<&str>) -> Option<std::path::PathBuf> {
    use sha2::Digest;
    let folder = std::env::var_os("LEGAL_SOURCE_ANSWERS")?;
    let method = if body.is_some() { "POST" } else { "GET" };
    let name = sha2::Sha256::digest(format!("{method} {url}").as_bytes()).iter().map(|byte| format!("{byte:02x}")).collect::<String>();
    Some(std::path::Path::new(&folder).join(format!("{name}.json")))
}

fn offline(url: &str, file: Option<&std::path::Path>) -> Option<Result<Reply, Unavailable>> {
    #[derive(Deserialize)]
    struct Saved { status: u16, body: String }
    if let Some(saved) = file.and_then(|file| std::fs::read(file).ok()).and_then(|bytes| serde_json::from_slice::<Saved>(&bytes).ok()) {
        return Some(Ok(Reply { status: saved.status, body: saved.body }));
    }
    std::env::var_os("LEGAL_SOURCE_OFFLINE")?;
    Some(if host(url) == "api.a2aj.ca" {
        Err(Unavailable { transient: false, ..Unavailable::new("error", Some(format!("A2AJ lookup not recorded and live lookups are off: {url}"))) })
    } else {
        Ok(Reply { status: 404, body: String::new() })
    })
}

/// `url`'s answer (a POST of `body` where given), kept for `kept` milliseconds (None: for good, Some(0): not kept). An
/// `authorization` never enters what the answer is kept by.
pub fn send(url: &str, accept: &str, body: Option<&str>, authorization: Option<&str>, kept: Option<u64>)
    -> Result<Reply, Unavailable> {
    let key = format!("{accept} {url} {}", body.unwrap_or_default());
    if let Some((_, reply)) = state().kept.get(&key).filter(|(expires, _)| *expires > now()) {
        return Ok(reply.clone());
    }
    let file = recorded(url, body);
    if let Some(answered) = offline(url, file.as_deref()) {
        return answered;
    }
    let host = host(url);
    let mut attempt = 0;
    let reply = loop {
        let carried = {
            let mut state = state();
            let at = now();
            if let Some(&(until, named)) = state.held.get(&host).filter(|(until, _)| *until > at) {
                Err(held(until, named))
            } else {
                let sent = state.sent.entry(host.clone()).or_default();
                sent.retain(|time| *time > at - DAY as f64);
                match limits(&host).iter().find_map(|&(requests, window)| {
                    let recent = sent.iter().filter(|time| **time > at - window).collect::<Vec<_>>();
                    (recent.len() >= requests).then(|| recent[0] + window)
                }) {
                    Some(until) => { state.held.insert(host.clone(), (until, false)); Err(held(until, false)) }
                    None => { if !limits(&host).is_empty() { sent.push(at); } Ok(()) }
                }
            }
        }.map(|()| carry(url, accept, body, authorization));
        let wait_named = |retry: &Option<String>| retry.as_deref().and_then(retry_after);
        let failure = match &carried {
            Ok(Ok((status, _, retry))) if *status == 429 => {
                let named = wait_named(retry);
                let until = now() + named.unwrap_or(60_000.0).clamp(1_000.0, 600_000.0);
                state().held.insert(host.clone(), (until, named.is_some()));
                held(until, named.is_some())
            }
            Ok(Ok((status, _, retry))) if *status >= 500 => {
                if let Some(named) = wait_named(retry) {
                    state().held.insert(host.clone(), (now() + named.clamp(1_000.0, 600_000.0), true));
                }
                Unavailable { transient: true, ..Unavailable::new("error", None) }
            }
            Ok(Ok((status, text, _))) => break Reply { status: *status, body: text.clone() },
            Ok(Err(reason)) => Unavailable::new(reason, None),
            Err(held) => held.clone(),
        };
        let pause = state().held.get(&host).map_or(0.0, |(until, _)| until - now());
        let Some(&wait) = RETRY_WAITS.get(attempt) else { return Err(failure) };
        let wait = (wait as f64).max(pause);
        if !failure.transient || wait > 60_000.0 {
            return Err(failure);
        }
        std::thread::sleep(Duration::from_millis(wait as u64));
        attempt += 1;
    };
    if let Some(file) = file {
        let _ = std::fs::create_dir_all(file.parent().unwrap_or(&file))
            .and_then(|()| std::fs::write(&file, serde_json::json!({ "status": reply.status, "body": reply.body }).to_string()));
    }
    if kept != Some(0) && reply.body.len() <= 2_000_000 {
        let mut state = state();
        let expires = kept.map_or(f64::INFINITY, |kept| now() + kept as f64);
        state.kept_bytes += reply.body.len();
        if let Some((_, old)) = state.kept.insert(key, (expires, reply.clone())) {
            state.kept_bytes -= old.body.len();
        }
        if state.kept_bytes > KEPT_BYTES {
            state.kept.clear();
            state.kept_bytes = 0;
        }
    }
    Ok(reply)
}

/// `url`'s answer as JSON (an answer of any other value an empty object); a failure where it is not 2xx.
pub fn json(url: &str, kept: Option<u64>, label: &str) -> Result<serde_json::Value, Error> {
    let reply = send(url, "application/json", None, None, kept)?;
    let value = serde_json::from_str::<serde_json::Value>(&reply.body).unwrap_or_default();
    if !(200..300).contains(&reply.status) {
        let detail = value.get("detail");
        let message = detail.and_then(serde_json::Value::as_str).map(str::to_owned).or_else(|| detail.and_then(serde_json::Value::as_array)
            .map(|items| items.iter().filter_map(|item| item.get("msg")?.as_str()).collect::<Vec<_>>().join("; ")))
            .filter(|message| !message.is_empty()).unwrap_or_else(|| format!("{label} API error ({})", reply.status));
        return Err(Unavailable::new("error", Some(message)).into());
    }
    Ok(if value.is_object() { value } else { serde_json::Value::Object(Default::default()) })
}
