//! The host's half of the cover picker's Unsplash search: the request and the
//! key. Upstream's rules and client (`createUnsplashClient` in shared-domain
//! unsplash.ts) run unchanged in `src/bridge/unsplash.ts`; this side keeps what
//! upstream keeps out of the renderer. The Access Key lives only in the OS
//! keychain and is attached here, so nothing the renderer hears carries it, and
//! a request can go only to api.unsplash.com's three endpoints.

use std::io::Read;
use std::time::Duration;

use serde::Serialize;
use url::Url;

use crate::secrets;

const KEY_ACCOUNT: &str = "unsplash:access-key";
const API_HOST: &str = "api.unsplash.com";
const TIMEOUT: Duration = Duration::from_secs(10);
const MAX_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnsplashReply {
    pub status: u16,
    pub ratelimit_limit: Option<String>,
    pub ratelimit_remaining: Option<String>,
    pub body: Option<String>,
}

/// The only requests the renderer may ask for: search, the feed a key check
/// reads, and a photo's download ping.
fn is_allowed_request(url: &Url) -> bool {
    if url.scheme() != "https"
        || url.host_str() != Some(API_HOST)
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return false;
    }
    let path = url.path();
    if path == "/search/photos" || path == "/photos" {
        return true;
    }
    path.strip_prefix("/photos/")
        .and_then(|rest| rest.strip_suffix("/download"))
        .is_some_and(|id| {
            !id.is_empty()
                && id.len() <= 64
                && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        })
}

/// api.unsplash.com, or the stand-in `ZENNOTES_UNSPLASH_API_URL` names for a
/// test run. As upstream, a packaged app honors it only with `ZEN_PERF=1`.
fn api_base_override() -> Option<String> {
    if !cfg!(debug_assertions) && std::env::var("ZEN_PERF").as_deref() != Ok("1") {
        return None;
    }
    let value = std::env::var("ZENNOTES_UNSPLASH_API_URL").ok()?;
    let value = value.trim().trim_end_matches('/');
    (!value.is_empty()).then(|| value.to_string())
}

/// One GET. `key` is a pasted key being checked before it is saved; without
/// one the stored key is used. No cookies (ureq keeps none here), a bare user
/// agent, no redirects, so the key goes nowhere but the host checked above.
pub fn request(url: &str, key: Option<&str>) -> Result<UnsplashReply, String> {
    let parsed = Url::parse(url).map_err(|e| e.to_string())?;
    if !is_allowed_request(&parsed) {
        return Err("Not an Unsplash API request.".to_string());
    }
    let key = match key {
        Some(key) => key.to_string(),
        None => secrets::keychain_get(KEY_ACCOUNT).ok_or("No Unsplash key is set.")?,
    };
    let target = match api_base_override() {
        Some(base) => {
            let query = parsed.query().map(|q| format!("?{q}")).unwrap_or_default();
            format!("{base}{}{query}", parsed.path())
        }
        None => parsed.to_string(),
    };
    send(&target, &key)
}

fn send(target: &str, key: &str) -> Result<UnsplashReply, String> {
    let agent = ureq::AgentBuilder::new()
        .timeout(TIMEOUT)
        .redirects(0)
        .build();
    let outcome = agent
        .get(target)
        .set("accept", "application/json")
        .set("accept-version", "v1")
        .set("authorization", &format!("Client-ID {key}"))
        .set("user-agent", "ZenNotes")
        .set("accept-language", "*")
        .call();
    let response = match outcome {
        Ok(response) => response,
        Err(ureq::Error::Status(_, response)) => response,
        Err(err) => return Err(err.kind().to_string()),
    };
    let status = response.status();
    let ratelimit_limit = response.header("x-ratelimit-limit").map(str::to_string);
    let ratelimit_remaining = response.header("x-ratelimit-remaining").map(str::to_string);
    let mut body = String::new();
    let read = response
        .into_reader()
        .take(MAX_BYTES)
        .read_to_string(&mut body);
    Ok(UnsplashReply {
        status,
        ratelimit_limit,
        ratelimit_remaining,
        body: read.ok().map(|_| body),
    })
}

