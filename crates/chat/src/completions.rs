//! The OpenAI Chat Completions format (`POST <address>/chat/completions`),
//! which nearly every other service takes: Google Gemini's compatible
//! endpoint, OpenRouter, Ollama, LM Studio, Azure OpenAI and the rest. The
//! conversation is that API's own `messages`.
//!
//! Services differ in the details, so this format is read generously and
//! written plainly: a streamed tool call is joined by its `index`, or
//! where a service sends none, by where a new id or name begins; whatever
//! else a tool call arrived with (Gemini's thought signature) goes back
//! unchanged (build brief 3, A-12); token counts are taken from whichever
//! chunk carries them. A picture cannot ride in a tool's result here, so
//! it follows in a message of its own. Shapes as in OpenAI's own SDK
//! (`openai-python`, `src/openai/types/chat`), read on 2026-11-11.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{bail, Result};
use serde_json::{json, Map, Value};

use crate::schema;
use crate::wire::{
    arguments, arguments_value, error_text, size, Ask, Assemble, Body, Call, Ids, Outcome, Refused,
    Reply, Stop, Wire, NO_PICTURE,
};
use crate::{HISTORY_CHARS, MAX_OUTPUT_TOKENS};

/// How long the service may send nothing in the middle of a reply (a
/// local model can take minutes to read a long conversation).
const IDLE: Duration = Duration::from_secs(600);

pub(crate) struct Completions {
    /// The conversation: the API's own `messages`, without the system
    /// message (each request puts it first).
    messages: Vec<Value>,
    /// Where the person's message is while no reply to it has come.
    open: Option<usize>,
    /// Where the latest reply begins: what is before it may be shortened.
    step: usize,
    ids: Ids,
    pictures: bool,
}

impl Completions {
    pub fn new(pictures: bool) -> Completions {
        Completions {
            messages: Vec::new(),
            open: None,
            step: 0,
            ids: Ids::default(),
            pictures,
        }
    }
}

/// The words of a message's `content`: a string, or parts with text.
fn content_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| p["text"].as_str())
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

impl Wire for Completions {
    fn user(&mut self, text: &str) {
        self.messages.push(json!({"role": "user", "content": text}));
        self.open = Some(self.messages.len() - 1);
    }

    fn forget_unanswered(&mut self) {
        if self.open.is_some_and(|i| i + 1 == self.messages.len()) {
            self.messages.pop();
        }
        self.open = None;
    }

    fn trim(&mut self) {
        if size(&self.messages) <= HISTORY_CHARS {
            return;
        }
        let last = self.step.min(self.messages.len());
        // Pictures go first (they are in messages of their own, after a
        // step's tool results), then the oldest tool results.
        for m in self.messages.iter_mut().take(last) {
            if m["role"] == "user" && m["content"].is_array() {
                m["content"] = json!("[Earlier pictures, removed to keep the conversation short.]");
            }
        }
        let mut total = size(&self.messages);
        for m in self.messages.iter_mut().take(last) {
            if total <= HISTORY_CHARS {
                break;
            }
            if m["role"] == "tool" {
                let before = m.to_string().len();
                m["content"] = json!("[An older tool result, removed to keep the conversation short. Call the tool again if it is needed.]");
                total = total.saturating_sub(before.saturating_sub(m.to_string().len()));
            }
        }
    }

    fn body(&self, ask: &Ask) -> Body {
        let tools: Vec<Value> = ask
            .tools
            .iter()
            .map(|t| {
                json!({"type": "function", "function": {
                    "name": t.name,
                    "description": schema::short(&t.description).0,
                    "parameters": schema::plain(&t.schema),
                }})
            })
            .collect();
        let mut messages = Vec::with_capacity(self.messages.len() + 1);
        messages.push(json!({"role": "system", "content": format!("{}{}", ask.system, schema::notes(ask.tools))}));
        messages.extend(self.messages.iter().cloned());
        let mut body = json!({
            "model": ask.model,
            "messages": messages,
            "tools": tools,
            "stream": true,
        });
        // What the format can do without: the token counts of a streamed
        // reply, and the cap on a reply.
        if !ask.bare {
            body["stream_options"] = json!({"include_usage": true});
            body["max_completion_tokens"] = json!(MAX_OUTPUT_TOKENS);
        }
        Body {
            json: body,
            optional: !ask.bare,
        }
    }

