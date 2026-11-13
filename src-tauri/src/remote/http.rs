//! The authenticated HTTP transport to a ZenNotes server: the Rust half of
//! upstream's apps/desktop/src/main/remote/{server-client,connection}.ts.
//!
//! The route table lives in the webview (src/bridge/remote/client.ts), where
//! the vendored shared-domain helpers around it already run. What stays here
//! is everything that must not: the token, TLS and the byte streams. The token
//! is attached on this side and never handed to the webview, and a request may
//! only name a path under `/api/` on the server this session connected to.

use std::io::Read;
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;

use super::tls;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// Roughly Node's undici defaults, which upstream relies on.
const READ_TIMEOUT: Duration = Duration::from_secs(300);
const WRITE_TIMEOUT: Duration = Duration::from_secs(300);
/// A JSON answer larger than this is refused rather than held in memory.
const MAX_JSON_BYTES: u64 = 256 * 1024 * 1024;
/// How much of an error body is quoted in the message.
const MAX_ERROR_BODY_BYTES: u64 = 16 * 1024;

/// An error the webview can tell apart, as upstream's two error classes do.
/// `src/bridge/ipc.ts` turns it back into `RemoteConnectionError` or
/// `RemoteRequestError`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteError {
    /// `connection`: the server never answered. `request`: it answered with a
    /// non-2xx status. `other`: anything else (bad input, no session).
    pub remote_error: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    pub message: String,
}

impl RemoteError {
    pub fn other(message: impl Into<String>) -> Self {
        RemoteError { remote_error: "other", status: None, message: message.into() }
    }

    pub fn connection(message: String) -> Self {
        RemoteError { remote_error: "connection", status: None, message }
    }

    pub fn request(status: u16, message: String) -> Self {
        RemoteError { remote_error: "request", status: Some(status), message }
    }
}

impl From<String> for RemoteError {
    fn from(message: String) -> Self {
        RemoteError::other(message)
    }
}

impl std::fmt::Display for RemoteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// Upstream's `normalizeBaseUrl`, with one deliberate difference: a URL typed
/// without a scheme gets `https://`, not `http://`, so a server on the
/// internet never receives the token in cleartext before any redirect. A
/// loopback host keeps `http://`, where nothing leaves the machine and a local
/// server (`localhost:7878`) rarely has a certificate.
pub fn normalize_base_url(value: &str) -> String {
    let trimmed = value.trim();
    let with_scheme = if super::profiles::has_http_scheme(trimmed) {
        trimmed.to_string()
    } else if is_loopback_authority(trimmed) {
        format!("http://{trimmed}")
    } else {
        format!("https://{trimmed}")
    };
    with_scheme.trim_end_matches('/').to_string()
}

fn is_loopback_authority(value: &str) -> bool {
    let authority = value.split('/').next().unwrap_or("");
    let host = if let Some(rest) = authority.strip_prefix('[') {
        rest.split(']').next().unwrap_or("")
    } else {
        authority.rsplit_once(':').map(|(h, _)| h).unwrap_or(authority)
    };
    let host = host.to_ascii_lowercase();
    host == "localhost"
        || host.ends_with(".localhost")
        || host == "::1"
        || host.parse::<std::net::Ipv4Addr>().map(|ip| ip.is_loopback()).unwrap_or(false)
}

/// Upstream's `connectionErrorMessage` (#481).
pub fn connection_error_message(base_url: &str, detail: &str) -> String {
    let detail = if detail.is_empty() {
        String::new()
    } else {
        format!(" Could not reach the server: {detail}.")
    };
    let local_network_hint = if cfg!(target_os = "macos") {
        " If the server is on your local network, check that ZenNotes-rs is allowed under System Settings → Privacy & Security → Local Network."
    } else {
        ""
    };
    format!(
        "Could not connect to the ZenNotes server at {base_url}. Make sure the server is running and the URL is correct.{detail}{local_network_hint}"
    )
}

/// Upstream's `requestErrorMessage`.
pub fn request_error_message(base_url: &str, path: &str, status: u16, status_text: &str, text: &str) -> String {
    if status == 401 {
        return format!(
            "The ZenNotes server rejected the connection. Check the auth token for {base_url} and try again."
        );
    }
    let text = if text.is_empty() { String::new() } else { format!(": {text}") };
    format!("Remote server request failed ({status} {status_text}) for {path}{text}")
}