pub fn key_is_set() -> bool {
    secrets::keychain_get(KEY_ACCOUNT).is_some()
}

pub fn key_can_store() -> bool {
    secrets::keychain_available(KEY_ACCOUNT)
}

pub fn key_set(key: &str) -> bool {
    secrets::keychain_set(KEY_ACCOUNT, key.trim())
}

pub fn key_clear() {
    secrets::keychain_delete(KEY_ACCOUNT);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::net::TcpListener;

    /// Answers one request with `head` and returns what was asked.
    fn serve_once(head: &'static str) -> (String, std::thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut seen = Vec::new();
            let mut buf = [0u8; 1024];
            while !seen.windows(4).any(|w| w == b"\r\n\r\n") {
                let n = stream.read(&mut buf).unwrap();
                seen.extend_from_slice(&buf[..n]);
            }
            stream.write_all(head.as_bytes()).unwrap();
            String::from_utf8(seen).unwrap().to_lowercase()
        });
        (base, handle)
    }

    #[test]
    fn sends_the_key_and_a_bare_agent_and_passes_on_the_rate_limit() {
        let (base, server) = serve_once(
            "HTTP/1.1 200 OK\r\nx-ratelimit-limit: 50\r\nx-ratelimit-remaining: 42\r\ncontent-length: 2\r\nconnection: close\r\n\r\n[]",
        );
        let reply = send(&format!("{base}/photos?per_page=1"), "the-key").unwrap();
        assert_eq!(reply.status, 200);
        assert_eq!(reply.ratelimit_limit.as_deref(), Some("50"));
        assert_eq!(reply.ratelimit_remaining.as_deref(), Some("42"));
        assert_eq!(reply.body.as_deref(), Some("[]"));
        let seen = server.join().unwrap();
        assert!(seen.contains("authorization: client-id the-key\r\n"));
        assert!(seen.contains("user-agent: zennotes\r\n"));
        assert!(seen.contains("accept-version: v1\r\n"));
        assert!(!seen.contains("cookie"));
    }

    #[test]
    fn a_refusal_is_an_answer_and_a_redirect_is_not_followed() {
        let (base, server) = serve_once(
            "HTTP/1.1 401 Unauthorized\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
        );
        assert_eq!(send(&format!("{base}/photos"), "bad").unwrap().status, 401);
        server.join().unwrap();

        let (base, server) = serve_once(
            "HTTP/1.1 302 Found\r\nlocation: http://127.0.0.1:9/steal\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
        );
        assert_eq!(send(&format!("{base}/photos"), "key").unwrap().status, 302);
        server.join().unwrap();
    }

    #[test]
    fn nothing_leaves_for_a_request_that_is_not_allowed() {
        assert!(request("https://evil.test/photos", Some("key")).is_err());
    }

    fn allowed(url: &str) -> bool {
        is_allowed_request(&Url::parse(url).unwrap())
    }

    #[test]
    fn only_the_three_endpoints_on_the_api_host_are_allowed() {
        assert!(allowed("https://api.unsplash.com/search/photos?query=a+b&page=1"));
        assert!(allowed("https://api.unsplash.com/photos?per_page=1"));
        assert!(allowed("https://api.unsplash.com/photos/Ab_c-1/download?ixid=x"));

        assert!(!allowed("http://api.unsplash.com/photos"));
        assert!(!allowed("https://api.unsplash.com:444/photos"));
        assert!(!allowed("https://user:pw@api.unsplash.com/photos"));
        assert!(!allowed("https://api.unsplash.com.evil.test/photos"));
        assert!(!allowed("https://images.unsplash.com/photos"));
        assert!(!allowed("https://api.unsplash.com/me"));
        assert!(!allowed("https://api.unsplash.com/photos//download"));
        assert!(!allowed("https://api.unsplash.com/photos/a/b/download"));
        assert!(!allowed("https://api.unsplash.com/photos/a"));
    }
}