    fn assembler(&self) -> Box<dyn Assemble> {
        Box::new(Assembly::default())
    }

    fn idle(&self) -> Duration {
        IDLE
    }

    fn busy(&self, e: &anyhow::Error) -> bool {
        e.downcast_ref::<Refused>().is_some_and(|r| r.busy)
    }

    fn reply(&mut self, answer: Value, _ask: &Ask) -> Result<Reply> {
        if let Some(r) = refused(&answer) {
            return Err(r.into());
        }
        let Some(choice) = answer["choices"].as_array().and_then(|c| c.first()) else {
            bail!("the AI service sent no reply");
        };
        let u = &answer["usage"];
        // Cached tokens are part of the count, and count a tenth.
        let input_tokens = u["prompt_tokens"].as_u64().map(|n| {
            let cached = u["prompt_tokens_details"]["cached_tokens"]
                .as_u64()
                .unwrap_or(0)
                .min(n);
            n - cached + cached / 10
        });
        let message = &choice["message"];
        let mut texts = Vec::new();
        let said = content_text(&message["content"]);
        if !said.trim().is_empty() {
            texts.push(said.clone());
        }
        if let Some(r) = message["refusal"].as_str().filter(|r| !r.trim().is_empty()) {
            texts.push(r.to_string());
        }
        let mut calls = Vec::new();
        let mut kept_calls = Vec::new();
        for tc in message["tool_calls"].as_array().into_iter().flatten() {
            // Kept whole, with whatever else it arrived with.
            let mut tc = if tc.is_object() {
                tc.clone()
            } else {
                json!({})
            };
            let id = self.ids.take(tc["id"].as_str().unwrap_or(""));
            tc["id"] = json!(id);
            tc["type"] = json!("function");
            if let Some(m) = tc.as_object_mut() {
                // Where a piece stood in the stream is no part of the call.
                m.remove("index");
            }
            let (input, fault) = match &tc["function"]["arguments"] {
                Value::String(s) => arguments(s),
                other => arguments_value(other),
            };
            let whole = tc["function"]["arguments"]
                .as_str()
                .is_some_and(|a| !a.trim().is_empty());
            if !tc["function"].is_object() {
                tc["function"] = json!({});
            }
            if fault.is_some() {
                tc["function"]["arguments"] = json!("{}");
            } else if !whole {
                tc["function"]["arguments"] = json!(input.to_string());
            }
            let name = tc["function"]["name"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            tc["function"]["name"] = json!(name);
            calls.push(Call {
                id,
                name,
                input,
                fault,
            });
            kept_calls.push(tc);
        }
        // What the service calls the end is read with what it sent: a
        // reply with tool calls asks for tools, whatever word ends it
        // (services differ), unless it was cut short.
        let stop = match choice["finish_reason"].as_str() {
            Some("length") => Stop::Length,
            Some("content_filter") => Stop::Filtered,
            _ if !calls.is_empty() => Stop::Tools,
            _ => Stop::End,
        };
        let mut kept = json!({"role": "assistant"});
        if stop == Stop::Tools {
            kept["content"] = if said.trim().is_empty() {
                Value::Null
            } else {
                json!(said)
            };
            kept["tool_calls"] = Value::Array(kept_calls);
        } else {
            // A tool call in a reply that was cut short is not run and
            // not kept: a call with no result breaks the conversation.
            calls.clear();
            let words = texts.join("\n");
            kept["content"] = json!(if !words.trim().is_empty() {
                words.as_str()
            } else {
                match stop {
                    Stop::Length => "(The reply reached its length limit.)",
                    Stop::Filtered => "(The service's content filter cut the reply short.)",
                    _ => "(No answer.)",
                }
            });
        }
        self.step = self.messages.len();
        self.messages.push(kept);
        self.open = None;
        Ok(Reply {
            texts,
            calls,
            stop,
            input_tokens,
        })
    }

    fn results(&mut self, outcomes: Vec<Outcome>) {
        // One `tool` message for each call; a tool message holds text only.
        let mut pictures = Vec::new();
        for o in &outcomes {
            let mut text = if o.is_error {
                error_text(&o.text)
            } else {
                o.text.clone()
            };
            if !o.images.is_empty() && !o.is_error {
                if self.pictures {
                    text.push_str("\n[The picture this tool made follows in the next message.]");
                    pictures.push(json!({"type": "text", "text": format!("The picture the tool {} made (its call was {}), for you to look at:", o.name, o.id)}));
                    for d in &o.images {
                        pictures.push(json!({"type": "image_url", "image_url": {"url": format!("data:image/png;base64,{d}")}}));
                    }
                } else {
                    text.push('\n');
                    text.push_str(NO_PICTURE);
                }
            }
            self.messages
                .push(json!({"role": "tool", "tool_call_id": o.id, "content": text}));
        }
        // After every tool message of the step: the pictures, as a message
        // from the person's side (the only place this format takes one).
        if !pictures.is_empty() {
            self.messages
                .push(json!({"role": "user", "content": pictures}));
        }
    }

    fn model_changed(&mut self) {
        // Nothing here belongs to one model alone: what a tool call arrived
        // with goes back as it came.
    }

    fn history(&self) -> &[Value] {
        &self.messages
    }
}

/// The service's own refusal, if `answer` is an error in place of a reply.
fn refused(answer: &Value) -> Option<Refused> {
    let e = &answer["error"];
    if e.is_null() {
        return None;
    }
    let code = match &e["code"] {
        Value::String(s) => s.to_ascii_lowercase(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    };
    let kind = e["type"].as_str().unwrap_or("").to_ascii_lowercase();
    let busy = ["429", "500", "502", "503", "504", "529"].contains(&code.as_str())
        || ["rate_limit", "overloaded", "server_error", "unavailable"]
            .iter()
            .any(|w| code.contains(w) || kind.contains(w));
    Some(Refused {
        busy,
        said: e["message"]
            .as_str()
            .or(e.as_str())
            .unwrap_or("an error")
            .to_string(),
    })
}

/// A streamed reply, put together as the `chat.completion` the service
/// would have answered without streaming.
#[derive(Default)]
struct Assembly {
    started: bool,
    finish: Option<String>,
    content: String,
    refusal: String,
    calls: Vec<Value>,
    /// Which call a piece's `index` belongs to.
    by_index: HashMap<u64, usize>,
    usage: Value,
}

impl Assembly {
    /// The call a piece belongs to (its place in `calls`).
    fn place(&mut self, piece: &Value) -> Result<usize> {
        let id = piece["id"].as_str().unwrap_or("");
        let named = !piece["function"]["name"].as_str().unwrap_or("").is_empty();
        let known = match piece["index"].as_u64() {
            // By its index, as the format says. A new id at an index that
            // already has another is a new call all the same.
            Some(i) => self.by_index.get(&i).copied().filter(|at| {
                let had = self.calls[*at]["id"].as_str().unwrap_or("");
                id.is_empty() || had.is_empty() || had == id
            }),
            // A service that sends no index: a piece with an id or a name
            // begins a call, and the others go on with the last one.
            None if id.is_empty() && !named => self.calls.len().checked_sub(1),
            None => None,
        };
        if let Some(at) = known {
            return Ok(at);
        }
        if self.calls.len() >= 1000 {
            bail!("the reply had too many parts");
        }
        self.calls.push(json!({}));
        let at = self.calls.len() - 1;
        if let Some(i) = piece["index"].as_u64() {
            self.by_index.insert(i, at);
        }
        Ok(at)
    }
}

/// Add a streamed piece of a tool call to what has arrived of it: the
/// arguments are joined; the id and the name are taken once (some services
/// repeat them on every piece); anything else is kept as first sent.
fn join(call: &mut Value, piece: &Value) {
    let Some(piece) = piece.as_object() else {
        return;
    };
    if !call.is_object() {
        *call = json!({});
    }
    for (k, v) in piece {
        match k.as_str() {
            "index" => {}
            "id" | "type" => {
                if call[k].as_str().unwrap_or("").is_empty() && v.is_string() {
                    call[k] = v.clone();
                }
            }
            "function" => {
                if !call["function"].is_object() {
                    call["function"] = json!({});
                }
                for (fk, fv) in v.as_object().into_iter().flatten() {
                    let had = call["function"][fk].as_str().unwrap_or("").to_string();
                    match (fk.as_str(), fv) {
                        ("arguments", Value::String(s)) => {
                            call["function"]["arguments"] = json!(had + s);
                        }
                        // Arguments sent as JSON already, in one piece.
                        ("arguments", other) if !other.is_null() && had.is_empty() => {
                            call["function"]["arguments"] = json!(other.to_string());
                        }
                        ("name", Value::String(s)) if had.is_empty() => {
                            call["function"]["name"] = json!(s);
                        }
                        // A name in pieces; a repeated one is the same name.
                        ("name", Value::String(s)) if !s.is_empty() && *s != had => {
                            call["function"]["name"] = json!(had + s);
                        }
                        ("name", _) | ("arguments", _) => {}
                        _ => keep_first(&mut call["function"], fk, fv),
                    }
                }
            }
            _ => keep_first(call, k, v),
        }
    }
}

/// `into[k]` takes `v` if it has nothing there; two objects are joined
/// key by key the same way.
fn keep_first(into: &mut Value, k: &str, v: &Value) {
    if v.is_null() {
        return;
    }
    let Some(m) = into.as_object_mut() else {
        return;
    };
    match m.get_mut(k) {
        None | Some(Value::Null) => {
            m.insert(k.to_string(), v.clone());
        }
        Some(had @ Value::Object(_)) => {
            for (k2, v2) in v.as_object().into_iter().flatten() {
                keep_first(had, k2, v2);
            }
        }
        Some(_) => {}
    }
}

impl Assemble for Assembly {
    fn take(&mut self, v: Value) -> Result<bool> {
        // An error in the middle of a stream (OpenRouter sends one so).
        if let Some(r) = refused(&v) {
            return Err(r.into());
        }
        // Token counts may ride on any chunk, with choices or without.
        if v["usage"].is_object() {
            self.usage = v["usage"].clone();
            self.started = true;
        }
        let Some(choice) = v["choices"].as_array().and_then(|c| c.first()) else {
            return Ok(false);
        };
        self.started = true;
        // (A service that streams whole messages instead of pieces.)
        let delta = if choice["delta"].is_object() {
            &choice["delta"]
        } else {
            &choice["message"]
        };
        if let Some(t) = delta["content"].as_str() {
            self.content.push_str(t);
        }
        if let Some(t) = delta["refusal"].as_str() {
            self.refusal.push_str(t);
        }
        for piece in delta["tool_calls"].as_array().into_iter().flatten() {
            let at = self.place(piece)?;
            join(&mut self.calls[at], piece);
        }
        if let Some(f) = choice["finish_reason"].as_str() {
            self.finish = Some(f.to_string());
        }
        // The stream says itself when it is over (`[DONE]`, or its end).
        Ok(false)
    }

    fn started(&self) -> bool {
        self.started
    }

    /// A reply whose end was said is whole when the stream ends.
    fn ended(&self) -> bool {
        self.finish.is_some()
    }

    fn whole(&mut self) -> Value {
        let mut message = Map::new();
        message.insert("role".into(), json!("assistant"));
        message.insert(
            "content".into(),
            if self.content.is_empty() {
                Value::Null
            } else {
                json!(std::mem::take(&mut self.content))
            },
        );
        if !self.refusal.is_empty() {
            message.insert("refusal".into(), json!(std::mem::take(&mut self.refusal)));
        }
        if !self.calls.is_empty() {
            message.insert(
                "tool_calls".into(),
                Value::Array(std::mem::take(&mut self.calls)),
            );
        }
        json!({
            "choices": [{"index": 0, "message": message, "finish_reason": self.finish.take()}],
            "usage": std::mem::take(&mut self.usage),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::Tool;

    fn ask<'a>(tools: &'a [Tool], bare: bool) -> Ask<'a> {
        Ask {
            model: "gemini-3.8-flash",
            effort: "",
            system: "You are the assistant.",
            tools,
            bare,
        }
    }

    fn tool() -> Vec<Tool> {
        vec![Tool {
            name: "map_snapshot".into(),
            description: "Draws layers.".into(),
            schema: json!({"$schema": "x", "type": "object", "properties": {"layers": {"type": "array", "items": {"type": "string"}}}}),
        }]
    }

    fn outcome(id: &str, text: &str, images: &[&str]) -> Outcome {
        Outcome {
            id: id.into(),
            name: "map_snapshot".into(),
            text: text.into(),
            is_error: false,
            images: images.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn the_request_has_a_system_message_and_nested_tools() {
        let tools = tool();
        let mut w = Completions::new(true);
        w.user("Draw the tracts.");
        let b = w.body(&ask(&tools, false));
        assert!(b.optional);
        let j = &b.json;
        assert_eq!(j["model"], "gemini-3.8-flash");
        assert_eq!(
            j["messages"],
            json!([{"role": "system", "content": "You are the assistant."}, {"role": "user", "content": "Draw the tracts."}])
        );
        assert_eq!(j["stream"], true);
        assert_eq!(j["stream_options"], json!({"include_usage": true}));
        assert_eq!(j["max_completion_tokens"], MAX_OUTPUT_TOKENS);
        let t = &j["tools"][0];
        assert_eq!(t["type"], "function");
        assert_eq!(t["function"]["name"], "map_snapshot");
        assert!(t.get("name").is_none(), "nested under function");
        assert_eq!(
            schema::not_plain(&t["function"]["parameters"]),
            Vec::<String>::new()
        );
        let b = w.body(&ask(&tools, true));
        assert!(!b.optional);
        assert!(
            b.json.get("stream_options").is_none() && b.json.get("max_completion_tokens").is_none()
        );
        assert_eq!(b.json["stream"], true);
    }

    #[test]
    fn tool_calls_keep_what_they_came_with_and_pictures_follow_the_results() {
        let tools = tool();
        let mut w = Completions::new(true);
        w.user("Draw them.");
        let answer = json!({"choices": [{"finish_reason": "tool_calls", "message": {"role": "assistant", "content": null, "tool_calls": [
            {"index": 0, "id": "", "type": "function", "function": {"name": "map_snapshot", "arguments": "{\"layers\": [\"ws://s/a.gpkg\"]}"},
             "extra_content": {"google": {"thought_signature": "c2lnbmF0dXJl"}}},
            {"id": "", "function": {"name": "map_snapshot", "arguments": {"layers": ["ws://s/b.gpkg"]}}},
            {"id": "call_x", "type": "function", "function": {"name": "map_snapshot", "arguments": "{\"layers\": ["}}]}}],
            "usage": {"prompt_tokens": 500, "completion_tokens": 9, "prompt_tokens_details": {"cached_tokens": 400}}});
        let r = w.reply(answer, &ask(&tools, false)).unwrap();
        assert_eq!(r.stop, Stop::Tools);
        assert_eq!(r.input_tokens, Some(100 + 40));
        let ids: Vec<&str> = r.calls.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(
            ids,
            ["call_columbus_1", "call_columbus_2", "call_x"],
            "empty ids are replaced"
        );
        assert_eq!(r.calls[1].input, json!({"layers": ["ws://s/b.gpkg"]}));
        assert!(r.calls[0].fault.is_none() && r.calls[1].fault.is_none());
        assert!(r.calls[2].fault.is_some(), "damaged arguments");
        let kept = &w.history()[1];
        assert_eq!(kept["content"], Value::Null);
        let tc = kept["tool_calls"].as_array().unwrap();
        // The extra field goes back unchanged, on its own call; the stream's index does not.
        assert_eq!(
            tc[0],
            json!({"id": "call_columbus_1", "type": "function",
                "function": {"name": "map_snapshot", "arguments": "{\"layers\": [\"ws://s/a.gpkg\"]}"},
                "extra_content": {"google": {"thought_signature": "c2lnbmF0dXJl"}}})
        );
        assert_eq!(
            tc[1]["function"]["arguments"], "{\"layers\":[\"ws://s/b.gpkg\"]}",
            "arguments go back as text"
        );
        assert_eq!(tc[2]["function"]["arguments"], "{}");
        w.results(vec![
            outcome("call_columbus_1", "drawn a", &["QUJD"]),
            outcome("call_columbus_2", "drawn b", &["REVG", "R0hJ"]),
            Outcome {
                is_error: true,
                ..outcome("call_x", "Not run.", &[])
            },
        ]);
        let h = w.history();
        assert_eq!(h.len(), 6);
        assert_eq!(
            h[2],
            json!({"role": "tool", "tool_call_id": "call_columbus_1", "content": "drawn a\n[The picture this tool made follows in the next message.]"})
        );
        assert_eq!(h[3]["tool_call_id"], "call_columbus_2");
        assert_eq!(h[4]["role"], "tool");
        assert!(h[4]["content"].as_str().unwrap().contains("Not run."));
        // After every tool message: one message with the pictures, each
        // under a line saying which tool made it.
        assert_eq!(h[5]["role"], "user");
        let parts = h[5]["content"].as_array().unwrap();
        let kinds: Vec<&str> = parts.iter().map(|p| p["type"].as_str().unwrap()).collect();
        assert_eq!(
            kinds,
            ["text", "image_url", "text", "image_url", "image_url"]
        );
        assert!(
            parts[0]["text"].as_str().unwrap().contains("map_snapshot")
                && parts[0]["text"]
                    .as_str()
                    .unwrap()
                    .contains("call_columbus_1")
        );
        assert_eq!(parts[1]["image_url"]["url"], "data:image/png;base64,QUJD");
        // Stopped while the model is asked again: the pictures are not the
        // person's message, and stay with the results.
        w.forget_unanswered();
        assert_eq!(w.history().len(), 6);
    }

    #[test]
    fn a_service_that_takes_no_pictures_gets_a_line_of_text() {
        let mut w = Completions::new(false);
        w.results(vec![outcome("a", "drawn", &["QUJD"])]);
        assert_eq!(w.history().len(), 1, "no message of pictures");
        assert_eq!(
            w.history()[0]["content"],
            json!(format!("drawn\n{NO_PICTURE}"))
        );
    }

    #[test]
    fn the_end_of_a_reply_is_read_with_what_it_holds() {
        let tools = tool();
        let call = json!({"id": "c", "type": "function", "function": {"name": "map_snapshot", "arguments": "{}"}});
        let reply = |finish: Value, content: Value, calls: Value| {
            let mut w = Completions::new(true);
            w.user("x");
            let mut m = json!({"role": "assistant", "content": content});
            if !calls.is_null() {
                m["tool_calls"] = calls;
            }
            let r = w
                .reply(
                    json!({"choices": [{"finish_reason": finish, "message": m}]}),
                    &ask(&tools, false),
                )
                .unwrap();
            (r, w.history()[1].clone())
        };
        // Tool calls ask for tools, whatever the service calls the end.
        for finish in [
            json!("tool_calls"),
            json!("stop"),
            json!("function_call"),
            Value::Null,
        ] {
            let (r, kept) = reply(finish.clone(), json!("Let me look."), json!([call]));
            assert_eq!(r.stop, Stop::Tools, "{finish}");
            assert_eq!(r.texts, ["Let me look."]);
            assert_eq!(kept["content"], "Let me look.");
            assert_eq!(kept["tool_calls"][0]["id"], "c");
            assert_eq!(r.input_tokens, None, "no count was sent");
        }
        // Cut short: nothing is run, nothing of the call is kept.
        let (r, kept) = reply(json!("length"), Value::Null, json!([call]));
        assert_eq!(r.stop, Stop::Length);
        assert!(r.calls.is_empty());
        assert_eq!(
            kept,
            json!({"role": "assistant", "content": "(The reply reached its length limit.)"})
        );
        let (r, kept) = reply(json!("content_filter"), json!("Part"), Value::Null);
        assert_eq!(r.stop, Stop::Filtered);
        assert_eq!(kept["content"], "Part");
        let (r, kept) = reply(json!("stop"), json!(""), Value::Null);
        assert_eq!(r.stop, Stop::End);
        assert_eq!(kept["content"], "(No answer.)");
        // Content in parts, and a refusal's words.
        let (r, _) = reply(
            json!("stop"),
            json!([{"type": "text", "text": "Two "}, {"type": "text", "text": "parts."}]),
            Value::Null,
        );
        assert_eq!(r.texts, ["Two parts."]);
        let mut w = Completions::new(true);
        w.user("x");
        let r = w
            .reply(json!({"choices": [{"finish_reason": "stop", "message": {"role": "assistant", "content": null, "refusal": "I can't."}}]}), &ask(&tools, false))
            .unwrap();
        assert_eq!(r.texts, ["I can't."]);
        assert_eq!(w.history()[1]["content"], "I can't.");
        // An error in place of a reply, and no reply at all.
        let e = w
            .reply(
                json!({"error": {"message": "Provider returned error", "code": 502}}),
                &ask(&tools, false),
            )
            .unwrap_err();
        assert!(w.busy(&e));
        let e = w
            .reply(
                json!({"error": {"message": "Bad request", "code": 400}}),
                &ask(&tools, false),
            )
            .unwrap_err();
        assert!(!w.busy(&e));
        assert!(w
            .reply(json!({"choices": []}), &ask(&tools, false))
            .is_err());
    }

    fn chunk(delta: Value, finish: Value) -> Value {
        json!({"id": "chatcmpl-1", "object": "chat.completion.chunk", "choices": [{"index": 0, "delta": delta, "finish_reason": finish}], "usage": null})
    }

    #[test]
    fn streamed_tool_calls_are_joined_by_index() {
        let mut s = Assembly::default();
        assert!(!s.started());
        for v in [
            chunk(json!({"role": "assistant", "content": "Look"}), Value::Null),
            chunk(json!({"content": "ing."}), Value::Null),
            chunk(
                json!({"tool_calls": [{"index": 0, "id": "call_a", "type": "function", "function": {"name": "map_snapshot", "arguments": ""}}]}),
                Value::Null,
            ),
            chunk(
                json!({"tool_calls": [{"index": 1, "id": "call_b", "type": "function", "function": {"name": "whoami", "arguments": ""}}]}),
                Value::Null,
            ),
            chunk(
                json!({"tool_calls": [{"index": 0, "function": {"arguments": "{\"layers\""}}]}),
                Value::Null,
            ),
            chunk(
                json!({"tool_calls": [{"index": 1, "function": {"arguments": "{}"}}, {"index": 0, "function": {"arguments": ": []}"}}]}),
                Value::Null,
            ),
        ] {
            assert!(!s.take(v).unwrap());
        }
        assert!(s.started() && !s.ended(), "not whole until its end is said");
        s.take(chunk(json!({}), json!("tool_calls"))).unwrap();
        // The counts come last, on a chunk with no choices.
        s.take(json!({"choices": [], "usage": {"prompt_tokens": 10, "completion_tokens": 5}}))
            .unwrap();
        assert!(s.ended());
        let whole = s.whole();
        let m = &whole["choices"][0]["message"];
        assert_eq!(m["content"], "Looking.");
        assert_eq!(
            m["tool_calls"],
            json!([
                {"id": "call_a", "type": "function", "function": {"name": "map_snapshot", "arguments": "{\"layers\": []}"}},
                {"id": "call_b", "type": "function", "function": {"name": "whoami", "arguments": "{}"}}])
        );
        assert_eq!(whole["choices"][0]["finish_reason"], "tool_calls");
        assert_eq!(whole["usage"]["prompt_tokens"], 10);
    }

    #[test]
    fn streamed_tool_calls_without_an_index_begin_at_an_id_or_a_name() {
        let mut s = Assembly::default();
        for v in [
            // Each call whole in its own piece, with an extra field (as Gemini's endpoint sends).
            chunk(
                json!({"tool_calls": [{"id": "", "type": "function", "function": {"name": "map_snapshot", "arguments": "{\"layers\": [\"a\"]}"},
                "extra_content": {"google": {"thought_signature": "c2ln"}}}]}),
                Value::Null,
            ),
            chunk(
                json!({"tool_calls": [{"function": {"name": "whoami", "arguments": ""}}]}),
                Value::Null,
            ),
            // A piece with neither goes on with the last call.
            chunk(
                json!({"tool_calls": [{"function": {"arguments": "{}"}}]}),
                Value::Null,
            ),
            chunk(
                json!({"tool_calls": [{"id": "call_c", "function": {"name": "whoami", "arguments": "{}"}}]}),
                json!("tool_calls"),
            ),
        ] {
            s.take(v).unwrap();
        }
        let whole = s.whole();
        let calls = whole["choices"][0]["message"]["tool_calls"]
            .as_array()
            .unwrap();
        assert_eq!(calls.len(), 3, "{calls:?}");
        assert_eq!(calls[0]["function"]["arguments"], "{\"layers\": [\"a\"]}");
        assert_eq!(
            calls[0]["extra_content"],
            json!({"google": {"thought_signature": "c2ln"}})
        );
        assert_eq!(
            calls[1]["function"],
            json!({"name": "whoami", "arguments": "{}"})
        );
        assert_eq!(calls[2]["id"], "call_c");
        assert_eq!(whole["choices"][0]["message"]["content"], Value::Null);
    }

    #[test]
    fn odd_streams_are_read_as_they_mean() {
        // The id and the name repeated on every piece are the same id and name.
        let mut s = Assembly::default();
        for args in ["{\"a\"", ": 1}"] {
            s.take(chunk(json!({"tool_calls": [{"index": 0, "id": "call_r", "type": "function", "function": {"name": "whoami", "arguments": args}}]}), Value::Null)).unwrap();
        }
        // Another id at the same index is another call.
        s.take(chunk(json!({"tool_calls": [{"index": 0, "id": "call_s", "type": "function", "function": {"name": "whoami", "arguments": "{}"}}]}), json!("stop"))).unwrap();
        let whole = s.whole();
        let calls = whole["choices"][0]["message"]["tool_calls"]
            .as_array()
            .unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(
            calls[0]["function"],
            json!({"name": "whoami", "arguments": "{\"a\": 1}"})
        );
        assert_eq!(calls[1]["id"], "call_s");
        // Counts on the last chunk that still has a choice (as OpenRouter sends).
        let mut s = Assembly::default();
        s.take(chunk(json!({"content": "Hi"}), Value::Null))
            .unwrap();
        let mut last = chunk(json!({"content": ""}), json!("stop"));
        last["usage"] = json!({"prompt_tokens": 7});
        s.take(last).unwrap();
        let whole = s.whole();
        assert_eq!(whole["usage"]["prompt_tokens"], 7);
        assert_eq!(whole["choices"][0]["message"]["content"], "Hi");
        // An error in the middle of a stream.
        let e = Assembly::default()
            .take(json!({"error": {"code": 502, "message": "Upstream failed"}, "choices": []}))
            .unwrap_err();
        let r = e.downcast_ref::<Refused>().unwrap();
        assert!(r.busy && r.said == "Upstream failed");
        // A stream that stops before its end was said is not a whole reply.
        let mut s = Assembly::default();
        s.take(chunk(json!({"tool_calls": [{"index": 0, "id": "c", "function": {"name": "whoami", "arguments": "{\"a"}}]}), Value::Null)).unwrap();
        assert!(s.started() && !s.ended());
    }

    #[test]
    fn old_results_are_shortened_pictures_first() {
        let tools = tool();
        let mut w = Completions::new(true);
        for n in 0..16 {
            w.user("more");
            let id = format!("c{n}");
            w.reply(json!({"choices": [{"finish_reason": "tool_calls", "message": {"role": "assistant", "content": null,
                "tool_calls": [{"id": id, "type": "function", "function": {"name": "map_snapshot", "arguments": "{}"}}]}}]}), &ask(&tools, false)).unwrap();
            w.results(vec![outcome(
                &id,
                &"t".repeat(40_000),
                &["A".repeat(60_000).as_str()],
            )]);
        }
        assert!(size(w.history()) > HISTORY_CHARS);
        w.trim();
        assert!(size(w.history()) <= HISTORY_CHARS, "{}", size(w.history()));
        let h = w.history();
        let tools_said: Vec<&Value> = h.iter().filter(|m| m["role"] == "tool").collect();
        assert_eq!(tools_said.len(), 16, "every call still has its result");
        assert!(tools_said[0]["content"]
            .as_str()
            .unwrap()
            .contains("removed to keep the conversation short"));
        assert!(tools_said[15]["content"]
            .as_str()
            .unwrap()
            .starts_with("tttt"));
        // Only the latest step's pictures are left.
        let with_pictures = h
            .iter()
            .filter(|m| m.to_string().contains("image_url"))
            .count();
        assert_eq!(with_pictures, 1);
        assert!(h.last().unwrap()["content"].is_array());
    }
}
