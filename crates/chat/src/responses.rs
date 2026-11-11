//! The OpenAI Responses format (`POST <address>/responses`), which OpenAI
//! and xAI take: the conversation as that API's own `input` items.
//!
//! Nothing is kept at the service (`store: false`): every request carries
//! the whole conversation. The items a reply is made of (reasoning,
//! message, function_call) are kept as they came and sent back while the
//! model is unchanged; the model's reasoning travels encrypted
//! (`include: ["reasoning.encrypted_content"]`) and only that model can
//! read it. Shapes as in OpenAI's own SDK (`openai-python`,
//! `src/openai/types/responses`), read on 2026-11-11.

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};

use crate::schema;
use crate::wire::{
    arguments, arguments_value, error_text, size, Ask, Assemble, Body, Call, Ids, Outcome, Refused,
    Reply, Stop, Wire, NO_PICTURE,
};
use crate::{HISTORY_CHARS, MAX_OUTPUT_TOKENS};

/// How long the service may send nothing in the middle of a reply. A model
/// that reasons sends nothing while it does, sometimes for minutes.
const IDLE: Duration = Duration::from_secs(600);

const LENGTH_NOTE: &str = "(The reply reached its length limit.)";
const FILTER_NOTE: &str = "(The service's content filter cut the reply short.)";

pub(crate) struct Responses {
    /// The conversation: the API's own `input` items.
    input: Vec<Value>,
    /// Where the person's message is while no reply to it has come.
    open: Option<usize>,
    /// Where the latest reply begins: what is before it may be shortened.
    step: usize,
    ids: Ids,
    pictures: bool,
}

impl Responses {
    pub fn new(pictures: bool) -> Responses {
        Responses {
            input: Vec::new(),
            open: None,
            step: 0,
            ids: Ids::default(),
            pictures,
        }
    }
}

/// An item of a reply as a plain input item: no id of the service's, so it
/// stands by itself (a reasoning item is tied to the items made with it by
/// their ids). `None` for what is not kept that way (reasoning).
fn plain_item(item: &Value) -> Option<Value> {
    match item["type"].as_str() {
        Some("message") => {
            let text = message_text(item).join("\n");
            if text.trim().is_empty() {
                return None;
            }
            let mut m = json!({"role": "assistant", "content": text});
            if let Some(p) = item.get("phase").filter(|p| p.is_string()) {
                m["phase"] = p.clone();
            }
            Some(m)
        }
        Some("function_call") => Some(json!({
            "type": "function_call",
            "call_id": item["call_id"],
            "name": item["name"],
            "arguments": item["arguments"],
        })),
        Some("reasoning") => None,
        // A message without a type (ours, or one already made plain) and
        // tool results stay as they are.
        _ => Some(item.clone()),
    }
}

/// The words of a message item: its text parts, and a refusal's words.
fn message_text(item: &Value) -> Vec<String> {
    match &item["content"] {
        Value::String(s) => vec![s.clone()],
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| match p["type"].as_str() {
                Some("output_text") | Some("text") => p["text"].as_str(),
                Some("refusal") => p["refusal"].as_str(),
                _ => None,
            })
            .map(String::from)
            .collect(),
        _ => vec![],
    }
}

impl Wire for Responses {
    fn user(&mut self, text: &str) {
        self.input.push(json!({"role": "user", "content": text}));
        self.open = Some(self.input.len() - 1);
    }

    fn forget_unanswered(&mut self) {
        if self.open.is_some_and(|i| i + 1 == self.input.len()) {
            self.input.pop();
        }
        self.open = None;
    }

    fn trim(&mut self) {
        if size(&self.input) <= HISTORY_CHARS {
            return;
        }
        let last = self.step.min(self.input.len());
        // Pictures go first, then the oldest tool results.
        for item in self.input.iter_mut().take(last) {
            if item["type"] != "function_call_output" {
                continue;
            }
            for p in item["output"].as_array_mut().into_iter().flatten() {
                if p["type"] == "input_image" {
                    *p = json!({"type": "input_text", "text": "[An earlier picture, removed to keep the conversation short.]"});
                }
            }
        }
        let mut total = size(&self.input);
        for item in self.input.iter_mut().take(last) {
            if total <= HISTORY_CHARS {
                break;
            }
            if item["type"] == "function_call_output" {
                let before = item.to_string().len();
                item["output"] = json!("[An older tool result, removed to keep the conversation short. Call the tool again if it is needed.]");
                total = total.saturating_sub(before.saturating_sub(item.to_string().len()));
            }
        }
    }

