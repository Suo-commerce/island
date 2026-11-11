//! The MCP helper: tools a coding session uses to write verses.
//!
//! A coding session cannot call the broker's `append` tool by itself: the
//! tool takes a signed submission, and signing takes a key. This helper
//! holds one writer key and offers the session three plain tools:
//! `append_verse`, `read_stream` and `my_capabilities`. It builds the
//! submission, signs it, and hands it to the broker.
//!
//! The helper runs outside the session's sandbox. The session can ask it
//! for a verse; it can never read the key, and it needs no way to the
//! broker of its own. Two ways to load it:
//!
//! - started by the harness, speaking MCP on standard input and output;
//! - started beside the harness with `--listen`, speaking MCP over HTTP
//!   on a local address, for a harness that is itself inside the sandbox
//!   and so cannot start it.
//!
//! One key means one installation: the log shows which helper wrote
//! what, and its grant can be revoked alone. Whoever can reach the
//! helper can write verses with that key, so it listens on loopback
//! unless told otherwise.
//!
//! What the session reads through the helper is verified first: the log
//! is pulled whole and replayed from genesis against the pinned root key.

use std::sync::Arc;

use island_client::wire::Code;
use island_client::{Broker, Error, Signed, Writer, wire};
use island_core::{Body, Evidence, Hash, PublicKey, Record, SigningKey, StreamId, Verse};
use island_log::replay;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{ServerHandler, ServiceExt, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;
use tokio::sync::Mutex;

use crate::keys::{hex, unhex};
use crate::{show, via};

/// How many entries `read_stream` shows when not told.
const DEFAULT_ENTRIES: usize = 20;

/// The most entries `read_stream` shows.
const MAX_ENTRIES: usize = 200;

/// The kinds of evidence a verse can name.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
enum EvidenceName {
    /// Something was decided.
    Decision,
    /// Something was made: a commit, a file, a release.
    Artefact,
    /// Something was measured.
    Measurement,
    /// A question that was open is now closed.
    ClosedQuestion,
}

impl From<EvidenceName> for Evidence {
    fn from(name: EvidenceName) -> Self {
        match name {
            EvidenceName::Decision => Self::Decision,
            EvidenceName::Artefact => Self::Artefact,
            EvidenceName::Measurement => Self::Measurement,
            EvidenceName::ClosedQuestion => Self::ClosedQuestion,
        }
    }
}

/// Input of `append_verse`.
#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct VerseInput {
    /// The stream to write to: the project the verse is about, for
    /// example "island". `my_capabilities` lists the streams this key
    /// may write to.
    stream: String,
    /// What kind of evidence of progress the verse is.
    evidence: EvidenceName,
    /// The verse: what was decided, made, measured or settled, in a
    /// sentence or a few. It is permanent and signed; say what happened,
    /// not what is planned.
    text: String,
    /// For an artefact: what it points at, each as 64 hex digits (a
    /// 32-byte hash). Leave out otherwise.
    #[serde(default)]
    refs: Vec<String>,
}

/// Input of `read_stream`.
#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReadInput {
    /// The stream to read, for example "island".
    stream: String,
    /// How many of its newest entries to show. 20 when left out, at most
    /// 200.
    #[serde(default)]
    entries: Option<usize>,
}

/// What the helper's tools share.
struct Inner {
    url: String,
    writer: Writer,
    root: PublicKey,
    /// The connection to the broker: made at the first call, and again
    /// after it breaks. Holding it is also what makes the helper do one
    /// thing at a time.
    broker: Mutex<Option<Broker>>,
    /// A signed verse that was sent and never answered. Whether it is in
    /// the log is unknown until it is sent again: an exact resend gets
    /// the first answer and adds nothing.
    unsettled: Mutex<Option<Signed>>,
}

