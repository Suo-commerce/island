//! An OpenAI-compatible chat completion: the request, and the streamed
//! answer read back into an [`Assistant`].
//!
//! Reading the stream is ported from Pi, `packages/ai/src/api/
//! openai-completions.ts` (<https://github.com/badlogic/pi-mono>),
//! Copyright (c) 2025 Mario Zechner, MIT License (PI-LICENSE): text,
//! reasoning from the first of `reasoning_content`, `reasoning` and
//! `reasoning_text` that has any, tool calls assembled by their index,
//! and the finish reason mapped to a stop. A stream that names no finish
//! reason stops as Pi's does for providers without one: on tool use if it
//! asked for a tool, otherwise as finished.

use serde_json::{Value, json};

use crate::message::{Assistant, Message, Stop, ToolCall};
use crate::tools::{Spec, openai};

/// The request body: the messages, the tools, streamed. No model is
/// named: the island socket uses its default.
pub fn request(messages: &[Message], tools: &[Spec]) -> Vec<u8> {
    let mut body = json!({
        "messages": messages.iter().map(Message::openai).collect::<Vec<_>>(),
        "stream": true,
        "stream_options": { "include_usage": true },
    });
    if !tools.is_empty() {
        body["tools"] = openai(tools);
    }
    serde_json::to_vec(&body).expect("a JSON value")
}

/// A tool call being assembled from its pieces.
#[derive(Default)]
struct Partial {
    id: String,
    name: String,
    arguments: String,
}

/// The answer as it streams in.
#[derive(Default)]
pub struct Stream {
    pending: Vec<u8>,
    text: String,
    reasoning: String,
    calls: Vec<Partial>,
    finish: Option<String>,
    done: bool,
    /// What the provider sent as an error inside the stream.
    error: Option<String>,
}

impl Stream {
    /// More of the body.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.pending.extend_from_slice(bytes);
        // Whole lines only: a character may be cut between two pieces.
        while let Some(end) = self.pending.iter().position(|&byte| byte == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=end).collect();
            let line = String::from_utf8_lossy(&line);
            self.line(line.trim_end_matches(['\r', '\n']));
        }
    }

    fn line(&mut self, line: &str) {
        let Some(data) = line.strip_prefix("data:") else {
            return;
        };
        let data = data.trim();
        if data == "[DONE]" {
            self.done = true;
            return;
        }
        let Ok(chunk) = serde_json::from_str::<Value>(data) else {
            return;
        };
        self.chunk(&chunk);
    }

    fn chunk(&mut self, chunk: &Value) {
        if let Some(error) = chunk.get("error") {
            self.error = Some(
                error
                    .get("message")
                    .and_then(Value::as_str)
                    .map_or_else(|| error.to_string(), str::to_owned),
            );
            return;
        }
        let Some(choice) = chunk.get("choices").and_then(|c| c.get(0)) else {
            return;
        };
        if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
            self.finish = Some(reason.to_owned());
        }
        let Some(delta) = choice.get("delta").or_else(|| choice.get("message")) else {
            return;
        };
        for field in ["reasoning_content", "reasoning", "reasoning_text"] {
            if let Some(piece) = delta.get(field).and_then(Value::as_str)
                && !piece.is_empty()
            {
                self.reasoning.push_str(piece);
                break;
            }
        }
        if let Some(piece) = delta.get("content").and_then(Value::as_str) {
            self.text.push_str(piece);
        }
        if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
            for call in calls {
                let index = call
                    .get("index")
                    .and_then(Value::as_u64)
                    .map_or(self.calls.len().saturating_sub(1), |i| i as usize);
                while self.calls.len() <= index {
                    self.calls.push(Partial::default());
                }
                let partial = &mut self.calls[index];
                if let Some(id) = call.get("id").and_then(Value::as_str)
                    && partial.id.is_empty()
                {
                    partial.id = id.to_owned();
                }
                let function = call.get("function");
                if let Some(name) = function.and_then(|f| f.get("name")).and_then(Value::as_str)
                    && partial.name.is_empty()
                {
                    partial.name = name.to_owned();
                }
                if let Some(arguments) = function.and_then(|f| f.get("arguments")) {
                    match arguments {
                        Value::String(piece) => partial.arguments.push_str(piece),
                        // Some providers send the arguments as an object.
                        other => partial.arguments = other.to_string(),
                    }
                }
            }
        }
    }

    /// The answer, once the body has ended.
    pub fn finish(mut self) -> Assistant {
        // A last line without a newline.
        let rest = std::mem::take(&mut self.pending);
        self.line(String::from_utf8_lossy(&rest).trim());
        let calls: Vec<ToolCall> = self
            .calls
            .into_iter()
            .enumerate()
            .filter(|(_, partial)| !partial.name.is_empty())
            .map(|(index, partial)| ToolCall {
                id: if partial.id.is_empty() {
                    format!("call_{index}")
                } else {
                    partial.id
                },
                name: partial.name,
                arguments: if partial.arguments.trim().is_empty() {
                    "{}".to_owned()
                } else {
                    partial.arguments
                },
            })
            .collect();
        let stop = if let Some(error) = self.error {
            Stop::Error(error)
        } else {
            match self.finish.as_deref() {
                Some("stop" | "end") => Stop::Stop,
                Some("length") => Stop::Length,
                Some("tool_calls" | "function_call") => Stop::ToolUse,
                Some(other) => Stop::Error(format!("Provider finish_reason: {other}")),
                None if !calls.is_empty() => Stop::ToolUse,
                None => Stop::Stop,
            }
        };
        Assistant {
            text: self.text,
            reasoning: self.reasoning,
            calls,
            stop,
        }
    }
}

