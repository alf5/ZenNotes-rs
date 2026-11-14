//! Requests to Notion. Upstream sends them from its main process with
//! net.fetch (notion-import.ts); the webview here would meet CORS, and must
//! not hold the credential, so the vendored clients run in the webview with
//! a fetch that lands here. This side allows only:
//!
//! - the REST API: `https://api.notion.com/v1/…`, GET or POST, with the saved
//!   integration token attached (or a pasted one being checked);
//! - the hosted MCP service: `POST https://mcp.notion.com/mcp`, with the
//!   signed-in session's access token attached;
//! - Sign in with Notion's own requests, only to mcp.notion.com
//!   (`sign_in_request`, used by notion_auth.rs);
//! - downloads of the files a page holds, which carry no credential: https
//!   only, never to a private address (each redirect is checked again, and
//!   the address a name resolves to is checked too).
//!
//! No redirects are followed where a credential goes, no cookies are kept.
//! `ZENNOTES_NOTION_API_URL` / `ZENNOTES_NOTION_MCP_URL` name a stand-in for
//! a test run; as upstream, a release build honors them only with ZEN_PERF=1.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Read, Write};
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::Serialize;
use url::{Host, Url};

const NOTION_MCP_BASE_URL: &str = "https://mcp.notion.com";
const API_HOST: &str = "api.notion.com";
const MCP_HOST: &str = "mcp.notion.com";
const MAX_BODY_BYTES: u64 = 64 * 1024 * 1024;
const MAX_REDIRECTS: usize = 5;
const API_HEADERS: &[&str] = &["notion-version", "content-type", "accept"];
const MCP_HEADERS: &[&str] = &["content-type", "accept", "mcp-protocol-version", "mcp-session-id"];
const API_REPLY_HEADERS: &[&str] = &["retry-after", "content-type"];
const MCP_REPLY_HEADERS: &[&str] = &["retry-after", "content-type", "mcp-session-id"];

fn overridable(name: &str) -> Option<String> {
    if !cfg!(debug_assertions) && std::env::var("ZEN_PERF").as_deref() != Ok("1") {
        return None;
    }
    let value = std::env::var(name).ok()?;
    let value = value.trim().trim_end_matches('/');
    (!value.is_empty()).then(|| value.to_string())
}

fn api_override() -> Option<String> {
    overridable("ZENNOTES_NOTION_API_URL")
}

fn mcp_override() -> Option<String> {
    overridable("ZENNOTES_NOTION_MCP_URL")
}

/// Upstream's `usingStandIn`: a test run may fetch files over plain http.
pub fn using_stand_in() -> bool {
    api_override().is_some() || mcp_override().is_some()
}

