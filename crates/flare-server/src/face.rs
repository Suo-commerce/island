//! The face: the page `flare-server` serves from itself (docs/slice-2.md,
//! "Session protocol, version 2", "The face").
//!
//! A browser client needs the server to serve the page and to accept that
//! one origin. So, with `--face DIR`:
//!
//! - every `GET` that is not `/session` or `/log` answers with a file of
//!   `DIR`; a path that names no file and has no extension is one of the
//!   page's own routes and answers with `index.html`;
//! - `GET /log?entries=N` is the log as the page reads it: the head and
//!   the newest entries, from the server's verified replica. A `see`
//!   would do, but a `see` is an event of the shared session, and a page
//!   that loads is not something every device needs to hear of. What it
//!   returns is the server's word: the page marks it so until it checks
//!   signatures itself.
//!
//! Both answer only to the host names the server answers to, like
//! `/session`: a page of another origin cannot read them (no CORS
//! header is ever sent), and a name that merely resolves here is refused.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_SECURITY_POLICY, CONTENT_TYPE};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use flare_core::session::Place;
use flare_core::{Line, hex, kind_name};
use serde::{Deserialize, Serialize};

use crate::{Server, addressed};

/// The most entries one `/log` answers with.
pub const MAX_LOG_ENTRIES: usize = 200;

/// How many entries `/log` answers with when it is not told.
pub const LOG_ENTRIES: usize = 20;

/// One entry, as the page shows it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct LogLine {
    /// The entry's position.
    pub seq: u64,
    /// When the appender ordered it, in microseconds since the epoch.
    pub at: u64,
    /// Its kind, as `see` names it.
    pub kind: &'static str,
    /// Its stream.
    pub stream: String,
    /// Who wrote it: the key, in hex.
    pub writer: String,
    /// What it says, as `see` shows it.
    pub content: String,
}

impl LogLine {
    /// The line for the page.
    pub fn of(line: &Line) -> Self {
        Self {
            seq: line.seq,
            at: line.at,
            kind: kind_name(line.kind),
            stream: line.stream.clone(),
            writer: hex(&line.writer),
            content: line.content.clone(),
        }
    }
}

/// What `/log` answers with.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct LogView {
    /// The newest entry's position and hash; `None`: the log is empty.
    pub head: Option<Place>,
    /// How many entries the server's replica holds.
    pub entries: u64,
    /// The root key the replica is verified against, in hex.
    pub root: String,
    /// Why the replica could not be brought up to date, if it could not:
    /// what follows is then the log as the server last saw it.
    pub problem: Option<String>,
    /// The newest entries, newest first.
    pub recent: Vec<LogLine>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LogQuery {
    entries: Option<usize>,
}

fn refused() -> Response {
    (
        StatusCode::FORBIDDEN,
        "this server does not answer to that host name\n",
    )
        .into_response()
}

/// `GET /log?entries=N`.
pub(crate) async fn log(
    State(server): State<Arc<Server>>,
    headers: HeaderMap,
    Query(query): Query<LogQuery>,
) -> Response {
    if addressed(&server, &headers).is_none() {
        return refused();
    }
    // Another site's page cannot read the answer, but it could still ask;
    // a browser says when a request comes from elsewhere.
    let elsewhere = headers
        .get("sec-fetch-site")
        .and_then(|site| site.to_str().ok())
        .is_some_and(|site| site != "same-origin" && site != "none");
    if elsewhere {
        return (
            StatusCode::FORBIDDEN,
            "the log is read by this server's own page\n",
        )
            .into_response();
    }
    let entries = query.entries.unwrap_or(LOG_ENTRIES).min(MAX_LOG_ENTRIES);
    let mut response = axum::Json(server.log(entries).await).into_response();
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// The content type of a file of the page, by its extension. A file of
/// any other kind is not served.
pub fn content_type(path: &Path) -> Option<&'static str> {
    Some(match path.extension()?.to_str()? {
        "html" => "text/html; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json",
        "webmanifest" => "application/manifest+json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "txt" => "text/plain; charset=utf-8",
        _ => return None,
    })
}

