//! The Anthropic Messages format (`POST <address>/v1/messages`): the
//! conversation as that API's own messages, its request body, and its
//! streamed reply put together.

use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};

use crate::wire::{
    arguments_value, size, Ask, Assemble, Body, Call, Ids, Outcome, Reply, Stop, Wire, NO_PICTURE,
};
use crate::{HISTORY_CHARS, MAX_OUTPUT_TOKENS};

/// How long the service may send nothing in the middle of a reply (it
/// sends a ping every few seconds while the model thinks).
const IDLE: Duration = Duration::from_secs(180);

pub(crate) struct Messages {
    /// The conversation: Anthropic's own `messages`.
    history: Vec<Value>,
    /// Where the person's message is while no reply to it has come.
    open: Option<usize>,
    ids: Ids,
    /// Whether the service's models are sent pictures.
    pictures: bool,
}

impl Messages {
    pub fn new(pictures: bool) -> Messages {
        Messages {
            history: Vec::new(),
            open: None,
            ids: Ids::default(),
            pictures,
        }
    }
}

impl Wire for Messages {
    fn user(&mut self, text: &str) {
        self.history
            .push(json!({"role": "user", "content": [{"type": "text", "text": text}]}));
        self.open = Some(self.history.len() - 1);
    }

    fn forget_unanswered(&mut self) {
        if self.open.is_some_and(|i| i + 1 == self.history.len()) {
            self.history.pop();
        }
        self.open = None;
    }

    /// Keep the conversation within [`HISTORY_CHARS`]: replace the oldest
    /// tool results with a note.
    fn trim(&mut self) {
        let last = self.history.len().saturating_sub(2);
        // Pictures go first: an earlier one is the biggest thing to keep,
        // and the words of every result are worth more.
        if size(&self.history) > HISTORY_CHARS {
            for m in self.history.iter_mut().take(last) {
                for b in m["content"].as_array_mut().into_iter().flatten() {
                    if let Some(parts) = b["content"].as_array_mut() {
                        for p in parts.iter_mut().filter(|p| p["type"] == "image") {
                            *p = json!({"type": "text", "text": "[An earlier picture, removed to keep the conversation short.]"});
                        }
                    }
                }
            }
        }
        let mut total = size(&self.history);
        for i in 0..last {
            if total <= HISTORY_CHARS {
                break;
            }
            let Some(blocks) = self.history[i]["content"].as_array_mut() else {
                continue;
            };
            for b in blocks.iter_mut() {
                if b["type"] == "tool_result" {
                    let before = b.to_string().len();
                    b["content"] = json!("[An older tool result, removed to keep the conversation short. Call the tool again if it is needed.]");
                    total = total.saturating_sub(before.saturating_sub(b.to_string().len()));
                }
            }
        }
    }

    fn body(&self, ask: &Ask) -> Body {
        // The system text and tools are the same every step: cache them.
        let mut tools: Vec<Value> = ask
            .tools
            .iter()
            .map(|t| {
                json!({
                    "name": t.name,
                    "description": t.description,
                    "input_schema": t.schema,
                })
            })
            .collect();
        if let Some(last) = tools.last_mut() {
            last["cache_control"] = json!({"type": "ephemeral"});
        }
        let mut body = json!({
            "model": ask.model,
            // Thinking counts toward this too.
            "max_tokens": MAX_OUTPUT_TOKENS,
            "system": [{"type": "text", "text": ask.system, "cache_control": {"type": "ephemeral"}}],
            "tools": tools,
            "messages": self.history,
        });
        // The one field this format can do without.
        let optional = !ask.effort.is_empty() && !ask.bare;
        if optional {
            body["output_config"] = json!({"effort": ask.effort});
        }
        // Streamed: bytes keep arriving while the model thinks, so a long
        // reply is not cut off by a proxy waiting for an idle connection.
        body["stream"] = json!(true);
        Body {
            json: body,
            optional,
        }
    }

    fn assembler(&self) -> Box<dyn Assemble> {
        Box::new(Assembly::default())
    }

    fn idle(&self) -> Duration {
        IDLE
    }

