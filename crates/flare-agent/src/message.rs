//! The conversation: what the model was told and what it answered, kept
//! in `/work/.flare` between turns, and turned into an OpenAI-compatible
//! chat request.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Why the model stopped.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stop {
    /// It finished.
    Stop,
    /// It hit the output limit: its tool calls may be cut short.
    Length,
    /// It asked for tools.
    ToolUse,
    /// The provider said it failed.
    Error(String),
}

/// A tool call the model asked for.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCall {
    /// Its id, which the result names.
    pub id: String,
    /// The tool.
    pub name: String,
    /// Its arguments, JSON as the model wrote them.
    pub arguments: String,
}

/// What the model answered.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Assistant {
    /// Its text.
    pub text: String,
    /// Its reasoning, if the provider sent it apart. Kept in the
    /// transcript; not sent back to the model.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reasoning: String,
    /// The tools it asked for.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub calls: Vec<ToolCall>,
    /// Why it stopped.
    pub stop: Stop,
}

/// One message of the conversation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum Message {
    /// What the agent is told it is.
    System {
        /// The text.
        text: String,
    },
    /// A task, from Jarmo.
    User {
        /// The text.
        text: String,
    },
    /// The model's answer.
    Assistant(Assistant),
    /// What a tool call came to.
    Tool {
        /// The call it answers.
        call_id: String,
        /// The tool.
        name: String,
        /// Its result.
        text: String,
        /// Whether it failed.
        is_error: bool,
    },
}

impl Message {
    /// The message as an OpenAI-compatible chat request carries it.
    pub fn openai(&self) -> Value {
        match self {
            Self::System { text } => json!({ "role": "system", "content": text }),
            Self::User { text } => json!({ "role": "user", "content": text }),
            Self::Assistant(answer) => {
                let mut message = json!({
                    "role": "assistant",
                    "content": if answer.text.is_empty() && !answer.calls.is_empty() { Value::Null } else { Value::String(answer.text.clone()) },
                });
                if !answer.calls.is_empty() {
                    message["tool_calls"] = Value::Array(
                        answer
                            .calls
                            .iter()
                            .map(|call| {
                                json!({
                                    "id": call.id,
                                    "type": "function",
                                    "function": { "name": call.name, "arguments": call.arguments },
                                })
                            })
                            .collect(),
                    );
                }
                message
            }
            Self::Tool { call_id, text, .. } => {
                json!({ "role": "tool", "tool_call_id": call_id, "content": text })
            }
        }
    }

    /// Its size, roughly, in bytes: for keeping a request small.
    pub fn size(&self) -> usize {
        match self {
            Self::System { text } | Self::User { text } => text.len(),
            Self::Assistant(answer) => {
                answer.text.len()
                    + answer
                        .calls
                        .iter()
                        .map(|call| call.arguments.len() + call.name.len())
                        .sum::<usize>()
            }
            Self::Tool { text, .. } => text.len(),
        }
    }
}

/// The messages a request carries: the system message, then the newest
/// messages that fit in `budget` bytes, starting at a task (a tool result
/// without its call would be refused by a provider).
pub fn window(system: &Message, history: &[Message], budget: usize) -> Vec<Message> {
    let mut start = history.len();
    let mut used = 0;
    for (index, message) in history.iter().enumerate().rev() {
        used += message.size();
        if used > budget {
            break;
        }
        start = index;
    }
    // Forward to the first task, unless that would leave nothing.
    let from = history[start..]
        .iter()
        .position(|message| matches!(message, Message::User { .. }))
        .map_or(start, |offset| start + offset);
    std::iter::once(system.clone())
        .chain(history[from..].iter().cloned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calls_and_results_read_as_openai_writes_them() {
        let answer = Message::Assistant(Assistant {
            text: String::new(),
            reasoning: "hm".into(),
            calls: vec![ToolCall {
                id: "c1".into(),
                name: "read".into(),
                arguments: "{\"path\":\"a\"}".into(),
            }],
            stop: Stop::ToolUse,
        });
        assert_eq!(
            answer.openai(),
            json!({ "role": "assistant", "content": null, "tool_calls": [
                { "id": "c1", "type": "function", "function": { "name": "read", "arguments": "{\"path\":\"a\"}" } }
            ]})
        );
        let result = Message::Tool {
            call_id: "c1".into(),
            name: "read".into(),
            text: "x".into(),
            is_error: false,
        };
        assert_eq!(
            result.openai(),
            json!({ "role": "tool", "tool_call_id": "c1", "content": "x" })
        );
        let back: Message = serde_json::from_value(serde_json::to_value(&answer).unwrap()).unwrap();
        assert_eq!(back, answer);
    }

    #[test]
    fn the_window_keeps_the_newest_from_a_task() {
        let system = Message::System { text: "s".into() };
        let user = |t: &str| Message::User { text: t.into() };
        let tool = |t: &str| Message::Tool {
            call_id: "c".into(),
            name: "bash".into(),
            text: t.into(),
            is_error: false,
        };
        let history = vec![
            user("old task"),
            tool(&"x".repeat(100)),
            user("new"),
            tool("ok"),
        ];
        let window = window(&system, &history, 50);
        assert_eq!(window, vec![system.clone(), user("new"), tool("ok")]);
        assert_eq!(super::window(&system, &history, 10_000).len(), 5);
    }
}
