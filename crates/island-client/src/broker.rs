//! Calls to a broker: its three MCP tools over streamable HTTP, and its
//! two plain paths, `/log` and `/head`.
//!
//! Nothing here is trusted on the broker's word. Records come back
//! decoded, not verified: replay them from genesis against the pinned
//! root key (`island_log::replay`) before believing one.

use std::fmt;
use std::time::Duration;

use island_core::{Hash, PublicKey, Record, Signature, Submission};
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, ClientConfig};
use rmcp::service::RunningService;
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::codec::{self, Malformed};
use crate::wire::{
    Capabilities, CapabilitiesRequest, Head, LogQuery, MAX_LIMIT, Position, ReadStreamRequest,
    Records, Refusal, Refused,
};

/// How long one call may take before it counts as unanswered. A broker
/// answers in milliseconds; this only bounds a wait on one that hangs.
const PATIENCE: Duration = Duration::from_secs(30);

/// Why a call to the broker gave no answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// The broker could not be reached, the connection broke, or no
    /// answer came in time. Nothing is known about what the call did;
    /// every call is safe to make again.
    Unreachable(String),
    /// The broker answered with something this client does not
    /// understand.
    Protocol(String),
    /// The answer held bytes that do not decode.
    Malformed(Malformed),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unreachable(why) => write!(f, "the broker cannot be reached: {why}"),
            Self::Protocol(why) => write!(f, "the broker's answer is not understood: {why}"),
            Self::Malformed(malformed) => write!(f, "the broker's answer: {malformed}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<Malformed> for Error {
    fn from(malformed: Malformed) -> Self {
        Self::Malformed(malformed)
    }
}

/// A connection to one broker.
pub struct Broker {
    base: String,
    http: reqwest::Client,
    mcp: RunningService<rmcp::RoleClient, ClientConfig>,
}

impl Broker {
    /// Connect to the broker at `base`, for example
    /// `http://127.0.0.1:4747`.
    pub async fn connect(base: &str) -> Result<Self, Error> {
        let base = base.trim_end_matches('/').to_owned();
        let transport = StreamableHttpClientTransport::from_config(
            StreamableHttpClientTransportConfig::with_uri(format!("{base}/mcp")),
        );
        let mcp = tokio::time::timeout(PATIENCE, ClientConfig::default().serve(transport))
            .await
            .map_err(|_| Error::Unreachable(format!("{base}: no answer")))?
            .map_err(|error| Error::Unreachable(format!("{base}: {error}")))?;
        let http = reqwest::Client::builder()
            .timeout(PATIENCE)
            .build()
            .map_err(|error| Error::Unreachable(format!("{base}: {error}")))?;
        Ok(Self { base, http, mcp })
    }

    /// Hand a signed submission to the broker. `Ok(Ok(..))` is the entry
    /// it became; `Ok(Err(..))` is a refusal. An exact resend gets the
    /// first answer again, so after an [`Error`] the same submission can
    /// simply be sent again.
    pub async fn append(
        &self,
        submission: &Submission,
        sig_writer: &Signature,
    ) -> Result<Result<(u64, Hash), Refusal>, Error> {
        let request = codec::append_request(submission, sig_writer);
        match self.call::<Position>("append", &request).await? {
            Ok(position) => Ok(Ok(codec::parse_position(&position)?)),
            Err(refusal) => Ok(Err(refusal)),
        }
    }

    /// The records of one stream after position `after`, oldest first,
    /// and whether there are more.
    pub async fn read_stream(
        &self,
        stream: &str,
        after: Option<u64>,
        limit: Option<u32>,
    ) -> Result<(Vec<Record>, bool), Error> {
        let request = ReadStreamRequest {
            stream: stream.to_owned(),
            after,
            limit,
        };
        let records = self.answer::<Records>("read_stream", &request).await?;
        Ok((codec::parse_records(&records)?, records.more))
    }

    /// A writer's grants and the counter its next submission carries.
    pub async fn capabilities(&self, writer: &PublicKey) -> Result<Capabilities, Error> {
        let request = CapabilitiesRequest {
            writer: codec::b64(writer),
        };
        self.answer("my_capabilities", &request).await
    }

