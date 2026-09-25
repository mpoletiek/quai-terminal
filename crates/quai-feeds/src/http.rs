//! Third-party HTTP for market data, explorer lookups and media.
//!
//! Every request goes through one client with per-host pacing (a token bucket), the server's own
//! rate-limit window when it publishes one (explorer.qu.ai: 300 requests per 60 s per IP, shared
//! with every other client on that IP), two priorities (background work keeps 40% of the server
//! window free for what the user is looking at), hard response-size caps, timeouts and back-off
//! on HTTP 429. `QW_HTTP_LOG=<file>` traces every request. Responses are untrusted display data and never feed amount arithmetic
//! or signing.

use quai_model::error::{CoreError, Result};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Largest JSON body accepted.
pub const MAX_JSON_BYTES: usize = 2 * 1024 * 1024;
/// Largest image body accepted.
pub const MAX_IMAGE_BYTES: usize = 8 * 1024 * 1024;

static OFFLINE: AtomicBool = AtomicBool::new(false);
static PROXY: OnceLock<Option<String>> = OnceLock::new();

/// Route every third-party request of this process through a proxy (`socks5h://127.0.0.1:9050`
/// for Tor). Set once, before the first request; with a proxy set nothing goes out directly, so a
/// proxy that is down fails the request instead of revealing the address it was hiding.
///
/// This covers the wallet's own lookups (explorer, prices, images, indexers) and node RPC to a
/// public node (`wallet_core::network::rpc_proxy`); a node on this machine or the LAN is reached
/// directly.
pub fn set_proxy(url: Option<&str>) -> Result<()> {
    let url = url.map(str::trim).filter(|u| !u.is_empty());
    if let Some(u) = url {
        validate_proxy(u)?;
    }
    let wanted = url.map(str::to_string);
    let current = PROXY.get_or_init(|| wanted.clone());
    if *current != wanted {
        return Err(CoreError::Invalid("the proxy is fixed once requests have started; restart to change it".into()));
    }
    Ok(())
}

/// The proxy in use, if any.
pub fn proxy() -> Option<&'static str> {
    PROXY.get().and_then(|p| p.as_deref())
}

/// A proxy URL the HTTP client can use: SOCKS5 (`socks5h` resolves names through the proxy, which
/// is what Tor needs) or HTTP(S), with a host and port.
pub fn validate_proxy(url: &str) -> Result<()> {
    let parsed = reqwest::Url::parse(url).map_err(|_| CoreError::Invalid(format!("proxy `{url}` is not a URL")))?;
    if !matches!(parsed.scheme(), "socks5h" | "socks5" | "http" | "https") || parsed.host_str().is_none() || parsed.port().is_none() {
        return Err(CoreError::Invalid(
            "proxy must look like socks5h://127.0.0.1:9050 (socks5h, socks5, http or https, with a port)".into(),
        ));
    }
    Ok(())
}

/// Disable every third-party lookup for this process (`--offline-data`).
pub fn set_offline(offline: bool) {
    OFFLINE.store(offline, Ordering::SeqCst);
}

/// Whether third-party lookups are disabled for this process.
pub fn offline() -> bool {
    OFFLINE.load(Ordering::SeqCst)
}

/// Who is asking. Background work (daemon polling, image prefetch) yields first when a host is
/// close to its limit, so what the user is looking at keeps working.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Priority {
    /// A view the user opened, a command they ran.
    Interactive,
    /// Polling, prefetching, images.
    Background,
}

static BACKGROUND_PROCESS: AtomicBool = AtomicBool::new(false);

/// Treat every request of this process as background (the daemon), except on threads that
/// serve someone waiting ([`serve_interactive`]).
pub fn set_background_process(background: bool) {
    BACKGROUND_PROCESS.store(background, Ordering::SeqCst);
}

