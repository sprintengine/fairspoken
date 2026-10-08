//! HTTP plumbing for the transcription host: the listeners and the
//! accept loop, authentication, query and header parsing, body limits,
//! client addresses and JSON responses.

use super::*;

pub(super) const MAX_BATCH_WAV_BYTES_PER_SECOND: u64 = (MAX_STREAM_SAMPLE_RATE as u64) * 2;

pub(super) const MAX_BATCH_WAV_HEADER_BYTES: u64 = 64 * 1024;

/// Concurrent handlers, across both listeners, for requests without a valid
/// token: the dashboard page, discovery, pairing and every 401. tiny_http
/// has no read timeout, so a handler reading a body waits on its client for
/// as long as the client likes; strangers get a small share and a `503`
/// above it, and can never hold up the accept loop or token holders.
pub(super) const MAX_PUBLIC_HANDLERS: usize = 8;

/// Concurrent handlers for requests with the token. Transcriptions and event
/// streams move to their own threads straight away, under their own limits.
pub(super) const MAX_AUTHENTICATED_HANDLERS: usize = 32;

/// Answers one listener's requests until it closes. Each request is handled
/// on its own thread, within the caps above, so the accept loop only ever
/// reads request heads (tiny_http's connection threads parse them). A
/// handler that panics is logged (the default hook prints the panic) and
/// costs only its own request.
pub(super) fn serve(
    server: &Server,
    runtime: &Arc<HostRuntime>,
    metrics: &Arc<Mutex<HostMetrics>>,
    updater: &Arc<Updater>,
    bind_addr: &str,
) {
    for request in server.incoming_requests() {
        let handlers = if is_public(&request, runtime.live.token().as_deref()) {
            &runtime.public_handlers
        } else {
            &runtime.authenticated_handlers
        };
        let Some(slot) = handlers.try_take() else {
            refuse_busy(request);
            continue;
        };
        let runtime = Arc::clone(runtime);
        let metrics = Arc::clone(metrics);
        let updater = Arc::clone(updater);
        let bind_addr = bind_addr.to_string();
        let spawned = thread::Builder::new()
            .name("transcription-host-http".to_string())
            .spawn(move || {
                let _slot = slot;
                let handled = panic::catch_unwind(AssertUnwindSafe(|| {
                    handle_request(request, runtime, metrics, &updater, &bind_addr)
                }));
                match handled {
                    Ok(Ok(())) => {}
                    Ok(Err(err)) => {
                        eprintln!("Failed to handle transcription host request: {err}")
                    }
                    Err(_) => eprintln!("A transcription host request handler panicked"),
                }
            });
        if let Err(err) = spawned {
            eprintln!("Failed to start a transcription host request handler: {err}");
        }
    }
}

/// Requests answered without the token: the dashboard page, discovery,
/// pairing, and every request that will be refused with 401.
pub(super) fn is_public(request: &Request, token: Option<&str>) -> bool {
    let public_route = matches!(
        (request.method(), request_path(request.url())),
        (&Method::Get, "/" | "/favicon.ico" | "/v1/hello") | (&Method::Post, "/v1/pair")
    );
    public_route || !authorized(request, token)
}

/// `503` for a request over its handler cap. Dropping a request drains the
/// body it declared, which a slow client can stretch out indefinitely, so a
/// request with a body is answered from a short-lived thread of its own
/// rather than on the accept loop; one per connection, like tiny_http's
/// own connection threads.
pub(super) fn refuse_busy(request: Request) {
    let has_body = request.body_length().is_some_and(|length| length > 0);
    let refuse = move || {
        let _ = respond_error(request, StatusCode(503), "Server busy; try again shortly");
    };
    if has_body {
        if let Err(err) = thread::Builder::new()
            .name("transcription-host-refuse".to_string())
            .spawn(refuse)
        {
            eprintln!("Failed to answer a request over capacity: {err}");
        }
    } else {
        refuse();
    }
}

/// `127.0.0.1:<port>` when `addr` is one specific non-loopback address;
/// `None` for loopback and the wildcard addresses, which already cover this
/// computer, and for anything that isn't an IP socket address.
pub(super) fn loopback_companion_addr(addr: &str) -> Option<String> {
    let socket: std::net::SocketAddr = addr.parse().ok()?;
    let ip = socket.ip();
    (!ip.is_loopback() && !ip.is_unspecified()).then(|| format!("127.0.0.1:{}", socket.port()))
}

