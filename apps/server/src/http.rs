//! HTTP server: the built web front end, plus the camera feeds as MJPEG.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use dji_common::logln;

use crate::video::{Feed, Feeds};
use crate::{HTTP_ADDR, die};

/// The feed named by a `/video/<name>.mjpg` URL (query ignored).
fn feed_name(url: &str) -> Option<&str> {
    url.split('?')
        .next()?
        .strip_prefix("/video/")?
        .strip_suffix(".mjpg")
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

/// Map a request URL to a path under the web root: `/` is `index.html`, the
/// query is dropped, and any `.`/`..` segment is rejected (no traversal).
fn resolve(url: &str) -> Option<&str> {
    let rel = url.split('?').next().unwrap_or("/").trim_start_matches('/');
    let rel = if rel.is_empty() { "index.html" } else { rel };
    let safe = !rel.split(['/', '\\']).any(|seg| seg == ".." || seg == ".");
    safe.then_some(rel)
}

/// Stream a feed's frames to one client as `multipart/x-mixed-replace` until
/// it disconnects. Runs on its own thread; the subscription keeps the feed
/// decoding while it lasts.
fn stream_video(request: tiny_http::Request, feed: &Arc<Feed>) {
    let peer = request
        .remote_addr()
        .map(ToString::to_string)
        .unwrap_or_default();
    let name = feed.name();
    let mut viewer = feed.subscribe();
    let mut out = request.into_writer();
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

/// Serve the built front end from `web_root` forever, and each feed at
/// `/video/<name>.mjpg`.
pub fn serve(web_root: PathBuf, feeds: Arc<Feeds>) {
    let server = tiny_http::Server::http(HTTP_ADDR)
        .unwrap_or_else(|e| die(&format!("http: bind {HTTP_ADDR}: {e}")));
    logln!("http: serving {} on {HTTP_ADDR}", web_root.display());
    for request in server.incoming_requests() {
        if let Some(feed) = feed_name(request.url()).and_then(|n| feeds.get(n)) {
            let feed = Arc::clone(feed);
            std::thread::spawn(move || stream_video(request, &feed));
            continue;
        }
        let file = resolve(request.url()).and_then(|rel| {
            let path = web_root.join(rel);
            std::fs::read(&path).ok().map(|bytes| (path, bytes))
        });
        let resp = match file {
            Some((path, bytes)) => {
                let ct = content_type(&path);
                let header =
                    tiny_http::Header::from_bytes(b"Content-Type".as_slice(), ct.as_bytes())
                        .expect("valid header");
                tiny_http::Response::from_data(bytes).with_header(header)
            }
            None => tiny_http::Response::from_string("not found").with_status_code(404),
        };
        let _ = request.respond(resp);
    }
}

#[cfg(test)]
mod tests {
    use super::{feed_name, resolve};

    #[test]
    fn parses_feed_urls() {
        assert_eq!(feed_name("/video/cam1.mjpg"), Some("cam1"));
        assert_eq!(feed_name("/video/fpv.mjpg?s=3"), Some("fpv"));
        assert_eq!(feed_name("/video/cam1.h264"), None);
        assert_eq!(feed_name("/fpv.mjpg"), None);
    }

    #[test]
    fn resolves_and_rejects_traversal() {
        assert_eq!(resolve("/"), Some("index.html"));
        assert_eq!(resolve("/assets/app.js?v=1"), Some("assets/app.js"));
        assert_eq!(resolve("/../etc/passwd"), None);
        assert_eq!(resolve("/a/./b"), None);
        assert_eq!(resolve("/a\\..\\b"), None);
    }
}