    fn busy(&self, e: &anyhow::Error) -> bool {
        e.to_string().contains("(overloaded_error)")
    }

    fn reply(&mut self, answer: Value, _ask: &Ask) -> Result<Reply> {
        let u = &answer["usage"];
        // Cached tokens count a tenth.
        let input_tokens = u["input_tokens"].as_u64().map(|n| {
            n + u["cache_creation_input_tokens"].as_u64().unwrap_or(0)
                + u["cache_read_input_tokens"].as_u64().unwrap_or(0) / 10
        });
        let mut content = answer["content"].as_array().cloned().unwrap_or_default();
        let mut calls = Vec::new();
        for b in content.iter_mut().filter(|b| b["type"] == "tool_use") {
            let id = self.ids.take(b["id"].as_str().unwrap_or(""));
            let (input, fault) = arguments_value(&b["input"]);
            b["id"] = json!(id);
            if fault.is_some() {
                // Kept as a call with no arguments, answered with why it
                // was not run.
                b["input"] = json!({});
            }
            calls.push(Call {
                id,
                name: b["name"].as_str().unwrap_or_default().to_string(),
                input,
                fault,
            });
        }
        let stop = match answer["stop_reason"].as_str() {
            Some("max_tokens") => Stop::Length,
            Some("tool_use") if !calls.is_empty() => Stop::Tools,
            _ => Stop::End,
        };
        let mut kept = content.clone();
        if stop != Stop::Tools {
            // A tool call that will not be run (cut off by the length
            // limit, or in a reply that did not end by asking for tools)
            // is not kept: a call with no result breaks the conversation.
            kept.retain(|b| b["type"] != "tool_use");
            calls.clear();
        }
        if stop == Stop::Length && kept.iter().all(|b| b["type"] != "text") {
            kept.push(json!({"type": "text", "text": "(The reply reached its length limit.)"}));
        }
        if kept.is_empty() {
            // A turn with nothing in it would be refused next time.
            kept.push(json!({"type": "text", "text": "(No answer.)"}));
        }
        self.history
            .push(json!({"role": "assistant", "content": kept}));
        self.open = None;
        let texts = content
            .iter()
            .filter(|b| b["type"] == "text")
            .filter_map(|b| b["text"].as_str())
            .filter(|t| !t.trim().is_empty())
            .map(String::from)
            .collect();
        Ok(Reply {
            texts,
            calls,
            stop,
            input_tokens,
        })
    }

    fn results(&mut self, outcomes: Vec<Outcome>) {
        let results: Vec<Value> = outcomes
            .iter()
            .map(|o| {
                if o.images.is_empty() || o.is_error {
                    tool_result(&o.id, &o.text, o.is_error)
                } else if self.pictures {
                    tool_result_with(&o.id, &o.text, &o.images)
                } else {
                    tool_result(&o.id, &format!("{}\n{NO_PICTURE}", o.text), false)
                }
            })
            .collect();
        self.history
            .push(json!({"role": "user", "content": results}));
    }

    fn model_changed(&mut self) {
        drop_thinking(&mut self.history);
    }

    fn history(&self) -> &[Value] {
        &self.history
    }
}

/// A streamed reply (server-sent events) put together as the whole message
/// the service would have answered without streaming.
pub async fn read_stream(r: reqwest::Response) -> Result<Value> {
    crate::http::read_sse(r, &mut Assembly::default(), IDLE).await
}

#[derive(Default)]
struct Assembly {
    started: bool,
    message: Value,
    blocks: Vec<Value>,
    json: Vec<String>,
}

