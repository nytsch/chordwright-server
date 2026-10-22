//! The app itself, for a browser: files under `/`, next to `/api`.
//!
//! The server does not bring an app of its own. Whoever starts it hands the
//! files in — the desktop app a build of its web app embedded in itself,
//! `--site <dir>` a folder — and the server only hands them out: the path as
//! asked, else `index.html` for anything that looks like a page (the app
//! routes in the `#` part, but a reload on `/library` should not be a 404).

use std::borrow::Cow;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::Response;

use crate::http::Body;

/// Where the files come from.
pub trait Files: Send + Sync {
    /// `path` is relative, never empty, and has no `.` or `..` in it.
    fn get(&self, path: &str) -> Option<Cow<'static, [u8]>>;
}

#[derive(Clone)]
pub struct Site(Arc<dyn Files>);

impl Site {
    pub fn new(files: impl Files + 'static) -> Site {
        Site(Arc::new(files))
    }

    /// A folder on disk, read at every request — `--site` while working on
    /// the app.
    pub fn dir(dir: impl Into<PathBuf>) -> Site {
        Site::new(Dir(dir.into()))
    }
}

impl fmt::Debug for Site {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Site")
    }
}

struct Dir(PathBuf);

impl Files for Dir {
    fn get(&self, path: &str) -> Option<Cow<'static, [u8]>> {
        let file = self.0.join(path);
        file.is_file()
            .then(|| std::fs::read(file).ok())
            .flatten()
            .map(Cow::Owned)
    }
}

/// `/a/b.js` → `a/b.js`; `None` for a path that tries to leave the site.
fn clean(path: &str) -> Option<String> {
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    if parts
        .iter()
        .any(|p| *p == "." || *p == ".." || p.contains('\\') || p.contains(':'))
    {
        return None;
    }
    Some(if parts.is_empty() {
        "index.html".into()
    } else {
        parts.join("/")
    })
}

pub fn content_type(path: &str) -> &'static str {
    let ext = path.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    match ext.to_ascii_lowercase().as_str() {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "webmanifest" => "application/manifest+json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "ttf" => "font/ttf",
        "txt" | "chordpro" | "cho" => "text/plain; charset=utf-8",
        "wasm" => "application/wasm",
        _ => "application/octet-stream",
    }
}

/// The answer to a GET (or HEAD) outside `/api`.
pub fn respond(site: &Site, path: &str, head: bool) -> Option<Response<Body>> {
    let path = clean(path)?;
    let (path, bytes) = match site.0.get(&path) {
        Some(bytes) => (path, bytes),
        // No dot in the last part: a page, not a file that is missing.
        None if !path.rsplit('/').next().unwrap_or("").contains('.') => {
            ("index.html".to_string(), site.0.get("index.html")?)
        }
        None => return None,
    };
    // Vite names what it builds after its content; everything else may change
    // with the next version of the app.
    let cache = if path.starts_with("assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    let length = bytes.len();
    let body = if head {
        Bytes::new()
    } else {
        match bytes {
            Cow::Borrowed(b) => Bytes::from_static(b),
            Cow::Owned(b) => Bytes::from(b),
        }
    };
    Some(
        Response::builder()
            .status(200)
            .header("content-type", content_type(&path))
            .header("content-length", length)
            .header("cache-control", cache)
            .header("x-content-type-options", "nosniff")
            .header("x-frame-options", "DENY")
            .header("referrer-policy", "no-referrer")
            .body(Full::new(body).boxed())
            .expect("valid response"),
    )
}

#[cfg(test)]
mod tests {
    use super::clean;

    #[test]
    fn paths_stay_inside() {
        assert_eq!(clean("/").as_deref(), Some("index.html"));
        assert_eq!(clean("/assets//a.js").as_deref(), Some("assets/a.js"));
        assert_eq!(clean("/../etc/passwd"), None);
        assert_eq!(clean("/a/./b"), None);
        assert_eq!(clean("/a\\..\\b"), None);
        assert_eq!(clean("/C:/x"), None);
    }
}