thread_local! {
    static SERVES_A_TERMINAL: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// This thread serves a terminal (a data worker in the daemon): its requests keep the priority
/// they ask for, even in a background process.
pub fn serve_interactive() {
    SERVES_A_TERMINAL.with(|s| s.set(true));
}

fn effective(priority: Priority) -> Priority {
    if BACKGROUND_PROCESS.load(Ordering::SeqCst) && !SERVES_A_TERMINAL.with(|s| s.get()) { Priority::Background } else { priority }
}

/// The server's own view of the limit (IETF `RateLimit-*` headers), shared by every client on
/// this IP: other wallet processes, the browser, everyone behind the same NAT.
#[derive(Clone, Copy, Debug)]
struct ServerWindow {
    limit: u32,
    remaining: u32,
    reset_at: Instant,
}

/// Per-host limiter: a token bucket (`rate` per minute, `burst` at once) plus the server window.
#[derive(Debug)]
struct Bucket {
    rate: f64,
    burst: f64,
    tokens: f64,
    refilled: Instant,
    /// Server-requested pause (429).
    blocked_until: Option<Instant>,
    server: Option<ServerWindow>,
    /// Request start times in the last minute, and totals since the process started.
    recent: std::collections::VecDeque<Instant>,
    total: u64,
    limited: u64,
}

impl Bucket {
    fn new((rate, burst): (u32, u32)) -> Self {
        Bucket {
            rate: f64::from(rate),
            burst: f64::from(burst.max(1)),
            tokens: f64::from(burst.max(1)),
            refilled: Instant::now(),
            blocked_until: None,
            server: None,
            recent: std::collections::VecDeque::new(),
            total: 0,
            limited: 0,
        }
    }

    /// Time to wait before a request may start (zero: take it now).
    fn take(&mut self, now: Instant, priority: Priority) -> Duration {
        if let Some(until) = self.blocked_until {
            if until > now {
                return until - now;
            }
            self.blocked_until = None;
        }
        if let Some(server) = self.server {
            if server.reset_at <= now {
                self.server = None;
            } else {
                // Keep a reserve of the shared window: a little for interactive requests, a lot
                // for background ones.
                let reserve = match priority {
                    Priority::Interactive => (server.limit / 20).max(2),
                    Priority::Background => (server.limit * 2 / 5).max(4),
                };
                if server.remaining <= reserve {
                    return server.reset_at - now + Duration::from_millis(250);
                }
            }
        }
        let elapsed = now.saturating_duration_since(self.refilled).as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.rate / 60.0).min(self.burst);
        self.refilled = now;
        // Background requests leave a quarter of the burst for interactive ones.
        let floor = if priority == Priority::Background { self.burst / 4.0 } else { 0.0 };
        if self.tokens >= 1.0 + floor {
            self.tokens -= 1.0;
            if let Some(server) = self.server.as_mut() {
                server.remaining = server.remaining.saturating_sub(1);
            }
            while self.recent.front().is_some_and(|t| now.saturating_duration_since(*t) > Duration::from_secs(60)) {
                self.recent.pop_front();
            }
            self.recent.push_back(now);
            self.total += 1;
            Duration::ZERO
        } else {
            Duration::from_secs_f64((1.0 + floor - self.tokens) * 60.0 / self.rate)
        }
    }

    /// Record the server's rate-limit headers from a response.
    fn observe(&mut self, now: Instant, limit: Option<u32>, remaining: Option<u32>, reset: Option<u64>) {
        let (Some(limit), Some(remaining)) = (limit, remaining) else { return };
        // `reset` is seconds until the window resets (a unix time on some servers).
        let secs = match reset {
            Some(r) if r > 1_000_000_000 => r.saturating_sub(quai_model::time::now()),
            Some(r) => r,
            None => 60,
        };
        self.server = Some(ServerWindow { limit, remaining, reset_at: now + Duration::from_secs(secs.min(3600)) });
    }
}

/// Local pacing per host: (requests per minute, burst). Servers that publish limits refine this
/// with their live counters.
pub fn host_policy(host: &str) -> (u32, u32) {
    match host {
        // The user's own IPFS node (or anything else on this machine or their network): not a
        // third party with a quota to respect.
        h if crate::ipfs::is_local(h) => (600, 60),
        // explorer.qu.ai: 300 requests / 60 s per IP, shared by every endpoint (API and media).
        h if h.ends_with("explorer.qu.ai") => (240, 30),
        // Bazarr indexer: no published limit; responses say max-age=5.
        h if h.ends_with("basedhash.cc") => (60, 10),
        // Quainance's subgraph: no published limit. On the default policy a Markets scroll paced
        // at one request per 2 s, which is most of what that screen's slowness was.
        h if h.ends_with("graph.quai.network") => (120, 20),
        h if h.ends_with("quaiscan.io") => (120, 20),
        h if h.ends_with("ipfs.io") => (30, 6),
        // Quainance's media proxy: launch metadata and logos. Content-addressed and served from a
        // CDN with a year's `immutable` cache, and the wallet keeps every answer for a month, so a
        // first visit to the launch zone is the only burst it ever sees.
        h if h.ends_with("quainance.com") => (120, 24),
        _ => (30, 6),
    }
}

/// Requests per minute allowed for a host.
pub fn host_budget(host: &str) -> u32 {
    host_policy(host).0
}

fn buckets() -> &'static Mutex<HashMap<String, Bucket>> {
    static BUCKETS: OnceLock<Mutex<HashMap<String, Bucket>>> = OnceLock::new();
    BUCKETS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Last error per host, for the Data sources screen.
fn last_errors() -> &'static Mutex<HashMap<String, (u64, String)>> {
    static ERRORS: OnceLock<Mutex<HashMap<String, (u64, String)>>> = OnceLock::new();
    ERRORS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Request budget state for one host.
#[derive(Clone, Debug, serde::Serialize)]
pub struct HostStatus {
    /// Host name.
    pub host: String,
    /// Local pacing: requests per minute.
    pub per_minute: u32,
    /// Requests this process could start right now.
    pub available: u32,
    /// Requests this process started in the last minute.
    pub last_minute: u32,
    /// Requests this process started in total.
    pub total: u64,
    /// Times the server answered 429.
    pub rate_limited: u64,
    /// The server's window, when it publishes one: (limit, remaining, seconds to reset).
    pub server: Option<(u32, u32, u64)>,
    /// Last error (unix seconds, message).
    pub last_error: Option<(u64, String)>,
}

