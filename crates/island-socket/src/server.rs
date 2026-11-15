//! The socket's HTTP face (docs/slice-2.md, "The island socket"). Every
//! request is judged by its connection's peer uid; none carries a token.
//!
//! | Request                     | Answer                                    |
//! | --------------------------- | ----------------------------------------- |
//! | `GET /whoami`               | who the caller is                         |
//! | `GET /v1/models`            | the models it may name                    |
//! | `POST /v1/chat/completions` | the model's answer, streamed as it comes  |
//! | `GET /instructions`         | the agent's instructions, from the config's file; 404 if there is none |
//! | `GET /task?after=N`         | the next message said to the agent after N, from `flare-server`, or 204 after waiting up to 50 s (the prime only) |
//!
//! The rest of the table (tasks, log reads, repos, notes) comes with the
//! steps that use it; until then it is a 404.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::connect_info::{ConnectInfo, Connected};
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use futures_util::StreamExt;
use serde_json::{Value, json};

use crate::access::{Batches, CallDigest};
use crate::callers::{Caller, Callers, Role};
use crate::config::Config;
use crate::usage::{Usage, today};

/// The largest request body: a long conversation, not a file upload.
pub const MAX_REQUEST: usize = 8 << 20;

/// What every request shares.
pub struct Shared {
    /// The config.
    pub config: Config,
    /// API keys by provider, read at start.
    pub keys: std::collections::BTreeMap<String, String>,
    /// The callers, and when their file was last read.
    pub callers: Mutex<(Callers, Option<SystemTime>)>,
    /// Today's usage.
    pub usage: Mutex<Usage>,
    /// Calls not yet in an `access` entry.
    pub batches: Arc<tokio::sync::Mutex<Batches>>,
    /// The client to the providers.
    pub client: crate::upstream::Client,
    /// The Warden config's path, for re-reading.
    pub warden_config: PathBuf,
}

/// A connection's peer.
#[derive(Clone, Copy, Debug)]
pub struct Peer {
    /// Its uid, as the kernel says.
    pub uid: Option<u32>,
}

impl Connected<axum::serve::IncomingStream<'_, tokio::net::UnixListener>> for Peer {
    fn connect_info(stream: axum::serve::IncomingStream<'_, tokio::net::UnixListener>) -> Self {
        Self {
            uid: stream.io().peer_cred().ok().map(|cred| cred.uid()),
        }
    }
}

/// The routes.
pub fn router(shared: Arc<Shared>) -> Router {
    Router::new()
        .route("/whoami", get(whoami))
        .route("/v1/models", get(models))
        .route("/v1/chat/completions", post(chat))
        .route("/task", get(task))
        .route("/instructions", get(instructions))
        .fallback(not_here)
        .layer(DefaultBodyLimit::max(MAX_REQUEST))
        .with_state(shared)
}

/// An error in the shape OpenAI clients read.
fn error(status: StatusCode, kind: &str, message: impl Into<String>) -> Response {
    let body = json!({ "error": { "message": message.into(), "type": kind } });
    (status, axum::Json(body)).into_response()
}