/// One call to the broker, on the helper's one connection. A connection
/// that turns out broken is dropped and the call made once more on a new
/// one: every call made here is safe to repeat.
macro_rules! ask {
    ($helper:expr, |$broker:ident| $call:expr) => {{
        let mut slot = $helper.inner.broker.lock().await;
        let mut answer = Err(Error::Unreachable("not tried".to_owned()));
        for _ in 0..2 {
            let $broker = match slot.take() {
                Some(broker) => broker,
                None => match Broker::connect(&$helper.inner.url).await {
                    Ok(broker) => broker,
                    Err(error) => {
                        answer = Err(error);
                        continue;
                    }
                },
            };
            answer = $call.await;
            if matches!(answer, Err(Error::Unreachable(_))) {
                $broker.close().await;
            } else {
                *slot = Some($broker);
                break;
            }
        }
        answer
    }};
}

/// The helper's tools.
#[derive(Clone)]
struct Helper {
    inner: Arc<Inner>,
    tool_router: ToolRouter<Self>,
}

/// A tool answer the session reads as a success. Its first line is also
/// said on standard error, so the helper's terminal shows every call.
fn said(text: String) -> CallToolResult {
    eprintln!(
        "island-cli mcp: {}",
        text.lines().next().unwrap_or_default()
    );
    CallToolResult::success(vec![ContentBlock::text(text)])
}

/// A tool answer the session reads as a failure, also said on standard
/// error.
fn failed(text: String) -> CallToolResult {
    eprintln!(
        "island-cli mcp: failed: {}",
        text.lines().next().unwrap_or_default()
    );
    CallToolResult::error(vec![ContentBlock::text(text)])
}

impl Helper {
    fn new(url: &str, key: SigningKey, root: PublicKey) -> Self {
        Self {
            inner: Arc::new(Inner {
                url: url.to_owned(),
                writer: Writer::new(key),
                root,
                broker: Mutex::new(None),
                unsettled: Mutex::new(None),
            }),
            tool_router: Self::tool_router(),
        }
    }

    /// Hand one signed submission to the broker.
    async fn send(&self, signed: &Signed) -> Result<Result<(u64, Hash), wire::Refusal>, Error> {
        ask!(self, |broker| broker
            .append(&signed.submission, &signed.sig_writer))
    }

    /// This key's grants and next counter.
    async fn capabilities(&self) -> Result<wire::Capabilities, Error> {
        let me = self.inner.writer.public();
        ask!(self, |broker| broker.capabilities(&me))
    }

    /// The whole log, verified from genesis against the pinned root key.
    async fn verified(&self) -> Result<Vec<Record>, String> {
        let records = ask!(self, |broker| broker.whole_log()).map_err(|error| error.to_string())?;
        replay(self.inner.root, &records)
            .map_err(|error| format!("the broker's log does not verify: {error}"))?;
        Ok(records)
    }
}

/// How an answered submission is told to the session.
fn told(stream: &str, answer: Result<(u64, Hash), wire::Refusal>) -> CallToolResult {
    match answer {
        Ok((seq, hash)) => said(format!(
            "written: entry {seq} on stream {stream}, hash {}",
            hex(&hash)
        )),
        Err(refusal) => failed(match (refusal.code, &refusal.verdict) {
            (Code::Gate, Some(verdict)) => format!(
                "{}. The refusal is recorded as entry {}; the verse is not in the log. \
                 Change the verse to meet the policy and send it again.",
                refusal.reason, verdict.seq
            ),
            (Code::Denied, _) => format!(
                "not written: {}. `my_capabilities` lists what this key may write.",
                refusal.reason
            ),
            _ => format!("not written: {}", refusal.reason),
        }),
    }
}