/// A host re-executed after an update may start before the old process has
/// released the port (Windows); it retries briefly instead of failing.
pub(super) fn bind_server(addr: &str) -> Result<Server, String> {
    let deadline = crate::app_dirs::env_var_os(RESTARTED_ENV)
        .map(|_| Instant::now() + Duration::from_secs(15));
    loop {
        match listen(addr) {
            Ok(server) => return Ok(server),
            Err(_) if deadline.is_some_and(|deadline| Instant::now() < deadline) => {
                thread::sleep(Duration::from_millis(250));
            }
            Err(err) => {
                return Err(format!(
                    "Failed to start transcription host on {addr}: {err}"
                ))
            }
        }
    }
}

/// Idle time before a connection's first keepalive probe, the gap between
/// probes, and how many unanswered probes drop it: a client that vanished
/// mid-request (a phone off Wi-Fi) fails its handler's read within about a
/// minute on Linux instead of holding it forever. macOS does not carry the
/// idle time over to accepted sockets, so there the first probe waits for
/// the system default (2 h) and the interval and count below follow it.
pub(super) const KEEPALIVE_IDLE: Duration = Duration::from_secs(30);

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
pub(super) const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(10);

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
pub(super) const KEEPALIVE_RETRIES: u32 = 3;

/// An HTTP server on `addr` (the first of its resolved addresses that binds)
/// whose connections use TCP keepalive.
pub(super) fn listen(addr: &str) -> Result<Server, Box<dyn std::error::Error + Send + Sync>> {
    let mut last_err = None;
    for socket_addr in addr.to_socket_addrs()? {
        match keepalive_listener(socket_addr) {
            Ok(listener) => return Server::from_listener(listener, None),
            Err(err) => last_err = Some(err),
        }
    }
    Err(last_err
        .map(Into::into)
        .unwrap_or_else(|| format!("{addr} resolves to no address").into()))
}

/// A listening socket with keepalive set; accepted connections inherit it
/// (a test checks what carries over on macOS and Linux), which is the only
/// way to reach the sockets tiny_http accepts.
pub(super) fn keepalive_listener(addr: SocketAddr) -> std::io::Result<TcpListener> {
    let socket = Socket::new(Domain::for_address(addr), Type::STREAM, Some(Protocol::TCP))?;
    // As std's TcpListener::bind does: a restarted host rebinds while old
    // connections sit in TIME_WAIT. Not on Windows, where the option would
    // let another process bind the same port.
    #[cfg(not(windows))]
    socket.set_reuse_address(true)?;
    let keepalive = TcpKeepalive::new().with_time(KEEPALIVE_IDLE);
    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    let keepalive = keepalive
        .with_interval(KEEPALIVE_INTERVAL)
        .with_retries(KEEPALIVE_RETRIES);
    socket.set_keepalive(true)?;
    socket.set_tcp_keepalive(&keepalive)?;
    socket.bind(&addr.into())?;
    socket.listen(128)?;
    Ok(socket.into())
}

pub(super) fn request_path(url: &str) -> &str {
    url.split_once('?').map(|(path, _)| path).unwrap_or(url)
}

/// Who a pairing attempt counts against. Like `client_ip`, but behind a
/// local proxy it is the *last* `X-Forwarded-For` hop: proxies append the
/// address they saw, while everything before it came from the client, which
/// could otherwise name a fresh address on every guess.
pub(super) fn pairing_rate_key(request: &Request) -> String {
    let Some(peer) = request.remote_addr().map(|addr| addr.ip()) else {
        return "unknown".to_string();
    };
    if peer.is_loopback() {
        let forwarded = header_value(request, "x-forwarded-for")
            .and_then(|value| value.rsplit(',').next())
            .or_else(|| header_value(request, "tailscale-user-login"))
            .map(str::trim)
            .filter(|value| !value.is_empty());
        if let Some(client) = forwarded {
            return client.to_string();
        }
    }
    peer.to_string()
}

pub(super) fn client_ip(request: &Request) -> Option<String> {
    let peer = request.remote_addr()?.ip();
    // Behind `tailscale serve` every request arrives from loopback, so the
    // proxy's forwarding headers are the only way to tell tailnet devices
    // apart. They are only trusted from loopback, where a local proxy set them.
    if peer.is_loopback() {
        let forwarded = header_value(request, "x-forwarded-for")
            .or_else(|| header_value(request, "tailscale-user-login"))
            .and_then(|value| value.split(',').next())
            .map(str::trim)
            .filter(|value| !value.is_empty());
        if let Some(client) = forwarded {
            return Some(client.to_string());
        }
    }
    Some(peer.to_string())
}

