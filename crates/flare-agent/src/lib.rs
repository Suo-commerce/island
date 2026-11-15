//! Flare's agent (docs/slice-2.md, "The Vessel and the agent", build
//! step 4): the program the Vessel `flare-home` runs. It holds no key, no
//! token and no network. It thinks through the island socket, works in
//! `/work`, and acts only by declaring actions on the Field, which Taso
//! holds and `island-sender` carries.
//!
//! - [`agent_loop`]: model, tools, results, again; ported from Pi.
//! - [`tools`]: `read`, `write`, `edit`, `bash` (ported from Pi) and `say`.
//! - [`openai`]: the chat request and the streamed answer.
//! - [`island`]: the island socket: who am I, the next task, the model.
//! - [`field`]: the Field socket: actions, the transcript, turns.
//! - [`run`]: one run of the prime, task by task.
//!
//! The parts ported from Pi (<https://github.com/badlogic/pi-mono>,
//! Copyright (c) 2025 Mario Zechner) are under the MIT License, in
//! PI-LICENSE; each such file says so.

#![warn(missing_docs)]

pub mod agent_loop;
pub mod field;
pub mod http;
pub mod island;
pub mod message;
pub mod openai;
pub mod run;
pub mod tools;
pub mod truncate;