    fn body(&self, ask: &Ask) -> Body {
        let tools: Vec<Value> = ask
            .tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "name": t.name,
                    "description": schema::short(&t.description).0,
                    "parameters": schema::plain(&t.schema),
                    "strict": false,
                })
            })
            .collect();
        let mut body = json!({
            "model": ask.model,
            "instructions": format!("{}{}", ask.system, schema::notes(ask.tools)),
            "input": self.input,
            "tools": tools,
            // Nothing of the chat is kept at the service.
            "store": false,
            "stream": true,
        });
        // What the format can do without: the reasoning sent back
        // encrypted, the cap on a reply (thinking and answer together),
        // and the thinking level.
        if !ask.bare {
            body["include"] = json!(["reasoning.encrypted_content"]);
            body["max_output_tokens"] = json!(MAX_OUTPUT_TOKENS);
            if !ask.effort.is_empty() {
                body["reasoning"] = json!({"effort": ask.effort});
            }
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

    fn reply(&mut self, answer: Value, ask: &Ask) -> Result<Reply> {
        if let Some(r) = refused(&answer) {
            return Err(r.into());
        }
        let Some(output) = answer["output"].as_array() else {
            bail!("the AI service sent no reply");
        };
        let u = &answer["usage"];
        // Cached tokens are part of the count, and count a tenth.
        let input_tokens = u["input_tokens"].as_u64().map(|n| {
            let cached = u["input_tokens_details"]["cached_tokens"]
                .as_u64()
                .unwrap_or(0)
                .min(n);
            n - cached + cached / 10
        });
        let reason = answer["incomplete_details"]["reason"].as_str();
        let cut = match (answer["status"].as_str(), reason) {
            (_, Some("content_filter")) => Some(Stop::Filtered),
            (Some("incomplete"), _) | (_, Some(_)) => Some(Stop::Length),
            _ => None,
        };
        let mut texts = Vec::new();
        let mut calls = Vec::new();
        let mut kept = Vec::new();
        for item in output {
            match item["type"].as_str() {
                Some("message") => {
                    texts.extend(
                        message_text(item)
                            .into_iter()
                            .filter(|t| !t.trim().is_empty()),
                    );
                    kept.push(item.clone());
                }
                Some("function_call") => {
                    let mut item = item.clone();
                    let id = self.ids.take(item["call_id"].as_str().unwrap_or(""));
                    item["call_id"] = json!(id);
                    let (input, mut fault) = match &item["arguments"] {
                        Value::String(s) => arguments(s),
                        other => arguments_value(other),
                    };
                    if fault.is_none() && item["status"] == "incomplete" {
                        fault = Some(crate::wire::INCOMPLETE.to_string());
                    }
                    // Sent back as it came, unless the service could not
                    // read it back: then as a call with no arguments.
                    let whole = item["arguments"]
                        .as_str()
                        .is_some_and(|a| !a.trim().is_empty());
                    if fault.is_some() {
                        item["arguments"] = json!("{}");
                    } else if !whole {
                        item["arguments"] = json!(input.to_string());
                    }
                    calls.push(Call {
                        id,
                        name: item["name"].as_str().unwrap_or_default().to_string(),
                        input,
                        fault,
                    });
                    kept.push(item);
                }
                Some("reasoning") => kept.push(item.clone()),
                // Columbus asks for no other kind of item, and keeps none.
                _ => {}
            }
        }
        let stop = match cut {
            Some(c) => c,
            None if !calls.is_empty() => Stop::Tools,
            None => Stop::End,
        };
        if stop != Stop::Tools {
            calls.clear();
        }
        // A reply that was cut short keeps its words only: a call in it
        // will not be run, and reasoning cannot be sent back without the
        // call it led to. The same holds once the service has refused the
        // fields that carry reasoning.
        if stop == Stop::Length || stop == Stop::Filtered || ask.bare {
            kept = kept
                .iter()
                .filter(|i| stop == Stop::Tools || i["type"] != "function_call")
                .filter_map(plain_item)
                .collect();
        }
        if kept.iter().all(|i| i["type"] == "reasoning" || i.is_null()) {
            // Nothing the next request could stand on: say so in a turn.
            kept.retain(|i| i["type"] != "reasoning");
            kept.push(json!({"role": "assistant", "content": match stop {
                Stop::Length => LENGTH_NOTE,
                Stop::Filtered => FILTER_NOTE,
                _ => "(No answer.)",
            }}));
        }
        self.step = self.input.len();
        self.input.extend(kept);
        self.open = None;
        Ok(Reply {
            texts,
            calls,
            stop,
            input_tokens,
        })
    }

    fn results(&mut self, outcomes: Vec<Outcome>) {
        for o in outcomes {
            let text = if o.is_error {
                error_text(&o.text)
            } else {
                o.text
            };
            let output = if o.images.is_empty() || o.is_error {
                json!(text)
            } else if self.pictures {
                // The picture rides inside the tool's result.
                let mut parts = vec![json!({"type": "input_text", "text": text})];
                for d in &o.images {
                    parts.push(json!({"type": "input_image", "image_url": format!("data:image/png;base64,{d}"), "detail": "auto"}));
                }
                Value::Array(parts)
            } else {
                json!(format!("{text}\n{NO_PICTURE}"))
            };
            self.input
                .push(json!({"type": "function_call_output", "call_id": o.id, "output": output}));
        }
    }

    fn model_changed(&mut self) {
        // Another model cannot read this one's reasoning; what stays is
        // made plain, so nothing still points at an item that is gone.
        self.input = self.input.iter().filter_map(plain_item).collect();
        self.open = None;
        self.step = self.input.len();
    }

    fn history(&self) -> &[Value] {
        &self.input
    }
}