/// Only GET and POST exist in the protocol.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Method {
    Get,
    Post,
}

impl Method {
    pub fn parse(value: &str) -> Result<Self, RemoteError> {
        match value {
            "GET" => Ok(Method::Get),
            "POST" => Ok(Method::Post),
            other => Err(RemoteError::other(format!("The ZenNotes server protocol has no {other} requests."))),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Post => "POST",
        }
    }
}

/// A path the webview may ask for: `/api/<route>` plus an optional query.
/// The route is letters, digits, `-`, `_` and single slashes, so it can never
/// climb out of `/api/`, name another host, or smuggle a fragment; the query
/// is printable ASCII, which is all `encodeURIComponent` produces.
pub fn check_api_path(path: &str) -> Result<(), RemoteError> {
    let refuse = || RemoteError::other(format!("Not a ZenNotes server route: {path}"));
    let (route, query) = match path.split_once('?') {
        Some((route, query)) => (route, Some(query)),
        None => (path, None),
    };
    let Some(rest) = route.strip_prefix("/api/") else { return Err(refuse()) };
    if rest.is_empty()
        || rest.starts_with('/')
        || rest.ends_with('/')
        || rest.contains("//")
        || !rest.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '/'))
    {
        return Err(refuse());
    }
    if let Some(query) = query {
        if !query.bytes().all(|b| b.is_ascii_graphic() && b != b'#') {
            return Err(refuse());
        }
    }
    Ok(())
}

fn read_capped(reader: impl Read, cap: u64) -> std::io::Result<Vec<u8>> {
    let mut buf = Vec::new();
    reader.take(cap + 1).read_to_end(&mut buf)?;
    if buf.len() as u64 > cap {
        return Err(std::io::Error::other("the server's answer is too large"));
    }
    Ok(buf)
}

fn error_body(response: ureq::Response) -> String {
    let mut buf = Vec::new();
    let _ = response.into_reader().take(MAX_ERROR_BODY_BYTES).read_to_end(&mut buf);
    String::from_utf8_lossy(&buf).trim().to_string()
}

/// One multipart part boundary per upload; random so file bytes can't fake it.
fn multipart_boundary() -> String {
    format!("----ZenNotesRs{}", uuid::Uuid::new_v4().simple())
}

/// A form field name or filename as browsers send it (WHATWG FormData):
/// `"`, CR and LF escaped, everything else as is.
fn form_escape(value: &str) -> String {
    value.replace('"', "%22").replace('\r', "%0D").replace('\n', "%0A")
}

/// The framing around an uploaded file's bytes: `notePath` first, then the
/// file part's headers; and the closing boundary.
fn multipart_frame(boundary: &str, note_path: &str, file_name: &str, mime: &str) -> (Vec<u8>, Vec<u8>) {
    let head = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"notePath\"\r\n\r\n{note_path}\r\n\
         --{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{}\"\r\nContent-Type: {mime}\r\n\r\n",
        form_escape(file_name)
    );
    let tail = format!("\r\n--{boundary}--\r\n");
    (head.into_bytes(), tail.into_bytes())
}

pub struct Transport {
    base_url: String,
    token: Option<String>,
    agent: ureq::Agent,
}

