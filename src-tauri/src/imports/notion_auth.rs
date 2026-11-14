//! Sign in with Notion, and the Notion credentials. Port of upstream's
//! notion-sign-in.ts plus the Notion half of secret-store.ts.
//!
//! OAuth with PKCE against Notion's hosted service: a client registered on
//! the spot, a loopback port listening for the redirect, no secret shipped and
//! no server of ours in between. Upstream keeps this in its main process so
//! the tokens never reach the renderer; here that means Rust: the webview's
//! MCP client is handed a stand-in token, and `notion_http.rs` attaches the
//! real one.
//!
//! The session and the integration token live in the OS keychain only. A
//! system without one keeps them in memory until the app quits (never in a
//! file, unlike upstream's base64 fallback), so a sign-in still works for the
//! run and the next launch asks again.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use url::Url;

use crate::secrets;

const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// Refresh a little early: a long import shouldn't race an expiring token.
const EXPIRY_MARGIN_MS: i64 = 60_000;

const CALLBACK_PAGE: &str = "<!doctype html><meta charset=\"utf-8\"><title>ZenNotes-rs</title>\n\
<body style=\"font:16px system-ui;display:grid;place-items:center;height:90vh;margin:0\">\n\
<p>Signed in. You can close this tab and go back to ZenNotes-rs.</p></body>";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct NotionSession {
    pub client_id: String,
    pub access_token: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub refresh_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub expires_at: Option<i64>,
}

pub struct HttpRequest<'a> {
    pub url: &'a str,
    pub method: &'a str,
    pub content_type: Option<&'a str>,
    pub body: Option<&'a str>,
}

/// Upstream's `NotionSignInDeps`: browser, fetch, storage and clock come in
/// from outside, so a test can run the whole flow against a stand-in.
pub trait SignInDeps: Send + Sync {
    fn base_url(&self) -> String;
    fn fetch(&self, request: HttpRequest<'_>) -> Result<(u16, String), String>;
    fn open_external(&self, url: &str) -> Result<(), String>;
    fn load(&self) -> Option<String>;
    fn save(&self, value: &str) -> bool;
    fn remove(&self);
    fn now(&self) -> i64;
}

#[derive(Deserialize)]
struct AuthServerMetadata {
    authorization_endpoint: String,
    token_endpoint: String,
    registration_endpoint: String,
}

#[derive(Deserialize)]
struct TokenAnswer {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<f64>,
}

#[derive(Deserialize)]
struct Registered {
    client_id: String,
}

fn ok(status: u16) -> bool {
    (200..300).contains(&status)
}

fn answered(status: u16) -> String {
    format!("Notion's sign-in answered {status}.")
}

fn base64_url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// Bytes from the OS's random source (through uuid's v4, which reads it).
fn random_bytes(count: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(count + 16);
    while out.len() < count {
        out.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
    }
    out.truncate(count);
    out
}

fn form(pairs: &[(&str, &str)]) -> String {
    url::form_urlencoded::Serializer::new(String::new()).extend_pairs(pairs).finish()
}

pub struct NotionSignIn<D: SignInDeps> {
    deps: D,
    pending: Mutex<Option<Arc<AtomicBool>>>,
    /// The last refresh's answer; the lock is what makes calls share one.
    refreshed: Mutex<Option<Result<Option<String>, String>>>,
    refresh_generation: AtomicU64,
}

impl<D: SignInDeps> NotionSignIn<D> {
    pub fn new(deps: D) -> Self {
        NotionSignIn {
            deps,
            pending: Mutex::new(None),
            refreshed: Mutex::new(None),
            refresh_generation: AtomicU64::new(0),
        }
    }

    fn metadata(&self) -> Result<AuthServerMetadata, String> {
        let base = self.deps.base_url();
        let (status, body) = self.deps.fetch(HttpRequest {
            url: &format!("{base}/.well-known/oauth-authorization-server"),
            method: "GET",
            content_type: None,
            body: None,
        })?;
        if !ok(status) {
            return Err(answered(status));
        }
        let server: AuthServerMetadata = serde_json::from_str(&body).map_err(|e| e.to_string())?;
        // Every endpoint must be the service's own: the code, the verifier and
        // the refresh token go to them.
        let ours = |endpoint: &str| crate::imports::notion_http::same_origin(&base, endpoint);
        if !ours(&server.authorization_endpoint) || !ours(&server.token_endpoint) || !ours(&server.registration_endpoint) {
            return Err("Notion's sign-in pointed somewhere else.".to_string());
        }
        Ok(server)
    }