/// Where Sign in with Notion talks to.
pub fn mcp_base_url() -> String {
    mcp_override().unwrap_or_else(|| NOTION_MCP_BASE_URL.to_string())
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostReply {
    pub status: u16,
    /// The few headers the clients read, by lower-case name.
    pub headers: BTreeMap<String, String>,
    pub body: String,
}

fn plain_https(url: &Url, host: &str) -> bool {
    url.scheme() == "https"
        && url.host_str() == Some(host)
        && url.port().is_none()
        && url.username().is_empty()
        && url.password().is_none()
        && url.fragment().is_none()
}

pub fn is_allowed_api_request(url: &Url) -> bool {
    plain_https(url, API_HOST) && url.path().starts_with("/v1/")
}

pub fn is_allowed_mcp_request(url: &Url) -> bool {
    plain_https(url, MCP_HOST) && url.path() == "/mcp" && url.query().is_none()
}

fn rebased(url: &Url, base: &str) -> String {
    let query = url.query().map(|q| format!("?{q}")).unwrap_or_default();
    format!("{base}{}{query}", url.path())
}

/// The headers the client set that may go on; its authorization never does.
fn pass_headers(headers: &BTreeMap<String, String>, allowed: &[&str]) -> Result<Vec<(String, String)>, String> {
    let mut out = Vec::new();
    for (name, value) in headers {
        let name = name.to_ascii_lowercase();
        if !allowed.contains(&name.as_str()) {
            continue;
        }
        if value.contains(['\r', '\n']) {
            return Err("A request header was malformed.".to_string());
        }
        out.push((name, value.clone()));
    }
    Ok(out)
}

fn read_capped(reader: impl Read) -> Result<String, String> {
    let mut bytes = Vec::new();
    reader
        .take(MAX_BODY_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_BODY_BYTES {
        return Err("Notion's answer was too large.".to_string());
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn credentialed_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(30))
        .timeout_read(Duration::from_secs(300))
        .timeout_write(Duration::from_secs(60))
        .redirects(0)
        .user_agent("ZenNotes-rs")
        .build()
}

/// One request with a bearer token. A refusal is an answer, not an error.
fn send(
    target: &str,
    method: &str,
    headers: &[(String, String)],
    bearer: &str,
    body: Option<&str>,
    keep: &[&str],
) -> Result<HostReply, String> {
    let mut request = credentialed_agent().request(method, target);
    for (name, value) in headers {
        request = request.set(name, value);
    }
    request = request.set("authorization", &format!("Bearer {bearer}"));
    let outcome = match body {
        Some(body) => request.send_string(body),
        None => request.call(),
    };
    let response = match outcome {
        Ok(response) => response,
        Err(ureq::Error::Status(_, response)) => response,
        Err(error) => return Err(format!("Couldn't reach Notion: {}", error.kind())),
    };
    let status = response.status();
    let mut reply_headers = BTreeMap::new();
    for name in keep {
        if let Some(value) = response.header(name) {
            reply_headers.insert((*name).to_string(), value.to_string());
        }
    }
    let body = read_capped(response.into_reader())?;
    Ok(HostReply { status, headers: reply_headers, body })
}

/// A REST call. `token` is a pasted one being checked before it's saved;
/// without one, the saved token goes.
pub fn api_request(
    url: &str,
    method: &str,
    headers: &BTreeMap<String, String>,
    body: Option<&str>,
    token: Option<String>,
) -> Result<HostReply, String> {
    let parsed = Url::parse(url).map_err(|e| e.to_string())?;
    if !is_allowed_api_request(&parsed) || !matches!(method, "GET" | "POST") {
        return Err("Not a Notion API request.".to_string());
    }
    let token = match token {
        Some(token) => token,
        None => super::notion_auth::saved_token().ok_or("Sign in to Notion first.")?,
    };
    let target = match api_override() {
        Some(base) => rebased(&parsed, &base),
        None => parsed.to_string(),
    };
    send(&target, method, &pass_headers(headers, API_HEADERS)?, &token, body, API_REPLY_HEADERS)
}

/// A call to Notion's hosted MCP service, as the signed-in person.
pub fn mcp_request(
    url: &str,
    method: &str,
    headers: &BTreeMap<String, String>,
    body: Option<&str>,
) -> Result<HostReply, String> {
    let parsed = Url::parse(url).map_err(|e| e.to_string())?;
    if !is_allowed_mcp_request(&parsed) || method != "POST" {
        return Err("Not a Notion MCP request.".to_string());
    }
    let token = super::notion_auth::access_token()?;
    let target = match mcp_override() {
        Some(base) => rebased(&parsed, &base),
        None => parsed.to_string(),
    };
    send(&target, method, &pass_headers(headers, MCP_HEADERS)?, &token, body, MCP_REPLY_HEADERS)
}

/// Sign in's own requests: only to the MCP service's origin, never
/// redirected. They carry the code, the verifier and the refresh token.
pub fn sign_in_request(
    base: &str,
    url: &str,
    method: &str,
    content_type: Option<&str>,
    body: Option<&str>,
) -> Result<(u16, String), String> {
    if !same_origin(base, url) {
        return Err("Notion's sign-in pointed somewhere else.".to_string());
    }
    let mut request = credentialed_agent().request(method, url).set("accept", "application/json");
    if let Some(content_type) = content_type {
        request = request.set("content-type", content_type);
    }
    let outcome = match body {
        Some(body) => request.send_string(body),
        None => request.call(),
    };
    let response = match outcome {
        Ok(response) => response,
        Err(ureq::Error::Status(_, response)) => response,
        Err(error) => return Err(format!("Couldn't reach Notion: {}", error.kind())),
    };
    let status = response.status();
    Ok((status, read_capped(response.into_reader())?))
}

/// The same scheme, host and port.
pub fn same_origin(base: &str, url: &str) -> bool {
    match (Url::parse(base), Url::parse(url)) {
        (Ok(a), Ok(b)) => {
            a.scheme() == b.scheme()
                && a.host_str() == b.host_str()
                && a.port_or_known_default() == b.port_or_known_default()
                && b.username().is_empty()
                && b.password().is_none()
        }
        _ => false,
    }
}

// ---- files ----------------------------------------------------------------

/// Loopback, private, link-local, carrier-grade NAT (a tailnet), unique
/// local and the like: anything inside the person's own network.
pub fn is_private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || a == 0
                || (a == 100 && (64..128).contains(&b))
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_private_ip(IpAddr::V4(v4));
            }
            let first = v6.segments()[0];
            v6.is_loopback() || v6.is_unspecified() || (first & 0xfe00) == 0xfc00 || (first & 0xffc0) == 0xfe80
        }
    }
}

