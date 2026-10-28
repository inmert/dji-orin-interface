//! HTTP server: the built web front end, plus the FPV camera as MJPEG.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use dji_common::logln;

use crate::fpv::Fpv;
use crate::{HTTP_ADDR, die};

/// Path of the FPV camera's MJPEG stream.
const FPV_PATH: &str = "/fpv.mjpg";

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

/// Stream FPV frames to one client as `multipart/x-mixed-replace` until it
/// disconnects. Runs on its own thread; the subscription keeps the aircraft
/// stream up while it lasts.
fn stream_fpv(request: tiny_http::Request, fpv: &Arc<Fpv>) {
    let peer = request
        .remote_addr()
        .map(ToString::to_string)
        .unwrap_or_default();
    let mut viewer = fpv.subscribe();
    let mut out = request.into_writer();
    logln!("http: fpv viewer {peer} connected");
    let header = "HTTP/1.1 200 OK\r\n\
        Content-Type: multipart/x-mixed-replace; boundary=frame\r\n\
        Cache-Control: no-cache, no-store\r\n\
        Connection: close\r\n\r\n";
    let mut ok = out.write_all(header.as_bytes()).is_ok();
    while ok {
        let Some(jpeg) = viewer.next_frame(Duration::from_secs(1)) else {
            continue;
        };
        let part = format!(
            "--frame\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\n\r\n",
            jpeg.len()
        );
        ok = out
            .write_all(part.as_bytes())
            .and_then(|()| out.write_all(&jpeg))
            .and_then(|()| out.write_all(b"\r\n"))
            .and_then(|()| out.flush())
            .is_ok();
    }
    logln!("http: fpv viewer {peer} gone");
}

/// Serve the built front end from `web_root` forever, and the FPV stream at
/// [`FPV_PATH`] when `fpv` is enabled.
pub fn serve(web_root: PathBuf, fpv: Option<Arc<Fpv>>) {
    let server = tiny_http::Server::http(HTTP_ADDR)
        .unwrap_or_else(|e| die(&format!("http: bind {HTTP_ADDR}: {e}")));
    logln!("http: serving {} on {HTTP_ADDR}", web_root.display());
    for request in server.incoming_requests() {
        if request.url().split('?').next() == Some(FPV_PATH)
            && let Some(fpv) = &fpv
        {
            let fpv = Arc::clone(fpv);
            std::thread::spawn(move || stream_fpv(request, &fpv));
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
    use super::resolve;

    #[test]
    fn resolves_and_rejects_traversal() {
        assert_eq!(resolve("/"), Some("index.html"));
        assert_eq!(resolve("/assets/app.js?v=1"), Some("assets/app.js"));
        assert_eq!(resolve("/../etc/passwd"), None);
        assert_eq!(resolve("/a/./b"), None);
        assert_eq!(resolve("/a\\..\\b"), None);
    }
}
