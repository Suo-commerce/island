//! The broker's MCP tools (docs/spec.md, "Interfaces").
//!
//! Three tools, and no credential but the signature inside a submission:
//! `append` takes any kind of signed submission, the root key's grants
//! and policies as well as a device's verses; `read_stream` and
//! `my_capabilities` are open to whoever can reach the broker.

use island_client::codec;
use island_client::wire::{
    AppendRequest, Capabilities, CapabilitiesRequest, Code, ReadStreamRequest, Records, Refusal,
    Refused,
};
use island_log::Rejected;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, Implementation, ServerCapabilities, ServerConfig};
use rmcp::{ErrorData, Json, ServerHandler, tool, tool_handler, tool_router};

use crate::keeper::{Gone, Keeper};

/// The tools, asking one keeper.
#[derive(Clone)]
pub struct Tools {
    keeper: Keeper,
    tool_router: ToolRouter<Self>,
}

impl Tools {
    /// Tools that ask `keeper`.
    pub fn new(keeper: Keeper) -> Self {
        Self {
            keeper,
            tool_router: Self::tool_router(),
        }
    }
}

/// What a writer is told when its submission did not become an entry.
pub fn refusal(rejected: &Rejected) -> Refusal {
    let plain = |code| (code, None, None);
    let (code, verdict, next_counter) = match rejected {
        Rejected::Stopped | Rejected::Storage(_) => plain(Code::Unavailable),
        Rejected::BadSignature => plain(Code::BadSignature),
        Rejected::CounterOld { expected, .. } => (Code::CounterOld, None, *expected),
        Rejected::CounterGap { expected, .. } => (Code::CounterGap, None, *expected),
        Rejected::CounterConflict { counter } => {
            (Code::CounterConflict, None, counter.checked_add(1))
        }
        Rejected::Refused { seq, hash, .. } => {
            (Code::Gate, Some(codec::position(*seq, hash)), None)
        }
        Rejected::Denied(_) => plain(Code::Denied),
        Rejected::VerdictFromOutside => plain(Code::VerdictFromOutside),
        Rejected::Invalid(_) => plain(Code::Invalid),
        Rejected::TooLarge => plain(Code::TooLarge),
    };
    let reason = match rejected {
        Rejected::Refused { reason, .. } => {
            format!("refused by the gate: {}", codec::gate_reason(reason))
        }
        other => other.to_string(),
    };
    Refusal {
        code,
        reason,
        verdict,
        next_counter,
    }
}

/// The refusal for a request the keeper never answered.
fn unavailable() -> Refusal {
    Refusal {
        code: Code::Unavailable,
        reason: "the broker is not writing right now; send the same submission again later"
            .to_owned(),
        verdict: None,
        next_counter: None,
    }
}

/// A refusal as a tool error.
fn refused(refusal: Refusal) -> CallToolResult {
    let value = serde_json::to_value(Refused { refused: refusal }).unwrap_or_default();
    CallToolResult::structured_error(value)
}

fn gone(_: Gone) -> ErrorData {
    ErrorData::internal_error("the broker is stopping; ask again later", None)
}

#[tool_router]
impl Tools {
    /// The write path.
    #[tool(
        description = "Append one signed submission to the Island log. `submission` is the \
                       submission's canonical bytes and `sig_writer` its writer's ed25519 \
                       signature over the submission hash, both base64. Answers `{seq, hash}` \
                       for the entry it became. A refusal is a tool error holding \
                       `{refused: {code, reason}}`; when the gate refused it, `verdict` names \
                       the entry that records the refusal. An exact resend gets the first \
                       answer again and adds nothing."
    )]
    async fn append(
        &self,
        Parameters(request): Parameters<AppendRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        let (submission, sig_writer) = match codec::parse_append(&request) {
            Ok(signed) => signed,
            Err(malformed) => {
                return Ok(refused(Refusal {
                    code: Code::Malformed,
                    reason: malformed.to_string(),
                    verdict: None,
                    next_counter: None,
                }));
            }
        };
        Ok(match self.keeper.append(submission, sig_writer).await {
            Ok(Ok(ack)) => {
                let position = codec::position(ack.seq, &ack.hash);
                CallToolResult::structured(serde_json::to_value(position).unwrap_or_default())
            }
            Ok(Err(rejected)) => refused(refusal(&rejected)),
            Err(Gone) => refused(unavailable()),
        })
    }

    /// One stream's records.
    #[tool(
        description = "Read the records of one stream of the Island log, oldest first, as \
                       base64 of each record's bytes. `after` is the position of the last \
                       record already held; `limit` is at most 500. `more` says whether \
                       records remain. Records are to be verified by replaying the log from \
                       genesis, not trusted as returned."
    )]
    async fn read_stream(
        &self,
        Parameters(request): Parameters<ReadStreamRequest>,
    ) -> Result<Json<Records>, ErrorData> {
        self.keeper.stream(request).await.map(Json).map_err(gone)
    }

    /// A writer's grants.
    #[tool(
        description = "List the grants a writer key holds or held in the Island log, and the \
                       counter its next submission must carry. `writer` is the ed25519 public \
                       key, base64. A grant with `revoked_at` or `replaced_by` is no longer \
                       in force."
    )]
    async fn my_capabilities(
        &self,
        Parameters(request): Parameters<CapabilitiesRequest>,
    ) -> Result<Json<Capabilities>, ErrorData> {
        let writer = codec::parse_writer(&request.writer)
            .map_err(|malformed| ErrorData::invalid_params(malformed.to_string(), None))?;
        self.keeper
            .capabilities(writer)
            .await
            .map(Json)
            .map_err(gone)
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Tools {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(
                "island-broker",
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(
                "The Island log's broker. Writes are signed submissions; the signature is the \
                 only credential. Reads are open.",
            )
    }
}