/// A whole response body, streamed or not, as an answer.
pub fn read(body: &[u8]) -> Assistant {
    // A provider that did not stream answers with one JSON object.
    if let Ok(whole) = serde_json::from_slice::<Value>(body)
        && whole.is_object()
    {
        let mut stream = Stream::default();
        stream.chunk(&whole);
        return stream.finish();
    }
    let mut stream = Stream::default();
    stream.feed(body);
    stream.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sse(chunks: &[Value]) -> Vec<u8> {
        let mut body = String::new();
        for chunk in chunks {
            body.push_str(&format!("data: {chunk}\n\n"));
        }
        body.push_str("data: [DONE]\n\n");
        body.into_bytes()
    }

    #[test]
    fn text_and_reasoning_stream_in() {
        let body = sse(&[
            json!({"choices": [{"delta": {"reasoning_content": "think "}}]}),
            json!({"choices": [{"delta": {"reasoning_content": "more", "reasoning": "more"}}]}),
            json!({"choices": [{"delta": {"content": "Hellö"}}]}),
            json!({"choices": [{"delta": {"content": " there"}, "finish_reason": "stop"}]}),
            json!({"choices": [], "usage": {"prompt_tokens": 3}}),
        ]);
        // In pieces that cut lines anywhere.
        let mut stream = Stream::default();
        for piece in body.chunks(7) {
            stream.feed(piece);
        }
        let answer = stream.finish();
        assert_eq!(
            answer.text, "Hellö there",
            "a character cut between pieces is whole"
        );
        assert_eq!(
            answer.reasoning, "think more",
            "the first field with any, not both"
        );
        assert_eq!(answer.stop, Stop::Stop);
        assert!(answer.calls.is_empty());
    }

    #[test]
    fn tool_calls_assemble_by_index() {
        let body = sse(&[
            json!({"choices": [{"delta": {"tool_calls": [{"index": 0, "id": "a", "function": {"name": "read", "arguments": "{\"pa"}}]}}]}),
            json!({"choices": [{"delta": {"tool_calls": [{"index": 1, "id": "b", "function": {"name": "bash", "arguments": ""}}]}}]}),
            json!({"choices": [{"delta": {"tool_calls": [{"index": 0, "function": {"arguments": "th\":\"x\"}"}}]}}]}),
            json!({"choices": [{"delta": {}, "finish_reason": "tool_calls"}]}),
        ]);
        let answer = read(&body);
        assert_eq!(answer.stop, Stop::ToolUse);
        assert_eq!(
            answer.calls,
            vec![
                ToolCall {
                    id: "a".into(),
                    name: "read".into(),
                    arguments: "{\"path\":\"x\"}".into()
                },
                ToolCall {
                    id: "b".into(),
                    name: "bash".into(),
                    arguments: "{}".into()
                },
            ]
        );
    }

    #[test]
    fn stops_are_mapped_and_errors_kept() {
        let finish = |reason: &str| {
            read(&sse(&[
                json!({"choices": [{"delta": {"content": "x"}, "finish_reason": reason}]}),
            ]))
            .stop
        };
        assert_eq!(finish("length"), Stop::Length);
        assert_eq!(
            finish("content_filter"),
            Stop::Error("Provider finish_reason: content_filter".into())
        );
        // No finish reason (the stub model sends none).
        assert_eq!(
            read(&sse(&[json!({"choices": [{"delta": {"content": "x"}}]})])).stop,
            Stop::Stop
        );
        let failed = read(&sse(&[json!({"error": {"message": "overloaded"}})]));
        assert_eq!(failed.stop, Stop::Error("overloaded".into()));
        // A provider that did not stream.
        let whole = read(br#"{"choices":[{"message":{"content":"hi"},"finish_reason":"stop"}]}"#);
        assert_eq!((whole.text.as_str(), whole.stop), ("hi", Stop::Stop));
    }

    #[test]
    fn the_request_names_no_model_and_streams() {
        let body: Value = serde_json::from_slice(&request(
            &[Message::User { text: "hi".into() }],
            &crate::tools::specs(),
        ))
        .unwrap();
        assert!(body.get("model").is_none());
        assert_eq!(body["stream"], true);
        let names: Vec<&str> = body["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["function"]["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["read", "write", "edit", "bash", "verse", "say"]);
    }
}