    fn read_session(&self) -> Option<NotionSession> {
        serde_json::from_str(&self.deps.load()?).ok()
    }

    fn write_session(&self, session: &NotionSession) -> Result<(), String> {
        let raw = serde_json::to_string(session).map_err(|e| e.to_string())?;
        if !self.deps.save(&raw) {
            return Err("This system has no secure storage to keep a sign-in in.".to_string());
        }
        Ok(())
    }

    fn from_token_answer(&self, client_id: &str, answer: TokenAnswer, previous_refresh: Option<String>) -> NotionSession {
        NotionSession {
            client_id: client_id.to_string(),
            access_token: answer.access_token,
            refresh_token: answer.refresh_token.or(previous_refresh),
            expires_at: answer
                .expires_in
                .filter(|s| *s > 0.0)
                .map(|s| self.deps.now() + (s * 1000.0) as i64),
        }
    }

    pub fn signed_in(&self) -> bool {
        self.read_session().is_some()
    }

    /// Returns once Notion has answered and the session is stored.
    pub fn sign_in(&self) -> Result<(), String> {
        let server = self.metadata()?;
        let state = base64_url(&random_bytes(24));
        let verifier = base64_url(&random_bytes(48));
        let challenge = base64_url(&Sha256::digest(verifier.as_bytes()));
        let cancel = Arc::new(AtomicBool::new(false));
        let redirect = Redirect::listen(&state, cancel.clone())?;
        *self.pending.lock().unwrap_or_else(|e| e.into_inner()) = Some(cancel);
        let result = self.finish_sign_in(&server, &redirect, &state, &verifier, &challenge);
        *self.pending.lock().unwrap_or_else(|e| e.into_inner()) = None;
        result
    }

    fn finish_sign_in(
        &self,
        server: &AuthServerMetadata,
        redirect: &Redirect,
        state: &str,
        verifier: &str,
        challenge: &str,
    ) -> Result<(), String> {
        let base = self.deps.base_url();
        let redirect_uri = redirect.redirect_uri();
        let registration = serde_json::json!({
            "client_name": "ZenNotes-rs",
            "redirect_uris": [redirect_uri],
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
            "token_endpoint_auth_method": "none"
        })
        .to_string();
        let (status, body) = self.deps.fetch(HttpRequest {
            url: &server.registration_endpoint,
            method: "POST",
            content_type: Some("application/json"),
            body: Some(&registration),
        })?;
        if !ok(status) {
            return Err(answered(status));
        }
        let Registered { client_id } = serde_json::from_str(&body).map_err(|e| e.to_string())?;

        let resource = format!("{base}/mcp");
        let mut authorize = Url::parse(&server.authorization_endpoint).map_err(|e| e.to_string())?;
        authorize.set_query(Some(&form(&[
            ("response_type", "code"),
            ("client_id", &client_id),
            ("redirect_uri", &redirect_uri),
            ("state", state),
            ("code_challenge", challenge),
            ("code_challenge_method", "S256"),
            ("resource", &resource),
        ])));
        redirect.check_cancelled()?;
        self.deps.open_external(authorize.as_str())?;

        let code = redirect.wait()?;
        let exchange = form(&[
            ("grant_type", "authorization_code"),
            ("code", &code),
            ("redirect_uri", &redirect_uri),
            ("client_id", &client_id),
            ("code_verifier", verifier),
            ("resource", &resource),
        ]);
        let (status, body) = self.deps.fetch(HttpRequest {
            url: &server.token_endpoint,
            method: "POST",
            content_type: Some("application/x-www-form-urlencoded"),
            body: Some(&exchange),
        })?;
        if !ok(status) {
            return Err(answered(status));
        }
        let answer: TokenAnswer = serde_json::from_str(&body).map_err(|e| e.to_string())?;
        self.write_session(&self.from_token_answer(&client_id, answer, None))
    }