impl Shared {
    /// The caller of a connection; the Warden config is read again when
    /// it changed. The refusal is the response itself, which every
    /// handler returns as it is.
    #[allow(clippy::result_large_err)]
    fn caller(&self, peer: Peer) -> Result<Caller, Response> {
        let refused = || error(StatusCode::FORBIDDEN, "forbidden", "not a Vessel's uid");
        let uid = peer.uid.ok_or_else(refused)?;
        let mut callers = self.callers.lock().expect("not poisoned");
        let modified = std::fs::metadata(&self.warden_config)
            .and_then(|meta| meta.modified())
            .ok();
        if modified != callers.1 {
            match Callers::load(&self.warden_config) {
                Ok(fresh) => *callers = (fresh, modified),
                // Keep the last good reading; say why.
                Err(why) => eprintln!("island-socket: the Warden config: {why}"),
            }
        }
        let caller = callers.0.of_uid(uid).cloned().ok_or_else(refused)?;
        drop(callers);
        if caller.role == Role::Fork {
            self.usage
                .lock()
                .expect("not poisoned")
                .saw_fork(&caller.prime, caller.slot);
        }
        Ok(caller)
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

async fn whoami(
    State(shared): State<Arc<Shared>>,
    ConnectInfo(peer): ConnectInfo<Peer>,
) -> Response {
    let caller = match shared.caller(peer) {
        Ok(caller) => caller,
        Err(refusal) => return refusal,
    };
    let forks_left = match caller.role {
        Role::Fork => 0,
        Role::Prime => caller.fork_slots.len().saturating_sub(
            shared
                .usage
                .lock()
                .expect("not poisoned")
                .forks_seen(&caller.prime),
        ),
    };
    let (_, _, default_model) = shared.config.route(None).expect("checked at load");
    axum::Json(json!({
        "vessel": hex(&caller.vessel),
        "subject": caller.subject,
        "role": caller.role.name(),
        "slot": caller.slot,
        "prime": caller.prime,
        "forks_left": forks_left,
        "default_model": default_model,
    }))
    .into_response()
}

async fn models(
    State(shared): State<Arc<Shared>>,
    ConnectInfo(peer): ConnectInfo<Peer>,
) -> Response {
    if let Err(refusal) = shared.caller(peer) {
        return refusal;
    }
    let data: Vec<Value> = shared
        .config
        .models()
        .into_iter()
        .map(|(provider, model)| json!({ "id": model, "object": "model", "owned_by": provider }))
        .collect();
    axum::Json(json!({ "object": "list", "data": data })).into_response()
}

async fn not_here(ConnectInfo(_peer): ConnectInfo<Peer>) -> Response {
    error(
        StatusCode::NOT_FOUND,
        "not_found",
        "not served (yet): this socket answers /whoami, /v1/models, /v1/chat/completions, /task and /instructions",
    )
}

/// What a call adds up to, written when its response ends, however it
/// ends: complete, cut short by the provider, or dropped by the agent.
struct Accounting {
    shared: Arc<Shared>,
    vessel: island_core::VesselId,
    provider: String,
    bytes_in: u64,
    bytes_out: u64,
    digest: CallDigest,
}

impl Accounting {
    fn saw(&mut self, bytes: &[u8]) {
        self.bytes_out += bytes.len() as u64;
        self.digest.update(bytes);
    }
}

impl Drop for Accounting {
    fn drop(&mut self) {
        let total = self.bytes_in.saturating_add(self.bytes_out);
        self.shared.usage.lock().expect("not poisoned").spend(
            today(),
            self.vessel,
            &self.provider,
            total,
        );
        let digest = self.digest.finish();
        let (shared, vessel, provider) = (
            self.shared.clone(),
            self.vessel,
            std::mem::take(&mut self.provider),
        );
        let (bytes_in, bytes_out) = (self.bytes_in, self.bytes_out);
        // Drop may run outside the runtime's context only in tests that
        // end the runtime; then the call is lost from the batch, said.
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn(async move {
                    shared.batches.lock().await.add(
                        vessel,
                        island_core::AccessKind::Inference,
                        &provider,
                        bytes_in,
                        bytes_out,
                        digest,
                        true,
                    );
                });
            }
            Err(_) => eprintln!("island-socket: a call ended after the runtime; not batched"),
        }
    }
}

async fn chat(
    State(shared): State<Arc<Shared>>,
    ConnectInfo(peer): ConnectInfo<Peer>,
    request: Bytes,
) -> Response {
    let caller = match shared.caller(peer) {
        Ok(caller) => caller,
        Err(refusal) => return refusal,
    };
    let Ok(Value::Object(mut body)) = serde_json::from_slice::<Value>(&request) else {
        return error(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "the body is not a JSON object",
        );
    };
    let named = match body.get("model") {
        None | Some(Value::Null) => None,
        Some(Value::String(model)) => Some(model.clone()),
        Some(_) => {
            return error(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                "`model` is not a string",
            );
        }
    };
    let Some((provider_name, provider, model)) = shared.config.route(named.as_deref()) else {
        return error(
            StatusCode::NOT_FOUND,
            "model_not_found",
            format!(
                "no provider here lists {:?}; GET /v1/models says which can be named",
                named.unwrap_or_default()
            ),
        );
    };
    let (provider_name, model) = (provider_name.to_owned(), model.to_owned());
    if let Err(over) = shared.usage.lock().expect("not poisoned").admit(
        today(),
        caller.vessel,
        &provider_name,
        provider.budget,
    ) {
        return error(
            StatusCode::TOO_MANY_REQUESTS,
            "budget_exceeded",
            format!(
                "today's budget with {provider_name} is spent: {} of {} calls, {} of {} bytes",
                over.spent.calls, over.budget.calls, over.spent.bytes, over.budget.bytes
            ),
        );
    }
    // The agent's bytes go on as they are, unless it named no model.
    let forwarded = if named.is_none() {
        body.insert("model".to_owned(), Value::String(model));
        Bytes::from(serde_json::to_vec(&body).expect("a JSON object"))
    } else {
        request.clone()
    };
    let mut accounting = Accounting {
        shared: shared.clone(),
        vessel: caller.vessel,
        provider: provider_name.clone(),
        bytes_in: request.len() as u64,
        bytes_out: 0,
        digest: CallDigest::new(&request),
    };
    let url = format!(
        "{}/chat/completions",
        provider.base_url.trim_end_matches('/')
    );
    let key = shared.keys.get(&provider_name).map(String::as_str);
    let answer = match shared.client.post_json(&url, key, forwarded).await {
        Ok(answer) => answer,
        Err(why) => {
            // A call made and not answered counts; the agent gets, and
            // the digest covers, this error.
            let response = error(
                StatusCode::BAD_GATEWAY,
                "provider_unreachable",
                format!("{provider_name} did not answer: {why}"),
            );
            let (parts, body) = response.into_parts();
            let bytes = axum::body::to_bytes(body, MAX_REQUEST)
                .await
                .unwrap_or_default();
            accounting.saw(&bytes);
            return Response::from_parts(parts, Body::from(bytes));
        }
    };
    let status = answer.status;
    let mut headers = HeaderMap::new();
    if let Some(kind) = answer.headers.get(header::CONTENT_TYPE) {
        headers.insert(header::CONTENT_TYPE, kind.clone());
    }
    headers.insert(
        "x-island-provider",
        HeaderValue::from_str(&provider_name).unwrap_or(HeaderValue::from_static("?")),
    );
    let accounting = Arc::new(Mutex::new(accounting));
    let stream = answer.body.map(move |chunk| {
        let chunk = chunk?;
        accounting.lock().expect("not poisoned").saw(&chunk);
        Ok::<Bytes, std::io::Error>(chunk)
    });
    let mut response = Response::new(Body::from_stream(stream));
    *response.status_mut() = status;
    *response.headers_mut() = headers;
    response
}

