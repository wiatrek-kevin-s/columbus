//! Stand-ins for AI services, one for each request format the app's chat
//! speaks: Anthropic Messages, OpenAI Responses and OpenAI Chat
//! Completions. No real service is ever contacted and no real key exists.
//!
//! A test writes what "the model" says next in terms that are the same for
//! every format ([`Said`]); the stand-in answers it in the format's own
//! shape, as plain JSON or as a stream, written from the shapes in the
//! services' own SDK sources (read on 2026-11-11). Like the real services,
//! a stand-in refuses a conversation that is not well formed (a tool call
//! without its result, a result without its call), with a 400 in its own
//! error shape: so every test here also shows that what Columbus sends is
//! a conversation the service would take.

#![allow(dead_code)]

use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Router;
use serde_json::{json, Value};
use tokio::sync::Notify;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fmt {
    Messages,
    Responses,
    Chat,
}

pub const ALL: [Fmt; 3] = [Fmt::Messages, Fmt::Responses, Fmt::Chat];
/// The two formats added with build brief 3, D-22.
pub const NEW: [Fmt; 2] = [Fmt::Responses, Fmt::Chat];

impl Fmt {
    /// What comes after the stand-in's own address in the service's
    /// address, and the path a chat request then goes to.
    pub fn base_path(self) -> &'static str {
        match self {
            Fmt::Messages => "",
            Fmt::Responses | Fmt::Chat => "/v1",
        }
    }

    pub fn chat_path(self) -> &'static str {
        match self {
            Fmt::Messages => "/v1/messages",
            Fmt::Responses => "/v1/responses",
            Fmt::Chat => "/v1/chat/completions",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Fmt::Messages => "messages",
            Fmt::Responses => "responses",
            Fmt::Chat => "chat",
        }
    }
}

/// How a reply ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum End {
    /// The model has answered.
    Stop,
    /// It asks for tools.
    Tools,
    /// It was cut off at its length limit.
    Length,
}

/// A tool call as the model "writes" it: its arguments as text, so a test
/// can send them damaged.
#[derive(Debug, Clone)]
pub struct SaidCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
    /// Extra fields on the call, as some services send (Chat Completions
    /// and Responses only).
    pub extra: Value,
}

/// One reply, in terms that are the same for every format.
#[derive(Debug, Clone)]
pub struct Said {
    pub text: Option<String>,
    pub calls: Vec<SaidCall>,
    pub end: End,
    /// Thinking before the answer (Messages: a thinking block; Responses:
    /// a reasoning item; Chat Completions has none to keep).
    pub thinking: bool,
    /// Input tokens: fresh ones, and ones read from the service's cache.
    pub usage: Option<(u64, u64)>,
}

pub fn say(text: &str) -> Said {
    Said {
        text: Some(text.to_string()),
        calls: vec![],
        end: End::Stop,
        thinking: false,
        usage: None,
    }
}

pub fn call(id: &str, name: &str, args: Value) -> Said {
    calls(&[(id, name, args)])
}

pub fn calls(list: &[(&str, &str, Value)]) -> Said {
    Said {
        text: Some(format!("Calling {}.", list[0].1)),
        calls: list
            .iter()
            .map(|(id, name, args)| SaidCall {
                id: id.to_string(),
                name: name.to_string(),
                arguments: args.to_string(),
                extra: Value::Null,
            })
            .collect(),
        end: End::Tools,
        thinking: false,
        usage: None,
    }
}

impl Said {
    pub fn usage(mut self, fresh: u64, cached: u64) -> Said {
        self.usage = Some((fresh, cached));
        self
    }
    pub fn thinking(mut self) -> Said {
        self.thinking = true;
        self
    }
    pub fn end(mut self, end: End) -> Said {
        self.end = end;
        self
    }
    pub fn no_text(mut self) -> Said {
        self.text = None;
        self
    }
    /// The arguments of call `n`, as this exact text.
    pub fn raw_arguments(mut self, n: usize, text: &str) -> Said {
        self.calls[n].arguments = text.to_string();
        self
    }
    pub fn extra(mut self, n: usize, extra: Value) -> Said {
        self.calls[n].extra = extra;
        self
    }
}