    pub fn cancel_sign_in(&self) {
        if let Some(cancel) = self.pending.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            cancel.store(true, Ordering::SeqCst);
        }
    }

    /// Swaps the refresh token for new ones; Notion rotates both. None means
    /// the person has to sign in again. Calls that arrive while one is in
    /// flight share its answer, since a rotated token only works once.
    pub fn refresh(&self) -> Result<Option<String>, String> {
        let seen = self.refresh_generation.load(Ordering::SeqCst);
        let mut last = self.refreshed.lock().unwrap_or_else(|e| e.into_inner());
        if self.refresh_generation.load(Ordering::SeqCst) != seen {
            if let Some(answer) = last.clone() {
                return answer;
            }
        }
        let answer = self.refresh_now();
        *last = Some(answer.clone());
        self.refresh_generation.fetch_add(1, Ordering::SeqCst);
        answer
    }

    fn refresh_now(&self) -> Result<Option<String>, String> {
        let Some(session) = self.read_session() else { return Ok(None) };
        let Some(refresh_token) = session.refresh_token.clone() else { return Ok(None) };
        let server = self.metadata()?;
        let (status, body) = self.deps.fetch(HttpRequest {
            url: &server.token_endpoint,
            method: "POST",
            content_type: Some("application/x-www-form-urlencoded"),
            body: Some(&form(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", &refresh_token),
                ("client_id", &session.client_id),
            ])),
        })?;
        if !ok(status) {
            return Ok(None);
        }
        let answer: TokenAnswer = serde_json::from_str(&body).map_err(|e| e.to_string())?;
        let next = self.from_token_answer(&session.client_id, answer, Some(refresh_token));
        self.write_session(&next)?;
        Ok(Some(next.access_token))
    }

    pub fn access_token(&self) -> Result<String, String> {
        let session = self.read_session().ok_or("Sign in to Notion first.")?;
        if session.expires_at.is_some_and(|at| at - self.deps.now() < EXPIRY_MARGIN_MS) {
            if let Some(fresh) = self.refresh()? {
                return Ok(fresh);
            }
        }
        Ok(session.access_token)
    }

    pub fn sign_out(&self) {
        self.deps.remove();
    }
}

/// The redirect, on a port the system picked. Refuses anything without the
/// state this sign-in made, and keeps listening after it.
struct Redirect {
    listener: TcpListener,
    port: u16,
    state: String,
    cancel: Arc<AtomicBool>,
    deadline: Instant,
}

impl Redirect {
    fn listen(state: &str, cancel: Arc<AtomicBool>) -> Result<Redirect, String> {
        let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        Ok(Redirect { listener, port, state: state.to_string(), cancel, deadline: Instant::now() + SIGN_IN_TIMEOUT })
    }

    fn redirect_uri(&self) -> String {
        format!("http://127.0.0.1:{}/callback", self.port)
    }

    fn check_cancelled(&self) -> Result<(), String> {
        if self.cancel.load(Ordering::SeqCst) {
            return Err("Sign-in cancelled.".to_string());
        }
        Ok(())
    }

