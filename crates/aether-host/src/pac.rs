//! PAC (Proxy Auto-Config) evaluation.
//!
//! The Host — not the plugins — evaluates `FindProxyForURL`, because a plugin
//! process serves many tasks and each URL may need a different route. Scripts
//! run inside QuickJS with a memory cap and a wall-clock deadline; nothing from
//! the host environment is exposed, so a malicious PAC file can at worst burn
//! its own sandbox until the deadline hits.
//!
//! Only the first directive of a PAC result is used: `PROXY a; PROXY b; DIRECT`
//! resolves to `a`. Failover between proxies would need a retrying connector
//! stack, which reqwest does not provide.

use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use aether_plugin_sdk::{ProxyConfig, ProxyMode};
use rquickjs::{Context, Function, Runtime};
use thiserror::Error;

/// Wall-clock budget for evaluating one script (parse + call).
pub const PAC_TIMEOUT: Duration = Duration::from_millis(300);
/// QuickJS heap cap; a PAC script that needs more is broken, not clever.
pub const PAC_MEMORY_LIMIT: usize = 16 * 1024 * 1024;
/// Refuse to load absurdly large PAC files.
pub const PAC_MAX_SCRIPT_BYTES: usize = 1024 * 1024;
/// How long a fetched PAC script stays fresh before a refetch is attempted.
pub const PAC_TTL: Duration = Duration::from_secs(30 * 60);

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PacError {
    #[error("pac script error: {0}")]
    Script(String),
    #[error("pac fetch failed: {0}")]
    Fetch(String),
    #[error("pac returned no usable proxy (script said {0:?})")]
    NoDirective(String),
    #[error("pac is not configured")]
    NotConfigured,
}

/// One entry of a PAC result string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PacDirective {
    Direct,
    Proxy {
        scheme: &'static str,
        host: String,
        port: u16,
    },
}

impl PacDirective {
    /// Convert to the plugin-facing proxy config.
    pub fn to_proxy_config(&self) -> ProxyConfig {
        match self {
            Self::Direct => ProxyConfig::direct(),
            Self::Proxy { scheme, host, port } => ProxyConfig {
                mode: ProxyMode::Custom,
                url: Some(format!("{scheme}://{host}:{port}")),
                username: None,
                password: None,
            },
        }
    }
}

/// Parse `PROXY host:port; SOCKS5 host:port; DIRECT`.
///
/// Unknown directives are skipped: the protocol is extensible and a client
/// that only understands the common subset should still work.
pub fn parse_chain(result: &str) -> Vec<PacDirective> {
    let mut directives = Vec::new();
    for entry in result.split(';') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let mut parts = entry.splitn(2, char::is_whitespace);
        let kind = parts.next().unwrap_or_default().to_ascii_uppercase();
        let rest = parts.next().unwrap_or_default().trim();
        match kind.as_str() {
            "DIRECT" => directives.push(PacDirective::Direct),
            "PROXY" | "HTTP" => {
                if let Some((host, port)) = split_host_port(rest, 80) {
                    directives.push(PacDirective::Proxy {
                        scheme: "http",
                        host,
                        port,
                    });
                }
            }
            "HTTPS" => {
                if let Some((host, port)) = split_host_port(rest, 443) {
                    directives.push(PacDirective::Proxy {
                        scheme: "https",
                        host,
                        port,
                    });
                }
            }
            "SOCKS" | "SOCKS5" => {
                if let Some((host, port)) = split_host_port(rest, 1080) {
                    directives.push(PacDirective::Proxy {
                        scheme: "socks5",
                        host,
                        port,
                    });
                }
            }
            _ => {}
        }
    }
    directives
}

fn split_host_port(value: &str, default_port: u16) -> Option<(String, u16)> {
    if value.is_empty() {
        return None;
    }
    match value.rsplit_once(':') {
        Some((host, port))
            if !host.is_empty() && !port.is_empty() && !host.contains(char::is_whitespace) =>
        {
            let port = port.parse().ok()?;
            Some((host.to_string(), port))
        }
        // Bare host: `PROXY proxy.local`.
        Some(_) => None,
        None => Some((value.to_string(), default_port)),
    }
}

/// The first directive of a PAC result as a proxy config.
pub fn first_proxy(directives: &[PacDirective]) -> Option<ProxyConfig> {
    directives.first().map(PacDirective::to_proxy_config)
}