/// The file of the page a request path names, under `root`: the path's
/// own file, or `index.html` for a path without an extension (one of the
/// page's routes). `None`: the path names nothing this server would
/// serve. Only plain names are taken: letters, digits, `.`, `-` and `_`,
/// none starting with a dot.
pub fn resolve(root: &Path, path: &str) -> Option<PathBuf> {
    let relative = path.strip_prefix('/')?;
    let plain = |name: &str| {
        !name.is_empty()
            && !name.starts_with('.')
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    };
    let names: Vec<&str> = relative
        .split('/')
        .filter(|name| !name.is_empty())
        .collect();
    if !names.iter().all(|name| plain(name)) {
        return None;
    }
    let named: PathBuf = names.iter().collect();
    if named
        .components()
        .any(|part| !matches!(part, Component::Normal(_)))
    {
        return None;
    }
    if named.extension().is_some() {
        return Some(root.join(named));
    }
    Some(root.join("index.html"))
}

/// What the page may load and talk to: itself, and the session at the
/// address it was served from.
fn policy(host: &str) -> String {
    format!(
        "default-src 'self'; connect-src 'self' ws://{host}; \
         style-src 'self' 'unsafe-inline'; img-src 'self' data:; \
         base-uri 'none'; form-action 'none'; frame-ancestors 'none'"
    )
}

/// A file of the page.
pub(crate) async fn file(
    State(server): State<Arc<Server>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    let Some(host) = addressed(&server, &headers) else {
        return refused();
    };
    if method != Method::GET && method != Method::HEAD {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    let Some(root) = server.config.face.as_deref() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let found = async {
        let path = resolve(root, uri.path())?;
        let kind = content_type(&path)?;
        // A link out of the directory is not followed.
        let real = tokio::fs::canonicalize(&path).await.ok()?;
        let home = tokio::fs::canonicalize(root).await.ok()?;
        if !real.starts_with(&home) {
            return None;
        }
        let bytes = tokio::fs::read(&real).await.ok()?;
        Some((path, kind, bytes))
    }
    .await;
    let Some((path, kind, bytes)) = found else {
        return (StatusCode::NOT_FOUND, "no such file\n").into_response();
    };
    // Built files carry their content's hash in their name and never
    // change; the page itself is asked for again each time.
    let hashed = path
        .parent()
        .and_then(Path::file_name)
        .is_some_and(|dir| dir == "assets");
    let cache = if hashed {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    let body = if method == Method::HEAD {
        Vec::new()
    } else {
        bytes
    };
    let mut response = body.into_response();
    let headers = response.headers_mut();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static(kind));
    headers.insert(CACHE_CONTROL, HeaderValue::from_static(cache));
    headers.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    headers.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    if let Ok(policy) = HeaderValue::from_str(&policy(&host)) {
        headers.insert(CONTENT_SECURITY_POLICY, policy);
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_names_a_file_a_route_or_nothing() {
        let root = Path::new("/srv/face");
        let at = |path: &str| resolve(root, path);
        assert_eq!(at("/"), Some(root.join("index.html")));
        assert_eq!(at("/session"), Some(root.join("index.html")));
        assert_eq!(at("/island/ledger"), Some(root.join("index.html")));
        assert_eq!(
            at("/assets/index-B1x_9.js"),
            Some(root.join("assets/index-B1x_9.js"))
        );
        assert_eq!(at("/fonts/plex.woff2"), Some(root.join("fonts/plex.woff2")));
        // Nothing outside the directory, nothing hidden, nothing encoded.
        for path in [
            "/../etc/passwd",
            "/assets/../../key",
            "/.env",
            "/assets/.hidden.js",
            "/%2e%2e/key",
            "/a b.js",
            "/a\\b.js",
            "session",
            "",
        ] {
            assert_eq!(at(path), None, "{path}");
        }
    }

    #[test]
    fn only_the_pages_kinds_of_file_are_served() {
        assert!(content_type(Path::new("index.html")).is_some());
        assert!(content_type(Path::new("a.woff2")).is_some());
        assert_eq!(content_type(Path::new("a.key")), None);
        assert_eq!(content_type(Path::new("a.sh")), None);
        assert_eq!(content_type(Path::new("index")), None);
    }

    #[test]
    fn the_policy_names_the_address_the_page_came_from() {
        let policy = policy("10.77.0.1:4749");
        assert!(policy.contains("connect-src 'self' ws://10.77.0.1:4749;"));
        assert!(policy.contains("default-src 'self'"));
    }
}