    fn wait(&self) -> Result<String, String> {
        loop {
            self.check_cancelled()?;
            if Instant::now() >= self.deadline {
                return Err("Sign-in timed out. Try again.".to_string());
            }
            match self.listener.accept() {
                Ok((stream, _)) => {
                    if let Some(outcome) = self.answer(stream) {
                        return outcome;
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(error) => return Err(error.to_string()),
            }
        }
    }

    /// One request to the loopback port. None: not the redirect, keep waiting.
    fn answer(&self, mut stream: TcpStream) -> Option<Result<String, String>> {
        let _ = stream.set_nonblocking(false);
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        let mut head = Vec::new();
        let mut buf = [0u8; 2048];
        while !head.windows(4).any(|w| w == b"\r\n\r\n") && head.len() < 16 * 1024 {
            match stream.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => head.extend_from_slice(&buf[..n]),
            }
        }
        let head = String::from_utf8_lossy(&head);
        let target = head.lines().next().and_then(|line| line.split(' ').nth(1)).unwrap_or("");
        let respond = |stream: &mut TcpStream, status: &str, content_type: &str, body: &str| {
            let _ = write!(
                stream,
                "HTTP/1.1 {status}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.flush();
        };
        let url = match target.starts_with('/').then(|| Url::parse(&format!("http://127.0.0.1{target}"))) {
            Some(Ok(url)) if url.path() == "/callback" => url,
            _ => {
                respond(&mut stream, "404 Not Found", "text/plain", "");
                return None;
            }
        };
        let query: HashMap<String, String> = url.query_pairs().into_owned().collect();
        if query.get("state") != Some(&self.state) {
            respond(&mut stream, "400 Bad Request", "text/plain", "Sign-in state did not match.");
            return None;
        }
        respond(&mut stream, "200 OK", "text/html; charset=utf-8", CALLBACK_PAGE);
        Some(match query.get("code") {
            Some(code) if !code.is_empty() => Ok(code.clone()),
            _ => Err(query
                .get("error_description")
                .cloned()
                .unwrap_or_else(|| "Notion did not grant access.".to_string())),
        })
    }
}

// ---- the app's own sign-in and credentials ----------------------------------

const TOKEN_ACCOUNT: &str = "notion:token";
const SESSION_ACCOUNT: &str = "notion:session";

/// What the keychain would not take, for this run only. Also a cache of the
/// session, so an import doesn't ask the keychain before every request.
static MEMORY: LazyLock<Mutex<HashMap<&'static str, Option<String>>>> = LazyLock::new(Default::default);

fn secret_get(account: &'static str) -> Option<String> {
    if let Some(known) = MEMORY.lock().unwrap_or_else(|e| e.into_inner()).get(account) {
        return known.clone();
    }
    // Only an answer is kept: a keychain that was locked a moment ago may
    // answer the next time.
    let value = secrets::keychain_get(account)?;
    MEMORY.lock().unwrap_or_else(|e| e.into_inner()).insert(account, Some(value.clone()));
    Some(value)
}

/// Always keeps it: in the keychain, or for this run when there is none.
fn secret_set(account: &'static str, value: &str) -> bool {
    if !secrets::keychain_set(account, value) {
        eprintln!("[notion] no OS keychain took the Notion credential; it is kept for this run only");
    }
    MEMORY.lock().unwrap_or_else(|e| e.into_inner()).insert(account, Some(value.to_string()));
    true
}

fn secret_delete(account: &'static str) {
    secrets::keychain_delete(account);
    MEMORY.lock().unwrap_or_else(|e| e.into_inner()).insert(account, None);
}

pub fn can_store() -> bool {
    secrets::keychain_available(TOKEN_ACCOUNT)
}

pub fn saved_token() -> Option<String> {
    secret_get(TOKEN_ACCOUNT)
}

pub fn token_is_set() -> bool {
    saved_token().is_some_and(|t| !t.is_empty())
}

pub fn set_token(token: &str) -> bool {
    let token = token.trim();
    !token.is_empty() && secret_set(TOKEN_ACCOUNT, token)
}

pub fn delete_token() {
    secret_delete(TOKEN_ACCOUNT);
}

struct HostDeps;

impl SignInDeps for HostDeps {
    fn base_url(&self) -> String {
        crate::imports::notion_http::mcp_base_url()
    }

    fn fetch(&self, request: HttpRequest<'_>) -> Result<(u16, String), String> {
        crate::imports::notion_http::sign_in_request(
            &self.base_url(),
            request.url,
            request.method,
            request.content_type,
            request.body,
        )
    }

    fn open_external(&self, url: &str) -> Result<(), String> {
        tauri_plugin_opener::open_url(url, None::<&str>).map_err(|e| e.to_string())
    }

    fn load(&self) -> Option<String> {
        secret_get(SESSION_ACCOUNT)
    }

    fn save(&self, value: &str) -> bool {
        secret_set(SESSION_ACCOUNT, value)
    }

    fn remove(&self) {
        secret_delete(SESSION_ACCOUNT)
    }

    fn now(&self) -> i64 {
        SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
    }
}

static SIGN_IN: LazyLock<NotionSignIn<HostDeps>> = LazyLock::new(|| NotionSignIn::new(HostDeps));

pub fn signed_in() -> bool {
    SIGN_IN.signed_in()
}

pub fn sign_in() -> Result<(), String> {
    SIGN_IN.sign_in()
}

pub fn cancel_sign_in() {
    SIGN_IN.cancel_sign_in()
}

pub fn refresh() -> Result<Option<String>, String> {
    SIGN_IN.refresh()
}

pub fn access_token() -> Result<String, String> {
    SIGN_IN.access_token()
}

/// Upstream's `notionImport.signOut`: the session and the token both go.
pub fn sign_out() {
    SIGN_IN.sign_out();
    delete_token();
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "https://notion.test";

    #[derive(Default)]
    struct Fake {
        stored: Mutex<Option<String>>,
        requests: Mutex<Vec<(String, String)>>,
        authorize: Mutex<String>,
        token_count: AtomicU64,
        token_status: Option<u16>,
        time: AtomicU64,
        slow_token: bool,
    }

    impl SignInDeps for Arc<Fake> {
        fn base_url(&self) -> String {
            BASE.to_string()
        }

        fn fetch(&self, request: HttpRequest<'_>) -> Result<(u16, String), String> {
            self.requests.lock().unwrap().push((request.url.to_string(), request.body.unwrap_or("").to_string()));
            if request.url.ends_with("/.well-known/oauth-authorization-server") {
                let body = serde_json::json!({
                    "authorization_endpoint": format!("{BASE}/authorize"),
                    "token_endpoint": format!("{BASE}/token"),
                    "registration_endpoint": format!("{BASE}/register"),
                });
                return Ok((200, body.to_string()));
            }
            if request.url == format!("{BASE}/register") {
                return Ok((200, r#"{"client_id":"client-1"}"#.to_string()));
            }
            if request.url == format!("{BASE}/token") {
                let n = self.token_count.fetch_add(1, Ordering::SeqCst) + 1;
                if self.slow_token {
                    std::thread::sleep(Duration::from_millis(150));
                }
                if let Some(status) = self.token_status {
                    return Ok((status, "no".to_string()));
                }
                let body = serde_json::json!({
                    "access_token": format!("access-{n}"),
                    "refresh_token": format!("refresh-{n}"),
                    "expires_in": 3600
                });
                return Ok((200, body.to_string()));
            }
            Ok((404, "?".to_string()))
        }

        fn open_external(&self, url: &str) -> Result<(), String> {
            *self.authorize.lock().unwrap() = url.to_string();
            Ok(())
        }

        fn load(&self) -> Option<String> {
            self.stored.lock().unwrap().clone()
        }

        fn save(&self, value: &str) -> bool {
            *self.stored.lock().unwrap() = Some(value.to_string());
            true
        }

        fn remove(&self) {
            *self.stored.lock().unwrap() = None;
        }

        fn now(&self) -> i64 {
            1_000_000 + self.time.load(Ordering::SeqCst) as i64
        }
    }

    struct Harness {
        fake: Arc<Fake>,
        sign_in: Arc<NotionSignIn<Arc<Fake>>>,
    }

    fn harness(fake: Fake) -> Harness {
        let fake = Arc::new(fake);
        Harness { sign_in: Arc::new(NotionSignIn::new(fake.clone())), fake }
    }

    impl Harness {
        fn start(&self) -> std::thread::JoinHandle<Result<(), String>> {
            let sign_in = self.sign_in.clone();
            std::thread::spawn(move || sign_in.sign_in())
        }

        fn authorize(&self) -> Url {
            for _ in 0..400 {
                let url = self.fake.authorize.lock().unwrap().clone();
                if !url.is_empty() {
                    return Url::parse(&url).unwrap();
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            panic!("the browser was never opened");
        }

        fn param(url: &Url, name: &str) -> Option<String> {
            url.query_pairs().find(|(k, _)| k == name).map(|(_, v)| v.into_owned())
        }

        /// Plays the browser coming back to the loopback port.
        fn approve(&self, query: Option<Vec<(&str, String)>>) -> (u16, String) {
            let auth = self.authorize();
            let redirect = Url::parse(&Self::param(&auth, "redirect_uri").unwrap()).unwrap();
            let query = query.unwrap_or_else(|| {
                vec![("code", "the-code".to_string()), ("state", Self::param(&auth, "state").unwrap())]
            });
            let pairs: Vec<(&str, &str)> = query.iter().map(|(k, v)| (*k, v.as_str())).collect();
            let mut stream = TcpStream::connect(("127.0.0.1", redirect.port().unwrap())).unwrap();
            write!(stream, "GET /callback?{} HTTP/1.1\r\nhost: 127.0.0.1\r\n\r\n", form(&pairs)).unwrap();
            let mut answer = String::new();
            stream.read_to_string(&mut answer).unwrap();
            let status = answer.split(' ').nth(1).unwrap().parse().unwrap();
            let body = answer.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
            (status, body)
        }

        fn token_bodies(&self) -> Vec<String> {
            let requests = self.fake.requests.lock().unwrap();
            requests.iter().filter(|(u, _)| u == &format!("{BASE}/token")).map(|(_, b)| b.clone()).collect()
        }

        fn signed_in(&self) {
            let done = self.start();
            self.approve(None);
            done.join().unwrap().unwrap();
        }
    }

    fn pairs(body: &str) -> HashMap<String, String> {
        url::form_urlencoded::parse(body.as_bytes()).into_owned().collect()
    }

    #[test]
    fn runs_the_flow_with_pkce_on_a_loopback_redirect_and_stores_the_session() {
        let h = harness(Fake::default());
        let done = h.start();
        let (status, page) = h.approve(None);
        assert_eq!(status, 200);
        assert!(page.contains("close this tab"));
        done.join().unwrap().unwrap();

        let auth = h.authorize();
        assert_eq!(Harness::param(&auth, "code_challenge_method").as_deref(), Some("S256"));
        let redirect = Harness::param(&auth, "redirect_uri").unwrap();
        assert!(redirect.starts_with("http://127.0.0.1:") && redirect.ends_with("/callback"));
        assert_eq!(Harness::param(&auth, "resource").unwrap(), format!("{BASE}/mcp"));

        let exchange = pairs(&h.token_bodies()[0]);
        assert_eq!(exchange.get("code").map(String::as_str), Some("the-code"));
        let verifier = exchange.get("code_verifier").unwrap();
        assert!(!verifier.is_empty());
        assert!(!exchange.contains_key("client_secret"));
        // The challenge the browser saw is the verifier's hash.
        assert_eq!(
            Harness::param(&auth, "code_challenge").unwrap(),
            base64_url(&Sha256::digest(verifier.as_bytes()))
        );

        let stored: NotionSession = serde_json::from_str(&h.fake.stored.lock().unwrap().clone().unwrap()).unwrap();
        assert_eq!(stored.client_id, "client-1");
        assert_eq!(stored.access_token, "access-1");
        assert_eq!(stored.refresh_token.as_deref(), Some("refresh-1"));
        assert!(h.sign_in.signed_in());
    }

    #[test]
    fn ignores_a_redirect_carrying_the_wrong_state() {
        let h = harness(Fake::default());
        let done = h.start();
        let (status, _) = h.approve(Some(vec![("code", "stolen".into()), ("state", "nope".into())]));
        assert_eq!(status, 400);
        h.approve(None);
        done.join().unwrap().unwrap();
        assert_eq!(pairs(&h.token_bodies()[0]).get("code").map(String::as_str), Some("the-code"));
    }

    #[test]
    fn reports_a_refusal_and_stores_nothing() {
        let h = harness(Fake::default());
        let done = h.start();
        let state = Harness::param(&h.authorize(), "state").unwrap();
        h.approve(Some(vec![
            ("error", "access_denied".into()),
            ("error_description", "nope".into()),
            ("state", state),
        ]));
        assert_eq!(done.join().unwrap().unwrap_err(), "nope");
        assert!(h.fake.stored.lock().unwrap().is_none());
    }

    #[test]
    fn a_refused_exchange_stores_nothing() {
        let h = harness(Fake { token_status: Some(400), ..Default::default() });
        let done = h.start();
        h.approve(None);
        assert_eq!(done.join().unwrap().unwrap_err(), "Notion's sign-in answered 400.");
        assert!(h.fake.stored.lock().unwrap().is_none());
    }

    #[test]
    fn can_be_cancelled_while_it_waits() {
        let h = harness(Fake::default());
        let done = h.start();
        h.authorize();
        h.sign_in.cancel_sign_in();
        assert!(done.join().unwrap().unwrap_err().contains("cancelled"));
    }

    #[test]
    fn refreshes_a_token_about_to_expire_and_keeps_the_rotated_one() {
        let h = harness(Fake::default());
        h.signed_in();
        assert_eq!(h.sign_in.access_token().unwrap(), "access-1");
        h.fake.time.store(3_600_000, Ordering::SeqCst);
        assert_eq!(h.sign_in.access_token().unwrap(), "access-2");
        let stored: NotionSession = serde_json::from_str(&h.fake.stored.lock().unwrap().clone().unwrap()).unwrap();
        assert_eq!(stored.refresh_token.as_deref(), Some("refresh-2"));
        let refresh = pairs(&h.token_bodies()[1]);
        assert_eq!(refresh.get("grant_type").map(String::as_str), Some("refresh_token"));
        assert_eq!(refresh.get("refresh_token").map(String::as_str), Some("refresh-1"));
    }

    #[test]
    fn shares_one_refresh_between_calls_since_a_rotated_token_works_once() {
        let h = harness(Fake { slow_token: true, ..Default::default() });
        h.signed_in();
        let a = {
            let sign_in = h.sign_in.clone();
            std::thread::spawn(move || sign_in.refresh())
        };
        std::thread::sleep(Duration::from_millis(30));
        let b = h.sign_in.refresh();
        let a = a.join().unwrap();
        assert_eq!(a, b);
        assert_eq!(h.token_bodies().len(), 2);
    }

    #[test]
    fn forgets_the_session_on_sign_out() {
        let h = harness(Fake::default());
        h.signed_in();
        h.sign_in.sign_out();
        assert!(!h.sign_in.signed_in());
        assert!(h.sign_in.access_token().unwrap_err().contains("Sign in"));
    }

    #[test]
    fn refuses_endpoints_off_the_service_s_own_origin() {
        struct Elsewhere;
        impl SignInDeps for Elsewhere {
            fn base_url(&self) -> String {
                BASE.to_string()
            }
            fn fetch(&self, _: HttpRequest<'_>) -> Result<(u16, String), String> {
                let body = serde_json::json!({
                    "authorization_endpoint": format!("{BASE}/authorize"),
                    "token_endpoint": "https://evil.test/token",
                    "registration_endpoint": format!("{BASE}/register"),
                });
                Ok((200, body.to_string()))
            }
            fn open_external(&self, _: &str) -> Result<(), String> {
                panic!("nothing should open")
            }
            fn load(&self) -> Option<String> {
                None
            }
            fn save(&self, _: &str) -> bool {
                true
            }
            fn remove(&self) {}
            fn now(&self) -> i64 {
                0
            }
        }
        let err = NotionSignIn::new(Elsewhere).sign_in().unwrap_err();
        assert_eq!(err, "Notion's sign-in pointed somewhere else.");
    }

    #[test]
    fn the_session_is_upstream_s_json() {
        let session = NotionSession {
            client_id: "c".into(),
            access_token: "a".into(),
            refresh_token: Some("r".into()),
            expires_at: Some(5),
        };
        assert_eq!(
            serde_json::to_string(&session).unwrap(),
            r#"{"clientId":"c","accessToken":"a","refreshToken":"r","expiresAt":5}"#
        );
        let bare: NotionSession = serde_json::from_str(r#"{"clientId":"c","accessToken":"a"}"#).unwrap();
        assert_eq!(bare.refresh_token, None);
    }
}