/// Host part of a URL, as PAC expects it in `FindProxyForURL(url, host)`.
pub fn url_host(url: &str) -> String {
    let rest = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    let authority = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default()
        .rsplit_once('@')
        .map(|(_, host)| host)
        .unwrap_or(rest.split(['/', '?', '#']).next().unwrap_or_default());
    if let Some(stripped) = authority.strip_prefix('[') {
        // IPv6 literal: keep the brackets, drop the port after `]`.
        if let Some(end) = stripped.find(']') {
            return format!("[{}]", &stripped[..end]);
        }
        return authority.to_string();
    }
    authority
        .rsplit_once(':')
        .map(|(host, port)| {
            if port.chars().all(|c| c.is_ascii_digit()) {
                host
            } else {
                authority
            }
        })
        .unwrap_or(authority)
        .to_string()
}

/// Evaluate a PAC script for one URL and return the resulting directives.
pub fn evaluate(script: &str, url: &str) -> Result<Vec<PacDirective>, PacError> {
    let host = url_host(url);
    let runtime = Runtime::new().map_err(|error| PacError::Script(error.to_string()))?;
    runtime.set_memory_limit(PAC_MEMORY_LIMIT);
    let deadline = Instant::now() + PAC_TIMEOUT;
    runtime.set_interrupt_handler(Some(Box::new(move || Instant::now() > deadline)));
    let context = Context::full(&runtime).map_err(|error| PacError::Script(error.to_string()))?;

    let result = context.with(|ctx| {
        ctx.eval::<(), _>(PAC_HELPERS)
            .map_err(|error| PacError::Script(format!("helpers: {error}")))?;
        ctx.eval::<(), _>(script)
            .map_err(|error| PacError::Script(format!("script: {error}")))?;
        let function: Function = ctx
            .globals()
            .get("FindProxyForURL")
            .map_err(|error| PacError::Script(format!("FindProxyForURL is missing: {error}")))?;
        function
            .call::<_, String>((url, host.as_str()))
            .map_err(|error| PacError::Script(format!("FindProxyForURL: {error}")))
    });
    Ok(parse_chain(&result?))
}

/// PAC directives plus caching for one configured PAC source.
pub struct PacResolver {
    url: Option<String>,
    inline: Option<String>,
    cached: Option<String>,
    fetched_at: Option<Instant>,
    ttl: Duration,
    last_error: Option<String>,
}

impl PacResolver {
    pub fn new(url: Option<String>, inline: Option<String>) -> Self {
        Self {
            url: url.filter(|value| !value.trim().is_empty()),
            inline: inline.filter(|value| !value.trim().is_empty()),
            cached: None,
            fetched_at: None,
            ttl: PAC_TTL,
            last_error: None,
        }
    }

    /// Shorten the cache lifetime (tests, or hosts with fast-changing PACs).
    pub fn with_ttl(mut self, ttl: Duration) -> Self {
        self.ttl = ttl;
        self
    }

    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    pub fn cached(&self) -> bool {
        self.inline.is_none() && self.cached.is_some()
    }

    /// The script to evaluate, fetching it when allowed and stale.
    ///
    /// A failed refresh keeps the previous script: a temporarily unreachable
    /// WPAD server must not turn into "no proxy at all".
    pub fn script(&mut self, allow_fetch: bool) -> Option<&str> {
        if let Some(inline) = &self.inline {
            return Some(inline);
        }
        let fresh = self
            .fetched_at
            .map(|at| at.elapsed() < self.ttl)
            .unwrap_or(false);
        if !fresh && allow_fetch {
            let Some(url) = &self.url else {
                return self.cached.as_deref();
            };
            match fetch_pac(url) {
                Ok(script) => {
                    self.cached = Some(script);
                    self.fetched_at = Some(Instant::now());
                    self.last_error = None;
                }
                Err(error) => {
                    self.last_error = Some(error.to_string());
                }
            }
        }
        self.cached.as_deref()
    }

    /// Resolve the proxy for one URL. `allow_fetch` gates network I/O: it is
    /// set for user-initiated adds, and off for queue promotions so the daemon
    /// never blocks on the network while draining the queue.
    pub fn resolve(&mut self, url: &str, allow_fetch: bool) -> Result<ProxyConfig, PacError> {
        let result = self.resolve_inner(url, allow_fetch);
        if let Err(error) = &result {
            // Keep the last failure readable for the settings/diagnostics API.
            self.last_error = Some(error.to_string());
        }
        result
    }