    /// The newest entry, and the broker's build.
    pub async fn head(&self) -> Result<Head, Error> {
        self.get("head", &LogQuery::default()).await
    }

    /// The records after position `after`, in order, and whether there
    /// are more.
    pub async fn log(
        &self,
        after: Option<u64>,
        limit: Option<u32>,
    ) -> Result<(Vec<Record>, bool), Error> {
        let records: Records = self.get("log", &LogQuery { after, limit }).await?;
        Ok((codec::parse_records(&records)?, records.more))
    }

    /// Every record from entry 0, pulled page by page.
    pub async fn whole_log(&self) -> Result<Vec<Record>, Error> {
        let mut all: Vec<Record> = Vec::new();
        loop {
            let after = all.last().map(|record| record.entry.seq);
            let (page, more) = self.log(after, Some(MAX_LIMIT)).await?;
            if more && page.is_empty() {
                return Err(Error::Protocol("an empty page with more to come".into()));
            }
            all.extend(page);
            if !more {
                return Ok(all);
            }
        }
    }

    /// Close the connection.
    pub async fn close(self) {
        let _ = self.mcp.cancel().await;
    }

    /// Call a tool whose every answer is a `T`.
    async fn answer<T: DeserializeOwned>(
        &self,
        tool: &'static str,
        input: &impl Serialize,
    ) -> Result<T, Error> {
        self.call(tool, input)
            .await?
            .map_err(|refusal| Error::Protocol(format!("{tool}: {}", refusal.reason)))
    }

    /// Call a tool: a `T`, or the refusal a tool error carries.
    async fn call<T: DeserializeOwned>(
        &self,
        tool: &'static str,
        input: &impl Serialize,
    ) -> Result<Result<T, Refusal>, Error> {
        let serde_json::Value::Object(arguments) = serde_json::to_value(input)
            .map_err(|error| Error::Protocol(format!("{tool}: {error}")))?
        else {
            return Err(Error::Protocol(format!(
                "{tool}: its input is not an object"
            )));
        };
        let call = self
            .mcp
            .call_tool(CallToolRequestParams::new(tool).with_arguments(arguments));
        let result = tokio::time::timeout(PATIENCE, call)
            .await
            .map_err(|_| Error::Unreachable(format!("{tool}: no answer")))?
            .map_err(|error| match error {
                // The broker answered, with an error of the protocol.
                rmcp::ServiceError::McpError(error) => {
                    Error::Protocol(format!("{tool}: {}", error.message))
                }
                other => Error::Unreachable(format!("{tool}: {other}")),
            })?;
        let content = result
            .structured_content
            .ok_or_else(|| Error::Protocol(format!("{tool}: no structured answer")))?;
        let unread = |error: serde_json::Error| Error::Protocol(format!("{tool}: {error}"));
        if result.is_error == Some(true) {
            let refused: Refused = serde_json::from_value(content).map_err(unread)?;
            return Ok(Err(refused.refused));
        }
        serde_json::from_value(content).map(Ok).map_err(unread)
    }

    /// `GET` one of the plain paths.
    async fn get<T: DeserializeOwned>(
        &self,
        path: &'static str,
        query: &LogQuery,
    ) -> Result<T, Error> {
        let mut url = format!("{}/{path}", self.base);
        let query: Vec<String> = [
            ("after", query.after),
            ("limit", query.limit.map(u64::from)),
        ]
        .into_iter()
        .filter_map(|(name, value)| value.map(|value| format!("{name}={value}")))
        .collect();
        if !query.is_empty() {
            url = format!("{url}?{}", query.join("&"));
        }
        let response = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|error| Error::Unreachable(format!("{url}: {error}")))?;
        if !response.status().is_success() {
            return Err(Error::Protocol(format!("{url}: {}", response.status())));
        }
        response
            .json()
            .await
            .map_err(|error| Error::Protocol(format!("{url}: {error}")))
    }
}
