//! A broker for Flare's tests: `/head`, `/log` and the MCP tools
//! `append` and `my_capabilities`, over a log kept in memory.
//!
//! The Flare half may not use the broker crate, even in tests. This one
//! seals what it is given with the appender's key and keeps it only if
//! the log's own rules accept it (`Chain::check`); anything else is
//! refused as invalid. Gate verdicts are not written here: the real
//! broker's tests cover them.

#![allow(dead_code)]

use std::sync::{Arc, Mutex};

use axum::Json;
use axum::extract::{Query, State};
use island_client::codec;
use island_client::wire::{
    AppendRequest, Capabilities, CapabilitiesRequest, Code, DEFAULT_LIMIT, Head, LogQuery,
    MAX_LIMIT, Records, Refusal, Refused, WIRE_VERSION,
};
use island_core::{
    Body, Entry, Grant, Kind, PublicKey, Record, SigningKey, StreamId, Submission, public_key,
    sign_hash,
};
use island_log::{Chain, genesis};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ServerCapabilities, ServerConfig};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{ErrorData, ServerHandler, tool, tool_handler, tool_router};

pub fn root() -> SigningKey {
    SigningKey::from_bytes(&[0x52; 32])
}

pub fn appender() -> SigningKey {
    SigningKey::from_bytes(&[0x41; 32])
}

/// The device key the tests write verses with.
pub fn device() -> SigningKey {
    SigningKey::from_bytes(&[0x11; 32])
}

pub fn pin() -> PublicKey {
    public_key(&root())
}

/// The log: valid from genesis, sealed here.
pub struct Log {
    pub chain: Chain,
    pub records: Vec<Record>,
    at: u64,
}

impl Log {
    /// Genesis, bootstrap, the first policy, and a grant for `device()`
    /// to write verses on `island`: entries 0 to 3.
    pub fn started() -> Self {
        let mut log = Self {
            chain: Chain::new(pin()),
            records: Vec::new(),
            at: 1_794_000_000_000_000,
        };
        let grant = Submission {
            stream: StreamId::from(StreamId::AUTHORITY),
            writer: pin(),
            counter: 4,
            body: Body::Grant(Grant {
                holder: public_key(&device()),
                scope: [StreamId::from("island")].into(),
                rights: [Kind::Verse].into(),
                narrows: None,
            }),
        };
        for submission in [
            genesis::genesis(pin()),
            genesis::bootstrap(pin(), public_key(&appender())),
            genesis::first_policy(pin(), "test"),
            grant,
        ] {
            let signature = sign_hash(&root(), &submission.hash());
            log.append(submission, signature).expect("a valid start");
        }
        log
    }

    /// Seal and keep, if the log's rules accept it.
    pub fn append(
        &mut self,
        submission: Submission,
        sig_writer: island_core::Signature,
    ) -> Result<Record, String> {
        self.at += 1_000_000;
        let record = Record::seal(
            Entry {
                seq: self.chain.next_seq(),
                at: self.at,
                policy: self.chain.policy_seq(),
                prev: self.chain.prev(),
                submission,
                sig_writer,
            },
            &appender(),
        );
        let step = self
            .chain
            .check(&record)
            .map_err(|fault| fault.to_string())?;
        self.chain.advance(&record, step);
        self.records.push(record.clone());
        Ok(record)
    }
}

pub type Shared = Arc<Mutex<Log>>;

#[derive(Clone)]
struct Tools {
    log: Shared,
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl Tools {
    #[tool(description = "append")]
    async fn append(&self, Parameters(request): Parameters<AppendRequest>) -> CallToolResult {
        let refused = |code, reason: String| {
            let refusal = Refused {
                refused: Refusal {
                    code,
                    reason,
                    verdict: None,
                    next_counter: None,
                },
            };
            CallToolResult::structured_error(serde_json::to_value(refusal).expect("JSON"))
        };
        let (submission, signature) = match codec::parse_append(&request) {
            Ok(signed) => signed,
            Err(error) => return refused(Code::Malformed, error.to_string()),
        };
        let mut log = self.log.lock().expect("lock");
        match log.append(submission, signature) {
            Ok(record) => CallToolResult::structured(
                serde_json::to_value(codec::position(record.entry.seq, &record.hash))
                    .expect("JSON"),
            ),
            Err(fault) => refused(Code::Invalid, fault),
        }
    }

    #[tool(description = "my_capabilities")]
    async fn my_capabilities(
        &self,
        Parameters(request): Parameters<CapabilitiesRequest>,
    ) -> Result<rmcp::Json<Capabilities>, ErrorData> {
        let writer = codec::parse_writer(&request.writer)
            .map_err(|error| ErrorData::invalid_params(error.to_string(), None))?;
        let log = self.log.lock().expect("lock");
        Ok(rmcp::Json(Capabilities {
            grants: Vec::new(),
            next_counter: log.chain.next_counter(&writer).unwrap_or(0),
        }))
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Tools {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
    }
}

async fn head(State(log): State<Shared>) -> Json<Head> {
    let log = log.lock().expect("lock");
    let last = log.records.last().expect("a started log");
    Json(Head {
        seq: last.entry.seq,
        hash: codec::b64(&last.hash),
        build: "test".to_owned(),
        wire: WIRE_VERSION,
    })
}

async fn page(State(log): State<Shared>, Query(query): Query<LogQuery>) -> Json<Records> {
    let log = log.lock().expect("lock");
    let from = query.after.map_or(0, |after| {
        usize::try_from(after).map_or(usize::MAX, |a| a.saturating_add(1))
    });
    let rest = log.records.get(from..).unwrap_or(&[]);
    let limit = query.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT) as usize;
    let taken = &rest[..rest.len().min(limit)];
    Json(codec::records(taken, taken.len() < rest.len()))
}

/// Serve `log` on a loopback port of the current runtime; its URL.
pub async fn serve(log: Shared) -> String {
    let tools = StreamableHttpService::new(
        {
            let log = log.clone();
            move || {
                Ok(Tools {
                    log: log.clone(),
                    tool_router: Tools::tool_router(),
                })
            }
        },
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default()
            .with_legacy_session_mode(false)
            .with_json_response(true),
    );
    let router = axum::Router::new()
        .route("/head", axum::routing::get(head))
        .route("/log", axum::routing::get(page))
        .with_state(log)
        .nest_service("/mcp", tools);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let url = format!("http://{}", listener.local_addr().expect("address"));
    tokio::spawn(async move { axum::serve(listener, router).await });
    url
}
