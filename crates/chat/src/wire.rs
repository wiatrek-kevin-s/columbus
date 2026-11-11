//! What the chat's loop and a request format say to each other.
//!
//! The loop (steps, tool calls, caps, Stop, events) is the same whatever
//! the service. A format owns the conversation in its own native shape and
//! turns it into requests; it hands the loop each reply in these neutral
//! terms. A chat never changes format, so nothing is ever translated from
//! one format's history to another's.

use std::collections::HashSet;

use anyhow::Result;
use serde_json::{json, Value};

/// One of the session's tools, as the Columbus server lists it.
#[derive(Debug, Clone)]
pub(crate) struct Tool {
    pub name: String,
    pub description: String,
    /// Its input schema, as the server wrote it.
    pub schema: Value,
}

/// What each request needs besides the conversation.
pub(crate) struct Ask<'a> {
    pub model: &'a str,
    /// The thinking level; "" for the model's own.
    pub effort: &'a str,
    pub system: &'a str,
    pub tools: &'a [Tool],
    /// Leave out every field the format can do without (the thinking
    /// level, the reply's length limit, the request for token counts):
    /// the service refused a request that carried them.
    pub bare: bool,
}

/// A request body, and whether it carries a field it could do without.
pub(crate) struct Body {
    pub json: Value,
    pub optional: bool,
}

/// Why a reply ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stop {
    /// The model has answered.
    End,
    /// It asked for tools, and the reply is whole.
    Tools,
    /// It reached its length limit: any tool call in it is cut off.
    Length,
    /// The service's content filter cut it short.
    Filtered,
}

/// A tool call the model asked for.
#[derive(Debug, Clone)]
pub(crate) struct Call {
    /// Never empty, and unlike every other call's in the conversation.
    pub id: String,
    pub name: String,
    /// The arguments: a JSON object unless `fault` says otherwise.
    pub input: Value,
    /// Why the call must not run (its arguments did not arrive whole).
    pub fault: Option<String>,
}

/// One reply, in the loop's terms. The format has already put the reply
/// into its conversation.
#[derive(Debug)]
pub(crate) struct Reply {
    /// The model's words, in order.
    pub texts: Vec<String>,
    pub calls: Vec<Call>,
    pub stop: Stop,
    /// Input tokens as the service counted them, cached ones a tenth;
    /// none when it sent no count.
    pub input_tokens: Option<u64>,
}

/// What a tool call came to, for the model.
#[derive(Debug, Clone)]
pub(crate) struct Outcome {
    pub id: String,
    pub name: String,
    pub text: String,
    pub is_error: bool,
    /// PNG pictures, base64 (only when the service's models take them).
    pub images: Vec<String>,
}

/// Puts a streamed reply together as the whole answer the service would
/// have given without streaming.
pub(crate) trait Assemble: Send {
    /// One event's data; true when the reply is complete.
    fn take(&mut self, event: Value) -> Result<bool>;
    /// Whether anything of a reply has arrived.
    fn started(&self) -> bool;
    /// Whether what has arrived is a whole reply, when the stream ends
    /// without an event that says so (a format with such an event says no).
    fn ended(&self) -> bool {
        false
    }
    /// The stream said in so many words that it is over (`data: [DONE]`,
    /// in the Chat Completions format).
    fn end_said(&mut self) {}
    /// The whole answer.
    fn whole(&mut self) -> Value;
}

/// A request format: the conversation in its native shape.
pub(crate) trait Wire: Send + Sync {
    /// The person's message.
    fn user(&mut self, text: &str);
    /// A reply was stopped before it came: if the person's message is the
    /// last thing in the conversation, drop it. Tool results stay: their
    /// calls are in the conversation, and a call without its result would
    /// be refused by the service.
    fn forget_unanswered(&mut self);
    /// Keep the conversation within the size Columbus sends.
    fn trim(&mut self);
    /// The next request.
    fn body(&self, ask: &Ask) -> Body;
    fn assembler(&self) -> Box<dyn Assemble>;
    /// How long the service may send nothing in the middle of a streamed
    /// reply before it is given up.
    fn idle(&self) -> std::time::Duration;
    /// Whether an error read from a stream says the service is busy (and
    /// asking again may help).
    fn busy(&self, e: &anyhow::Error) -> bool;
    /// Take the service's whole answer into the conversation.
    fn reply(&mut self, answer: Value, ask: &Ask) -> Result<Reply>;
    /// The step's tool results, one for every call of the last reply.
    fn results(&mut self, outcomes: Vec<Outcome>);
    /// The model changed: what only the earlier model can read (its
    /// thinking) leaves the conversation; answers and tool calls stay.
    fn model_changed(&mut self);
    /// The conversation as the service is sent it.
    fn history(&self) -> &[Value];
}

/// A service's own refusal, read from inside a reply (an error event in a
/// stream, a response that failed).
#[derive(Debug)]
pub(crate) struct Refused {
    /// Whether it says the service is busy, so asking again may help.
    pub busy: bool,
    /// Its words.
    pub said: String,
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the AI service answered with an error: {}", self.said)
    }
}

impl std::error::Error for Refused {}

/// Said in a picture's place to a model that is not sent pictures.
pub(crate) const NO_PICTURE: &str =
    "[This tool also made a picture. This AI service is not set up to be sent pictures, so it was left out.]";

pub(crate) const INCOMPLETE: &str = "a tool call arrived incomplete, so it was not run";

/// A failed or refused call's words, for a format whose tool results have
/// no mark for failure: the words say it.
pub(crate) fn error_text(text: &str) -> String {
    format!("The tool call failed or was refused:\n{text}")
}