impl Assemble for Assembly {
    /// One event; true when the message is complete.
    fn take(&mut self, v: Value) -> Result<bool> {
        let at = |v: &Value| v["index"].as_u64().map(|i| i as usize);
        match v["type"].as_str().unwrap_or("") {
            "message_start" => {
                self.started = true;
                self.message = v["message"].clone();
            }
            "content_block_start" => {
                let Some(i) = at(&v) else { return Ok(false) };
                if i > 1000 {
                    bail!("the reply had too many parts");
                }
                while self.blocks.len() <= i {
                    self.blocks.push(Value::Null);
                    self.json.push(String::new());
                }
                self.blocks[i] = v["content_block"].clone();
            }
            "content_block_delta" => {
                let Some(i) = at(&v).filter(|i| *i < self.blocks.len()) else {
                    return Ok(false);
                };
                let d = &v["delta"];
                let b = &mut self.blocks[i];
                let append = |b: &mut Value, k: &str, t: &str| {
                    let now = b[k].as_str().unwrap_or("").to_string();
                    b[k] = json!(now + t);
                };
                match d["type"].as_str().unwrap_or("") {
                    "text_delta" => append(b, "text", d["text"].as_str().unwrap_or("")),
                    "thinking_delta" => append(b, "thinking", d["thinking"].as_str().unwrap_or("")),
                    "signature_delta" => {
                        append(b, "signature", d["signature"].as_str().unwrap_or(""))
                    }
                    "input_json_delta" => {
                        self.json[i].push_str(d["partial_json"].as_str().unwrap_or(""))
                    }
                    _ => {}
                }
            }
            "content_block_stop" => {
                let Some(i) = at(&v).filter(|i| *i < self.blocks.len()) else {
                    return Ok(false);
                };
                if self.blocks[i]["type"] == "tool_use"
                    || self.blocks[i]["type"] == "server_tool_use"
                {
                    let j = std::mem::take(&mut self.json[i]);
                    self.blocks[i]["input"] = if j.trim().is_empty() {
                        json!({})
                    } else {
                        serde_json::from_str(&j).map_err(|_| {
                            anyhow!("a tool call arrived incomplete, so it was not run")
                        })?
                    };
                }
            }
            "message_delta" => {
                for (k, x) in v["delta"].as_object().into_iter().flatten() {
                    self.message[k] = x.clone();
                }
                if let Some(u) = v["usage"].as_object() {
                    for (k, x) in u {
                        self.message["usage"][k] = x.clone();
                    }
                }
            }
            "message_stop" => return Ok(true),
            "error" => bail!(
                "the AI service answered ({}): {}",
                v["error"]["type"].as_str().unwrap_or("error"),
                v["error"]["message"].as_str().unwrap_or("an error")
            ),
            _ => {}
        }
        Ok(false)
    }

    fn started(&self) -> bool {
        self.started
    }

    fn whole(&mut self) -> Value {
        let mut m = std::mem::take(&mut self.message);
        if !m.is_object() {
            m = json!({});
        }
        m["content"] = Value::Array(
            std::mem::take(&mut self.blocks)
                .into_iter()
                .filter(|b| !b.is_null())
                .collect(),
        );
        m
    }
}

/// Remove thinking from the assistant's earlier turns.
fn drop_thinking(history: &mut [Value]) {
    for m in history.iter_mut().filter(|m| m["role"] == "assistant") {
        if let Some(blocks) = m["content"].as_array_mut() {
            blocks.retain(|b| b["type"] != "thinking" && b["type"] != "redacted_thinking");
            if blocks.is_empty() {
                blocks.push(json!({"type": "text", "text": "(thinking only)"}));
            }
        }
    }
}

/// A tool result with its pictures, as the AI service takes them.
fn tool_result_with(id: &str, text: &str, images: &[String]) -> Value {
    let mut content = vec![json!({"type": "text", "text": text})];
    for d in images {
        content.push(json!({"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": d}}));
    }
    json!({"type": "tool_result", "tool_use_id": id, "content": content, "is_error": false})
}