    fn resolve_inner(&mut self, url: &str, allow_fetch: bool) -> Result<ProxyConfig, PacError> {
        let script = match self.script(allow_fetch) {
            Some(script) => script.to_string(),
            None => {
                return Err(self
                    .last_error
                    .clone()
                    .map(PacError::Fetch)
                    .unwrap_or(PacError::NotConfigured))
            }
        };
        if script.trim().is_empty() {
            return Err(PacError::NotConfigured);
        }
        let directives = evaluate(&script, url)?;
        first_proxy(&directives)
            .ok_or_else(|| PacError::NoDirective(directives_summary(&directives)))
    }
}

fn directives_summary(directives: &[PacDirective]) -> String {
    if directives.is_empty() {
        "nothing".into()
    } else {
        format!("{} directive(s)", directives.len())
    }
}

/// Load a PAC script from `file://…`, `http(s)://…` or a plain path.
pub fn fetch_pac(source: &str) -> Result<String, PacError> {
    let source = source.trim();
    if let Some(path) = source.strip_prefix("file://") {
        return read_script(PathBuf::from(path));
    }
    if source.starts_with("http://") || source.starts_with("https://") {
        let client = reqwest::blocking::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|error| PacError::Fetch(error.to_string()))?;
        let response = client
            .get(source)
            .send()
            .map_err(|error| PacError::Fetch(error.to_string()))?;
        let response = response
            .error_for_status()
            .map_err(|error| PacError::Fetch(error.to_string()))?;
        if let Some(length) = response.content_length() {
            if length as usize > PAC_MAX_SCRIPT_BYTES {
                return Err(PacError::Fetch(format!(
                    "PAC script is {length} bytes (limit {PAC_MAX_SCRIPT_BYTES})"
                )));
            }
        }
        let text = response
            .text()
            .map_err(|error| PacError::Fetch(error.to_string()))?;
        if text.len() > PAC_MAX_SCRIPT_BYTES {
            return Err(PacError::Fetch(format!(
                "PAC script is {} bytes (limit {PAC_MAX_SCRIPT_BYTES})",
                text.len()
            )));
        }
        return Ok(text);
    }
    if source.contains("://") {
        return Err(PacError::Fetch(format!("unsupported PAC scheme: {source}")));
    }
    read_script(PathBuf::from(source))
}

fn read_script(path: PathBuf) -> Result<String, PacError> {
    let metadata = fs::metadata(&path)
        .map_err(|error| PacError::Fetch(format!("{}: {error}", path.display())))?;
    if metadata.len() as usize > PAC_MAX_SCRIPT_BYTES {
        return Err(PacError::Fetch(format!(
            "{} is {} bytes (limit {PAC_MAX_SCRIPT_BYTES})",
            path.display(),
            metadata.len()
        )));
    }
    fs::read_to_string(&path)
        .map_err(|error| PacError::Fetch(format!("{}: {error}", path.display())))
}