impl Transport {
    pub fn new(base_url: &str, token: Option<&str>) -> Result<Self, RemoteError> {
        let agent = ureq::AgentBuilder::new()
            .tls_config(tls::client_config()?)
            .timeout_connect(CONNECT_TIMEOUT)
            .timeout_read(READ_TIMEOUT)
            .timeout_write(WRITE_TIMEOUT)
            // Like fetch: the token follows a redirect only to the same host.
            .redirect_auth_headers(ureq::RedirectAuthHeaders::SameHost)
            .user_agent(concat!("ZenNotes-rs/", env!("CARGO_PKG_VERSION")))
            .build();
        Ok(Transport {
            base_url: normalize_base_url(base_url),
            token: token.map(str::trim).filter(|t| !t.is_empty()).map(str::to_string),
            agent,
        })
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn has_token(&self) -> bool {
        self.token.is_some()
    }

    /// For the change feed's upgrade request, which is built in this module's
    /// sibling. Never leaves Rust.
    pub(super) fn token(&self) -> Option<&str> {
        self.token.as_deref()
    }

    fn request(&self, method: Method, path: &str) -> ureq::Request {
        let request = self.agent.request(method.as_str(), &format!("{}{}", self.base_url, path));
        match &self.token {
            Some(token) => request.set("Authorization", &format!("Bearer {token}")),
            None => request,
        }
    }

    fn connection_error(&self, err: &ureq::Transport) -> RemoteError {
        RemoteError::connection(connection_error_message(&self.base_url, &err.to_string()))
    }

    fn status_error(&self, path: &str, code: u16, response: ureq::Response) -> RemoteError {
        let status_text = response.status_text().to_string();
        let text = error_body(response);
        RemoteError::request(code, request_error_message(&self.base_url, path, code, &status_text, &text))
    }

    /// One JSON request. `path` must pass `check_api_path`; the body, when
    /// present, is sent as JSON. A 204 or empty answer is `null`.
    pub fn json(&self, method: Method, path: &str, body: Option<&Value>) -> Result<Value, RemoteError> {
        check_api_path(path)?;
        let request = self.request(method, path);
        let result = match body {
            Some(body) => request
                .set("Content-Type", "application/json")
                .send_string(&serde_json::to_string(body).map_err(|e| RemoteError::other(e.to_string()))?),
            None => request.call(),
        };
        let response = match result {
            Ok(response) => response,
            Err(ureq::Error::Status(code, response)) => return Err(self.status_error(path, code, response)),
            Err(ureq::Error::Transport(err)) => return Err(self.connection_error(&err)),
        };
        if response.status() == 204 {
            return Ok(Value::Null);
        }
        let bytes = read_capped(response.into_reader(), MAX_JSON_BYTES)
            .map_err(|e| RemoteError::connection(connection_error_message(&self.base_url, &e.to_string())))?;
        if bytes.iter().all(u8::is_ascii_whitespace) {
            return Ok(Value::Null);
        }
        serde_json::from_slice(&bytes).map_err(|_| {
            RemoteError::other(format!(
                "The ZenNotes server at {} answered {path} with something that is not JSON.",
                self.base_url
            ))
        })
    }

    /// `uploadAsset`: multipart `notePath` + `file`, streamed from `reader`.
    pub fn upload(
        &self,
        note_path: &str,
        file_name: &str,
        mime: &str,
        reader: impl Read,
        len: u64,
    ) -> Result<Value, RemoteError> {
        let path = "/api/assets/upload";
        let boundary = multipart_boundary();
        let (head, tail) = multipart_frame(&boundary, note_path, file_name, mime);
        let total = head.len() as u64 + len + tail.len() as u64;
        let body = std::io::Cursor::new(head).chain(reader.take(len)).chain(std::io::Cursor::new(tail));
        let result = self
            .request(Method::Post, path)
            .set("Content-Type", &format!("multipart/form-data; boundary={boundary}"))
            .set("Content-Length", &total.to_string())
            .send(body);
        let response = match result {
            Ok(response) => response,
            Err(ureq::Error::Status(code, response)) => return Err(self.status_error(path, code, response)),
            Err(ureq::Error::Transport(err)) => return Err(self.connection_error(&err)),
        };
        let bytes = read_capped(response.into_reader(), MAX_JSON_BYTES)
            .map_err(|e| RemoteError::connection(connection_error_message(&self.base_url, &e.to_string())))?;
        serde_json::from_slice(&bytes).map_err(|_| {
            RemoteError::other(format!("The ZenNotes server at {} answered the upload with something that is not JSON.", self.base_url))
        })
    }

    /// `fetchAssetResponse`: the raw bytes of a vault asset, with a `Range`
    /// passed through for media. Errors use upstream's own wording.
    pub fn asset(&self, asset_path: &str, range: Option<&str>) -> Result<ureq::Response, RemoteError> {
        let path = format!("/api/assets/raw?path={}", urlencoding::encode(asset_path));
        let mut request = self.request(Method::Get, &path);
        if let Some(range) = range {
            request = request.set("Range", range);
        }
        match request.call() {
            Ok(response) => Ok(response),
            Err(ureq::Error::Status(code, response)) => {
                let status_text = response.status_text().to_string();
                let text = error_body(response);
                let text = if text.is_empty() { String::new() } else { format!(": {text}") };
                Err(RemoteError::request(
                    code,
                    format!("Remote asset request failed ({code} {status_text}) for {asset_path}{text}"),
                ))
            }
            Err(ureq::Error::Transport(err)) => Err(self.connection_error(&err)),
        }
    }

    /// The whole asset, for reading into memory or a temp file.
    pub fn asset_bytes(&self, asset_path: &str, cap: u64) -> Result<Vec<u8>, RemoteError> {
        let response = self.asset(asset_path, None)?;
        read_capped(response.into_reader(), cap)
            .map_err(|e| RemoteError::connection(connection_error_message(&self.base_url, &e.to_string())))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;

    /// What a test server saw.
    #[derive(Debug, Clone)]
    pub struct Seen {
        pub request_line: String,
        pub headers: Vec<(String, String)>,
        pub body: Vec<u8>,
    }

    impl Seen {
        pub fn header(&self, name: &str) -> Option<&str> {
            self.headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(name))
                .map(|(_, v)| v.as_str())
        }
    }

    /// A one-request-per-connection HTTP server answering each request with
    /// the next canned response. Returns its base URL and what it saw.
    pub fn serve(responses: Vec<String>) -> (String, mpsc::Receiver<Seen>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for response in responses {
                let Ok((stream, _)) = listener.accept() else { return };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request_line = String::new();
                reader.read_line(&mut request_line).unwrap();
                let mut headers = Vec::new();
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    let line = line.trim_end().to_string();
                    if line.is_empty() {
                        break;
                    }
                    if let Some((k, v)) = line.split_once(':') {
                        headers.push((k.trim().to_string(), v.trim().to_string()));
                    }
                }
                let len = headers
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                    .and_then(|(_, v)| v.parse::<usize>().ok())
                    .unwrap_or(0);
                let mut body = vec![0; len];
                reader.read_exact(&mut body).unwrap();
                let _ = tx.send(Seen { request_line: request_line.trim_end().to_string(), headers, body });
                let mut stream = stream;
                stream.write_all(response.as_bytes()).unwrap();
                let _ = stream.flush();
            }
        });
        (base, rx)
    }

    pub fn reply(status: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    #[test]
    fn urls_without_a_scheme_default_to_https_except_on_loopback() {
        assert_eq!(normalize_base_url(" zen.example.com/ "), "https://zen.example.com");
        assert_eq!(normalize_base_url("zen.example.com/notes//"), "https://zen.example.com/notes");
        assert_eq!(normalize_base_url("localhost:7878"), "http://localhost:7878");
        assert_eq!(normalize_base_url("127.0.0.1:7878"), "http://127.0.0.1:7878");
        assert_eq!(normalize_base_url("[::1]:7878"), "http://[::1]:7878");
        assert_eq!(normalize_base_url("HTTP://Zen.example.com"), "HTTP://Zen.example.com");
        assert_eq!(normalize_base_url("http://zen.example.com"), "http://zen.example.com");
        assert_eq!(normalize_base_url("localhost.evil.com"), "https://localhost.evil.com");
    }

    #[test]
    fn only_api_routes_pass() {
        for ok in [
            "/api/capabilities",
            "/api/notes/read?path=inbox%2FA%20note.md",
            "/api/search/text?q=a%2Bb&backend=auto",
            "/api/notes/read?path=..%2F..%2Fetc",
            "/api/workflows/runs/delete",
        ] {
            assert!(check_api_path(ok).is_ok(), "{ok}");
        }
        for bad in [
            "/capabilities",
            "/api/",
            "/api/../etc",
            "/api//notes",
            "/api/notes/",
            "https://evil.example/api/notes",
            "//evil.example/api/notes",
            "/api/notes#frag",
            "/api/notes?q=a b",
            "/api/notes?q=a#b",
            "/api/no tes",
            "/api/notes\\x",
            "/api/nötes",
        ] {
            assert!(check_api_path(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn json_requests_carry_the_token_and_the_body() {
        let (base, seen) = serve(vec![reply("200 OK", r#"{"path":"inbox/a.md"}"#)]);
        let transport = Transport::new(&base, Some(" secret ")).unwrap();
        let body = serde_json::json!({ "path": "inbox/a.md", "body": "hi" });
        let answer = transport.json(Method::Post, "/api/notes/write", Some(&body)).unwrap();
        assert_eq!(answer["path"], "inbox/a.md");
        let seen = seen.recv().unwrap();
        assert_eq!(seen.request_line, "POST /api/notes/write HTTP/1.1");
        assert_eq!(seen.header("authorization"), Some("Bearer secret"));
        assert_eq!(seen.header("content-type"), Some("application/json"));
        assert_eq!(serde_json::from_slice::<Value>(&seen.body).unwrap(), body);
    }

    #[test]
    fn a_base_path_is_kept() {
        let (base, seen) = serve(vec![reply("200 OK", "null")]);
        let transport = Transport::new(&format!("{base}/notes/"), None).unwrap();
        assert_eq!(transport.json(Method::Get, "/api/vault", None).unwrap(), Value::Null);
        let seen = seen.recv().unwrap();
        assert_eq!(seen.request_line, "GET /notes/api/vault HTTP/1.1");
        assert_eq!(seen.header("authorization"), None);
    }

    #[test]
    fn statuses_and_dead_servers_are_told_apart() {
        let (base, _seen) = serve(vec![
            reply("401 Unauthorized", ""),
            reply("404 Not Found", r#"{"code":"not_found"}"#),
            "HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n".to_string(),
        ]);
        let transport = Transport::new(&base, Some("t")).unwrap();
        let err = transport.json(Method::Get, "/api/vault", None).unwrap_err();
        assert_eq!(err.remote_error, "request");
        assert_eq!(err.status, Some(401));
        assert_eq!(
            err.message,
            format!("The ZenNotes server rejected the connection. Check the auth token for {base} and try again.")
        );
        let err = transport.json(Method::Get, "/api/notes/read?path=x", None).unwrap_err();
        assert_eq!(err.status, Some(404));
        assert_eq!(
            err.message,
            r#"Remote server request failed (404 Not Found) for /api/notes/read?path=x: {"code":"not_found"}"#
        );
        assert_eq!(transport.json(Method::Post, "/api/notes/empty-trash", None).unwrap(), Value::Null);

        // Nothing listens on a port that was just freed.
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let dead = Transport::new(&format!("http://127.0.0.1:{port}"), None).unwrap();
        let err = dead.json(Method::Get, "/api/vault", None).unwrap_err();
        assert_eq!(err.remote_error, "connection");
        assert_eq!(err.status, None);
        assert!(err.message.starts_with(&format!(
            "Could not connect to the ZenNotes server at http://127.0.0.1:{port}. Make sure the server is running and the URL is correct. Could not reach the server: "
        )));
    }

    #[test]
    fn a_route_outside_api_is_refused_before_anything_is_sent() {
        let transport = Transport::new("http://127.0.0.1:9", Some("t")).unwrap();
        let err = transport.json(Method::Get, "/../secret", None).unwrap_err();
        assert_eq!(err.remote_error, "other");
    }

    #[test]
    fn uploads_are_multipart_with_the_note_path_first() {
        let (base, seen) = serve(vec![reply("200 OK", r#"{"name":"a.png","path":"assets/a.png"}"#)]);
        let transport = Transport::new(&base, Some("t")).unwrap();
        let bytes = b"\x89PNG\r\n--not-a-boundary";
        let answer = transport
            .upload("inbox/Note.md", "we\"ird\nname.png", "image/png", &bytes[..], bytes.len() as u64)
            .unwrap();
        assert_eq!(answer["path"], "assets/a.png");
        let seen = seen.recv().unwrap();
        assert_eq!(seen.request_line, "POST /api/assets/upload HTTP/1.1");
        assert_eq!(seen.header("authorization"), Some("Bearer t"));
        let content_type = seen.header("content-type").unwrap().to_string();
        let boundary = content_type.strip_prefix("multipart/form-data; boundary=").unwrap();
        let body = String::from_utf8_lossy(&seen.body).to_string();
        assert!(body.starts_with(&format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"notePath\"\r\n\r\ninbox/Note.md\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"we%22ird%0Aname.png\"\r\nContent-Type: image/png\r\n\r\n"
        )));
        assert!(seen.body.windows(bytes.len()).any(|w| w == bytes));
        assert!(body.ends_with(&format!("\r\n--{boundary}--\r\n")));
    }
}
