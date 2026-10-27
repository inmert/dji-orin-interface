//! Static HTTP server for the built web front end.

use std::path::{Path, PathBuf};

use dji_common::logln;

use crate::{HTTP_ADDR, die};

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

/// Serve the built front end from `web_root` forever.
pub fn serve(web_root: PathBuf) {
    let server = tiny_http::Server::http(HTTP_ADDR)
        .unwrap_or_else(|e| die(&format!("http: bind {HTTP_ADDR}: {e}")));
    logln!("http: serving {} on {HTTP_ADDR}", web_root.display());
    for request in server.incoming_requests() {
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
