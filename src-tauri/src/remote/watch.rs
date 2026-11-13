//! The live change feed from a ZenNotes server: port of
//! `RemoteServerClient.watchVaultChanges` (apps/desktop/src/main/remote/
//! server-client.ts), on a thread of its own with a blocking WebSocket.
//!
//! The rules are upstream's:
//! - every text frame is one `VaultChangeEvent`; a malformed one is ignored;
//! - after a close, reconnect in `min(30 s, 1 s · 2^failures)`; the failure
//!   count resets only once a socket has stayed up for 15 s, so a peer that
//!   accepts and drops at once backs off instead of re-pulling every second;
//! - while the socket is down, ask for a resync every 30 s (#734: a proxy that
//!   never forwards the upgrade leaves the feed unavailable, not briefly down);
//! - a socket that opens after a gap asks for a resync, since what changed in
//!   the gap is gone.
//!
//! One difference: upstream builds the URL with `new URL('/api/watch', base)`,
//! which drops a base path like `/notes`; here the base path is kept, as every
//! other request keeps it.

use std::io::ErrorKind;
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::Value;
use tungstenite::client::IntoClientRequest;
use tungstenite::http::HeaderValue;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

use super::tls;

/// What the feed tells its owner.
#[derive(Debug, Clone, PartialEq)]
pub enum FeedEvent {
    /// A change event, passed through as the server sent it.
    Change(Value),
    /// Re-pull everything: the feed missed events, or is down.
    Resync,
}

#[derive(Debug, Clone)]
pub struct FeedTimings {
    pub stable_after: Duration,
    pub poll_while_down: Duration,
    pub base_delay: Duration,
    pub max_delay: Duration,
    /// How often a blocked read wakes to check for a stop.
    pub tick: Duration,
    pub connect_timeout: Duration,
}

impl Default for FeedTimings {
    fn default() -> Self {
        FeedTimings {
            stable_after: Duration::from_secs(15),
            // REMOTE_CHANGE_POLL_MS in shared-domain/remote-workspace-messages.ts.
            poll_while_down: Duration::from_secs(30),
            base_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(30),
            tick: Duration::from_millis(250),
            connect_timeout: Duration::from_secs(15),
        }
    }
}

/// Stops the feed when dropped. The thread notices within one tick and ends;
/// the app never waits for it.
pub struct FeedHandle {
    stop: Arc<AtomicBool>,
    #[cfg_attr(not(test), allow(dead_code))]
    thread: Option<JoinHandle<()>>,
}