fn tool_result(id: &str, text: &str, is_error: bool) -> Value {
    json!({"type": "tool_result", "tool_use_id": id, "content": text, "is_error": is_error})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tool_call_cut_off_in_the_stream_is_refused() {
        let mut s = Assembly::default();
        s.take(json!({"type": "message_start", "message": {"role": "assistant", "content": []}}))
            .unwrap();
        s.take(json!({"type": "content_block_start", "index": 0, "content_block": {"type": "tool_use", "id": "t", "name": "sql_select", "input": {}}})).unwrap();
        s.take(json!({"type": "content_block_delta", "index": 0, "delta": {"type": "input_json_delta", "partial_json": "{\"sql\": \"select"}})).unwrap();
        let e = s
            .take(json!({"type": "content_block_stop", "index": 0}))
            .unwrap_err();
        assert!(e.to_string().contains("incomplete"), "{e}");
    }

    #[test]
    fn an_error_in_the_stream_names_its_type() {
        let mut s = Assembly::default();
        let e = s
            .take(json!({"type": "error", "error": {"type": "overloaded_error", "message": "Overloaded"}}))
            .unwrap_err();
        assert!(e.to_string().contains("(overloaded_error)"), "{e}");
    }

    fn ask<'a>(effort: &'a str, bare: bool) -> Ask<'a> {
        Ask {
            model: "claude-sonnet-5-5",
            effort,
            system: "sys",
            tools: &[],
            bare,
        }
    }

    #[test]
    fn a_reply_that_asks_for_no_tools_keeps_no_tool_call() {
        let mut w = Messages::new(true);
        w.user("hello");
        // Cut off at the length limit: the call in it is dropped, not run.
        let r = w
            .reply(
                json!({"content": [{"type": "tool_use", "id": "a", "name": "whoami", "input": {}}], "stop_reason": "max_tokens"}),
                &ask("", false),
            )
            .unwrap();
        assert_eq!(r.stop, Stop::Length);
        assert!(r.calls.is_empty());
        assert_eq!(
            w.history()[1]["content"],
            json!([{"type": "text", "text": "(The reply reached its length limit.)"}])
        );
        // A reply with nothing in it is still a turn the service takes back.
        let r = w
            .reply(
                json!({"content": [], "stop_reason": "end_turn"}),
                &ask("", false),
            )
            .unwrap();
        assert_eq!(r.stop, Stop::End);
        assert_eq!(w.history()[2]["content"][0]["text"], "(No answer.)");
    }

    #[test]
    fn a_stopped_request_forgets_the_persons_message_but_keeps_tool_results() {
        let mut w = Messages::new(true);
        w.user("one");
        w.forget_unanswered();
        assert!(w.history().is_empty(), "the unanswered message is dropped");
        w.user("two");
        let r = w
            .reply(
                json!({"content": [{"type": "tool_use", "id": "a", "name": "whoami", "input": {}}], "stop_reason": "tool_use"}),
                &ask("", false),
            )
            .unwrap();
        assert_eq!(r.stop, Stop::Tools);
        w.results(vec![Outcome {
            id: r.calls[0].id.clone(),
            name: "whoami".into(),
            text: "{}".into(),
            is_error: false,
            images: vec![],
        }]);
        // Stopped while the model was asked again: the call keeps its result.
        w.forget_unanswered();
        assert_eq!(w.history().len(), 3);
        assert_eq!(w.history()[2]["content"][0]["type"], "tool_result");
        w.user("three");
        assert_eq!(w.history().len(), 4);
    }

    #[test]
    fn the_thinking_level_is_the_field_the_format_can_do_without() {
        let w = Messages::new(true);
        let b = w.body(&ask("high", false));
        assert!(b.optional);
        assert_eq!(b.json["output_config"], json!({"effort": "high"}));
        let b = w.body(&ask("high", true));
        assert!(!b.optional && b.json.get("output_config").is_none());
        let b = w.body(&ask("", false));
        assert!(!b.optional && b.json.get("output_config").is_none());
        assert_eq!(b.json["max_tokens"], MAX_OUTPUT_TOKENS);
        assert_eq!(b.json["stream"], true);
    }

    #[test]
    fn a_service_that_takes_no_pictures_gets_a_line_of_text() {
        let out = |pictures: bool| {
            let mut w = Messages::new(pictures);
            w.results(vec![Outcome {
                id: "a".into(),
                name: "map_snapshot".into(),
                text: "drawn".into(),
                is_error: false,
                images: vec!["AAAA".into()],
            }]);
            w.history()[0]["content"][0].clone()
        };
        assert_eq!(out(true)["content"][1]["type"], "image");
        let plain = out(false);
        assert_eq!(plain["content"], json!(format!("drawn\n{NO_PICTURE}")));
        assert_eq!(plain["is_error"], false);
    }
}