/// What the stand-in does with the next request.
pub enum Next {
    Reply(Said),
    /// Export, or take a snapshot of, the layer the last tool result made
    /// (as a model would after reading it).
    ExportLast,
    SnapshotLast,
    /// An answer that is not a reply: a status, headers and a body.
    Status(u16, Vec<(&'static str, String)>, Value),
    /// A body in the format's own shape, sent as JSON with status 200.
    Raw(Value),
    /// Server-sent events exactly as given.
    Stream(String),
    /// Wait until the test says go, then do what comes next.
    Hold(Arc<Notify>),
}

/// One request as the stand-in received it.
#[derive(Debug, Clone)]
pub struct Seen {
    pub method: String,
    pub path: String,
    pub headers: HeaderMap,
    pub body: Value,
}

impl Seen {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }
}

pub struct Inner {
    pub fmt: Fmt,
    pub streamed: bool,
    pub script: VecDeque<Next>,
    pub seen: Vec<Seen>,
    /// Requests the stand-in refused as not well formed, with why.
    pub refused: Vec<String>,
    /// The models `GET …/models` lists.
    pub models: Value,
}

#[derive(Clone)]
pub struct StandIn {
    /// The service's address as Columbus is given it.
    pub base: String,
    pub fmt: Fmt,
    pub inner: Arc<Mutex<Inner>>,
}

impl StandIn {
    pub async fn start(fmt: Fmt, streamed: bool, script: Vec<Next>) -> StandIn {
        let inner = Arc::new(Mutex::new(Inner {
            fmt,
            streamed,
            script: script.into(),
            seen: vec![],
            refused: vec![],
            models: json!({"object": "list", "data": [{"id": "model-a", "object": "model"}, {"id": "model-b", "object": "model"}]}),
        }));
        let app = Router::new().fallback(any).with_state(inner.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        StandIn {
            base: format!("http://{addr}{}", fmt.base_path()),
            fmt,
            inner,
        }
    }

    pub fn push(&self, next: Next) {
        self.inner.lock().unwrap().script.push_back(next);
    }

    pub fn seen(&self) -> Vec<Seen> {
        self.inner.lock().unwrap().seen.clone()
    }

    /// The chat requests received (not the model lists).
    pub fn requests(&self) -> Vec<Value> {
        self.seen()
            .into_iter()
            .filter(|s| s.method == "POST")
            .map(|s| s.body)
            .collect()
    }

    pub fn refused(&self) -> Vec<String> {
        self.inner.lock().unwrap().refused.clone()
    }

    pub fn left(&self) -> usize {
        self.inner.lock().unwrap().script.len()
    }

    /// Wait until `n` chat requests have arrived.
    pub async fn until_requests(&self, n: usize) {
        for _ in 0..600 {
            if self.requests().len() >= n {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("the stand-in was not sent request {n}");
    }
}

async fn any(
    State(st): State<Arc<Mutex<Inner>>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    bytes: Bytes,
) -> Response {
    let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    let path = uri.path().to_string();
    let (fmt, streamed) = {
        let mut g = st.lock().unwrap();
        g.seen.push(Seen {
            method: method.to_string(),
            path: path.clone(),
            headers,
            body: body.clone(),
        });
        (g.fmt, g.streamed)
    };
    if method == Method::GET {
        // (After each format's own address, the list is at this one path.)
        if path == "/v1/models" {
            let models = st.lock().unwrap().models.clone();
            return axum::Json(models).into_response();
        }
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    if path != fmt.chat_path() {
        return error(fmt, 404, "not_found_error", "nothing is served here");
    }
    if let Err(why) = well_formed(fmt, &body) {
        st.lock().unwrap().refused.push(why.clone());
        return error(fmt, 400, "invalid_request_error", &why);
    }
    loop {
        let next = st.lock().unwrap().script.pop_front();
        let said = match next {
            None => say("(script ended)"),
            Some(Next::Reply(s)) => s,
            Some(Next::ExportLast) => {
                let h = last_handle(fmt, &body);
                call(
                    "t3",
                    "export_output",
                    json!({"handle": h, "folder": "outbox", "name": "tracts"}),
                )
            }
            Some(Next::SnapshotLast) => {
                let h = last_handle(fmt, &body);
                call("s1", "map_snapshot", json!({"layers": [h]}))
            }
            Some(Next::Status(code, headers, body)) => {
                let mut r = (StatusCode::from_u16(code).unwrap(), axum::Json(body)).into_response();
                for (k, v) in headers {
                    r.headers_mut().insert(k, v.parse().unwrap());
                }
                return r;
            }
            Some(Next::Raw(v)) => return axum::Json(v).into_response(),
            Some(Next::Stream(text)) => {
                return ([("content-type", "text/event-stream")], text).into_response()
            }
            Some(Next::Hold(go)) => {
                go.notified().await;
                continue;
            }
        };
        return if streamed {
            ([("content-type", "text/event-stream")], stream(fmt, &said)).into_response()
        } else {
            axum::Json(plain(fmt, &said)).into_response()
        };
    }
}

/// An error answer in the format's own shape.
pub fn error_body(fmt: Fmt, kind: &str, message: &str) -> Value {
    match fmt {
        Fmt::Messages => json!({"type": "error", "error": {"type": kind, "message": message}}),
        _ => json!({"error": {"message": message, "type": kind, "param": null, "code": null}}),
    }
}

fn error(fmt: Fmt, code: u16, kind: &str, message: &str) -> Response {
    (
        StatusCode::from_u16(code).unwrap(),
        axum::Json(error_body(fmt, kind, message)),
    )
        .into_response()
}

/// The text of the last tool result in a request.
pub fn last_result(fmt: Fmt, body: &Value) -> String {
    let text_of = |v: &Value| match v {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .find_map(|p| p["text"].as_str())
            .unwrap_or("")
            .to_string(),
        _ => String::new(),
    };
    match fmt {
        Fmt::Messages => body["messages"]
            .as_array()
            .into_iter()
            .flatten()
            .rev()
            .flat_map(|m| m["content"].as_array().into_iter().flatten())
            .find(|b| b["type"] == "tool_result")
            .map(|b| text_of(&b["content"]))
            .unwrap_or_default(),
        Fmt::Responses => body["input"]
            .as_array()
            .into_iter()
            .flatten()
            .rev()
            .find(|i| i["type"] == "function_call_output")
            .map(|i| text_of(&i["output"]))
            .unwrap_or_default(),
        Fmt::Chat => body["messages"]
            .as_array()
            .into_iter()
            .flatten()
            .rev()
            .find(|m| m["role"] == "tool")
            .map(|m| text_of(&m["content"]))
            .unwrap_or_default(),
    }
}

/// Every tool result in a request, oldest first: the call's id and what
/// the model is given for it (the whole result, in the format's shape).
pub fn results(fmt: Fmt, body: &Value) -> Vec<(String, Value)> {
    let s = |v: &Value| v.as_str().unwrap_or("").to_string();
    match fmt {
        Fmt::Messages => body["messages"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|m| m["content"].as_array().into_iter().flatten())
            .filter(|b| b["type"] == "tool_result")
            .map(|b| (s(&b["tool_use_id"]), b.clone()))
            .collect(),
        Fmt::Responses => body["input"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|i| i["type"] == "function_call_output")
            .map(|i| (s(&i["call_id"]), i.clone()))
            .collect(),
        Fmt::Chat => body["messages"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|m| m["role"] == "tool")
            .map(|m| (s(&m["tool_call_id"]), m.clone()))
            .collect(),
    }
}

/// A tool result's words, whatever shape it is in.
pub fn result_text(fmt: Fmt, result: &Value) -> String {
    let v = match fmt {
        Fmt::Messages | Fmt::Chat => &result["content"],
        Fmt::Responses => &result["output"],
    };
    match v {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| p["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn last_handle(fmt: Fmt, body: &Value) -> String {
    let v: Value = serde_json::from_str(&last_result(fmt, body)).unwrap_or(Value::Null);
    v["result"]["layer"]["handle"]
        .as_str()
        .unwrap_or("")
        .to_string()
}

/// The system text of a request, wherever the format puts it.
pub fn system_text(fmt: Fmt, body: &Value) -> String {
    match fmt {
        Fmt::Messages => body["system"][0]["text"].as_str(),
        Fmt::Responses => body["instructions"].as_str(),
        Fmt::Chat => body["messages"][0]["content"].as_str(),
    }
    .unwrap_or("")
    .to_string()
}

/// The tools of a request: name, description and schema.
pub fn tools(fmt: Fmt, body: &Value) -> Vec<(String, String, Value)> {
    let s = |v: &Value| v.as_str().unwrap_or("").to_string();
    body["tools"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|t| match fmt {
            Fmt::Messages => (
                s(&t["name"]),
                s(&t["description"]),
                t["input_schema"].clone(),
            ),
            Fmt::Responses => (s(&t["name"]), s(&t["description"]), t["parameters"].clone()),
            Fmt::Chat => (
                s(&t["function"]["name"]),
                s(&t["function"]["description"]),
                t["function"]["parameters"].clone(),
            ),
        })
        .collect()
}

/// The conversation of a request, in the format's own shape (without the
/// system message).
pub fn conversation(fmt: Fmt, body: &Value) -> Vec<Value> {
    match fmt {
        Fmt::Messages => body["messages"].as_array().cloned().unwrap_or_default(),
        Fmt::Responses => body["input"].as_array().cloned().unwrap_or_default(),
        Fmt::Chat => body["messages"]
            .as_array()
            .map(|m| m.iter().skip(1).cloned().collect())
            .unwrap_or_default(),
    }
}

/// What the real service would refuse a conversation for.
pub fn well_formed(fmt: Fmt, body: &Value) -> Result<(), String> {
    if !body.is_object() {
        return Err("the request body is not JSON".into());
    }
    if !body["model"].as_str().is_some_and(|m| !m.is_empty()) {
        return Err("model: field required".into());
    }
    match fmt {
        Fmt::Messages => {
            let msgs = body["messages"]
                .as_array()
                .ok_or("messages: field required")?;
            if msgs.first().is_none_or(|m| m["role"] != "user") {
                return Err("messages: first message must use the \"user\" role".into());
            }
            for (i, m) in msgs.iter().enumerate() {
                let blocks = m["content"].as_array().cloned().unwrap_or_default();
                if blocks.is_empty() {
                    return Err(format!(
                        "messages.{i}: all messages must have non-empty content"
                    ));
                }
                if m["role"] != "assistant" {
                    continue;
                }
                let ids: Vec<&str> = blocks
                    .iter()
                    .filter(|b| b["type"] == "tool_use")
                    .filter_map(|b| b["id"].as_str())
                    .collect();
                if ids.is_empty() {
                    continue;
                }
                if ids.iter().collect::<HashSet<_>>().len() != ids.len() || ids.contains(&"") {
                    return Err(format!("messages.{i}: `tool_use` ids must be unique"));
                }
                let answered: HashSet<&str> = msgs
                    .get(i + 1)
                    .filter(|n| n["role"] == "user")
                    .and_then(|n| n["content"].as_array())
                    .into_iter()
                    .flatten()
                    .filter(|b| b["type"] == "tool_result")
                    .filter_map(|b| b["tool_use_id"].as_str())
                    .collect();
                if let Some(missing) = ids.iter().find(|id| !answered.contains(**id)) {
                    return Err(format!("messages.{}: `tool_use` ids were found without `tool_result` blocks immediately after: {missing}", i + 1));
                }
            }
        }
        Fmt::Responses => {
            if body["store"] != false {
                return Err("this stand-in takes only requests that keep nothing at the service (store: false)".into());
            }
            if !body["instructions"].is_string() {
                return Err("instructions: expected a string".into());
            }
            for (i, t) in body["tools"].as_array().into_iter().flatten().enumerate() {
                if t["type"] != "function"
                    || !t["name"].is_string()
                    || !t["parameters"].is_object()
                    || !t["strict"].is_boolean()
                {
                    return Err(format!("Invalid 'tools[{i}]': a function tool needs type, name, parameters and strict"));
                }
            }
            let items = body["input"].as_array().ok_or("input: expected a list")?;
            let mut called: Vec<&str> = vec![];
            let mut answered: HashSet<&str> = HashSet::new();
            for (i, item) in items.iter().enumerate() {
                match item["type"].as_str() {
                    Some("function_call") => {
                        let id = item["call_id"].as_str().unwrap_or("");
                        if id.is_empty() || called.contains(&id) {
                            return Err(format!(
                                "Invalid 'input[{i}].call_id': empty or used before"
                            ));
                        }
                        let parsed = item["arguments"]
                            .as_str()
                            .map(serde_json::from_str::<Value>);
                        if !matches!(parsed, Some(Ok(Value::Object(_)))) {
                            return Err(format!(
                                "Invalid 'input[{i}].arguments': expected a JSON object as text"
                            ));
                        }
                        called.push(id);
                    }
                    Some("function_call_output") => {
                        let id = item["call_id"].as_str().unwrap_or("");
                        if !called.contains(&id) {
                            return Err(format!(
                                "No tool call found for function call output with call_id {id}."
                            ));
                        }
                        if !(item["output"].is_string() || item["output"].is_array()) {
                            return Err(format!(
                                "Invalid 'input[{i}].output': expected text or a list of content"
                            ));
                        }
                        answered.insert(id);
                    }
                    Some("reasoning") => {
                        // A reasoning item is sent back only with the item it led to.
                        let next = items.get(i + 1).map(|n| n["type"].as_str().unwrap_or(""));
                        if !matches!(next, Some("message" | "function_call" | "reasoning")) {
                            return Err(format!("Item '{}' of type 'reasoning' was provided without its required following item.", item["id"].as_str().unwrap_or("")));
                        }
                        if !item["encrypted_content"].is_string() {
                            return Err(format!(
                                "Item '{}' not found: reasoning is not kept when store is false",
                                item["id"].as_str().unwrap_or("")
                            ));
                        }
                    }
                    Some("message") | None => {
                        if !item["role"].is_string() {
                            return Err(format!("Invalid 'input[{i}]': a message needs a role"));
                        }
                    }
                    Some(other) => return Err(format!("Invalid 'input[{i}].type': {other}")),
                }
            }
            if let Some(missing) = called.iter().find(|id| !answered.contains(**id)) {
                return Err(format!("No tool output found for function call {missing}."));
            }
        }
        Fmt::Chat => {
            let msgs = body["messages"]
                .as_array()
                .ok_or("messages: expected a list")?;
            if msgs.first().is_none_or(|m| m["role"] != "system") {
                return Err("this stand-in expects the system message first".into());
            }
            for (i, t) in body["tools"].as_array().into_iter().flatten().enumerate() {
                if t["type"] != "function"
                    || !t["function"]["name"].is_string()
                    || !t["function"]["parameters"].is_object()
                {
                    return Err(format!("Invalid 'tools[{i}]': a function tool needs type and function with name and parameters"));
                }
            }
            let mut waiting: Vec<String> = vec![];
            let mut seen: HashSet<String> = HashSet::new();
            for (i, m) in msgs.iter().enumerate() {
                let role = m["role"].as_str().unwrap_or("");
                if role == "tool" {
                    let id = m["tool_call_id"].as_str().unwrap_or("").to_string();
                    let Some(at) = waiting.iter().position(|w| *w == id) else {
                        return Err(format!("Invalid parameter: messages with role 'tool' must be a response to a preceeding message with 'tool_calls' (messages[{i}])."));
                    };
                    if !m["content"].is_string() {
                        return Err(format!(
                            "Invalid 'messages[{i}].content': a tool message holds text only"
                        ));
                    }
                    waiting.remove(at);
                    continue;
                }
                if !waiting.is_empty() {
                    return Err(format!("An assistant message with 'tool_calls' must be followed by tool messages responding to each 'tool_call_id'. The following tool_call_ids did not have response messages: {}", waiting.join(", ")));
                }
                if role == "assistant" {
                    let calls = m["tool_calls"].as_array().cloned().unwrap_or_default();
                    if calls.is_empty() && !m["content"].as_str().is_some_and(|c| !c.is_empty()) {
                        return Err(format!("Invalid 'messages[{i}]': an assistant message needs content or tool_calls"));
                    }
                    for c in &calls {
                        let id = c["id"].as_str().unwrap_or("").to_string();
                        if id.is_empty() || !seen.insert(id.clone()) {
                            return Err(format!(
                                "Invalid 'messages[{i}].tool_calls': an id is empty or used before"
                            ));
                        }
                        let parsed = c["function"]["arguments"]
                            .as_str()
                            .map(serde_json::from_str::<Value>);
                        if c["type"] != "function" || !matches!(parsed, Some(Ok(Value::Object(_))))
                        {
                            return Err(format!("Invalid 'messages[{i}].tool_calls': arguments must be a JSON object as text"));
                        }
                        waiting.push(id);
                    }
                }
            }
            if !waiting.is_empty() {
                return Err(format!("An assistant message with 'tool_calls' must be followed by tool messages responding to each 'tool_call_id'. The following tool_call_ids did not have response messages: {}", waiting.join(", ")));
            }
        }
    }
    Ok(())
}

fn usage(fmt: Fmt, said: &Said) -> Value {
    let Some((fresh, cached)) = said.usage else {
        return Value::Null;
    };
    match fmt {
        Fmt::Messages => {
            json!({"input_tokens": fresh, "cache_read_input_tokens": cached, "cache_creation_input_tokens": 0, "output_tokens": 7})
        }
        Fmt::Responses => {
            json!({"input_tokens": fresh + cached, "input_tokens_details": {"cached_tokens": cached, "cache_write_tokens": 0},
            "output_tokens": 7, "output_tokens_details": {"reasoning_tokens": 0}, "total_tokens": fresh + cached + 7})
        }
        Fmt::Chat => {
            json!({"prompt_tokens": fresh + cached, "completion_tokens": 7, "total_tokens": fresh + cached + 7,
            "prompt_tokens_details": {"cached_tokens": cached}})
        }
    }
}

fn with(mut v: Value, extra: &Value) -> Value {
    for (k, x) in extra.as_object().into_iter().flatten() {
        v[k] = x.clone();
    }
    v
}

/// The items of a Responses reply.
fn response_items(said: &Said) -> Vec<Value> {
    let mut out = vec![];
    if said.thinking {
        out.push(json!({"type": "reasoning", "id": "rs_1", "summary": [], "encrypted_content": "gAAAAB-encrypted-reasoning", "status": "completed"}));
    }
    if let Some(t) = &said.text {
        out.push(
            json!({"type": "message", "id": "msg_1", "role": "assistant", "status": "completed",
            "content": [{"type": "output_text", "text": t, "annotations": []}]}),
        );
    }
    for (n, c) in said.calls.iter().enumerate() {
        let status = if said.end == End::Length {
            "incomplete"
        } else {
            "completed"
        };
        out.push(with(
            json!({"type": "function_call", "id": format!("fc_{n}"), "call_id": c.id, "name": c.name, "arguments": c.arguments, "status": status}),
            &c.extra,
        ));
    }
    out
}

fn response(said: &Said, items: Vec<Value>) -> Value {
    let cut = said.end == End::Length;
    json!({"id": "resp_1", "object": "response", "model": "stand-in",
        "status": if cut { "incomplete" } else { "completed" },
        "incomplete_details": if cut { json!({"reason": "max_output_tokens"}) } else { Value::Null },
        "error": null, "output": items, "usage": usage(Fmt::Responses, said)})
}

fn chat_calls(said: &Said) -> Vec<Value> {
    said.calls
        .iter()
        .map(|c| {
            with(
                json!({"id": c.id, "type": "function", "function": {"name": c.name, "arguments": c.arguments}}),
                &c.extra,
            )
        })
        .collect()
}

fn chat_finish(said: &Said) -> &'static str {
    match said.end {
        End::Stop => "stop",
        End::Tools => "tool_calls",
        End::Length => "length",
    }
}

/// A reply as plain JSON, in the format's own shape.
pub fn plain(fmt: Fmt, said: &Said) -> Value {
    match fmt {
        Fmt::Messages => {
            let mut content = vec![];
            if said.thinking {
                content.push(json!({"type": "thinking", "thinking": "plan", "signature": "sig"}));
            }
            if let Some(t) = &said.text {
                content.push(json!({"type": "text", "text": t}));
            }
            for c in &said.calls {
                let input: Value = serde_json::from_str(&c.arguments).unwrap_or(json!(c.arguments));
                content
                    .push(json!({"type": "tool_use", "id": c.id, "name": c.name, "input": input}));
            }
            let mut m = json!({"id": "msg_1", "type": "message", "role": "assistant", "content": content,
                "stop_reason": match said.end { End::Stop => "end_turn", End::Tools => "tool_use", End::Length => "max_tokens" }});
            if said.usage.is_some() {
                m["usage"] = usage(fmt, said);
            }
            m
        }
        Fmt::Responses => response(said, response_items(said)),
        Fmt::Chat => {
            let mut message = json!({"role": "assistant", "content": said.text});
            if !said.calls.is_empty() {
                message["tool_calls"] = Value::Array(chat_calls(said));
            }
            let mut v = json!({"id": "chatcmpl-1", "object": "chat.completion", "created": 1, "model": "stand-in",
                "choices": [{"index": 0, "message": message, "finish_reason": chat_finish(said)}]});
            if said.usage.is_some() {
                v["usage"] = usage(fmt, said);
            }
            v
        }
    }
}

fn halves(s: &str) -> [&str; 2] {
    let mut at = s.len() / 2;
    while !s.is_char_boundary(at) {
        at += 1;
    }
    let (a, b) = s.split_at(at);
    [a, b]
}

/// One server-sent event.
pub fn event(name: Option<&str>, data: &Value) -> String {
    match name {
        Some(n) => format!("event: {n}\ndata: {data}\n\n"),
        None => format!("data: {data}\n\n"),
    }
}

/// A reply as the service streams it: text and tool arguments in pieces.
pub fn stream(fmt: Fmt, said: &Said) -> String {
    let mut out = String::new();
    match fmt {
        Fmt::Messages => {
            let msg = plain(fmt, said);
            let mut ev = |t: &str, v: Value| out.push_str(&event(Some(t), &v));
            ev(
                "message_start",
                json!({"type": "message_start", "message": {"id": "m", "role": "assistant", "model": "x", "content": [], "usage": {"input_tokens": said.usage.map(|u| u.0), "cache_read_input_tokens": said.usage.map(|u| u.1)}}}),
            );
            for (i, b) in msg["content"].as_array().unwrap().iter().enumerate() {
                match b["type"].as_str().unwrap() {
                    "thinking" => {
                        ev(
                            "content_block_start",
                            json!({"type": "content_block_start", "index": i, "content_block": {"type": "thinking", "thinking": ""}}),
                        );
                        ev(
                            "content_block_delta",
                            json!({"type": "content_block_delta", "index": i, "delta": {"type": "thinking_delta", "thinking": b["thinking"]}}),
                        );
                        ev(
                            "content_block_delta",
                            json!({"type": "content_block_delta", "index": i, "delta": {"type": "signature_delta", "signature": b["signature"]}}),
                        );
                    }
                    "text" => {
                        ev(
                            "content_block_start",
                            json!({"type": "content_block_start", "index": i, "content_block": {"type": "text", "text": ""}}),
                        );
                        for part in halves(b["text"].as_str().unwrap()) {
                            ev(
                                "content_block_delta",
                                json!({"type": "content_block_delta", "index": i, "delta": {"type": "text_delta", "text": part}}),
                            );
                        }
                    }
                    _ => {
                        ev(
                            "content_block_start",
                            json!({"type": "content_block_start", "index": i, "content_block": {"type": "tool_use", "id": b["id"], "name": b["name"], "input": {}}}),
                        );
                        let j = b["input"].to_string();
                        for part in halves(&j) {
                            ev(
                                "content_block_delta",
                                json!({"type": "content_block_delta", "index": i, "delta": {"type": "input_json_delta", "partial_json": part}}),
                            );
                        }
                    }
                }
                ev(
                    "content_block_stop",
                    json!({"type": "content_block_stop", "index": i}),
                );
                ev("ping", json!({"type": "ping"}));
            }
            ev(
                "message_delta",
                json!({"type": "message_delta", "delta": {"stop_reason": msg["stop_reason"]}, "usage": {"output_tokens": 5}}),
            );
            ev("message_stop", json!({"type": "message_stop"}));
        }
        Fmt::Responses => {
            let items = response_items(said);
            let mut n = 0;
            let mut ev = |t: &str, mut v: Value| {
                v["type"] = json!(t);
                v["sequence_number"] = json!(n);
                n += 1;
                out.push_str(&event(Some(t), &v));
            };
            let empty = json!({"id": "resp_1", "object": "response", "status": "in_progress", "output": [], "usage": null});
            ev("response.created", json!({"response": empty}));
            ev("response.in_progress", json!({"response": empty}));
            for (i, item) in items.iter().enumerate() {
                let id = item["id"].clone();
                match item["type"].as_str().unwrap() {
                    "message" => {
                        let text = item["content"][0]["text"].as_str().unwrap();
                        ev(
                            "response.output_item.added",
                            json!({"output_index": i, "item": {"type": "message", "id": id, "role": "assistant", "status": "in_progress", "content": []}}),
                        );
                        ev(
                            "response.content_part.added",
                            json!({"item_id": id, "output_index": i, "content_index": 0, "part": {"type": "output_text", "text": "", "annotations": []}}),
                        );
                        for part in halves(text) {
                            ev(
                                "response.output_text.delta",
                                json!({"item_id": id, "output_index": i, "content_index": 0, "delta": part, "logprobs": []}),
                            );
                        }
                        ev(
                            "response.output_text.done",
                            json!({"item_id": id, "output_index": i, "content_index": 0, "text": text}),
                        );
                    }
                    "function_call" => {
                        let mut started = item.clone();
                        started["arguments"] = json!("");
                        started["status"] = json!("in_progress");
                        ev(
                            "response.output_item.added",
                            json!({"output_index": i, "item": started}),
                        );
                        let args = item["arguments"].as_str().unwrap();
                        for part in halves(args) {
                            ev(
                                "response.function_call_arguments.delta",
                                json!({"item_id": id, "output_index": i, "delta": part}),
                            );
                        }
                        ev(
                            "response.function_call_arguments.done",
                            json!({"item_id": id, "output_index": i, "arguments": args}),
                        );
                    }
                    _ => {
                        // Reasoning: its encrypted content is whole only when the item is done.
                        ev(
                            "response.output_item.added",
                            json!({"output_index": i, "item": {"type": "reasoning", "id": id, "summary": [], "encrypted_content": "gAAAAB-part"}}),
                        );
                    }
                }
                ev(
                    "response.output_item.done",
                    json!({"output_index": i, "item": item}),
                );
            }
            let last = if said.end == End::Length {
                "response.incomplete"
            } else {
                "response.completed"
            };
            ev(last, json!({"response": response(said, items.clone())}));
        }
        Fmt::Chat => {
            let chunk = |delta: Value, finish: Value| {
                json!({"id": "chatcmpl-1", "object": "chat.completion.chunk", "created": 1, "model": "stand-in",
                    "choices": [{"index": 0, "delta": delta, "finish_reason": finish}], "usage": null})
            };
            out.push_str(&event(
                None,
                &chunk(json!({"role": "assistant", "content": ""}), Value::Null),
            ));
            if let Some(t) = &said.text {
                for part in halves(t) {
                    out.push_str(&event(None, &chunk(json!({"content": part}), Value::Null)));
                }
            }
            for (i, c) in chat_calls(said).into_iter().enumerate() {
                let args = c["function"]["arguments"].as_str().unwrap().to_string();
                let mut first = c.clone();
                first["index"] = json!(i);
                first["function"]["arguments"] = json!("");
                out.push_str(&event(
                    None,
                    &chunk(json!({"tool_calls": [first]}), Value::Null),
                ));
                for part in halves(&args) {
                    out.push_str(&event(
                        None,
                        &chunk(
                            json!({"tool_calls": [{"index": i, "function": {"arguments": part}}]}),
                            Value::Null,
                        ),
                    ));
                }
            }
            out.push_str(&event(None, &chunk(json!({}), json!(chat_finish(said)))));
            if said.usage.is_some() {
                out.push_str(&event(None, &json!({"id": "chatcmpl-1", "object": "chat.completion.chunk", "choices": [], "usage": usage(fmt, said)})));
            }
            out.push_str("data: [DONE]\n\n");
        }
    }
    out
}
