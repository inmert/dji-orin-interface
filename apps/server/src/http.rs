//! HTTP server, plain and over TLS: the built web front end, the camera
//! feeds as MJPEG, the WebSocket at `/ws` ([`crate::ws`]), voice input
//! (`POST /voice`, [`crate::voice`]) and the online services' status
//! (`GET /ai/status`, speech and understanding).
//!
//! Hand-rolled HTTP/1.1, one thread per connection, `Connection: close` on
//! every response: enough for a LAN portal, and it lets one listener (and
//! one certificate acceptance in the browser) carry the page, the socket,
//! the video and the microphone uploads. Browsers only allow microphone
//! capture on a secure origin, which is why TLS is here at all.

use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use openssl::ssl::{SslAcceptor, SslStream};

use dji_common::logln;
use dji_vehicle::Vehicle;

use crate::video::{Feed, Feeds};
use crate::{die, understand, voice, ws};

/// Longest request head (request line + headers) accepted.
const MAX_HEAD: usize = 16 * 1024;
/// Longest request body accepted (a voice recording).
const MAX_BODY: usize = 32 * 1024 * 1024;

/// A client connection, plain or TLS, with its TCP socket reachable for
/// timeouts.
pub trait Conn: Read + Write + Send {
    fn tcp(&self) -> &TcpStream;
}

impl Conn for TcpStream {
    fn tcp(&self) -> &TcpStream {
        self
    }
}

impl Conn for SslStream<TcpStream> {
    fn tcp(&self) -> &TcpStream {
        self.get_ref()
    }
}

/// A connection whose first bytes were already read (the request head):
/// hands them out again before reading on, so the WebSocket handshake can
/// parse the request itself.
pub struct Replay<C> {
    head: Vec<u8>,
    pos: usize,
    conn: C,
}

impl<C: Conn> Read for Replay<C> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pos < self.head.len() {
            let n = (self.head.len() - self.pos).min(buf.len());
            buf[..n].copy_from_slice(&self.head[self.pos..self.pos + n]);
            self.pos += n;
            return Ok(n);
        }
        self.conn.read(buf)
    }
}

impl<C: Conn> Write for Replay<C> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.conn.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.conn.flush()
    }
}

impl<C: Conn> Conn for Replay<C> {
    fn tcp(&self) -> &TcpStream {
        self.conn.tcp()
    }
}

/// A parsed request head.
#[derive(Debug, PartialEq, Eq)]
pub struct Head {
    pub method: String,
    /// The request target, query included.
    pub target: String,
    /// Header names lower-cased.
    pub headers: Vec<(String, String)>,
}

impl Head {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    /// The path without its query.
    pub fn path(&self) -> &str {
        self.target.split('?').next().unwrap_or("/")
    }

    fn wants_websocket(&self) -> bool {
        self.header("upgrade")
            .is_some_and(|u| u.eq_ignore_ascii_case("websocket"))
    }
}

/// Parse `GET /x HTTP/1.1\r\nName: value\r\n…` (without the final blank line).
pub fn parse_head(head: &[u8]) -> Option<Head> {
    let text = std::str::from_utf8(head).ok()?;
    let mut lines = text.split("\r\n");
    let mut request = lines.next()?.split(' ');
    let method = request.next()?.to_string();
    let target = request.next()?.to_string();
    if !request.next()?.starts_with("HTTP/1.") {
        return None;
    }
    let headers = lines
        .filter(|l| !l.is_empty())
        .filter_map(|l| {
            let (name, value) = l.split_once(':')?;
            Some((name.trim().to_ascii_lowercase(), value.trim().to_string()))
        })
        .collect();
    Some(Head {
        method,
        target,
        headers,
    })
}

/// Read up to and including the blank line that ends the head. Returns the
/// head bytes and whatever came after them in the same reads.
fn read_head(conn: &mut dyn Read) -> io::Result<(Vec<u8>, Vec<u8>)> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let rest = buf.split_off(end + 4);
            return Ok((buf, rest));
        }
        if buf.len() > MAX_HEAD {
            return Err(io::Error::other("request head too long"));
        }
        let n = conn.read(&mut chunk)?;
        if n == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("ico") => "image/x-icon",
        Some("png") => "image/png",
        _ => "application/octet-stream",
    }
}

/// The feed named by a `/video/<name>.mjpg` path.
fn feed_name(path: &str) -> Option<&str> {
    path.strip_prefix("/video/")?.strip_suffix(".mjpg")
}