impl FeedHandle {
    /// Stop and wait for the thread to finish.
    #[cfg(test)]
    pub fn stop_and_join(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for FeedHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

/// `ws(s)://<base>/api/watch` for an `http(s)://<base>` server URL.
pub fn watch_url(base_url: &str) -> Result<url::Url, String> {
    let mut url = url::Url::parse(&format!("{}/api/watch", base_url.trim_end_matches('/')))
        .map_err(|e| format!("Bad server URL {base_url}: {e}"))?;
    let scheme = match url.scheme() {
        "http" => "ws",
        "https" => "wss",
        other => return Err(format!("Bad server URL scheme: {other}")),
    };
    url.set_scheme(scheme).map_err(|_| "Bad server URL".to_string())?;
    Ok(url)
}

type Socket = WebSocket<MaybeTlsStream<TcpStream>>;

fn connect(url: &url::Url, token: Option<&str>, timings: &FeedTimings) -> Result<Socket, String> {
    let host = url.host_str().ok_or("no host")?.to_string();
    let port = url.port_or_known_default().ok_or("no port")?;
    let addrs: Vec<_> = (host.as_str(), port)
        .to_socket_addrs()
        .map_err(|e| e.to_string())?
        .collect();
    let mut last_err = String::from("no address");
    let mut tcp = None;
    for addr in addrs {
        match TcpStream::connect_timeout(&addr, timings.connect_timeout) {
            Ok(stream) => {
                tcp = Some(stream);
                break;
            }
            Err(e) => last_err = e.to_string(),
        }
    }
    let tcp = tcp.ok_or(last_err)?;
    // Generous during the handshake; shortened once open so a stop is noticed.
    tcp.set_read_timeout(Some(timings.connect_timeout)).map_err(|e| e.to_string())?;
    tcp.set_write_timeout(Some(timings.connect_timeout)).map_err(|e| e.to_string())?;
    let _ = tcp.set_nodelay(true);

    let mut request = url.as_str().into_client_request().map_err(|e| e.to_string())?;
    if let Some(token) = token {
        let value = HeaderValue::from_str(&format!("Bearer {token}")).map_err(|e| e.to_string())?;
        request.headers_mut().insert("Authorization", value);
    }
    let connector = match url.scheme() {
        "wss" => Some(tungstenite::Connector::Rustls(tls::client_config()?)),
        _ => Some(tungstenite::Connector::Plain),
    };
    let (socket, _response) = tungstenite::client_tls_with_config(request, tcp, None, connector)
        .map_err(|e| e.to_string())?;
    let stream = match socket.get_ref() {
        MaybeTlsStream::Plain(s) => s,
        MaybeTlsStream::Rustls(s) => s.get_ref(),
        _ => return Err("unexpected stream".into()),
    };
    stream.set_read_timeout(Some(timings.tick)).map_err(|e| e.to_string())?;
    Ok(socket)
}

fn decode(message: Message) -> Option<Value> {
    let text = match message {
        Message::Text(text) => text.as_str().to_string(),
        Message::Binary(bytes) => String::from_utf8(bytes.to_vec()).ok()?,
        _ => return None,
    };
    serde_json::from_str(&text).ok()
}

/// Start following `base_url`'s change feed. `sink` is called on the feed's
/// thread.
pub fn spawn(
    base_url: &str,
    token: Option<&str>,
    timings: FeedTimings,
    sink: impl Fn(FeedEvent) + Send + 'static,
) -> Result<FeedHandle, String> {
    let url = watch_url(base_url)?;
    let token = token.map(str::to_string);
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = stop.clone();
    let base = base_url.to_string();
    let thread = std::thread::Builder::new()
        .name("zennotes-remote-feed".into())
        .spawn(move || run(&base, &url, token.as_deref(), &timings, &thread_stop, &sink))
        .map_err(|e| e.to_string())?;
    Ok(FeedHandle { stop, thread: Some(thread) })
}

fn run(
    base_url: &str,
    url: &url::Url,
    token: Option<&str>,
    timings: &FeedTimings,
    stop: &AtomicBool,
    sink: &dyn Fn(FeedEvent),
) {
    let stopped = || stop.load(Ordering::SeqCst);
    let mut failed_attempts: u32 = 0;
    // When polling is on: the next time a resync is due.
    let mut next_poll: Option<Instant> = None;
    let mut warned_about_polling = false;

    while !stopped() {
        if let Ok(mut socket) = connect(url, token, timings) {
            if stopped() {
                let _ = socket.close(None);
                return;
            }
            let had_gap = failed_attempts > 0;
            next_poll = None;
            let opened = Instant::now();
            let mut stable = false;
            if had_gap {
                sink(FeedEvent::Resync);
            }
            loop {
                if stopped() {
                    let _ = socket.close(None);
                    let _ = socket.flush();
                    return;
                }
                if !stable && opened.elapsed() >= timings.stable_after {
                    stable = true;
                    failed_attempts = 0;
                }
                match socket.read() {
                    Ok(Message::Close(_)) => break,
                    Ok(message) => {
                        if let Some(event) = decode(message) {
                            sink(FeedEvent::Change(event));
                        }
                    }
                    Err(tungstenite::Error::Io(e))
                        if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
                    Err(_) => break,
                }
            }
        }
        if stopped() {
            return;
        }
        // Closed or never opened: back off, and poll while down.
        let delay = timings
            .base_delay
            .saturating_mul(2u32.saturating_pow(failed_attempts.min(16)))
            .min(timings.max_delay);
        failed_attempts = failed_attempts.saturating_add(1);
        if next_poll.is_none() {
            if !warned_about_polling {
                warned_about_polling = true;
                eprintln!(
                    "[remote] {base_url}: the change feed at /api/watch is not staying connected (a proxy without WebSocket support?); refreshing every {}s instead",
                    timings.poll_while_down.as_secs()
                );
            }
            next_poll = Some(Instant::now() + timings.poll_while_down);
        }
        let reconnect_at = Instant::now() + delay;
        while Instant::now() < reconnect_at {
            if stopped() {
                return;
            }
            if let Some(due) = next_poll {
                if Instant::now() >= due {
                    sink(FeedEvent::Resync);
                    next_poll = Some(due + timings.poll_while_down);
                }
            }
            std::thread::sleep(timings.tick.min(reconnect_at.saturating_duration_since(Instant::now())));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::sync::mpsc;

    fn fast() -> FeedTimings {
        FeedTimings {
            stable_after: Duration::from_millis(400),
            poll_while_down: Duration::from_millis(300),
            base_delay: Duration::from_millis(50),
            max_delay: Duration::from_millis(400),
            tick: Duration::from_millis(20),
            connect_timeout: Duration::from_secs(2),
        }
    }

    fn recv(rx: &mpsc::Receiver<FeedEvent>) -> FeedEvent {
        rx.recv_timeout(Duration::from_secs(5)).expect("no feed event in time")
    }

    fn feed(base: &str, token: Option<&str>, timings: FeedTimings) -> (FeedHandle, mpsc::Receiver<FeedEvent>) {
        let (tx, rx) = mpsc::channel();
        let handle = spawn(base, token, timings, move |ev| {
            let _ = tx.send(ev);
        })
        .unwrap();
        (handle, rx)
    }

    /// Accept one socket on `listener`, checking the token, and hand it back.
    fn accept(listener: &TcpListener, want_token: Option<&str>) -> WebSocket<TcpStream> {
        let (stream, _) = listener.accept().unwrap();
        let want = want_token.map(|t| format!("Bearer {t}"));
        let callback = move |req: &tungstenite::handshake::server::Request,
                             res: tungstenite::handshake::server::Response| {
            assert_eq!(req.uri().path(), "/base/api/watch");
            let got = req.headers().get("authorization").and_then(|v| v.to_str().ok()).map(str::to_string);
            assert_eq!(got, want);
            Ok(res)
        };
        tungstenite::accept_hdr(stream, callback).unwrap()
    }

    #[test]
    fn urls_keep_the_base_path_and_swap_the_scheme() {
        assert_eq!(watch_url("http://h:7878").unwrap().as_str(), "ws://h:7878/api/watch");
        assert_eq!(watch_url("https://h/notes").unwrap().as_str(), "wss://h/notes/api/watch");
        assert!(watch_url("ftp://h").is_err());
    }

    #[test]
    fn events_flow_and_a_restart_resubscribes_with_a_resync() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}/base", listener.local_addr().unwrap());
        let (handle, rx) = feed(&base, Some("tok"), fast());

        let mut ws = accept(&listener, Some("tok"));
        ws.send(Message::text(r#"{"kind":"change","path":"inbox/a.md","folder":"inbox"}"#)).unwrap();
        ws.send(Message::text("not json")).unwrap();
        ws.send(Message::text(r#"{"kind":"add","path":"inbox/b.md","folder":"inbox"}"#)).unwrap();
        assert_eq!(recv(&rx), FeedEvent::Change(serde_json::json!({"kind":"change","path":"inbox/a.md","folder":"inbox"})));
        // The malformed frame was skipped.
        assert_eq!(recv(&rx), FeedEvent::Change(serde_json::json!({"kind":"add","path":"inbox/b.md","folder":"inbox"})));

        // The server goes away and comes back: the feed reconnects and says
        // what it missed has to be re-pulled.
        drop(ws);
        let mut ws = accept(&listener, Some("tok"));
        assert_eq!(recv(&rx), FeedEvent::Resync);
        ws.send(Message::text(r#"{"kind":"unlink","path":"inbox/a.md","folder":"inbox"}"#)).unwrap();
        assert_eq!(recv(&rx), FeedEvent::Change(serde_json::json!({"kind":"unlink","path":"inbox/a.md","folder":"inbox"})));
        handle.stop_and_join();
    }

    #[test]
    fn a_peer_that_drops_at_once_is_backed_off() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}/base", listener.local_addr().unwrap());
        let mut timings = fast();
        timings.stable_after = Duration::from_secs(60);
        timings.poll_while_down = Duration::from_secs(60);
        let (handle, _rx) = feed(&base, None, timings);
        let mut accepted_at = Vec::new();
        for _ in 0..4 {
            let ws = accept(&listener, None);
            accepted_at.push(Instant::now());
            drop(ws);
        }
        let gaps: Vec<Duration> = accepted_at.windows(2).map(|w| w[1] - w[0]).collect();
        // 50 ms, 100 ms, 200 ms, give or take scheduling.
        assert!(gaps[0] >= Duration::from_millis(40), "{gaps:?}");
        assert!(gaps[2] >= Duration::from_millis(180), "{gaps:?}");
        assert!(gaps[2] > gaps[0], "{gaps:?}");
        handle.stop_and_join();
    }

    #[test]
    fn a_stable_socket_resets_the_backoff() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}/base", listener.local_addr().unwrap());
        let mut timings = fast();
        timings.stable_after = Duration::from_millis(150);
        timings.base_delay = Duration::from_millis(300);
        timings.max_delay = Duration::from_secs(5);
        timings.poll_while_down = Duration::from_secs(60);
        let (handle, _rx) = feed(&base, None, timings);
        // Fail twice so the next delay would be 1.2 s ...
        for _ in 0..3 {
            drop(accept(&listener, None));
        }
        // ... then stay up past stable_after: the count resets, so the next
        // reconnect comes after the base delay again.
        let ws = accept(&listener, None);
        std::thread::sleep(Duration::from_millis(400));
        let dropped = Instant::now();
        drop(ws);
        drop(accept(&listener, None));
        let gap = dropped.elapsed();
        assert!(gap < Duration::from_millis(900), "{gap:?}");
        handle.stop_and_join();
    }

    #[test]
    fn a_refused_upgrade_polls_until_stopped() {
        // A "proxy" answering every upgrade with a plain HTTP error.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}/base", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            use std::io::{Read, Write};
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf);
                let _ = stream.write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            }
        });
        let (handle, rx) = feed(&base, None, fast());
        assert_eq!(recv(&rx), FeedEvent::Resync);
        assert_eq!(recv(&rx), FeedEvent::Resync);
        handle.stop_and_join();
        // Stopped: nothing more arrives.
        assert!(rx.recv_timeout(Duration::from_millis(500)).is_err());
    }

    #[test]
    fn an_unreachable_server_neither_panics_nor_blocks_a_stop() {
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let (handle, rx) = feed(&format!("http://127.0.0.1:{port}"), None, fast());
        assert_eq!(recv(&rx), FeedEvent::Resync);
        let started = Instant::now();
        handle.stop_and_join();
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