#[tool_router]
impl Helper {
    /// Write one verse.
    #[tool(
        description = "Write a verse to the Island log: one signed, permanent record of \
                       progress on a project. Use it when something was decided, made, \
                       measured or settled, and say what happened in a sentence or a few. \
                       The verse is judged against the log's policy: it must name its kind \
                       of evidence and stay under the policy's length, and an artefact may \
                       need a ref. Answers with the entry's position and hash, or says why \
                       it was refused. Calling again with the same verse after a failure \
                       never writes it twice."
    )]
    async fn append_verse(&self, Parameters(input): Parameters<VerseInput>) -> CallToolResult {
        let Some(refs) = input
            .refs
            .iter()
            .map(|text| unhex::<32>(text))
            .collect::<Option<Vec<Hash>>>()
        else {
            return failed("not written: each ref is 64 hex digits (a 32-byte hash)".to_owned());
        };
        let verse = Verse::new(input.evidence.into(), input.text, refs);
        let stream = input.stream;
        let mut unsettled = self.inner.unsettled.lock().await;

        // First, a verse whose answer never arrived. Sent again exactly,
        // it gets the answer it would have got, and is not written twice.
        let mut before = String::new();
        if let Some(earlier) = unsettled.clone() {
            let Ok(answer) = self.send(&earlier).await else {
                return failed(unreachable_again());
            };
            *unsettled = None;
            let same = earlier.submission.stream == StreamId::from(stream.as_str())
                && earlier.submission.body == Body::Verse(verse.clone());
            if same {
                return told(&stream, answer);
            }
            // Another verse than the one asked for now: say what became
            // of it, then go on.
            before = match answer {
                Ok((seq, _)) => format!("(The earlier verse that got no answer is entry {seq}.) "),
                Err(refusal) => format!(
                    "(The earlier verse that got no answer was not written: {}.) ",
                    refusal.reason
                ),
            };
        }

        // The counter comes from the log. If another process writes with
        // this key at the same moment, the counter has moved: ask again.
        for _ in 0..3 {
            let counter = match self.capabilities().await {
                Ok(mine) => mine.next_counter,
                Err(error) => return failed(format!("{before}not written: {error}")),
            };
            let signed = self.inner.writer.verse(&stream, counter, verse.clone());
            match self.send(&signed).await {
                Err(_) => {
                    *unsettled = Some(signed);
                    return failed(format!("{before}{}", unreachable_again()));
                }
                Ok(Err(refusal))
                    if matches!(
                        refusal.code,
                        Code::CounterOld | Code::CounterGap | Code::CounterConflict
                    ) => {}
                Ok(answer) => {
                    let CallToolResult {
                        content, is_error, ..
                    } = told(&stream, answer);
                    let text = content
                        .first()
                        .and_then(|block| block.as_text())
                        .map_or(String::new(), |text| text.text.clone());
                    let text = format!("{before}{text}");
                    return if is_error == Some(true) {
                        failed(text)
                    } else {
                        said(text)
                    };
                }
            }
        }
        failed(format!(
            "{before}not written: this key's counter keeps moving. Is another process \
             writing with the same key? Each installation should have its own."
        ))
    }

    /// Read one stream.
    #[tool(
        description = "Read the newest entries of one stream of the Island log, oldest first: \
                       position, time, kind, stream, the writer's key (first digits) and the \
                       content. The log is verified from genesis before anything is shown."
    )]
    async fn read_stream(&self, Parameters(input): Parameters<ReadInput>) -> CallToolResult {
        let records = match self.verified().await {
            Ok(records) => records,
            Err(error) => return failed(error),
        };
        let stream = StreamId::from(input.stream.as_str());
        let on_stream: Vec<&Record> = records
            .iter()
            .filter(|record| record.entry.submission.stream == stream)
            .collect();
        let entries = input
            .entries
            .unwrap_or(DEFAULT_ENTRIES)
            .clamp(1, MAX_ENTRIES);
        let shown = &on_stream[on_stream.len().saturating_sub(entries)..];
        let mut text = format!(
            "stream {}: {} entries, showing the newest {}; log verified from genesis ({} entries)\n",
            input.stream,
            on_stream.len(),
            shown.len(),
            records.len()
        );
        for record in shown {
            text.push_str(&show::entry(record));
            text.push('\n');
        }
        said(text)
    }

    /// What this key may write.
    #[tool(
        description = "Show what this installation's key may write to the Island log: its \
                       grants (which kinds on which streams, and whether each is still in \
                       force) and the key itself. Use it to find the streams `append_verse` \
                       can write to."
    )]
    async fn my_capabilities(&self) -> CallToolResult {
        let me = self.inner.writer.public();
        let mine = match self.capabilities().await {
            Ok(mine) => mine,
            Err(error) => return failed(error.to_string()),
        };
        let mut text = format!("writer key  {}\n", hex(&me));
        if mine.grants.is_empty() {
            text.push_str(
                "no grants: this key cannot write yet. The root key grants it with \
                 `island-cli grant --holder <this key>`.\n",
            );
        }
        for grant in &mine.grants {
            let state = match (&grant.revoked_at, &grant.replaced_by) {
                (Some(at), _) => format!("revoked at entry {at}"),
                (_, Some(by)) => format!("replaced by grant {by}"),
                _ => "in force".to_owned(),
            };
            text.push_str(&format!(
                "grant {:<6} may write {} on {}  ({state})\n",
                grant.id,
                grant.rights.join(" "),
                grant.scope.join(" ")
            ));
        }
        text.push_str(&format!("next counter  {}\n", mine.next_counter));
        said(text)
    }
}