pub(super) fn respond_error(
    request: Request,
    status: StatusCode,
    message: &str,
) -> Result<(), String> {
    let body = serde_json::json!({ "error": message }).to_string();
    respond_json(request, status, body)
}

pub(super) fn read_limited_body(reader: &mut impl Read, max_bytes: u64) -> Result<Vec<u8>, String> {
    let mut body = Vec::new();
    let mut limited = reader.take(max_bytes.saturating_add(1));
    limited
        .read_to_end(&mut body)
        .map_err(|err| format!("Failed to read transcription request body: {err}"))?;
    if body.len() as u64 > max_bytes {
        return Err("Transcription request body exceeds host maximum upload size".to_string());
    }
    Ok(body)
}

/// Reads a JSON body of at most `max_bytes`. The error is the status and
/// message to answer with: `413` over the limit, `400` when it doesn't
/// parse (worded by `invalid`, so a handler can keep secrets out of it).
pub(super) fn read_json_body<T: DeserializeOwned>(
    request: &mut Request,
    max_bytes: u64,
    invalid: impl FnOnce(&[u8], serde_json::Error) -> String,
) -> Result<T, (StatusCode, String)> {
    let body = read_limited_body(&mut request.as_reader(), max_bytes)
        .map_err(|err| (StatusCode(413), err))?;
    serde_json::from_slice(&body).map_err(|err| (StatusCode(400), invalid(&body, err)))
}

pub(super) fn max_batch_body_bytes(max_recording_seconds: u16) -> u64 {
    MAX_BATCH_WAV_HEADER_BYTES.saturating_add(
        MAX_BATCH_WAV_BYTES_PER_SECOND.saturating_mul(u64::from(max_recording_seconds)),
    )
}

pub(super) fn authorized(request: &Request, token: Option<&str>) -> bool {
    let Some(token) = token.filter(|value| !value.trim().is_empty()) else {
        return true;
    };
    if header_value(request, "authorization")
        .and_then(|value| value.strip_prefix("Bearer "))
        .is_some_and(|value| constant_time_eq(value.as_bytes(), token.as_bytes()))
    {
        return true;
    }
    // Allow GETs to authenticate via ?token=… so the host operator can
    // bookmark a single URL in the browser. Mutating routes need the header,
    // which keeps the token out of proxy and access logs for writes.
    request.method() == &Method::Get
        && query_param(request.url(), "token")
            .is_some_and(|value| constant_time_eq(value.as_bytes(), token.as_bytes()))
}

/// Compares secrets without stopping at the first differing byte, so the
/// time taken does not reveal how much of a guess was right. A length
/// mismatch returns early; callers comparing passwords hash both sides first.
pub(super) fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

pub(super) fn query_param(url: &str, name: &str) -> Option<String> {
    let query_start = url.find('?')? + 1;
    let query = &url[query_start..];
    for pair in query.split('&') {
        let mut parts = pair.splitn(2, '=');
        let key = parts.next()?;
        if key == name {
            let raw_value = parts.next().unwrap_or("");
            return Some(percent_decode(raw_value));
        }
    }
    None
}

pub(super) fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'+' {
            out.push(b' ');
            i += 1;
        } else if b == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            match (hi, lo) {
                (Some(hi), Some(lo)) => {
                    out.push(((hi << 4) | lo) as u8);
                    i += 3;
                }
                _ => {
                    out.push(b);
                    i += 1;
                }
            }
        } else {
            out.push(b);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// An `x-fairspoken-<name>` request header. Clients from before the rename
/// send `x-multivoice-<name>`, which is still accepted.
pub(super) fn protocol_header<'a>(request: &'a Request, name: &str) -> Option<&'a str> {
    header_value(request, &format!("x-fairspoken-{name}"))
        .or_else(|| header_value(request, &format!("x-multivoice-{name}")))
}

pub(super) fn header_value<'a>(request: &'a Request, name: &str) -> Option<&'a str> {
    request
        .headers()
        .iter()
        .find(|header| header.field.to_string().eq_ignore_ascii_case(name))
        .map(|header| header.value.as_str())
}