/// The service's own refusal, if `answer` is one: a response that failed,
/// or an error in place of a response.
fn refused(answer: &Value) -> Option<Refused> {
    let e = &answer["error"];
    if answer["status"] != "failed" && e.is_null() {
        return None;
    }
    let code = e["code"].as_str().or(e["type"].as_str()).unwrap_or("");
    let said = e["message"]
        .as_str()
        .or(e.as_str())
        .unwrap_or("it could not make a reply")
        .to_string();
    Some(Refused {
        busy: is_busy(code),
        said,
    })
}

fn is_busy(code: &str) -> bool {
    let c = code.to_ascii_lowercase();
    ["server_error", "rate_limit", "overloaded", "unavailable"]
        .iter()
        .any(|w| c.contains(w))
}

/// A streamed response: the whole `response` its last event carries.
#[derive(Default)]
struct Assembly {
    started: bool,
    response: Value,
    /// Items as each was finished, by their place in the output: used if
    /// the last event's response does not list them.
    done: BTreeMap<u64, Value>,
}

impl Assemble for Assembly {
    fn take(&mut self, v: Value) -> Result<bool> {
        let kind = v["type"].as_str().unwrap_or("");
        if kind.starts_with("response.") {
            self.started = true;
        }
        match kind {
            "response.output_item.done" => {
                if let Some(i) = v["output_index"].as_u64() {
                    if i > 1000 {
                        bail!("the reply had too many parts");
                    }
                    self.done.insert(i, v["item"].clone());
                }
            }
            "response.completed" | "response.incomplete" => {
                self.response = v["response"].clone();
                return Ok(true);
            }
            "response.failed" => {
                let r = refused(&json!({"status": "failed", "error": v["response"]["error"]}));
                return Err(r
                    .map(Into::into)
                    .unwrap_or_else(|| anyhow!("the reply failed")));
            }
            "error" => {
                // The event carries its fields itself; some services nest them.
                let e = if v["error"].is_object() {
                    &v["error"]
                } else {
                    &v
                };
                let code = e["code"].as_str().or(e["type"].as_str()).unwrap_or("");
                return Err(Refused {
                    busy: is_busy(code),
                    said: e["message"].as_str().unwrap_or("an error").to_string(),
                }
                .into());
            }
            _ => {}
        }
        Ok(false)
    }

    fn started(&self) -> bool {
        self.started
    }