/// Upstream's `isPrivateHost`, reading an address as an address.
pub fn is_private_host(host: &Host<&str>) -> bool {
    match host {
        Host::Ipv4(ip) => is_private_ip(IpAddr::V4(*ip)),
        Host::Ipv6(ip) => is_private_ip(IpAddr::V6(*ip)),
        Host::Domain(name) => {
            let name = name.to_ascii_lowercase();
            name == "localhost"
                || name.ends_with(".localhost")
                || name.ends_with(".local")
                || name.ends_with(".internal")
        }
    }
}

fn check_file_link(url: &Url, allow_insecure: bool) -> Result<(), String> {
    let scheme = url.scheme();
    if scheme != "https" && !(allow_insecure && scheme == "http") {
        return Err(format!("Refused a file link over {scheme}:"));
    }
    let host = url.host().ok_or("Refused a file link without a host")?;
    if !allow_insecure && is_private_host(&host) {
        return Err("Refused a file link to a private address".to_string());
    }
    Ok(())
}

/// A name that resolves into the private network is refused too, so a
/// public-looking name can't lead the download inside.
fn public_addresses(netloc: &str) -> io::Result<Vec<SocketAddr>> {
    let addresses: Vec<SocketAddr> = netloc.to_socket_addrs()?.collect();
    if addresses.iter().any(|address| is_private_ip(address.ip())) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Refused a file link to a private address",
        ));
    }
    Ok(addresses)
}

fn file_agent(allow_insecure: bool) -> ureq::Agent {
    let builder = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(30))
        .timeout_read(Duration::from_secs(120))
        .redirects(0)
        .user_agent("ZenNotes-rs");
    if allow_insecure {
        builder.build()
    } else {
        builder.resolver(public_addresses).build()
    }
}

#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DownloadReply {
    pub status: u16,
    pub content_type: Option<String>,
    /// The body was written to the target.
    pub saved: bool,
}