/// Budget and error state of every host contacted by this process.
pub fn host_statuses() -> Vec<HostStatus> {
    let now = Instant::now();
    let errors = last_errors().lock().map(|e| e.clone()).unwrap_or_default();
    let mut hosts: Vec<HostStatus> = buckets()
        .lock()
        .map(|b| {
            b.iter()
                .map(|(host, bucket)| {
                    let elapsed = now.saturating_duration_since(bucket.refilled).as_secs_f64();
                    let tokens = (bucket.tokens + elapsed * bucket.rate / 60.0).min(bucket.burst);
                    HostStatus {
                        host: host.clone(),
                        per_minute: bucket.rate as u32,
                        available: tokens as u32,
                        last_minute: bucket.recent.iter().filter(|t| now.saturating_duration_since(**t) <= Duration::from_secs(60)).count()
                            as u32,
                        total: bucket.total,
                        rate_limited: bucket.limited,
                        server: bucket
                            .server
                            .filter(|s| s.reset_at > now)
                            .map(|s| (s.limit, s.remaining, s.reset_at.saturating_duration_since(now).as_secs())),
                        last_error: errors.get(host).cloned(),
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    hosts.sort_by(|a, b| a.host.cmp(&b.host));
    hosts
}

fn record_error(host: &str, message: &str) {
    if let Ok(mut e) = last_errors().lock() {
        e.insert(host.to_string(), (quai_model::time::now(), message.to_string()));
    }
}

/// Append one line per request to `QW_HTTP_LOG` when set: unix ms, host, path (ids and
/// addresses masked), status, bytes, the server's remaining/limit, and priority.
fn trace(host: &str, url: &str, status: u16, bytes: usize, server: Option<(u32, u32)>, priority: Priority) {
    static LOG: OnceLock<Option<Mutex<std::fs::File>>> = OnceLock::new();
    let log = LOG.get_or_init(|| {
        std::env::var_os("QW_HTTP_LOG").and_then(|p| std::fs::OpenOptions::new().create(true).append(true).open(p).ok()).map(Mutex::new)
    });
    let Some(log) = log else { return };
    let path = reqwest::Url::parse(url).map(|u| u.path().to_string()).unwrap_or_default();
    let masked: String = path
        .split('/')
        .map(|seg| {
            if seg.len() > 12 && seg.chars().all(|c| c.is_ascii_hexdigit() || c == 'x')
                || seg.chars().all(|c| c.is_ascii_digit()) && !seg.is_empty()
            {
                "{id}"
            } else {
                seg
            }
        })
        .collect::<Vec<_>>()
        .join("/");
    let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    let server = server.map(|(r, l)| format!("{r}/{l}")).unwrap_or_else(|| "-".into());
    if let Ok(mut f) = log.lock() {
        use std::io::Write;
        let _ = writeln!(f, "{ms} {host} {masked} {status} {bytes} {server} {priority:?}");
    }
}

/// Host part of an http(s) URL.
pub fn host_of(url: &str) -> Result<String> {
    let parsed = reqwest::Url::parse(url).map_err(|_| CoreError::Invalid(format!("invalid URL `{url}`")))?;
    if !matches!(parsed.scheme(), "https" | "http") {
        return Err(CoreError::Invalid(format!("unsupported URL scheme in `{url}`")));
    }
    parsed.host_str().map(str::to_lowercase).ok_or_else(|| CoreError::Invalid(format!("URL without host `{url}`")))
}

/// Wait for permission to start a request (bounded by `max_wait`).
async fn acquire(host: &str, max_wait: Duration, priority: Priority) -> Result<()> {
    let started = Instant::now();
    loop {
        let wait = {
            let mut map = buckets().lock().map_err(|_| CoreError::Storage("rate limiter poisoned".into()))?;
            let bucket = map.entry(host.to_string()).or_insert_with(|| Bucket::new(host_policy(host)));
            bucket.take(Instant::now(), priority)
        };
        if wait.is_zero() {
            return Ok(());
        }
        if started.elapsed() + wait > max_wait {
            return Err(CoreError::Network(format!("{host}: request budget exhausted, try again shortly")));
        }
        tokio::time::sleep(wait.min(Duration::from_secs(2))).await;
    }
}

fn block_host(host: &str, secs: u64) {
    if let Ok(mut map) = buckets().lock() {
        let bucket = map.entry(host.to_string()).or_insert_with(|| Bucket::new(host_policy(host)));
        bucket.blocked_until = Some(Instant::now() + Duration::from_secs(secs.clamp(1, 300)));
        bucket.tokens = 0.0;
        bucket.limited += 1;
    }
}

/// Feed a response's rate-limit headers to the host's limiter; returns (remaining, limit).
fn observe_headers(host: &str, headers: &reqwest::header::HeaderMap) -> Option<(u32, u32)> {
    let num =
        |name: &str| headers.get(name).and_then(|v| v.to_str().ok()).and_then(|v| v.trim().split(',').next()?.trim().parse::<u64>().ok());
    let limit = num("ratelimit-limit").or_else(|| num("x-ratelimit-limit")).and_then(|v| u32::try_from(v).ok());
    let remaining = num("ratelimit-remaining").or_else(|| num("x-ratelimit-remaining")).and_then(|v| u32::try_from(v).ok());
    let reset = num("ratelimit-reset").or_else(|| num("x-ratelimit-reset"));
    if let Ok(mut map) = buckets().lock() {
        let bucket = map.entry(host.to_string()).or_insert_with(|| Bucket::new(host_policy(host)));
        bucket.observe(Instant::now(), limit, remaining, reset);
    }
    remaining.zip(limit)
}

fn build_client() -> std::result::Result<reqwest::Client, String> {
    let builder = reqwest::Client::builder()
        .user_agent(concat!("quai-terminal/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(6))
        .timeout(Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::custom(same_host_redirect));
    // With no proxy configured, say so explicitly. reqwest otherwise picks up `HTTPS_PROXY` from
    // the environment, so an inherited variable would route every lookup through someone else's
    // server while `data status` reports no proxy at all — the opposite of what the user was told.
    let builder = match proxy() {
        Some(url) => builder.proxy(reqwest::Proxy::all(url).map_err(|e| format!("proxy: {e}"))?),
        None => builder.no_proxy(),
    };
    builder.build().map_err(|e| e.to_string())
}

/// Follow at most three redirects, and only to HTTPS on the host that was asked. Pacing, the
/// media allowlist and the user's privacy choices all reason about the host a request names; a
/// redirect elsewhere would quietly step around every one of them.
fn same_host_redirect(attempt: reqwest::redirect::Attempt<'_>) -> reqwest::redirect::Action {
    let first = attempt.previous().first().and_then(|u| u.host_str()).map(str::to_lowercase);
    let next = attempt.url().host_str().map(str::to_lowercase);
    if attempt.previous().len() > 3 {
        attempt.error("too many redirects")
    } else if attempt.url().scheme() != "https" || first.is_none() || first != next {
        attempt.stop()
    } else {
        attempt.follow()
    }
}

/// One client per thread. A pooled connection is driven by the tokio runtime that opened it, so
/// sharing one client between the TUI's worker threads (each with its own current-thread runtime)
/// let a request on one thread stall until the other thread's runtime next ran.
fn client() -> Result<reqwest::Client> {
    thread_local! {
        static CLIENT: std::result::Result<reqwest::Client, String> = build_client();
    }
    CLIENT.with(|c| c.clone()).map_err(|e| CoreError::Network(format!("http client: {e}")))
}

/// A client that never uses the proxy, for hosts on this machine or the user's network — their own
/// IPFS node, say. Those are not third parties, and a Tor proxy cannot reach `127.0.0.1` on the
/// user's side anyway; node RPC is left unproxied for the same reason.
fn direct_client() -> Result<reqwest::Client> {
    thread_local! {
        static CLIENT: std::result::Result<reqwest::Client, String> = reqwest::Client::builder()
            .user_agent(concat!("quai-terminal/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(4))
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::custom(same_host_redirect))
            .no_proxy()
            .build()
            .map_err(|e| e.to_string());
    }
    CLIENT.with(|c| c.clone()).map_err(|e| CoreError::Network(format!("http client: {e}")))
}

/// How a request that never reached the server says so.
const UNREACHABLE: &str = "could not connect";

/// Whether a request failed because the host could not be reached at all, as opposed to
/// answering badly or slowly.
pub fn unreachable(e: &CoreError) -> bool {
    matches!(e, CoreError::Network(m) if m.contains(UNREACHABLE))
}

/// A fetched body with its declared content type.
#[derive(Clone, Debug)]
pub struct Fetched {
    /// `content-type` header (lowercase, parameters stripped).
    pub content_type: String,
    /// Body bytes (at most the requested cap).
    pub bytes: Vec<u8>,
}

/// GET a URL with the host budget and a size cap. 429 responses pause the host and retry once.
pub async fn get(url: &str, max_bytes: usize) -> Result<Fetched> {
    get_with(url, max_bytes, Priority::Interactive).await
}

/// GET with an explicit priority (background requests wait less and yield more).
pub async fn get_with(url: &str, max_bytes: usize, priority: Priority) -> Result<Fetched> {
    fetch(url, max_bytes, priority, None).await
}

/// POST a JSON body and parse the JSON response (capped at [`MAX_JSON_BYTES`]).
///
/// Same host budget, back-off and size caps as [`get`]; only the method and body differ.
pub async fn post_json(url: &str, body: &serde_json::Value) -> Result<serde_json::Value> {
    post_json_with(url, body, Priority::Interactive).await
}

/// POST with an explicit priority, so a prefetch for a row the user has only scrolled past
/// yields to the chart they are actually looking at.
pub async fn post_json_with(url: &str, body: &serde_json::Value, priority: Priority) -> Result<serde_json::Value> {
    let fetched = fetch(url, MAX_JSON_BYTES, priority, Some(body)).await?;
    let host = host_of(url)?;
    serde_json::from_slice(&fetched.bytes).map_err(|e| CoreError::Network(format!("{host}: invalid JSON ({e})")))
}

/// A read's answer as the callers who joined it get it: the bytes, or what went wrong.
type Answer = Option<std::result::Result<Fetched, String>>;

/// Reads in flight across the whole process, by request. The daemon serves every terminal's data
/// from one process, so two terminals (or a terminal and the daemon's own watch) asking for the
/// same page at the same moment would otherwise spend the host's budget twice.
fn in_flight() -> &'static Mutex<HashMap<String, tokio::sync::watch::Receiver<Answer>>> {
    static IN_FLIGHT: OnceLock<Mutex<HashMap<String, tokio::sync::watch::Receiver<Answer>>>> = OnceLock::new();
    IN_FLIGHT.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The caller that went out for a read. It takes the read off the list when it is done or
/// dropped; callers still waiting on a read nobody finished go out themselves.
struct Leader {
    key: String,
    answer: tokio::sync::watch::Sender<Answer>,
}

impl Drop for Leader {
    fn drop(&mut self) {
        if let Ok(mut map) = in_flight().lock() {
            map.remove(&self.key);
        }
    }
}

/// One request, GET when `body` is None and POST when it is Some. A caller asking for exactly
/// what another is already fetching (same URL, body and size cap) waits for that answer instead.
/// It gets the same bytes; a failure reaches it as a network error with the same message.
async fn fetch(url: &str, max_bytes: usize, priority: Priority, body: Option<&serde_json::Value>) -> Result<Fetched> {
    let key = format!("{max_bytes} {url} {}", body.map(|b| b.to_string()).unwrap_or_default());
    let joined = {
        let mut map = in_flight().lock().map_err(|_| CoreError::Network("http: in-flight list poisoned".into()))?;
        match map.get(&key) {
            Some(waiting) => Err(waiting.clone()),
            None => {
                let (answer, waiting) = tokio::sync::watch::channel(None);
                map.insert(key.clone(), waiting);
                Ok(Leader { key, answer })
            }
        }
    };
    let leader = match joined {
        Ok(leader) => leader,
        Err(mut waiting) => {
            if let Ok(answer) = waiting.wait_for(Option::is_some).await
                && let Some(answer) = answer.clone()
            {
                return answer.map_err(CoreError::Network);
            }
            // The one who went out gave up (its screen moved on): go out ourselves.
            return fetch_once(url, max_bytes, priority, body).await;
        }
    };
    let result = fetch_once(url, max_bytes, priority, body).await;
    // Off the list first: nobody joins from here, so a copy is made only for who already did.
    if let Ok(mut map) = in_flight().lock() {
        map.remove(&leader.key);
    }
    if leader.answer.receiver_count() > 0 {
        leader.answer.send_replace(Some(result.as_ref().map(Clone::clone).map_err(|e| e.to_string())));
    }
    result
}

/// One request, sent.
async fn fetch_once(url: &str, max_bytes: usize, priority: Priority, body: Option<&serde_json::Value>) -> Result<Fetched> {
    let priority = effective(priority);
    let max_wait = if priority == Priority::Background { Duration::from_secs(8) } else { Duration::from_secs(20) };
    if offline() {
        return Err(CoreError::Network("third-party lookups are disabled (--offline-data)".into()));
    }
    let host = crate::ipfs::budget_host(&host_of(url)?);
    for attempt in 0..2 {
        acquire(&host, max_wait, priority).await?;
        let client = if crate::ipfs::is_local(&host) { direct_client()? } else { client()? };
        let request = match body {
            Some(json) => client.post(url).json(json),
            None => client.get(url),
        };
        let response = match request.send().await {
            Ok(r) => r,
            Err(e) => {
                let connect = e.is_connect();
                let mut message = if e.is_timeout() { "timed out".to_string() } else { e.without_url().to_string() };
                if connect {
                    message = format!("{UNREACHABLE}: {message}");
                }
                if let Some(p) = proxy().filter(|_| !crate::ipfs::is_local(&host))
                    && connect
                {
                    message = format!("{message} (through proxy {p}; is it running?)");
                }
                trace(&host, url, 0, 0, None, priority);
                record_error(&host, &message);
                return Err(CoreError::Network(format!("{host}: {message}")));
            }
        };
        let status = response.status();
        let server = observe_headers(&host, response.headers());
        trace(&host, url, status.as_u16(), response.content_length().unwrap_or(0) as usize, server, priority);
        if status.as_u16() == 429 {
            let reset = response
                .headers()
                .get("ratelimit-reset")
                .or_else(|| response.headers().get("retry-after"))
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<u64>().ok())
                .unwrap_or(30);
            block_host(&host, reset);
            record_error(&host, &format!("rate limited for {reset}s"));
            if attempt == 0 && reset <= 5 {
                continue;
            }
            return Err(CoreError::Network(format!("{host} is rate limiting requests; retry in {reset}s")));
        }
        if !status.is_success() {
            let message = format!("HTTP {}", status.as_u16());
            if status.as_u16() != 404 {
                record_error(&host, &message);
            }
            return Err(if status.as_u16() == 404 {
                CoreError::NotFound(format!("{host}: not found"))
            } else {
                CoreError::Network(format!("{host}: {message}"))
            });
        }
        if response.content_length().is_some_and(|len| len as usize > max_bytes) {
            return Err(CoreError::Network(format!("{host}: response larger than {} bytes", max_bytes)));
        }
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .map(|v| v.split(';').next().unwrap_or("").trim().to_lowercase())
            .unwrap_or_default();
        let mut response = response;
        let mut bytes = Vec::new();
        loop {
            match response.chunk().await {
                Ok(Some(chunk)) => {
                    if bytes.len() + chunk.len() > max_bytes {
                        return Err(CoreError::Network(format!("{host}: response larger than {max_bytes} bytes")));
                    }
                    bytes.extend_from_slice(&chunk);
                }
                Ok(None) => break,
                Err(e) => {
                    let message = e.without_url().to_string();
                    record_error(&host, &message);
                    return Err(CoreError::Network(format!("{host}: {message}")));
                }
            }
        }
        return Ok(Fetched { content_type, bytes });
    }
    Err(CoreError::Network(format!("{host}: request failed")))
}

/// How long a head stream may say nothing before it is treated as dead.
///
/// A stream held open across a laptop suspend comes back as a socket the kernel will never
/// deliver anything on again. Without this, the read simply blocked until the whole-request
/// timeout hours later, and block-driven refresh was silently dead the entire time.
pub const STREAM_IDLE: Duration = Duration::from_secs(90);

/// Follow a server-sent events stream: `on_data` gets each event's `data` payload (at most
/// [`MAX_JSON_BYTES`]) until the server closes the stream or `on_data` returns false. Opening it
/// costs one request against the host's budget, at background priority.
///
/// Returns once the server closes the stream, `on_data` returns false, or nothing arrives for
/// [`STREAM_IDLE`] — the caller reconnects, which is the only way a resumed laptop gets blocks
/// again.
pub async fn event_stream(url: &str, on_data: impl FnMut(&str) -> bool) -> Result<()> {
    event_stream_with(url, STREAM_IDLE, on_data).await
}

/// [`event_stream`] with an explicit idle timeout (tests use a short one).
pub async fn event_stream_with(url: &str, idle: Duration, mut on_data: impl FnMut(&str) -> bool) -> Result<()> {
    if offline() {
        return Err(CoreError::Network("third-party lookups are disabled (--offline-data)".into()));
    }
    let host = host_of(url)?;
    acquire(&host, Duration::from_secs(8), Priority::Background).await?;
    let response = client()?
        .get(url)
        .header("accept", "text/event-stream")
        // A healthy stream is meant to stay open, so the whole-request cap is only a backstop;
        // what actually has to be noticed is silence, and that is the per-read timeout below.
        .timeout(Duration::from_secs(6 * 3600))
        .send()
        .await
        .map_err(|e| CoreError::Network(format!("{host}: {}", e.without_url())))?;
    let server = observe_headers(&host, response.headers());
    trace(&host, url, response.status().as_u16(), 0, server, Priority::Background);
    if !response.status().is_success() {
        return Err(CoreError::Network(format!("{host}: HTTP {}", response.status().as_u16())));
    }
    let mut response = response;
    let mut pending: Vec<u8> = Vec::new();
    let mut data = String::new();
    loop {
        let next = match tokio::time::timeout(idle, response.chunk()).await {
            Ok(r) => r.map_err(|e| CoreError::Network(format!("{host}: {}", e.without_url())))?,
            Err(_) => return Err(CoreError::Network(format!("{host}: no events for {}s", idle.as_secs()))),
        };
        let Some(chunk) = next else { break };
        pending.extend_from_slice(&chunk);
        while let Some(end) = pending.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = pending.drain(..=end).collect();
            let line = String::from_utf8_lossy(&line);
            let line = line.trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                // A blank line ends an event.
                if !data.is_empty() && !on_data(&data) {
                    return Ok(());
                }
                data.clear();
            } else if let Some(payload) = line.strip_prefix("data:")
                && data.len() + payload.len() <= MAX_JSON_BYTES
            {
                if !data.is_empty() {
                    data.push('\n');
                }
                data.push_str(payload.trim_start());
            }
        }
        if pending.len() > MAX_JSON_BYTES {
            pending.clear();
            data.clear();
        }
    }
    Ok(())
}

/// GET and parse JSON (capped at [`MAX_JSON_BYTES`]).
pub async fn get_json(url: &str) -> Result<serde_json::Value> {
    let fetched = get(url, MAX_JSON_BYTES).await?;
    let host = host_of(url)?;
    serde_json::from_slice(&fetched.bytes).map_err(|e| CoreError::Network(format!("{host}: invalid JSON ({e})")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bucket_paces_bursts_and_background_yields() {
        let start = Instant::now();
        let mut b = Bucket::new((60, 10));
        for _ in 0..10 {
            assert_eq!(b.take(start, Priority::Interactive), Duration::ZERO);
        }
        let wait = b.take(start, Priority::Interactive);
        assert!(wait > Duration::ZERO && wait <= Duration::from_secs(1), "{wait:?}");
        assert_eq!(b.take(start + Duration::from_secs(1), Priority::Interactive), Duration::ZERO);
        // Background requests leave a quarter of the burst.
        let later = start + Duration::from_secs(60);
        let mut taken = 0;
        while b.take(later, Priority::Background).is_zero() {
            taken += 1;
        }
        assert_eq!(taken, 7, "10 burst − floor 2.5");
        assert!(b.take(later, Priority::Interactive).is_zero(), "interactive can still go");
    }

    #[test]
    fn server_window_reserves_headroom() {
        let now = Instant::now();
        let mut b = Bucket::new((240, 30));
        b.observe(now, Some(300), Some(100), Some(40));
        assert!(b.take(now, Priority::Interactive).is_zero(), "plenty left for interactive");
        let wait = b.take(now, Priority::Background);
        assert!(wait >= Duration::from_secs(39), "background waits for the reset below 40%: {wait:?}");
        b.observe(now, Some(300), Some(12), Some(40));
        assert!(!b.take(now, Priority::Interactive).is_zero(), "interactive keeps a 5% reserve");
        // The window resets; limits apply again from scratch.
        assert!(b.take(now + Duration::from_secs(41), Priority::Background).is_zero());
        // Unix-time resets are understood too.
        b.observe(now, Some(300), Some(1), Some(quai_model::time::now() + 30));
        assert!(b.take(now, Priority::Interactive) >= Duration::from_secs(29));
    }

    #[test]
    fn hosts_have_budgets_and_schemes_are_checked() {
        assert_eq!(host_budget("explorer.qu.ai"), 240);
        assert_eq!(host_budget("watcher.basedhash.cc"), 60);
        assert_eq!(host_of("https://explorer.qu.ai/api/x").unwrap(), "explorer.qu.ai");
        assert!(host_of("file:///etc/passwd").is_err());
        assert!(host_of("ipfs://abc").is_err());
    }

    #[test]
    fn proxies_are_checked_before_use() {
        for good in ["socks5h://127.0.0.1:9050", "socks5://10.0.0.2:1080", "http://127.0.0.1:8118"] {
            assert!(validate_proxy(good).is_ok(), "{good}");
        }
        for bad in ["127.0.0.1:9050", "socks5h://127.0.0.1", "ftp://127.0.0.1:21", "socks4://127.0.0.1:9050"] {
            assert!(validate_proxy(bad).is_err(), "{bad}");
        }
    }

    /// `--offline-data` is one flag for the whole process: tests that read it must not run while
    /// another test has it switched on.
    static OFFLINE_FLAG: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    #[tokio::test]
    async fn event_stream_splits_events_across_chunks() {
        use tokio::io::AsyncWriteExt;
        let _flag = OFFLINE_FLAG.lock().await;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut req = [0u8; 1024];
            let _ = tokio::io::AsyncReadExt::read(&mut sock, &mut req).await;
            sock.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n: ok\n\nevent: blocks\ndata: {\"items\":[{\"hei")
                .await
                .unwrap();
            sock.flush().await.unwrap();
            tokio::time::sleep(Duration::from_millis(20)).await;
            sock.write_all(b"ght\":\"7\"}]}\r\n\r\ndata: second\n\ndata: third\n\n").await.unwrap();
        });
        let mut seen = Vec::new();
        event_stream(&format!("http://127.0.0.1:{port}/api/stream/live"), |d| {
            seen.push(d.to_string());
            seen.len() < 2
        })
        .await
        .unwrap();
        assert_eq!(seen, vec![r#"{"items":[{"height":"7"}]}"#.to_string(), "second".to_string()], "comments skipped; stops when asked");
    }

    /// A stream that goes quiet is given up on, so the caller can reconnect. Without this, a
    /// socket left dead by a laptop suspend held the head watcher open for hours and blocks
    /// simply stopped arriving.
    #[tokio::test]
    async fn a_silent_stream_times_out() {
        use tokio::io::AsyncWriteExt;
        let _flag = OFFLINE_FLAG.lock().await;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let held = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut req = [0u8; 1024];
            let _ = tokio::io::AsyncReadExt::read(&mut sock, &mut req).await;
            sock.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\ndata: one\n\n").await.unwrap();
            sock.flush().await.unwrap();
            // Then nothing at all, with the connection still open.
            tokio::time::sleep(Duration::from_secs(30)).await;
        });
        let mut seen = Vec::new();
        let started = Instant::now();
        let result = event_stream_with(&format!("http://127.0.0.1:{port}/api/stream/live"), Duration::from_millis(200), |d| {
            seen.push(d.to_string());
            true
        })
        .await;
        assert_eq!(seen, vec!["one".to_string()], "what did arrive was delivered");
        assert!(result.is_err_and(|e| e.to_string().contains("no events")), "silence ends the stream");
        assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
        held.abort();
    }

    /// A redirect to another host is not followed: the request fails where it was sent instead of
    /// quietly landing somewhere the pacing and privacy rules never saw.
    #[tokio::test]
    async fn a_redirect_to_another_host_is_not_followed() {
        use tokio::io::AsyncWriteExt;
        let _flag = OFFLINE_FLAG.lock().await;
        let target = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let target_port = target.local_addr().unwrap().port();
        let hit = std::sync::Arc::new(AtomicBool::new(false));
        let seen = hit.clone();
        tokio::spawn(async move {
            if target.accept().await.is_ok() {
                seen.store(true, Ordering::SeqCst);
            }
        });
        let origin = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = origin.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut sock, _) = origin.accept().await.unwrap();
            let mut req = [0u8; 1024];
            let _ = tokio::io::AsyncReadExt::read(&mut sock, &mut req).await;
            let reply = format!("HTTP/1.1 302 Found\r\nlocation: http://localhost:{target_port}/elsewhere\r\ncontent-length: 0\r\n\r\n");
            sock.write_all(reply.as_bytes()).await.unwrap();
        });
        assert!(get(&format!("http://127.0.0.1:{port}/start"), 1024).await.is_err(), "the 302 itself is not a success");
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!hit.load(Ordering::SeqCst), "the other host was never contacted");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn identical_reads_at_once_are_one_request() {
        use tokio::io::AsyncWriteExt;
        let _flag = OFFLINE_FLAG.lock().await;
        let server = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = server.local_addr().unwrap().port();
        let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = hits.clone();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = server.accept().await {
                counted.fetch_add(1, Ordering::SeqCst);
                tokio::spawn(async move {
                    let mut req = [0u8; 1024];
                    let _ = tokio::io::AsyncReadExt::read(&mut sock, &mut req).await;
                    // Slow enough that the second caller arrives while the first is out.
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    let _ = sock.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\ncontent-length: 5\r\n\r\nhello").await;
                });
            }
        });
        let url = format!("http://127.0.0.1:{port}/same");
        // Two workers, each on a runtime of its own, as in the daemon.
        let reads: Vec<_> = (0..2)
            .map(|_| {
                let url = url.clone();
                std::thread::spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
                    rt.block_on(get(&url, 1024)).map(|f| f.bytes)
                })
            })
            .collect();
        for read in reads {
            assert_eq!(read.join().unwrap().unwrap(), b"hello");
        }
        assert_eq!(hits.load(Ordering::SeqCst), 1, "one request served both");
        // Done reads are not remembered: the next one goes out again.
        assert_eq!(get(&url, 1024).await.unwrap().bytes, b"hello");
        assert_eq!(hits.load(Ordering::SeqCst), 2);
        assert!(in_flight().lock().unwrap().is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_read_whose_first_caller_gave_up_is_still_answered() {
        use tokio::io::AsyncWriteExt;
        let _flag = OFFLINE_FLAG.lock().await;
        let server = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = server.local_addr().unwrap().port();
        let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = hits.clone();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = server.accept().await {
                counted.fetch_add(1, Ordering::SeqCst);
                tokio::spawn(async move {
                    let mut req = [0u8; 1024];
                    let _ = tokio::io::AsyncReadExt::read(&mut sock, &mut req).await;
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    let _ = sock.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok").await;
                });
            }
        });
        let url = format!("http://127.0.0.1:{port}/abandoned");
        let first = {
            let url = url.clone();
            tokio::spawn(async move { tokio::time::timeout(Duration::from_millis(80), get(&url, 64)).await })
        };
        tokio::time::sleep(Duration::from_millis(20)).await;
        let second = get(&url, 64).await.unwrap();
        assert!(first.await.unwrap().is_err(), "the first caller gave up");
        assert_eq!(second.bytes, b"ok", "the one who joined went out itself");
        assert_eq!(hits.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn a_background_process_keeps_interactive_reads_for_the_terminals_it_serves() {
        let _flag = OFFLINE_FLAG.blocking_lock();
        set_background_process(true);
        let own = effective(Priority::Interactive);
        let served = std::thread::spawn(|| {
            serve_interactive();
            (effective(Priority::Interactive), effective(Priority::Background))
        })
        .join()
        .unwrap();
        set_background_process(false);
        assert_eq!(own, Priority::Background, "the daemon's own polling yields");
        assert_eq!(served, (Priority::Interactive, Priority::Background), "a terminal's read keeps its priority");
    }

    #[tokio::test]
    async fn offline_refuses_requests() {
        let _flag = OFFLINE_FLAG.lock().await;
        set_offline(true);
        assert!(get("https://explorer.qu.ai/api/price/current", 10).await.is_err());
        set_offline(false);
    }
}
