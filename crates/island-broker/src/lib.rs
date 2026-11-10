//! Island broker: the log's network face, in one process with the
//! appender (docs/spec.md, "Interfaces" and build step 7).
//!
//! - [`keeper`]: the one thread that owns the appender and every record.
//! - [`tools`]: the MCP tools `append`, `read_stream`, `my_capabilities`.
//! - [`http`]: the routes: the tools at `/mcp`, and `GET /log` and
//!   `GET /head` for replicas.
//!
//! The broker holds the appender's key and no other. A writer signs its
//! own submissions; the signature is the only credential the broker
//! asks for, and reads are open to whoever can reach it. It is meant to
//! be reached over loopback or the tailnet, never from the open network.

#![warn(missing_docs)]

pub mod http;
pub mod keeper;
pub mod tools;

/// Where the broker listens when it is not told otherwise.
pub const DEFAULT_LISTEN: &str = "127.0.0.1:4747";