/// Map a request path to a path under the web root: `/` is `index.html`,
/// and any `.`/`..` segment is rejected (no traversal).
fn resolve(path: &str) -> Option<&str> {
    let rel = path.trim_start_matches('/');
    let rel = if rel.is_empty() { "index.html" } else { rel };
    let safe = !rel.split(['/', '\\']).any(|seg| seg == ".." || seg == ".");
    safe.then_some(rel)
}

fn respond(conn: &mut dyn Write, status: &str, content_type: &str, body: &[u8]) -> io::Result<()> {
    write!(
        conn,
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\
         Cache-Control: no-cache\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    conn.write_all(body)?;
    conn.flush()
}

/// Stream a feed's frames to one client as `multipart/x-mixed-replace` until
/// it disconnects. The subscription keeps the feed decoding while it lasts.
fn stream_video(out: &mut dyn Write, peer: &str, feed: &Arc<Feed>) {
    let name = feed.name();
    let mut viewer = feed.subscribe();
    logln!("http: {name} viewer {peer} connected");
    let header = "HTTP/1.1 200 OK\r\n\
        Content-Type: multipart/x-mixed-replace; boundary=frame\r\n\
        Cache-Control: no-cache, no-store\r\n\
        Connection: close\r\n\r\n";
    // Flush now: a feed with no video yet must still answer the request.
    let mut ok = out
        .write_all(header.as_bytes())
        .and_then(|()| out.flush())
        .is_ok();
    while ok {
        let Some(frame) = viewer.next_frame(Duration::from_secs(1)) else {
            // No video: write a blank line (ignored between parts) so a
            // client that left is noticed and its subscription released.
            ok = out.write_all(b"\r\n").and_then(|()| out.flush()).is_ok();
            continue;
        };
        let part = format!(
            "--frame\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\nX-Sequence: {}\r\nX-Timestamp-Ms: {}\r\n\r\n",
            frame.jpeg.len(),
            frame.sequence,
            frame.timestamp_ms
        );
        ok = out
            .write_all(part.as_bytes())
            .and_then(|()| out.write_all(&frame.jpeg))
            .and_then(|()| out.write_all(b"\r\n"))
            .and_then(|()| out.flush())
            .is_ok();
    }
    logln!("http: {name} viewer {peer} gone");
}

/// What every connection handler shares.
pub struct Context {
    pub web_root: PathBuf,
    pub feeds: Arc<Feeds>,
    pub vehicle: Arc<Vehicle>,
}

/// Serve one connection: a WebSocket upgrade, a video stream, a voice upload
/// or a file.
fn handle<C: Conn + 'static>(mut conn: C, peer: String, cx: &Context) -> io::Result<()> {
    let (head_bytes, rest) = read_head(&mut conn)?;
    let Some(head) = parse_head(&head_bytes[..head_bytes.len() - 4]) else {
        return respond(&mut conn, "400 Bad Request", "text/plain", b"bad request");
    };
    let path = head.path().to_string();
    if head.wants_websocket() {
        if path != "/ws" {
            return respond(&mut conn, "404 Not Found", "text/plain", b"not found");
        }
        let mut replay = head_bytes;
        replay.extend_from_slice(&rest);
        ws::client(
            Replay {
                head: replay,
                pos: 0,
                conn,
            },
            peer,
            &cx.vehicle,
            &cx.feeds,
        );
        return Ok(());
    }
    // Body, if any (the voice upload).
    let length: usize = head
        .header("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    if length > MAX_BODY {
        return respond(
            &mut conn,
            "413 Payload Too Large",
            "text/plain",
            b"too large",
        );
    }
    let mut body = rest;
    body.truncate(body.len().min(length));
    if body.len() < length {
        let start = body.len();
        body.resize(length, 0);
        conn.read_exact(&mut body[start..])?;
    }

    match (head.method.as_str(), path.as_str()) {
        ("GET", p) if feed_name(p).is_some() => match cx.feeds.get(feed_name(p).unwrap()) {
            Some(feed) => {
                let feed = Arc::clone(feed);
                stream_video(&mut conn, &peer, &feed);
                Ok(())
            }
            None => respond(&mut conn, "404 Not Found", "text/plain", b"no such feed"),
        },
        ("GET", "/ai/status") => respond(
            &mut conn,
            "200 OK",
            "application/json",
            serde_json::json!({
                "speech": voice::statuses(),
                "understanding": understand::statuses(),
            })
            .to_string()
            .as_bytes(),
        ),
        ("POST", "/voice") => {
            let kind = head.header("content-type").unwrap_or("audio/webm");
            match voice::transcribe(&body, kind) {
                Ok(t) => respond(
                    &mut conn,
                    "200 OK",
                    "application/json",
                    serde_json::to_string(&t).unwrap_or_default().as_bytes(),
                ),
                Err(e) => {
                    logln!("voice: {e}");
                    respond(
                        &mut conn,
                        "503 Service Unavailable",
                        "text/plain",
                        e.as_bytes(),
                    )
                }
            }
        }
        ("GET", p) => {
            let file = resolve(p).and_then(|rel| {
                let path = cx.web_root.join(rel);
                std::fs::read(&path).ok().map(|bytes| (path, bytes))
            });
            match file {
                Some((path, bytes)) => respond(&mut conn, "200 OK", content_type(&path), &bytes),
                None => respond(&mut conn, "404 Not Found", "text/plain", b"not found"),
            }
        }
        _ => respond(
            &mut conn,
            "405 Method Not Allowed",
            "text/plain",
            b"method not allowed",
        ),
    }
}

