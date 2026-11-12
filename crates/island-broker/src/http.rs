//! The broker's HTTP face: the MCP tools at `/mcp`, and the two plain
//! paths a replica pulls from, `GET /log` and `GET /head`.

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::header::HOST;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use island_client::wire::LogQuery;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

use crate::keeper::{Gone, Keeper};
use crate::tools::Tools;

/// The names a request may address the broker by when nothing else is
/// allowed: loopback only.
pub const LOOPBACK: [&str; 3] = ["localhost", "127.0.0.1", "::1"];

/// What the plain paths need.
#[derive(Clone)]
struct Plain {
    keeper: Keeper,
    hosts: Arc<[String]>,
}

/// The broker's routes.
///
/// `hosts` are the names and addresses a request's `Host` header may
/// carry, with or without a port. A request addressed to any other name
/// is refused, which keeps a web page from reaching a broker on loopback
/// or on the private network through a name it controls. `shutdown` ends the MCP
/// service's open requests.
pub fn router(keeper: Keeper, hosts: Vec<String>, shutdown: &CancellationToken) -> Router {
    // Stateless, with plain JSON answers: nothing is kept per client, so
    // a broker restart loses no session.
    let config = StreamableHttpServerConfig::default()
        .with_legacy_session_mode(false)
        .with_json_response(true)
        .with_allowed_hosts(hosts.clone())
        .with_cancellation_token(shutdown.child_token());
    let tools = StreamableHttpService::new(
        {
            let keeper = keeper.clone();
            move || Ok(Tools::new(keeper.clone()))
        },
        Arc::new(LocalSessionManager::default()),
        config,
    );
    Router::new()
        .route("/log", get(log))
        .route("/head", get(head))
        .with_state(Plain {
            keeper,
            hosts: hosts.into(),
        })
        .nest_service("/mcp", tools)
}

/// Serve `router` on `listener` until `shutdown` is cancelled, then let
/// requests in flight finish.
pub async fn serve(
    listener: TcpListener,
    router: Router,
    shutdown: CancellationToken,
) -> std::io::Result<()> {
    axum::serve(listener, router)
        .with_graceful_shutdown(async move { shutdown.cancelled_owned().await })
        .await
}

/// `GET /log?after=<seq>&limit=<n>`: records in order.
async fn log(
    State(plain): State<Plain>,
    headers: HeaderMap,
    Query(query): Query<LogQuery>,
) -> Response {
    if !addressed(&headers, &plain.hosts) {
        return elsewhere();
    }
    match plain.keeper.log(query).await {
        Ok(records) => Json(records).into_response(),
        Err(Gone) => stopping(),
    }
}

/// `GET /head`: the newest entry.
async fn head(State(plain): State<Plain>, headers: HeaderMap) -> Response {
    if !addressed(&headers, &plain.hosts) {
        return elsewhere();
    }
    match plain.keeper.head().await {
        Ok(head) => Json(head).into_response(),
        Err(Gone) => stopping(),
    }
}

fn stopping() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        "the broker is stopping; ask again later\n",
    )
        .into_response()
}

fn elsewhere() -> Response {
    (
        StatusCode::FORBIDDEN,
        "this broker does not answer to that host name\n",
    )
        .into_response()
}

/// Whether the request is addressed to a name the broker answers to.
fn addressed(headers: &HeaderMap, hosts: &[String]) -> bool {
    headers
        .get(HOST)
        .and_then(|host| host.to_str().ok())
        .is_some_and(|authority| host_allowed(authority, hosts))
}

/// Whether `authority` (a `Host` header: a name or address, perhaps with
/// a port) is one of `hosts`. An entry of `hosts` with a port matches
/// only that port; one without matches any.
pub fn host_allowed(authority: &str, hosts: &[String]) -> bool {
    let authority = authority.to_ascii_lowercase();
    // `[::1]:4747` and `[::1]` to `::1`; `name:4747` to `name`.
    let name = match authority.strip_prefix('[') {
        Some(rest) => rest.split_once(']').map_or(rest, |(name, _)| name),
        None => authority
            .rsplit_once(':')
            .filter(|(_, port)| port.bytes().all(|byte| byte.is_ascii_digit()))
            .map_or(authority.as_str(), |(name, _)| name),
    };
    hosts.iter().any(|allowed| {
        let allowed = allowed.to_ascii_lowercase();
        allowed == authority || allowed == name || allowed == format!("[{name}]")
    })
}

#[cfg(test)]
mod tests {
    use super::host_allowed;

    #[test]
    fn a_host_is_matched_with_or_without_its_port() {
        let hosts = [
            "localhost".to_owned(),
            "::1".to_owned(),
            "island.example:4747".to_owned(),
        ];
        for allowed in [
            "localhost",
            "LOCALHOST:4747",
            "[::1]:4747",
            "[::1]",
            "island.example:4747",
        ] {
            assert!(host_allowed(allowed, &hosts), "{allowed}");
        }
        for refused in [
            "evil.example",
            "localhost.evil.example",
            "island.example:80",
            "island.example",
            "",
        ] {
            assert!(!host_allowed(refused, &hosts), "{refused}");
        }
    }
}
