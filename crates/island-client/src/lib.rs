//! Island client: what a writer needs on its own side of the broker
//! (docs/spec.md, "Interfaces").
//!
//! - [`Writer`] builds and signs submissions. The key never leaves it.
//! - [`wire`] is the broker's wire format, and [`codec`] goes between it
//!   and Island's own types. The broker uses the same two modules, so the
//!   format has one implementation.
//! - [`Broker`] (feature `net`) calls a broker: its MCP tools, `/log` and
//!   `/head`. [`Plain`] calls only the last two, which is all a replica
//!   needs.
//!
//! - [`approval`] is what approves a Vessel's effect, until slice 2's
//!   approval kind: shared by `island-cli approve` and `island-sender`.
//!
//! Flare and the coding-session tools both build on this crate.

#![warn(missing_docs)]

pub mod approval;
#[cfg(feature = "net")]
pub mod broker;
pub mod codec;
pub mod wire;
mod writer;

#[cfg(feature = "net")]
pub use broker::{Broker, Error, Plain};
pub use codec::Malformed;
pub use writer::{Signed, Writer};