/// Accept connections on `addr` forever, plain or (with `tls`) over TLS,
/// one thread each.
pub fn serve(addr: &str, tls: Option<SslAcceptor>, cx: Arc<Context>) {
    let listener =
        TcpListener::bind(addr).unwrap_or_else(|e| die(&format!("http: bind {addr}: {e}")));
    logln!(
        "http{}: serving {} on {addr}",
        if tls.is_some() { "s" } else { "" },
        cx.web_root.display()
    );
    for stream in listener.incoming().flatten() {
        let peer = stream
            .peer_addr()
            .map(|a| a.to_string())
            .unwrap_or_default();
        // Generous for the request; WebSocket clients get their own below.
        let _ = stream.set_read_timeout(Some(Duration::from_secs(15)));
        let _ = stream.set_write_timeout(Some(Duration::from_secs(10)));
        let (tls, cx) = (tls.clone(), Arc::clone(&cx));
        std::thread::spawn(move || {
            let result = match tls {
                Some(acceptor) => match acceptor.accept(stream) {
                    Ok(conn) => handle(conn, peer.clone(), &cx),
                    Err(e) => {
                        // A browser probing the certificate aborts; routine.
                        logln!("https: handshake with {peer} failed: {e}");
                        return;
                    }
                },
                None => handle(stream, peer.clone(), &cx),
            };
            if let Err(e) = result
                && !matches!(
                    e.kind(),
                    io::ErrorKind::UnexpectedEof
                        | io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                        | io::ErrorKind::ConnectionReset
                        | io::ErrorKind::BrokenPipe
                )
            {
                logln!("http: {peer}: {e}");
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_feed_paths() {
        assert_eq!(feed_name("/video/cam1.mjpg"), Some("cam1"));
        assert_eq!(feed_name("/video/cam1.h264"), None);
        assert_eq!(feed_name("/fpv.mjpg"), None);
    }

    #[test]
    fn resolves_and_rejects_traversal() {
        assert_eq!(resolve("/"), Some("index.html"));
        assert_eq!(resolve("/assets/app.js"), Some("assets/app.js"));
        assert_eq!(resolve("/../etc/passwd"), None);
        assert_eq!(resolve("/a/./b"), None);
        assert_eq!(resolve("/a\\..\\b"), None);
    }

    #[test]
    fn parses_a_request_head_and_replays_it() {
        let head = parse_head(b"GET /ws?x=1 HTTP/1.1\r\nHost: orin:8080\r\nUpgrade: WebSocket\r\n")
            .unwrap();
        assert_eq!((head.method.as_str(), head.path()), ("GET", "/ws"));
        assert_eq!(head.header("host"), Some("orin:8080"));
        assert!(head.wants_websocket());
        assert!(parse_head(b"nonsense").is_none());
        assert!(parse_head(b"GET / FTP/1.0\r\n").is_none());

        let (a, b) = TcpListener::bind("127.0.0.1:0")
            .map(|l| {
                let addr = l.local_addr().unwrap();
                let c = TcpStream::connect(addr).unwrap();
                (l.accept().unwrap().0, c)
            })
            .unwrap();
        b.set_read_timeout(Some(Duration::from_millis(200)))
            .unwrap();
        let mut r = Replay {
            head: b"abc".to_vec(),
            pos: 0,
            conn: a,
        };
        let mut out = [0u8; 2];
        assert_eq!(r.read(&mut out).unwrap(), 2);
        assert_eq!(&out, b"ab");
        assert_eq!(r.read(&mut out).unwrap(), 1);
        assert_eq!(out[0], b'c');
        r.write_all(b"z").unwrap();
        let mut got = [0u8; 1];
        (&b).read_exact(&mut got).unwrap();
        assert_eq!(&got, b"z");
    }
}
