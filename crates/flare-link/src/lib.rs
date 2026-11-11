//! Flare link: pulls the Island log to a verified local replica
//! (docs/spec.md, "The replica pull" and build step 8).
//!
//! - [`replica`]: the local file, which only grows, and only by records
//!   that verify.
//! - [`pull`]: bringing it up to the broker's head.
//!
//! The replica pins the root key. Every record is checked against it from
//! genesis: its position, its link to the entry before, its hash, the
//! writer's and the appender's signatures, and every rule of the log
//! (grants, the gate, verdicts judged again). A batch with one bad record
//! in it is refused whole, and so is a broker whose log has gone back or
//! changed.

#![warn(missing_docs)]

pub mod pull;
pub mod replica;

pub use pull::{PullError, Pulled, pull};
pub use replica::{Refused, Replica, ReplicaError, Snapshot};