/// The helper subset PAC scripts rely on. DNS is deliberately not exposed:
/// `dnsResolve` returns null and `isResolvable` is false, so a script cannot
/// make the daemon resolve arbitrary names.
const PAC_HELPERS: &str = r#"
function isPlainHostName(host) { return host.indexOf(".") === -1; }
function dnsDomainIs(host, domain) {
  return host.length >= domain.length && host.substring(host.length - domain.length) === domain;
}
function localHostOrDomainIs(host, hostdom) {
  return host === hostdom || hostdom.indexOf(host + ".") === 0;
}
function isResolvable(host) { return false; }
function dnsResolve(host) { return null; }
function myIpAddress() { return "127.0.0.1"; }
function dnsDomainLevels(host) { return host.split(".").length - 1; }
function shExpMatch(str, shexp) {
  var escaped = String(shexp).replace(/[.+^${}()|[\]\\]/g, "\\$&");
  var pattern = "^" + escaped.replace(/\*/g, ".*").replace(/\?/g, ".") + "$";
  return new RegExp(pattern).test(String(str));
}
function alert() {}
var PAC_WEEKDAYS = { SUN: 0, MON: 1, TUE: 2, WED: 3, THU: 4, FRI: 5, SAT: 6 };
function weekdayRange(wd1, wd2, gmt) {
  var first = PAC_WEEKDAYS[String(wd1).toUpperCase()];
  var last = (wd2 === undefined || typeof wd2 !== "string") ? first : PAC_WEEKDAYS[String(wd2).toUpperCase()];
  if (first === undefined) { return false; }
  if (last === undefined) { return false; }
  var now = new Date();
  var today = (gmt === "GMT") ? now.getUTCDay() : now.getDay();
  if (first <= last) { return today >= first && today <= last; }
  return today >= first || today <= last;
}
function timeRange(h1, m1, s1, h2, m2, s2, gmt) {
  var args = [];
  for (var i = 0; i < arguments.length; i++) {
    if (typeof arguments[i] === "number") { args.push(arguments[i]); }
  }
  if (args.length === 0) { return true; }
  if (args.length === 1) { args = [args[0], 0, 0, args[0], 59, 59]; }
  else if (args.length === 2) { args = [args[0], args[1], 0, args[0], args[1], 59]; }
  var now = new Date();
  var nowSeconds = ((gmt === "GMT") ? now.getUTCHours() : now.getHours()) * 3600 +
    ((gmt === "GMT") ? now.getUTCMinutes() : now.getMinutes()) * 60 +
    ((gmt === "GMT") ? now.getUTCSeconds() : now.getSeconds());
  var start = args[0] * 3600 + args[1] * 60 + args[2];
  var end = args[3] * 3600 + args[4] * 60 + args[5];
  if (start <= end) { return nowSeconds >= start && nowSeconds <= end; }
  return nowSeconds >= start || nowSeconds <= end;
}
function dateRange(y1, m1, d1, y2, m2, d2, gmt) {
  var args = [];
  for (var i = 0; i < arguments.length; i++) {
    if (typeof arguments[i] === "number") { args.push(arguments[i]); }
  }
  var now = new Date();
  var today = (gmt === "GMT") ? now.getUTCDate() : now.getDate();
  if (args.length === 1) { return today === args[0]; }
  if (args.length === 2) { return today >= args[0] && today <= args[1]; }
  if (args.length >= 6) {
    var current = ((gmt === "GMT") ? Date.UTC(now.getUTCFullYear(), now.getUTCMonth(), now.getUTCDate())
      : Date.UTC(now.getFullYear(), now.getMonth(), now.getDate())) / 86400000;
    function stamp(y, mo, d) { return Date.UTC(y, mo - 1, d) / 86400000; }
    return current >= stamp(args[0], args[1], args[2]) &&
      current <= stamp(args[3], args[4], args[5]);
  }
  return true;
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn resolve_inline(script: &str, url: &str) -> Result<ProxyConfig, PacError> {
        PacResolver::new(None, Some(script.to_string())).resolve(url, true)
    }

    #[test]
    fn parses_directive_lists() {
        assert_eq!(parse_chain("DIRECT"), vec![PacDirective::Direct]);
        assert_eq!(
            parse_chain("PROXY proxy.local:3128; SOCKS5 127.0.0.1:1080; DIRECT"),
            vec![
                PacDirective::Proxy {
                    scheme: "http",
                    host: "proxy.local".into(),
                    port: 3128
                },
                PacDirective::Proxy {
                    scheme: "socks5",
                    host: "127.0.0.1".into(),
                    port: 1080
                },
                PacDirective::Direct,
            ]
        );
        // Default ports and unknown directives.
        assert_eq!(
            parse_chain("PIEPROXY x; PROXY bare-host; HTTPS secure:8443"),
            vec![
                PacDirective::Proxy {
                    scheme: "http",
                    host: "bare-host".into(),
                    port: 80
                },
                PacDirective::Proxy {
                    scheme: "https",
                    host: "secure".into(),
                    port: 8443
                },
            ]
        );
        // Garbage directives are skipped rather than guessed at.
        assert_eq!(parse_chain("PROXY bare-host HTTPS secure:8443"), vec![]);
        assert_eq!(first_proxy(&[]), None);
    }

    #[test]
    fn extracts_url_hosts() {
        assert_eq!(url_host("https://example.com/a/b?c"), "example.com");
        assert_eq!(url_host("https://user:pw@example.com:8443/"), "example.com");
        assert_eq!(url_host("http://[::1]:8080/x"), "[::1]");
        assert_eq!(url_host("magnet:?xt=urn:btih:abc"), "magnet");
        assert_eq!(url_host("example.com/path"), "example.com");
    }

    #[test]
    fn evaluates_scripts_with_helper_functions() {
        let script = r#"
            function FindProxyForURL(url, host) {
                if (isPlainHostName(host)) { return "DIRECT"; }
                if (shExpMatch(url, "https://*.corp.example/*")) { return "PROXY corp:3128"; }
                if (dnsDomainIs(host, ".internal")) { return "SOCKS5 10.0.0.2:1080"; }
                return "DIRECT";
            }
        "#;

        let proxy = resolve_inline(script, "https://intranet/wiki").unwrap();
        assert_eq!(proxy.mode, ProxyMode::Direct);

        let proxy = resolve_inline(script, "https://docs.corp.example/x").unwrap();
        assert_eq!(proxy.url.as_deref(), Some("http://corp:3128"));

        let proxy = resolve_inline(script, "http://host.internal/").unwrap();
        assert_eq!(proxy.url.as_deref(), Some("socks5://10.0.0.2:1080"));
    }

    #[test]
    fn first_directive_wins() {
        let script = r#"function FindProxyForURL(){ return "PROXY a:1; PROXY b:2; DIRECT"; }"#;
        let proxy = resolve_inline(script, "http://x/").unwrap();
        assert_eq!(proxy.url.as_deref(), Some("http://a:1"));

        let script = r#"function FindProxyForURL(){ return "DIRECT; PROXY a:1"; }"#;
        let proxy = resolve_inline(script, "http://x/").unwrap();
        assert_eq!(proxy.mode, ProxyMode::Direct);
    }

    #[test]
    fn rejects_broken_or_hostile_scripts() {
        let missing = resolve_inline("var x = 1;", "http://x/");
        assert!(matches!(missing, Err(PacError::Script(_))), "{missing:?}");

        let syntax = resolve_inline("function FindProxyForURL( {", "http://x/");
        assert!(matches!(syntax, Err(PacError::Script(_))));

        let empty = resolve_inline("function FindProxyForURL(){ return '   '; }", "http://x/");
        assert!(matches!(empty, Err(PacError::NoDirective(_))), "{empty:?}");

        let bomb = resolve_inline(
            "function FindProxyForURL(){ var a = []; while (true) { a.push(new Array(1e5)); } }",
            "http://x/",
        );
        assert!(matches!(bomb, Err(PacError::Script(_))), "{bomb:?}");

        let loop_ = resolve_inline("function FindProxyForURL(){ while (true) {} }", "http://x/");
        assert!(matches!(loop_, Err(PacError::Script(_))), "{loop_:?}");
    }

    #[test]
    fn loads_scripts_from_files_and_keeps_them_warm() {
        let path = std::env::temp_dir().join(format!("aether-pac-{}.dat", std::process::id()));
        fs::write(
            &path,
            r#"function FindProxyForURL(url, host){ return "PROXY file-proxy:8080"; }"#,
        )
        .unwrap();
        let source = format!("file://{}", path.display());

        let mut resolver =
            PacResolver::new(Some(source.clone()), None).with_ttl(Duration::from_secs(600));
        let proxy = resolver.resolve("http://example.com/", true).unwrap();
        assert_eq!(proxy.url.as_deref(), Some("http://file-proxy:8080"));

        // Cached: a later failure to read the file must not break resolution.
        fs::remove_file(&path).unwrap();
        let proxy = resolver.resolve("http://example.com/", true).unwrap();
        assert_eq!(proxy.url.as_deref(), Some("http://file-proxy:8080"));

        // Without a cache an unreachable source is an error, not "direct".
        let mut cold = PacResolver::new(Some(source), None);
        assert!(cold.resolve("http://example.com/", true).is_err());
        assert!(cold.last_error().is_some());

        // Queue promotions do not touch the network.
        let mut offline = PacResolver::new(Some("http://127.0.0.1:9/pac".into()), None);
        assert!(offline.resolve("http://example.com/", false).is_err());
    }

    #[test]
    fn fetches_scripts_over_http() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut buffer = [0_u8; 1024];
                let _ = stream.read(&mut buffer);
                let body = r#"function FindProxyForURL(){ return "PROXY http-proxy:3128"; }"#;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/x-ns-proxy-autoconfig\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });

        let mut resolver = PacResolver::new(Some(format!("http://{addr}/wpad.dat")), None);
        let proxy = resolver.resolve("http://example.com/", true).unwrap();
        assert_eq!(proxy.url.as_deref(), Some("http://http-proxy:3128"));
        assert!(resolver.cached());
    }
}