/// Call ids the conversation has used. An id that is empty, was used
/// before, or is not a plain short word is replaced by one of Columbus's
/// own, so every call and its result can be told from every other.
#[derive(Debug, Default)]
pub(crate) struct Ids {
    used: HashSet<String>,
    made: u64,
}

impl Ids {
    pub fn take(&mut self, given: &str) -> String {
        let plain = !given.is_empty()
            && given.len() <= 120
            && given
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-.:".contains(&b));
        if plain && self.used.insert(given.to_string()) {
            return given.to_string();
        }
        loop {
            self.made += 1;
            let id = format!("call_columbus_{}", self.made);
            if self.used.insert(id.clone()) {
                return id;
            }
        }
    }
}

/// A call's arguments from the JSON text a service sent: an object, or the
/// reason the call must not run. Nothing sent means no arguments.
pub(crate) fn arguments(text: &str) -> (Value, Option<String>) {
    if text.trim().is_empty() {
        return (json!({}), None);
    }
    match serde_json::from_str::<Value>(text) {
        Ok(v @ Value::Object(_)) => (v, None),
        _ => (json!({}), Some(INCOMPLETE.to_string())),
    }
}

/// The arguments of a call that arrived as JSON already.
pub(crate) fn arguments_value(v: &Value) -> (Value, Option<String>) {
    match v {
        Value::Object(_) => (v.clone(), None),
        Value::Null => (json!({}), None),
        _ => (json!({}), Some(INCOMPLETE.to_string())),
    }
}

/// How many characters of JSON the conversation holds.
pub(crate) fn size(history: &[Value]) -> usize {
    history.iter().map(|m| m.to_string().len()).sum()
}

/// A rough count of the tokens a request holds, for a service that sends
/// no count of its own: about a token for every three characters of text
/// (JSON is dense), and a flat amount for each picture, whose size in
/// characters says nothing of its cost.
pub(crate) fn estimate_tokens(body: &Value) -> u64 {
    const PICTURE: u64 = 1_500;
    fn chars(v: &Value, pictures: &mut u64) -> u64 {
        match v {
            Value::String(s) => {
                if s.len() > 2_000 && (s.starts_with("data:image/") || looks_base64(s)) {
                    *pictures += 1;
                    0
                } else {
                    s.len() as u64 + 2
                }
            }
            Value::Array(a) => a.iter().map(|x| chars(x, pictures) + 1).sum::<u64>() + 2,
            Value::Object(m) => {
                m.iter()
                    .map(|(k, x)| k.len() as u64 + 4 + chars(x, pictures))
                    .sum::<u64>()
                    + 2
            }
            Value::Null => 4,
            Value::Bool(_) => 5,
            Value::Number(n) => n.to_string().len() as u64,
        }
    }
    fn looks_base64(s: &str) -> bool {
        s.bytes()
            .take(400)
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'='))
    }
    let mut pictures = 0;
    let n = chars(body, &mut pictures);
    n.div_ceil(3) + pictures * PICTURE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_and_repeated_call_ids_are_replaced() {
        let mut ids = Ids::default();
        assert_eq!(ids.take("call_abc"), "call_abc");
        assert_eq!(ids.take("call_abc"), "call_columbus_1");
        assert_eq!(ids.take(""), "call_columbus_2");
        assert_eq!(ids.take("has space"), "call_columbus_3");
        assert_eq!(ids.take(&"x".repeat(121)), "call_columbus_4");
        // One of Columbus's own ids sent by the service is not trusted to be new.
        assert_eq!(ids.take("call_columbus_5"), "call_columbus_5");
        assert_eq!(ids.take(""), "call_columbus_6");
        assert_eq!(ids.take("toolu_01A.b:c-d"), "toolu_01A.b:c-d");
    }

    #[test]
    fn only_whole_json_objects_are_arguments() {
        assert_eq!(arguments(""), (json!({}), None));
        assert_eq!(arguments("  "), (json!({}), None));
        assert_eq!(arguments("{\"a\": 1}"), (json!({"a": 1}), None));
        for damaged in [
            "{\"a\": 1",
            "[1, 2]",
            "\"text\"",
            "42",
            "null",
            "{\"a\": 1} trailing",
        ] {
            let (v, fault) = arguments(damaged);
            assert_eq!(v, json!({}), "{damaged}");
            assert_eq!(fault.as_deref(), Some(INCOMPLETE), "{damaged}");
        }
        assert_eq!(arguments_value(&json!({"a": 1})), (json!({"a": 1}), None));
        assert_eq!(arguments_value(&Value::Null), (json!({}), None));
        assert!(arguments_value(&json!("{}")).1.is_some());
        assert!(arguments_value(&json!([1])).1.is_some());
    }

    #[test]
    fn a_request_without_a_count_is_estimated_from_its_size() {
        let small = json!({"messages": [{"role": "user", "content": "hello"}]});
        let n = estimate_tokens(&small);
        assert!((10..40).contains(&n), "{n}");
        // About a token for every three characters.
        let text = "word ".repeat(3_000);
        let big = json!({"messages": [{"role": "user", "content": text}]});
        let n = estimate_tokens(&big);
        assert!((4_900..5_400).contains(&n), "{n}");
        // A picture counts a flat amount, whatever its size.
        let png = "A".repeat(900_000);
        let with = json!({"messages": [{"role": "user", "content": [
            {"type": "image_url", "image_url": {"url": format!("data:image/png;base64,{png}")}},
            {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": png}}]}]});
        let n = estimate_tokens(&with);
        assert!((3_000..3_200).contains(&n), "{n}");
    }
}