    fn whole(&mut self) -> Value {
        let mut r = std::mem::take(&mut self.response);
        if !r.is_object() {
            r = json!({});
        }
        let listed = r["output"].as_array().is_some_and(|a| !a.is_empty());
        if !listed && !self.done.is_empty() {
            r["output"] = Value::Array(std::mem::take(&mut self.done).into_values().collect());
        }
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::Tool;

    fn ask<'a>(tools: &'a [Tool], effort: &'a str, bare: bool) -> Ask<'a> {
        Ask {
            model: "gpt-6.1-sol",
            effort,
            system: "You are the assistant.",
            tools,
            bare,
        }
    }

    fn tool() -> Vec<Tool> {
        vec![Tool {
            name: "map_view".into(),
            description: "Shows the map.".into(),
            schema: json!({"$schema": "x", "type": "object", "properties": {"zoom_to": {"type": ["string", "array", "null"], "default": null}}}),
        }]
    }

    #[test]
    fn the_request_is_stateless_with_flat_tools() {
        let tools = tool();
        let mut w = Responses::new(true);
        w.user("Where are the tracts?");
        let b = w.body(&ask(&tools, "low", false));
        assert!(b.optional);
        let j = &b.json;
        assert_eq!(j["model"], "gpt-6.1-sol");
        assert_eq!(j["instructions"], "You are the assistant.");
        assert_eq!(
            j["input"],
            json!([{"role": "user", "content": "Where are the tracts?"}])
        );
        assert_eq!(j["store"], false);
        assert_eq!(j["stream"], true);
        assert_eq!(j["include"], json!(["reasoning.encrypted_content"]));
        assert_eq!(j["max_output_tokens"], MAX_OUTPUT_TOKENS);
        assert_eq!(j["reasoning"], json!({"effort": "low"}));
        let t = &j["tools"][0];
        assert_eq!(t["type"], "function");
        assert_eq!(t["name"], "map_view");
        assert_eq!(t["strict"], false);
        assert!(t.get("function").is_none(), "flat, not nested");
        assert_eq!(schema::not_plain(&t["parameters"]), Vec::<String>::new());
        // The model's own thinking level: nothing is said about it.
        assert!(w
            .body(&ask(&tools, "", false))
            .json
            .get("reasoning")
            .is_none());
        // After a refusal: without what the format can do without.
        let b = w.body(&ask(&tools, "low", true));
        assert!(!b.optional);
        for k in ["include", "max_output_tokens", "reasoning"] {
            assert!(b.json.get(k).is_none(), "{k}");
        }
        assert_eq!(b.json["store"], false, "never kept at the service");
    }

    #[test]
    fn a_reply_is_kept_as_it_came_and_calls_get_their_results() {
        let tools = tool();
        let mut w = Responses::new(true);
        w.user("Show me.");
        let answer = json!({"status": "completed", "output": [
            {"type": "reasoning", "id": "rs_1", "summary": [], "encrypted_content": "gAAAA"},
            {"type": "message", "id": "msg_1", "role": "assistant", "status": "completed", "phase": "commentary",
             "content": [{"type": "output_text", "text": "Looking.", "annotations": []}]},
            {"type": "function_call", "id": "fc_1", "call_id": "call_a", "name": "map_view", "arguments": "{\"zoom_to\": \"ws://s/a.gpkg\"}", "status": "completed"},
            {"type": "function_call", "id": "fc_2", "call_id": "call_a", "name": "map_view", "arguments": "{\"zoom_to\": ", "status": "completed"},
            {"type": "web_search_call", "id": "ws_1"}],
            "usage": {"input_tokens": 1000, "input_tokens_details": {"cached_tokens": 800}, "output_tokens": 5}});
        let r = w.reply(answer, &ask(&tools, "", false)).unwrap();
        assert_eq!(r.stop, Stop::Tools);
        assert_eq!(r.texts, ["Looking."]);
        assert_eq!(r.input_tokens, Some(200 + 80));
        assert_eq!(r.calls.len(), 2);
        assert_eq!(r.calls[0].id, "call_a");
        assert_eq!(r.calls[0].input, json!({"zoom_to": "ws://s/a.gpkg"}));
        assert!(r.calls[0].fault.is_none());
        // The repeated id is replaced; the damaged call is marked.
        assert_eq!(r.calls[1].id, "call_columbus_1");
        assert!(r.calls[1].fault.as_deref().unwrap().contains("incomplete"));
        let h = w.history();
        assert_eq!(h.len(), 5, "{h:?}");
        assert_eq!(
            h[1]["encrypted_content"], "gAAAA",
            "reasoning goes back as it came"
        );
        assert_eq!(h[2]["id"], "msg_1");
        assert_eq!(h[2]["phase"], "commentary");
        assert_eq!(h[3]["call_id"], "call_a");
        assert_eq!(h[4]["call_id"], "call_columbus_1");
        assert_eq!(
            h[4]["arguments"], "{}",
            "a damaged call goes back as one the service can read"
        );
        w.results(vec![
            Outcome {
                id: "call_a".into(),
                name: "map_view".into(),
                text: "changed".into(),
                is_error: false,
                images: vec!["QUJD".into()],
            },
            Outcome {
                id: "call_columbus_1".into(),
                name: "map_view".into(),
                text: "Not run.".into(),
                is_error: true,
                images: vec![],
            },
        ]);
        let h = w.history();
        assert_eq!(
            h[5],
            json!({"type": "function_call_output", "call_id": "call_a", "output": [
                {"type": "input_text", "text": "changed"},
                {"type": "input_image", "image_url": "data:image/png;base64,QUJD", "detail": "auto"}]})
        );
        assert_eq!(h[6]["call_id"], "call_columbus_1");
        assert!(h[6]["output"].as_str().unwrap().contains("Not run."));
        // Another model: the reasoning goes, and what stays stands by itself.
        w.model_changed();
        let h = w.history();
        assert_eq!(h.len(), 6);
        assert!(
            h.iter()
                .all(|i| i["type"] != "reasoning" && i.get("id").is_none()),
            "{h:?}"
        );
        assert_eq!(
            h[1],
            json!({"role": "assistant", "content": "Looking.", "phase": "commentary"})
        );
        assert_eq!(
            h[2],
            json!({"type": "function_call", "call_id": "call_a", "name": "map_view", "arguments": "{\"zoom_to\": \"ws://s/a.gpkg\"}"})
        );
        assert_eq!(h[4]["type"], "function_call_output");
    }

    #[test]
    fn a_reply_cut_short_keeps_its_words_and_runs_nothing() {
        let tools = tool();
        let mut w = Responses::new(true);
        w.user("Go on.");
        let r = w
            .reply(
                json!({"status": "incomplete", "incomplete_details": {"reason": "max_output_tokens"}, "output": [
                    {"type": "reasoning", "id": "rs_1", "summary": []},
                    {"type": "function_call", "call_id": "c1", "name": "map_view", "arguments": "{\"zo", "status": "incomplete"}]}),
                &ask(&tools, "", false),
            )
            .unwrap();
        assert_eq!(r.stop, Stop::Length);
        assert!(r.calls.is_empty());
        assert_eq!(r.input_tokens, None);
        assert_eq!(
            w.history()[1],
            json!({"role": "assistant", "content": LENGTH_NOTE})
        );
        let r = w
            .reply(
                json!({"status": "incomplete", "incomplete_details": {"reason": "content_filter"}, "output": [
                    {"type": "message", "id": "m", "role": "assistant", "status": "incomplete", "content": [{"type": "output_text", "text": "Part of an answer"}]}]}),
                &ask(&tools, "", false),
            )
            .unwrap();
        assert_eq!(r.stop, Stop::Filtered);
        assert_eq!(
            w.history()[2],
            json!({"role": "assistant", "content": "Part of an answer"})
        );
        // Nothing at all: still a turn.
        let r = w
            .reply(
                json!({"status": "completed", "output": []}),
                &ask(&tools, "", false),
            )
            .unwrap();
        assert_eq!(r.stop, Stop::End);
        assert_eq!(w.history()[3]["content"], "(No answer.)");
        // A refusal's words are the model's words.
        let r = w
            .reply(
                json!({"status": "completed", "output": [{"type": "message", "id": "m2", "role": "assistant", "status": "completed",
                    "content": [{"type": "refusal", "refusal": "I can't help with that."}]}]}),
                &ask(&tools, "", false),
            )
            .unwrap();
        assert_eq!(r.texts, ["I can't help with that."]);
    }

    #[test]
    fn a_failed_response_is_the_services_refusal() {
        let tools = tool();
        let mut w = Responses::new(true);
        w.user("x");
        let e = w
            .reply(json!({"status": "failed", "error": {"code": "server_error", "message": "The server had an error."}, "output": []}), &ask(&tools, "", false))
            .unwrap_err();
        let r = e.downcast_ref::<Refused>().expect("a refusal");
        assert!(r.busy && r.said == "The server had an error.");
        assert!(w.busy(&e));
        assert_eq!(w.history().len(), 1, "nothing of a failed reply is kept");
        let e = w
            .reply(
                json!({"error": {"message": "Unknown parameter", "type": "invalid_request_error"}}),
                &ask(&tools, "", false),
            )
            .unwrap_err();
        assert!(!w.busy(&e));
        assert!(w
            .reply(json!({"status": "completed"}), &ask(&tools, "", false))
            .unwrap_err()
            .to_string()
            .contains("no reply"));
    }

    #[test]
    fn a_stream_is_read_to_its_last_event() {
        let mut s = Assembly::default();
        assert!(!s.started());
        assert!(!s
            .take(json!({"type": "response.created", "response": {"status": "in_progress"}}))
            .unwrap());
        assert!(s.started());
        assert!(!s
            .take(json!({"type": "response.output_text.delta", "delta": "Hel"}))
            .unwrap());
        assert!(!s.take(json!({"type": "response.output_item.done", "output_index": 0, "item": {"type": "message", "content": [{"type": "output_text", "text": "Hello"}]}})).unwrap());
        assert!(!s.ended(), "only the last event ends a response");
        assert!(s.take(json!({"type": "response.completed", "response": {"status": "completed", "output": [{"type": "message", "content": [{"type": "output_text", "text": "Hello"}]}], "usage": {"input_tokens": 3}}})).unwrap());
        let whole = s.whole();
        assert_eq!(whole["status"], "completed");
        assert_eq!(whole["output"][0]["content"][0]["text"], "Hello");
        // A service whose last event lists no items: the finished ones, in order.
        let mut s = Assembly::default();
        s.take(json!({"type": "response.output_item.done", "output_index": 1, "item": {"type": "function_call", "call_id": "b"}})).unwrap();
        s.take(json!({"type": "response.output_item.done", "output_index": 0, "item": {"type": "reasoning", "id": "r"}})).unwrap();
        assert!(s.take(json!({"type": "response.incomplete", "response": {"status": "incomplete", "output": []}})).unwrap());
        let whole = s.whole();
        assert_eq!(whole["output"][0]["type"], "reasoning");
        assert_eq!(whole["output"][1]["call_id"], "b");
        // Errors: the event's own fields, or nested; busy ones are told apart.
        let e = Assembly::default()
            .take(json!({"type": "error", "code": "rate_limit_exceeded", "message": "Slow down"}))
            .unwrap_err();
        let r = e.downcast_ref::<Refused>().unwrap();
        assert!(r.busy && r.said == "Slow down");
        let e = Assembly::default().take(json!({"type": "error", "error": {"type": "invalid_request_error", "message": "Bad input"}})).unwrap_err();
        assert!(!e.downcast_ref::<Refused>().unwrap().busy);
        let e = Assembly::default().take(json!({"type": "response.failed", "response": {"status": "failed", "error": {"code": "server_error", "message": "Oops"}}})).unwrap_err();
        assert!(e.downcast_ref::<Refused>().unwrap().busy);
    }

    #[test]
    fn old_results_are_shortened_pictures_first() {
        let tools = tool();
        let mut w = Responses::new(true);
        for n in 0..16 {
            w.user("more");
            let id = format!("c{n}");
            w.reply(json!({"status": "completed", "output": [{"type": "function_call", "call_id": id, "name": "map_view", "arguments": "{}"}]}), &ask(&tools, "", false)).unwrap();
            w.results(vec![Outcome {
                id,
                name: "map_view".into(),
                text: "t".repeat(40_000),
                is_error: false,
                images: vec!["A".repeat(60_000)],
            }]);
        }
        assert!(size(w.history()) > HISTORY_CHARS);
        w.trim();
        assert!(size(w.history()) <= HISTORY_CHARS, "{}", size(w.history()));
        let outputs: Vec<&Value> = w
            .history()
            .iter()
            .filter(|i| i["type"] == "function_call_output")
            .collect();
        // Every call still has its result; the latest step is whole; no
        // earlier picture is left; the oldest results are a note.
        assert_eq!(outputs.len(), 16);
        assert_eq!(outputs[15]["output"][1]["type"], "input_image");
        assert!(outputs[..15]
            .iter()
            .all(|o| !o.to_string().contains("input_image")));
        assert!(outputs[0]["output"]
            .as_str()
            .unwrap()
            .contains("removed to keep the conversation short"));
        assert!(
            outputs[14]["output"][0]["text"]
                .as_str()
                .unwrap()
                .starts_with("tttt"),
            "the newer ones keep their words"
        );
    }

    #[test]
    fn a_service_that_takes_no_pictures_gets_a_line_of_text() {
        let mut w = Responses::new(false);
        w.results(vec![Outcome {
            id: "a".into(),
            name: "map_snapshot".into(),
            text: "drawn".into(),
            is_error: false,
            images: vec!["QUJD".into()],
        }]);
        assert_eq!(
            w.history()[0]["output"],
            json!(format!("drawn\n{NO_PICTURE}"))
        );
    }
}