fn unreachable_again() -> String {
    "the broker did not answer, so it is not known whether the verse was written. \
     Call append_verse again with the same verse: it will not be written twice."
        .to_owned()
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Helper {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("island", env!("CARGO_PKG_VERSION")))
            .with_instructions(
                "Island is a signed, append-only log of progress across projects. A verse is \
                 one record of evidence: a decision, an artefact, a measurement or a closed \
                 question. Write one with `append_verse` when the work in this session \
                 reaches such a point. Verses are permanent and signed with this \
                 installation's key.",
            )
    }
}

/// Run the helper until its session ends (standard input closes) or it
/// is interrupted.
pub fn run(
    broker: &str,
    key: SigningKey,
    root: PublicKey,
    listen: Option<&str>,
    allow_host: Vec<String>,
) -> Result<(), String> {
    let helper = Helper::new(broker, key, root);
    let me = hex(&helper.inner.writer.public());
    via::runtime()?.block_on(async {
        let Some(address) = listen else {
            // Standard output is the protocol's; anything said goes to
            // standard error.
            eprintln!("island-cli mcp: writer key {me}, broker {broker}");
            let service = helper
                .serve(rmcp::transport::stdio())
                .await
                .map_err(|error| format!("the session did not start: {error}"))?;
            service
                .waiting()
                .await
                .map_err(|error| format!("the session ended badly: {error}"))?;
            return Ok(());
        };

        let listener = tokio::net::TcpListener::bind(address)
            .await
            .map_err(|error| format!("{address}: {error}"))?;
        let bound = listener
            .local_addr()
            .map_err(|error| format!("{address}: {error}"))?;
        // Whoever reaches the helper writes verses with its key: never on
        // every interface at once.
        if bound.ip().is_unspecified() {
            return Err(format!(
                "{address}: the helper does not listen on every interface; give one \
                 address, 127.0.0.1 unless the session must reach it elsewhere"
            ));
        }
        let mut hosts: Vec<String> = ["localhost", "127.0.0.1", "::1"]
            .map(str::to_owned)
            .to_vec();
        let listened = (!bound.ip().is_unspecified()).then(|| bound.ip().to_string());
        for host in listened.into_iter().chain(allow_host) {
            if !hosts.contains(&host) {
                hosts.push(host);
            }
        }

        let shutdown = tokio_util::sync::CancellationToken::new();
        let config = StreamableHttpServerConfig::default()
            .with_legacy_session_mode(false)
            .with_json_response(true)
            .with_allowed_hosts(hosts.clone())
            .with_cancellation_token(shutdown.child_token());
        let service = StreamableHttpService::new(
            move || Ok(helper.clone()),
            Arc::new(LocalSessionManager::default()),
            config,
        );
        let router = axum::Router::new().nest_service("/mcp", service);

        println!("island-cli mcp  http://{bound}/mcp");
        println!("writer key      {me}");
        println!("broker          {broker}");
        println!("answers to      {}", hosts.join(", "));
        axum::serve(listener, router)
            .with_graceful_shutdown(async move {
                let _ = tokio::signal::ctrl_c().await;
                shutdown.cancel();
            })
            .await
            .map_err(|error| format!("serving stopped: {error}"))
    })
}