fn write_body(mut reader: impl Read, target: &Path, stop: &AtomicBool) -> Result<(), String> {
    let mut file = fs::File::create(target).map_err(|e| e.to_string())?;
    let mut buffer = vec![0u8; 64 * 1024];
    let result = loop {
        if stop.load(Ordering::SeqCst) {
            break Err("Import stopped.".to_string());
        }
        match reader.read(&mut buffer) {
            Ok(0) => break file.sync_all().map_err(|e| e.to_string()),
            Ok(n) => {
                if let Err(error) = file.write_all(&buffer[..n]) {
                    break Err(error.to_string());
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => break Err(format!("The file link stopped answering: {error}")),
        }
    };
    if result.is_err() {
        drop(file);
        let _ = fs::remove_file(target);
    }
    result
}

/// Downloads a page's file into `target` (in staging), streamed, since an
/// attachment can be far bigger than anything worth holding. A failed answer
/// or, when `refuse_html`, a web page where a file should be, comes back
/// unsaved for the caller to report as upstream does.
pub fn download(
    url: &str,
    target: &Path,
    refuse_html: bool,
    allow_insecure: bool,
    stop: &AtomicBool,
) -> Result<DownloadReply, String> {
    let mut current = Url::parse(url).map_err(|_| "Not a file link.".to_string())?;
    let agent = file_agent(allow_insecure);
    for _ in 0..=MAX_REDIRECTS {
        check_file_link(&current, allow_insecure)?;
        if stop.load(Ordering::SeqCst) {
            return Err("Import stopped.".to_string());
        }
        let response = match agent.get(current.as_str()).call() {
            Ok(response) => response,
            Err(ureq::Error::Status(_, response)) => response,
            Err(error) => return Err(format!("Couldn't fetch a file: {error}")),
        };
        let status = response.status();
        if (300..400).contains(&status) {
            if let Some(location) = response.header("location") {
                current = current.join(location).map_err(|_| "A file link redirected nowhere.".to_string())?;
                continue;
            }
        }
        let content_type = response.header("content-type").map(str::to_string);
        if !(200..300).contains(&status) {
            return Ok(DownloadReply { status, content_type, saved: false });
        }
        let is_html = content_type
            .as_deref()
            .is_some_and(|t| t.trim_start().to_ascii_lowercase().starts_with("text/html"));
        if refuse_html && is_html {
            return Ok(DownloadReply { status, content_type, saved: false });
        }
        write_body(response.into_reader(), target, stop)?;
        return Ok(DownloadReply { status, content_type, saved: true });
    }
    Err("A file link redirected too many times.".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    fn url(text: &str) -> Url {
        Url::parse(text).unwrap()
    }

    #[test]
    fn only_notion_s_api_and_mcp_endpoints_are_allowed() {
        assert!(is_allowed_api_request(&url("https://api.notion.com/v1/search")));
        assert!(is_allowed_api_request(&url("https://api.notion.com/v1/blocks/abc/children?page_size=100")));
        for bad in [
            "http://api.notion.com/v1/search",
            "https://api.notion.com:8443/v1/search",
            "https://user:pw@api.notion.com/v1/search",
            "https://api.notion.com.evil.test/v1/search",
            "https://evil.test/v1/search",
            "https://api.notion.com/v2/search",
            "https://api.notion.com/",
            "https://mcp.notion.com/v1/search",
        ] {
            assert!(!is_allowed_api_request(&url(bad)), "{bad}");
        }

        assert!(is_allowed_mcp_request(&url("https://mcp.notion.com/mcp")));
        for bad in [
            "http://mcp.notion.com/mcp",
            "https://mcp.notion.com/mcp?x=1",
            "https://mcp.notion.com/token",
            "https://mcp.notion.com:444/mcp",
            "https://api.notion.com/mcp",
            "https://mcp.notion.com.evil.test/mcp",
        ] {
            assert!(!is_allowed_mcp_request(&url(bad)), "{bad}");
        }
    }

    #[test]
    fn nothing_leaves_for_a_request_that_is_not_allowed() {
        let headers = BTreeMap::new();
        let err = api_request("https://evil.test/v1/users/me", "GET", &headers, None, Some("tok".into())).unwrap_err();
        assert_eq!(err, "Not a Notion API request.");
        assert!(api_request("https://api.notion.com/v1/users/me", "DELETE", &headers, None, Some("t".into())).is_err());
        assert!(mcp_request("https://evil.test/mcp", "POST", &headers, Some("{}")).is_err());
        assert!(mcp_request("https://mcp.notion.com/mcp", "GET", &headers, None).is_err());
    }

    #[test]
    fn the_client_s_authorization_never_goes_and_other_headers_are_dropped() {
        let mut headers = BTreeMap::new();
        headers.insert("Authorization".to_string(), "Bearer webview".to_string());
        headers.insert("notion-version".to_string(), "2025-09-03".to_string());
        headers.insert("cookie".to_string(), "x".to_string());
        let passed = pass_headers(&headers, API_HEADERS).unwrap();
        assert_eq!(passed, vec![("notion-version".to_string(), "2025-09-03".to_string())]);
        headers.insert("accept".to_string(), "a\r\nx: y".to_string());
        assert!(pass_headers(&headers, API_HEADERS).is_err());
    }

    #[test]
    fn sign_in_talks_only_to_its_own_origin() {
        assert!(same_origin("https://mcp.notion.com", "https://mcp.notion.com/token"));
        assert!(same_origin("https://mcp.notion.com", "https://mcp.notion.com:443/register"));
        assert!(!same_origin("https://mcp.notion.com", "https://evil.test/token"));
        assert!(!same_origin("https://mcp.notion.com", "http://mcp.notion.com/token"));
        assert!(!same_origin("https://mcp.notion.com", "https://u:p@mcp.notion.com/token"));
        assert!(sign_in_request("https://mcp.notion.com", "https://evil.test/token", "POST", None, None).is_err());
    }

    #[test]
    fn private_addresses_are_refused() {
        for host in ["localhost", "127.0.0.1", "10.0.0.2", "169.254.169.254", "[::1]", "printer.local", "192.168.1.10", "100.101.102.103", "[fd00::1]", "[::ffff:127.0.0.1]", "0.0.0.0", "db.internal"] {
            let parsed = url(&format!("https://{host}/a.png"));
            assert!(is_private_host(&parsed.host().unwrap()), "{host}");
        }
        for host in ["images.unsplash.com", "172.32.0.1", "8.8.8.8", "fcbarcelona.example", "[2606:4700::1111]"] {
            let parsed = url(&format!("https://{host}/a.png"));
            assert!(!is_private_host(&parsed.host().unwrap()), "{host}");
        }
        let stop = AtomicBool::new(false);
        let target = tempfile::tempdir().unwrap();
        let file = target.path().join("f");
        assert_eq!(download("http://files.test/a.png", &file, true, false, &stop).unwrap_err(), "Refused a file link over http:");
        assert_eq!(
            download("https://192.168.1.10/a.png", &file, true, false, &stop).unwrap_err(),
            "Refused a file link to a private address"
        );
        assert!(public_addresses("127.0.0.1:443").is_err());
        assert!(!file.exists());
    }

    /// Answers each request in turn with one of `answers`.
    fn serve(answers: Vec<String>) -> (String, std::thread::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for answer in answers {
                let (mut stream, _) = listener.accept().unwrap();
                let mut head = Vec::new();
                let mut buf = [0u8; 1024];
                while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                    let n = stream.read(&mut buf).unwrap();
                    head.extend_from_slice(&buf[..n]);
                }
                seen.push(String::from_utf8_lossy(&head).to_lowercase());
                stream.write_all(answer.as_bytes()).unwrap();
            }
            seen
        });
        (base, handle)
    }

    #[test]
    fn a_download_follows_a_redirect_but_checks_it_again() {
        let stop = AtomicBool::new(false);
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("file.tmp");
        let (base, server) = serve(vec![
            "HTTP/1.1 302 Found\r\nlocation: /real.png\r\ncontent-length: 0\r\nconnection: close\r\n\r\n".into(),
            "HTTP/1.1 200 OK\r\ncontent-type: image/png\r\ncontent-length: 3\r\nconnection: close\r\n\r\n\x01\x02\x03".into(),
        ]);
        let reply = download(&format!("{base}/a.png"), &target, true, true, &stop).unwrap();
        assert_eq!(reply, DownloadReply { status: 200, content_type: Some("image/png".into()), saved: true });
        assert_eq!(fs::read(&target).unwrap(), vec![1, 2, 3]);
        let seen = server.join().unwrap();
        assert!(seen[1].starts_with("get /real.png"));
        assert!(seen.iter().all(|s| !s.contains("authorization") && !s.contains("cookie")));

        // Every hop is checked again. (The stand-in is plain http on loopback,
        // so the allowance a test run gets is on; a scheme is still refused.)
        let (base, server) = serve(vec![
            "HTTP/1.1 302 Found\r\nlocation: ftp://files.test/x\r\ncontent-length: 0\r\nconnection: close\r\n\r\n".into(),
        ]);
        let err = download(&format!("{base}/a.png"), &target, true, true, &stop).unwrap_err();
        assert_eq!(err, "Refused a file link over ftp:");
        server.join().unwrap();
        assert!(check_file_link(&url("https://192.168.0.1/x"), false).is_err());
        assert!(check_file_link(&url("https://files.test/x"), false).is_ok());
    }

    #[test]
    fn a_web_page_or_a_refusal_is_not_saved() {
        let stop = AtomicBool::new(false);
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("file.tmp");
        let (base, server) = serve(vec![
            "HTTP/1.1 200 OK\r\ncontent-type: text/html\r\ncontent-length: 6\r\nconnection: close\r\n\r\n<html>".into(),
            "HTTP/1.1 403 Forbidden\r\ncontent-length: 4\r\nconnection: close\r\n\r\ngone".into(),
        ]);
        let page = download(&format!("{base}/a.png"), &target, true, true, &stop).unwrap();
        assert!(!page.saved);
        let refused = download(&format!("{base}/a.png"), &target, true, true, &stop).unwrap();
        assert_eq!(refused.status, 403);
        assert!(!refused.saved);
        assert!(!target.exists());
        server.join().unwrap();
    }

    #[test]
    fn a_stop_ends_a_download_and_leaves_nothing() {
        let stop = AtomicBool::new(true);
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("file.tmp");
        assert_eq!(write_body(&b"abc"[..], &target, &stop).unwrap_err(), "Import stopped.");
        assert!(!target.exists());
    }

    #[test]
    fn sends_the_bearer_and_passes_on_the_session_header() {
        let (base, server) = serve(vec![
            "HTTP/1.1 200 OK\r\nmcp-session-id: s-1\r\ncontent-type: text/event-stream\r\nset-cookie: a=b\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok".into(),
        ]);
        let headers = vec![("mcp-protocol-version".to_string(), "2025-06-18".to_string())];
        // No body: the stand-in reads only the head, and an unread body would
        // make its close a reset.
        let reply = send(&format!("{base}/mcp"), "POST", &headers, "the-token", None, MCP_REPLY_HEADERS).unwrap();
        assert_eq!(reply.status, 200);
        assert_eq!(reply.body, "ok");
        assert_eq!(reply.headers.get("mcp-session-id").map(String::as_str), Some("s-1"));
        assert!(!reply.headers.contains_key("set-cookie"));
        let seen = server.join().unwrap();
        assert!(seen[0].contains("authorization: bearer the-token\r\n"));
        assert!(seen[0].contains("mcp-protocol-version: 2025-06-18\r\n"));
    }
}