/// How long a task request waits for a message, in seconds.
pub const TASK_WAIT: u64 = 50;

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskQuery {
    after: Option<u64>,
}

/// The next message said to the agent, from `flare-server`'s agent socket.
async fn task(
    State(shared): State<Arc<Shared>>,
    ConnectInfo(peer): ConnectInfo<Peer>,
    uri: axum::http::Uri,
) -> Response {
    let caller = match shared.caller(peer) {
        Ok(caller) => caller,
        Err(refusal) => return refusal,
    };
    if caller.role == Role::Fork {
        return error(
            StatusCode::FORBIDDEN,
            "forbidden",
            "tasks are the prime's; a fork takes its task from its files",
        );
    }
    let query: TaskQuery = match serde_urlencoded_query(uri.query()) {
        Ok(query) => query,
        Err(why) => return error(StatusCode::BAD_REQUEST, "invalid_request_error", why),
    };
    let target = uri
        .path_and_query()
        .map_or_else(|| "/task".to_owned(), |path| path.as_str().to_owned());
    let upstream = match query.after {
        Some(after) => format!("/task?after={after}&wait={TASK_WAIT}"),
        None => format!("/task?wait={TASK_WAIT}"),
    };
    let (status, body) = match crate::upstream::get_unix(&shared.config.tasks, &upstream).await {
        Ok(answer) => answer,
        Err(why) => {
            return error(
                StatusCode::SERVICE_UNAVAILABLE,
                "tasks_unavailable",
                format!("the session server: {why}"),
            );
        }
    };
    let urgent = status == StatusCode::OK;
    let digest = crate::access::task_digest(target.as_bytes(), &body);
    shared.batches.lock().await.add(
        caller.vessel,
        island_core::AccessKind::Task,
        "",
        target.len() as u64,
        body.len() as u64,
        digest,
        urgent,
    );
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = status;
    if urgent {
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
    }
    response
}

/// `after=N` or nothing.
fn serde_urlencoded_query(query: Option<&str>) -> Result<TaskQuery, String> {
    let mut after = None;
    for pair in query
        .unwrap_or_default()
        .split('&')
        .filter(|pair| !pair.is_empty())
    {
        match pair.split_once('=') {
            Some(("after", value)) => {
                after = Some(
                    value
                        .parse()
                        .map_err(|_| format!("after: {value:?} is not a number"))?,
                );
            }
            _ => return Err(format!("{pair:?}: a task request takes after=N only")),
        }
    }
    Ok(TaskQuery { after })
}

/// The agent's instructions: the file named in the config, read now, so
/// an edit on the server takes effect at the agent's next task. Counted as
/// a read; the agent records the text's digest in its transcript.
async fn instructions(
    State(shared): State<Arc<Shared>>,
    ConnectInfo(peer): ConnectInfo<Peer>,
) -> Response {
    let caller = match shared.caller(peer) {
        Ok(caller) => caller,
        Err(refusal) => return refusal,
    };
    let text = match tokio::fs::read(&shared.config.instructions).await {
        Ok(text) => text,
        Err(why) if why.kind() == std::io::ErrorKind::NotFound => {
            return error_response_not_found(&shared.config.instructions);
        }
        Err(why) => {
            return error(
                StatusCode::SERVICE_UNAVAILABLE,
                "instructions_unavailable",
                format!("{}: {why}", shared.config.instructions.display()),
            );
        }
    };
    let target = b"/instructions";
    let digest = crate::access::task_digest(target, &text);
    shared.batches.lock().await.add(
        caller.vessel,
        island_core::AccessKind::Read,
        "",
        target.len() as u64,
        text.len() as u64,
        digest,
        true,
    );
    let mut response = Response::new(Body::from(text));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/markdown; charset=utf-8"),
    );
    response
}

fn error_response_not_found(path: &std::path::Path) -> Response {
    error(
        StatusCode::NOT_FOUND,
        "not_found",
        format!(
            "no instructions at {}: the agent uses its own",
            path.display()
        ),
    )
}