pub(super) fn respond_json(
    request: Request,
    status: StatusCode,
    body: String,
) -> Result<(), String> {
    respond_json_with(request, status, body, None)
}

pub(super) fn respond_json_with(
    request: Request,
    status: StatusCode,
    body: String,
    extra: Option<Header>,
) -> Result<(), String> {
    let content_type = Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..])
        .map_err(|_| "Failed to create response content-type header".to_string())?;
    let mut response = Response::from_string(body)
        .with_status_code(status)
        .with_header(content_type);
    if let Some(header) = extra {
        response = response.with_header(header);
    }
    request
        .respond(response)
        .map_err(|err| format!("Failed to send response: {err}"))
}

pub(super) fn respond_html(request: Request, body: &str) -> Result<(), String> {
    let content_type = Header::from_bytes(&b"Content-Type"[..], &b"text/html; charset=utf-8"[..])
        .map_err(|_| "Failed to create response content-type header".to_string())?;
    request
        .respond(
            Response::from_string(body)
                .with_status_code(StatusCode(200))
                .with_header(content_type),
        )
        .map_err(|err| format!("Failed to send response: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::test_support::*;
    use std::io::Cursor;

    #[test]
    fn only_one_specific_address_gets_a_loopback_companion() {
        assert_eq!(
            super::loopback_companion_addr("100.91.70.66:48173").as_deref(),
            Some("127.0.0.1:48173")
        );
        assert_eq!(
            super::loopback_companion_addr("[fd7a:115c:a1e0::1]:48200").as_deref(),
            Some("127.0.0.1:48200")
        );
        for covered in [
            "127.0.0.1:48173",
            "[::1]:48173",
            "0.0.0.0:48173",
            "[::]:48173",
            "localhost:48173",
        ] {
            assert_eq!(super::loopback_companion_addr(covered), None, "{covered}");
        }
    }

    #[test]
    fn request_path_ignores_query_string_for_route_matching() {
        assert_eq!(request_path("/?token=secret"), "/");
        assert_eq!(request_path("/v1/stats?token=secret"), "/v1/stats");
        assert_eq!(request_path("/v1/health"), "/v1/health");
    }

    #[test]
    fn constant_time_eq_matches_only_identical_tokens() {
        assert!(constant_time_eq(b"secret", b"secret"));
        assert!(!constant_time_eq(b"secret", b"secreT"));
        assert!(!constant_time_eq(b"secret", b"secret2"));
        assert!(!constant_time_eq(b"", b"secret"));
    }

    #[test]
    fn limited_batch_body_rejects_oversized_uploads() {
        let max_bytes = max_batch_body_bytes(10);
        let mut body = Cursor::new(vec![0_u8; max_bytes as usize + 1]);

        let err = read_limited_body(&mut body, max_bytes).expect_err("oversized body rejects");

        assert!(err.contains("maximum upload size"));
    }

    #[test]
    fn a_huge_content_length_never_takes_the_host_down() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = token_runtime(
            dir.path().join("host-config.json"),
            super::HostAuth {
                token: Some("secret".to_string()),
                ..Default::default()
            },
        );
        let addr = serve_test_host(runtime);

        // Each answer drops a request whose claimed body was never sent:
        // usize::MAX used to panic on "capacity overflow", 2^62 to abort on
        // the failed allocation, and a lazily allocated terabyte to block
        // the accept loop draining a connection its client keeps open.
        let mut open = Vec::new();
        for claimed in [
            "18446744073709551615",
            "4611686018427387904",
            "1000000000000",
            "4294967296",
        ] {
            for (request, expected) in [("GET /v1/hello", 200), ("POST /v1/config", 401)] {
                use std::io::{Read, Write};
                let mut socket = std::net::TcpStream::connect(addr).expect("connect");
                socket
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .expect("timeout");
                socket
                    .write_all(
                        format!("{request} HTTP/1.1\r\nHost: test\r\nConnection: close\r\nContent-Length: {claimed}\r\n\r\n")
                            .as_bytes(),
                    )
                    .expect("send request");
                let mut response = String::new();
                socket
                    .read_to_string(&mut response)
                    .unwrap_or_else(|err| panic!("{request} with Content-Length {claimed}: {err}"));
                assert!(
                    response.starts_with(&format!("HTTP/1.1 {expected} ")),
                    "{request} with Content-Length {claimed}: {response}"
                );
                open.push(socket);
            }
        }
        let (status, _) = exchange(
            addr,
            "GET /v1/health HTTP/1.1\r\nHost: test\r\nAuthorization: Bearer secret\r\nConnection: close\r\n\r\n",
        );
        assert_eq!(status, 200, "the host keeps serving");
    }

    /// Opens a connection and sends a request head that promises a body
    /// which never comes, as a stalling client would.
    fn stall(addr: std::net::SocketAddr, head: &str) -> std::net::TcpStream {
        use std::io::Write;
        let mut socket = std::net::TcpStream::connect(addr).expect("connect");
        socket.write_all(head.as_bytes()).expect("send head");
        socket
    }

    #[test]
    fn stalled_bodies_hold_only_their_own_handlers() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = token_runtime(
            dir.path().join("host-config.json"),
            super::HostAuth {
                token: Some("secret".to_string()),
                ..Default::default()
            },
        );
        let addr = serve_test_host(Arc::clone(&runtime));
        let hello = "GET /v1/hello HTTP/1.1\r\nHost: test\r\nConnection: close\r\n\r\n";
        let health = "GET /v1/health HTTP/1.1\r\nHost: test\r\nAuthorization: Bearer secret\r\nConnection: close\r\n\r\n";
        // Over tiny_http's 1 KiB read-ahead, so the handler reads the body.
        let stalled_pair = "POST /v1/pair HTTP/1.1\r\nHost: test\r\nContent-Length: 2000\r\n\r\n";

        let public_handlers = || {
            runtime
                .public_handlers
                .taken
                .load(std::sync::atomic::Ordering::Acquire)
        };
        // One stall at a time, each waited on until its handler holds a
        // slot: tiny_http's connection pool can leave a connection opened in
        // a burst queued behind long-lived ones.
        let stall_handler = |stalled: &mut Vec<std::net::TcpStream>| {
            let expected = public_handlers() + 1;
            stalled.push(stall(addr, stalled_pair));
            let started = Instant::now();
            while public_handlers() < expected {
                assert!(
                    started.elapsed() < Duration::from_secs(5),
                    "stall never reached a handler"
                );
                thread::sleep(Duration::from_millis(5));
            }
        };

        // One stalled pairing body used to block the accept loop for good.
        let mut stalled = Vec::new();
        stall_handler(&mut stalled);
        assert_eq!(exchange(addr, hello).0, 200);

        // Strangers fill their share; past it they get 503 (also with a
        // body, which is answered off the accept loop), token holders don't.
        while stalled.len() < super::MAX_PUBLIC_HANDLERS {
            stall_handler(&mut stalled);
        }
        let (status, busy) = exchange(addr, hello);
        assert_eq!(status, 503);
        assert!(busy.contains("Server busy"));
        let mut over = stall(addr, stalled_pair);
        over.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut status_line = String::new();
        std::io::BufRead::read_line(&mut std::io::BufReader::new(&mut over), &mut status_line)
            .expect("an answer while the body is still owed");
        assert!(status_line.starts_with("HTTP/1.1 503 "), "{status_line}");
        stalled.push(over);
        assert_eq!(exchange(addr, hello).0, 503);
        assert_eq!(exchange(addr, health).0, 200);

        // A stalled client hanging up frees its handler.
        drop(stalled);
        let started = Instant::now();
        while public_handlers() > 0 {
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "handlers never freed"
            );
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(exchange(addr, hello).0, 200);
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn accepted_connections_inherit_the_listeners_keepalive() {
        let listener = super::keepalive_listener("127.0.0.1:0".parse().unwrap()).expect("bind");
        let _client =
            std::net::TcpStream::connect(listener.local_addr().unwrap()).expect("connect");
        // tiny_http accepts with the same std call.
        let (accepted, _) = listener.accept().expect("accept");
        let socket = socket2::SockRef::from(&accepted);
        assert!(socket.keepalive().unwrap());
        // macOS keeps the system idle time (2 h) on accepted sockets; the
        // probe interval and count do carry over.
        #[cfg(target_os = "linux")]
        assert_eq!(socket.tcp_keepalive_time().unwrap(), super::KEEPALIVE_IDLE);
        assert_eq!(
            socket.tcp_keepalive_interval().unwrap(),
            super::KEEPALIVE_INTERVAL
        );
        assert_eq!(
            socket.tcp_keepalive_retries().unwrap(),
            super::KEEPALIVE_RETRIES
        );
    }
}
