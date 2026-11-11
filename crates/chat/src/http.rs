//! Talking to an AI service: where a request may go, how the key rides,
//! reading a streamed or a plain answer, and what the person is told when
//! the service refuses (build brief 3, R-25, R-26).
//!
//! The key goes only to the service's own address, in the one header that
//! service takes it in; no redirect is ever followed, so it cannot be
//! carried to another host. Every text that comes from a service is shown
//! only after the key has been taken out of it, and for a key that looks
//! random any long piece of it too (see [`scrub`]).

use std::borrow::Cow;
use std::collections::HashSet;
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use serde_json::Value;

use crate::services::{check_address, Format, KeyRule, Service};
use crate::wire::Assemble;

/// Anthropic's API version (the Messages format).
const API_VERSION: &str = "2023-06-01";

/// Most bytes of one reply kept, streamed or plain.
pub(crate) const MAX_STREAM: usize = 32 * 1024 * 1024;
/// Most bytes read of an answer that is an error.
const MAX_ERROR_BODY: usize = 256 * 1024;
/// Most characters of a service's own words shown.
const MAX_SAID: usize = 600;

/// The connection to one service: its checked address and its key.
pub(crate) struct Net {
    http: reqwest::Client,
    /// The address requests go to, without a trailing `/`.
    base: String,
    key: String,
    /// Whether the address is this computer.
    local: bool,
    pub service: Service,
}

impl Net {
    /// `address` is checked here, whoever gave it: a key and a chat are
    /// never sent anywhere [`check_address`] refuses.
    pub fn new(address: &str, key: &str, service: Service) -> Result<Net> {
        let checked = check_address(address).map_err(|e| anyhow!("{}: {e}", service.name))?;
        let key = if service.key == KeyRule::None {
            String::new()
        } else {
            key.trim().to_string()
        };
        // No redirects: the key header would follow them to another host.
        let mut b = reqwest::Client::builder()
            // A streamed reply may take long in all; each read waits at
            // most a few minutes (see read_sse).
            .timeout(Duration::from_secs(3600))
            .redirect(reqwest::redirect::Policy::none());
        if checked.local {
            // A service on this computer is spoken to directly: a proxy
            // would carry a plain-HTTP chat off the computer.
            b = b.no_proxy();
        }
        Ok(Net {
            http: b.build()?,
            base: checked.url,
            key,
            local: checked.local,
            service,
        })
    }

    pub fn key(&self) -> &str {
        &self.key
    }

    /// What the person is told when a request did not reach the service.
    pub fn unreachable(&self, e: reqwest::Error) -> String {
        if self.local && e.is_connect() {
            // Nothing listens at an address on this computer: the service
            // is not started. (The network has no part in it.)
            let name = &self.service.name;
            let who = name.strip_suffix(" on this computer").unwrap_or(name);
            return format!("{who} is not running on this computer (nothing answers at its address). Start it and try again");
        }
        format!("could not reach {}: {}", self.service.name, plain(e))
    }

    fn with_key(&self, mut r: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if !self.key.is_empty() {
            let (name, value) = self.service.key_header.header(&self.key);
            if let Ok(mut v) = reqwest::header::HeaderValue::from_str(&value) {
                v.set_sensitive(true);
                r = r.header(name, v);
            }
        }
        if self.service.format == Format::Messages {
            r = r.header("anthropic-version", API_VERSION);
        }
        r
    }

    /// A chat request to the service's own address.
    pub fn post(&self, body: &Value) -> reqwest::RequestBuilder {
        let url = format!("{}{}", self.base, self.service.format.path());
        self.with_key(self.http.post(url)).json(body)
    }

    /// The request for the service's list of models.
    fn models(&self) -> reqwest::RequestBuilder {
        let url = format!("{}{}", self.base, self.service.format.models_path());
        self.with_key(self.http.get(url))
            .timeout(Duration::from_secs(30))
    }
}

/// Most models taken from a service's own list.
pub const MAX_LISTED_MODELS: usize = 500;

/// The models a service lists (`GET <address>/models`): ids only, each a
/// plain model id, at most [`MAX_LISTED_MODELS`]. The ids are data from
/// outside: anything else in the answer is ignored.
pub async fn list_models(address: &str, key: &str, service: &Service) -> Result<Vec<String>> {
    let net = Net::new(address, key, service.clone())?;
    let name = &service.name;
    let r = net
        .models()
        .send()
        .await
        .map_err(|e| anyhow!("{}", net.unreachable(e)))?;
    let code = r.status();
    if code.is_redirection() {
        bail!("{}", redirect_text(name));
    }
    let bytes = read_body(r, 4 * 1024 * 1024).await?;
    let v: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    if !code.is_success() {
        let said = Said::read(&v, &bytes, net.key());
        bail!(
            "{}",
            scrub(
                &refusal(name, "", code.as_u16(), &said, !net.key().is_empty()),
                net.key()
            )
        );
    }
    let items = match &v {
        Value::Array(a) => a.as_slice(),
        _ => v["data"]
            .as_array()
            .or(v["models"].as_array())
            .map(Vec::as_slice)
            .unwrap_or(&[]),
    };
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for m in items {
        let id = m.as_str().or(m["id"].as_str()).or(m["name"].as_str());
        // A plain id, and never the key handed back as one.
        let Some(id) = id
            .filter(|id| crate::services::model_id_ok(id))
            .filter(|id| matches!(scrub(id, net.key()), Cow::Borrowed(_)))
        else {
            continue;
        };
        if seen.insert(id.to_string()) {
            out.push(id.to_string());
        }
        if out.len() >= MAX_LISTED_MODELS {
            break;
        }
    }
    if out.is_empty() {
        bail!("{name} listed no models Columbus can use");
    }
    Ok(out)
}

/// A reply read as server-sent events and put together by `s`. `idle` is
/// how long the service may send nothing before the reply is given up.
pub(crate) async fn read_sse(
    mut r: reqwest::Response,
    s: &mut dyn Assemble,
    idle: Duration,
) -> Result<Value> {
    let mut buf: Vec<u8> = Vec::new();
    let mut seen = 0usize;
    'read: loop {
        let chunk = tokio::time::timeout(idle, r.chunk())
            .await
            .map_err(|_| anyhow!("the AI service stopped sending in the middle of a reply"))?
            .map_err(|e| anyhow!("the reply was cut off: {}", plain(e)))?;
        let Some(chunk) = chunk else { break };
        seen += chunk.len();
        if seen > MAX_STREAM {
            bail!("the reply was larger than Columbus takes");
        }
        // Only the new bytes (and the three before them) can end an event.
        let from = buf.len().saturating_sub(3);
        buf.extend_from_slice(&chunk);
        let mut start = from;
        while let Some(end) = find_event_end(&buf[start..]).map(|e| e + start) {
            start = 0;
            let event: Vec<u8> = buf.drain(..end).collect();
            for line in String::from_utf8_lossy(&event).lines() {
                if let Some(data) = line.strip_prefix("data:") {
                    let data = data.trim_start();
                    // How the Chat Completions format says a stream is over.
                    if data.trim_end() == "[DONE]" {
                        s.end_said();
                        break 'read;
                    }
                    if let Ok(v) = serde_json::from_str::<Value>(data) {
                        if s.take(v)? {
                            return Ok(s.whole());
                        }
                    }
                }
            }
        }
    }
    if s.ended() {
        return Ok(s.whole());
    }
    // A stream that ends before its last event is not a whole reply: a
    // tool call in it may be cut off.
    bail!(if s.started() {
        "the reply was cut off before it ended"
    } else {
        "the AI service sent no reply"
    })
}

/// Where the first event in `buf` ends (after its blank line).
fn find_event_end(buf: &[u8]) -> Option<usize> {
    let a = buf.windows(2).position(|w| w == b"\n\n").map(|i| i + 2);
    let b = buf.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4);
    match (a, b) {
        (Some(x), Some(y)) => Some(x.min(y)),
        (x, y) => x.or(y),
    }
}

/// An answer's body, up to `cap` bytes; a longer one is refused.
pub(crate) async fn read_body(mut r: reqwest::Response, cap: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    loop {
        let chunk = tokio::time::timeout(Duration::from_secs(180), r.chunk())
            .await
            .map_err(|_| anyhow!("the AI service stopped sending in the middle of a reply"))?
            .map_err(|e| anyhow!("the reply was cut off: {}", plain(e)))?;
        let Some(chunk) = chunk else { break };
        if bytes.len() + chunk.len() > cap {
            bail!("the reply was larger than Columbus takes");
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

/// The body of an answer that is an error: what fits, and no failure.
pub(crate) async fn read_error_body(mut r: reqwest::Response) -> Vec<u8> {
    let mut bytes = Vec::new();
    while let Ok(Ok(Some(chunk))) = tokio::time::timeout(Duration::from_secs(20), r.chunk()).await {
        let room = MAX_ERROR_BODY.saturating_sub(bytes.len());
        bytes.extend_from_slice(&chunk[..chunk.len().min(room)]);
        if room <= chunk.len() {
            break;
        }
    }
    bytes
}

pub(crate) fn plain(e: reqwest::Error) -> String {
    if e.is_timeout() {
        "it did not answer in time".into()
    } else if e.is_connect() {
        "no connection (offline, or blocked by your network)".into()
    } else {
        // Without the address: a person-typed one is theirs to see in
        // Settings, not in every error.
        e.without_url().to_string()
    }
}

pub(crate) fn redirect_text(name: &str) -> String {
    format!("{name} answered with a redirect to another address. Columbus does not follow it: your key and your chat go only to the address the service was set up with. Check that address in Settings › Assistant")
}

/// What a service said when it refused: its message and its own codes.
#[derive(Debug, Default, Clone)]
pub(crate) struct Said {
    /// Its message, in one line, with the key taken out.
    pub message: String,
    /// Its code, type and status words, lower case, for telling refusals apart.
    pub codes: String,
}

impl Said {
    /// From an error answer's JSON (`v`), or its bytes when it is not JSON.
    pub fn read(v: &Value, bytes: &[u8], key: &str) -> Said {
        // Gemini's endpoint wraps its error in a list.
        let v = match v {
            Value::Array(a) => a.first().unwrap_or(&Value::Null),
            other => other,
        };
        let e = &v["error"];
        let message = e["message"]
            .as_str()
            .or(e.as_str())
            .or(v["message"].as_str())
            .or(v["detail"].as_str())
            .map(String::from)
            .unwrap_or_else(|| {
                if v.is_null() {
                    // Not JSON (a proxy's page, say): its first words.
                    String::from_utf8_lossy(&bytes[..bytes.len().min(MAX_SAID)]).to_string()
                } else {
                    String::new()
                }
            });
        let word = |x: &Value| match x {
            Value::String(s) => s.to_ascii_lowercase(),
            Value::Number(n) => n.to_string(),
            _ => String::new(),
        };
        let codes = [&e["code"], &e["type"], &e["status"], &v["code"], &v["type"]]
            .into_iter()
            .map(word)
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        Said {
            message: one_line(&scrub(&message, key)),
            codes,
        }
    }

    /// Whether the refusal is about money: the account has no credit left
    /// or a spending limit is reached. Asking again does not help.
    pub fn no_credit(&self, status: u16) -> bool {
        let m = self.message.to_ascii_lowercase();
        status == 402
            || [
                "insufficient_quota",
                "spend_limit",
                "usage_limit",
                "credit",
                "billing",
                "insufficient_balance",
                "payment",
            ]
            .iter()
            .any(|w| self.codes.contains(w))
            || [
                "credit balance",
                "insufficient balance",
                "insufficient credit",
                "insufficient funds",
                "billing hard limit",
                "exceeded your current quota, please check your plan and billing",
            ]
            .iter()
            .any(|w| m.contains(w))
    }

    /// Whether the refusal names the model as the thing it does not know.
    pub fn unknown_model(&self, status: u16) -> bool {
        let m = self.message.to_ascii_lowercase();
        self.codes.contains("model_not_found")
            || (status == 404 && m.contains("model"))
            || (status == 400
                && m.contains("model")
                && (m.contains("does not exist")
                    || m.contains("not found")
                    || m.contains("unknown model")))
    }

    fn in_brackets(&self) -> String {
        if self.message.is_empty() {
            String::new()
        } else {
            format!(" ({})", self.message)
        }
    }
}

/// What the person is told when a service answers with an error status,
/// in plain words that name the service.
pub(crate) fn refusal(name: &str, model: &str, status: u16, said: &Said, has_key: bool) -> String {
    let its = said.in_brackets();
    let the_model = if model.is_empty() {
        "this model".to_string()
    } else {
        format!("the model {model}")
    };
    if said.no_credit(status) {
        return format!("your account at {name} has no credit left, or has reached its spending limit{its}. Columbus did not try again");
    }
    if said.unknown_model(status) {
        return format!(
            "{name} does not know {the_model}{its}. Choose another model in Settings › Assistant"
        );
    }
    match status {
        401 if has_key => format!("{name} refused the key{its}. Check it in Settings › Assistant"),
        401 => format!("{name} asks for a key{its}. Save one in Settings › Assistant"),
        403 => format!("{name} says the key may not use {the_model}{its}"),
        404 => format!("{name} has nothing at the address Columbus was given for it{its}. Check the service's address and format in Settings › Assistant"),
        429 => format!("{name} is limiting how much you may send right now{its}. Try again in a little while"),
        500..=599 => format!("{name} could not answer ({status}){its}. Try again in a little while"),
        400 => format!("{name} refused the request{its}"),
        _ => format!("{name} answered {status}{its}"),
    }
}

/// How long a busy service asks to be left alone (`retry-after-ms`, or
/// `retry-after` in seconds), if it says.
pub(crate) fn retry_after(h: &reqwest::header::HeaderMap) -> Option<Duration> {
    let get = |k: &str| h.get(k).and_then(|v| v.to_str().ok()).map(str::trim);
    if let Some(ms) = get("retry-after-ms").and_then(|v| v.parse::<f64>().ok()) {
        if ms.is_finite() && ms >= 0.0 {
            return Some(Duration::from_millis(ms.min(86_400_000.0) as u64));
        }
    }
    let s = get("retry-after")?.parse::<f64>().ok()?;
    (s.is_finite() && s >= 0.0).then(|| Duration::from_millis((s.min(86_400.0) * 1000.0) as u64))
}

/// Whether the service says not to ask again (`x-should-retry: false`).
pub(crate) fn told_not_to_retry(h: &reqwest::header::HeaderMap) -> bool {
    h.get("x-should-retry")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.trim().eq_ignore_ascii_case("false"))
}

fn one_line(s: &str) -> String {
    let t = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if t.chars().count() > MAX_SAID {
        format!("{}…", t.chars().take(MAX_SAID).collect::<String>())
    } else {
        t
    }
}

/// What stands where a key was.
pub(crate) const REMOVED: &str = "[key removed]";

/// Pieces of a key this long or longer may be taken out of a text. (A page
/// shows a saved key's last four characters, so four are no secret.)
const PIECE: usize = 8;

/// A key shorter than this is looked for whole only.
const RANDOM_LENGTH: usize = 20;

/// Whether these characters read like ordinary writing: a word or two
/// (letters with `- _ . '`, and at most three changes between small and
/// capital letters, so `RoadName` and `gis-conn` are words and `xQzRtKpW`
/// is not), or a number or a date (digits with `. , - : /`). Such a stretch
/// of a key is in everyday text for reasons that have nothing to do with
/// the key.
fn ordinary(piece: &[u8]) -> bool {
    if piece
        .iter()
        .all(|b| b.is_ascii_digit() || b".,-:/".contains(b))
    {
        return true;
    }
    if !piece
        .iter()
        .all(|b| b.is_ascii_alphabetic() || b"-_.'".contains(b))
    {
        return false;
    }
    let capital: Vec<bool> = piece
        .iter()
        .filter(|b| b.is_ascii_alphabetic())
        .map(u8::is_ascii_uppercase)
        .collect();
    capital.windows(2).filter(|w| w[0] != w[1]).count() <= 3
}

/// Whether a key looks random, as the keys AI services issue do: at least
/// [`RANDOM_LENGTH`] characters, and no more than a quarter of its
/// [`PIECE`]-character stretches read like ordinary writing. A key a
/// person made up from words (`gis-connection-2026`, for a gateway or a
/// local server) does not: its pieces are everyday words, and taking them
/// out of every text would garble ordinary work.
pub(crate) fn looks_random(key: &str) -> bool {
    let k = key.trim().as_bytes();
    if !key.is_ascii() || k.len() < RANDOM_LENGTH {
        return false;
    }
    let all = k.len() - PIECE + 1;
    let plain = k.windows(PIECE).filter(|w| ordinary(w)).count();
    plain * 4 <= all
}

/// `text` without `key`. For a key that looks random ([`looks_random`]),
/// also without any piece of it [`PIECE`] characters or longer that does
/// not itself read like ordinary writing: some services quote the key they
/// refused, whole or in part. This is a safeguard against a service
/// quoting the key, not a proof that no form of it can pass (a key written
/// in short groups, backwards or encoded is not looked for).
pub(crate) fn scrub<'a>(text: &'a str, key: &str) -> Cow<'a, str> {
    let key = key.trim();
    if key.len() < 4 || text.len() < key.len().min(PIECE) {
        return Cow::Borrowed(text);
    }
    // The key itself, always.
    let whole: Cow<'a, str> = if text.contains(key) {
        Cow::Owned(text.replace(key, REMOVED))
    } else {
        Cow::Borrowed(text)
    };
    if !looks_random(key) {
        return whole;
    }
    let k = key.as_bytes();
    let t = whole.as_bytes();
    let pieces: HashSet<&[u8]> = k.windows(PIECE).filter(|w| !ordinary(w)).collect();
    let mut out: Option<Vec<u8>> = None;
    let mut i = 0;
    let mut copied = 0;
    while i + PIECE <= t.len() {
        if !pieces.contains(&t[i..i + PIECE]) {
            i += 1;
            continue;
        }
        // The longest run from here that is a piece of the key.
        let mut longest = PIECE;
        for at in 0..=k.len() - PIECE {
            if k[at..at + PIECE] != t[i..i + PIECE] {
                continue;
            }
            let mut n = PIECE;
            while at + n < k.len() && i + n < t.len() && k[at + n] == t[i + n] {
                n += 1;
            }
            longest = longest.max(n);
        }
        let o = out.get_or_insert_with(Vec::new);
        o.extend_from_slice(&t[copied..i]);
        o.extend_from_slice(REMOVED.as_bytes());
        i += longest;
        copied = i;
    }
    match out {
        None => whole,
        Some(mut o) => {
            o.extend_from_slice(&t[copied..]);
            // The key is ASCII, so what was cut out began and ended
            // between characters.
            Cow::Owned(String::from_utf8_lossy(&o).into_owned())
        }
    }
}

/// Every text in `v`, scrubbed for showing (names too: nothing of a key
/// is shown).
pub(crate) fn scrub_json(v: &Value, key: &str) -> Value {
    match v {
        Value::String(s) => Value::String(scrub(s, key).into_owned()),
        Value::Array(a) => Value::Array(a.iter().map(|x| scrub_json(x, key)).collect()),
        Value::Object(m) => Value::Object(
            m.iter()
                .map(|(k, x)| (scrub(k, key).into_owned(), scrub_json(x, key)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Whether a value in `v` holds the key (or, for a key that looks random,
/// a long piece of it). Only values are looked at: the names of a tool's
/// arguments are Columbus's own words (`connection`, `dataset`), and a key
/// that happens to be made of such words must not stop every call.
pub(crate) fn holds_key(v: &Value, key: &str) -> bool {
    if key.trim().len() < PIECE {
        return false;
    }
    match v {
        Value::String(s) => matches!(scrub(s, key), Cow::Owned(_)),
        Value::Array(a) => a.iter().any(|x| holds_key(x, key)),
        Value::Object(m) => m.values().any(|x| holds_key(x, key)),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // Made up for these tests: no service ever issued it.
    const KEY: &str = "tk51Hq8ZpL0vXn3TdRy7WbQe2MfUaKc9Gj4sVhN6";

    #[test]
    fn a_key_and_its_long_pieces_are_taken_out_of_text() {
        // Quoted whole.
        let t = scrub(&format!("Incorrect API key provided: {KEY}."), KEY).into_owned();
        assert_eq!(t, format!("Incorrect API key provided: {REMOVED}."));
        // Quoted in part, as services do: the start, stars, the end.
        let t = scrub("Incorrect API key provided: tk51Hq8ZpL0vXn***************************hN6. Find yours at the site.", KEY);
        assert!(
            !t.contains("tk51Hq8ZpL0vXn") && !t.contains("51Hq8Z"),
            "{t}"
        );
        assert!(
            t.contains("Incorrect API key provided") && t.contains("Find yours"),
            "{t}"
        );
        // Any eight characters in a row, wherever they come from in the key.
        for at in 0..=KEY.len() - 8 {
            let piece = &KEY[at..at + 8];
            let t = scrub(&format!("x {piece} y"), KEY).into_owned();
            assert_eq!(t, format!("x {REMOVED} y"), "{piece}");
        }
        // Twice, and next to each other.
        let t = scrub(&format!("{KEY}{KEY} and {}", &KEY[10..30]), KEY).into_owned();
        assert!(!t.contains("Hq8ZpL0v") && !t.contains(&KEY[10..30]), "{t}");
        // Seven characters are not a long piece; the last four are shown anyway.
        let t = scrub("ends in VhN6, starts tk51Hq8", KEY);
        assert_eq!(t, "ends in VhN6, starts tk51Hq8");
        // Text beside it keeps its letters, accents included.
        let t = scrub(&format!("clé refusée : {KEY} — voilà"), KEY).into_owned();
        assert_eq!(t, format!("clé refusée : {REMOVED} — voilà"));
        // No key, nothing done.
        assert_eq!(scrub("anything", ""), "anything");
        assert!(matches!(scrub("no key here", KEY), Cow::Borrowed(_)));
        // A short key is looked for whole.
        assert_eq!(
            scrub("token abc123 here", "abc123"),
            format!("token {REMOVED} here")
        );
        assert_eq!(scrub("token abc12 here", "abc123"), "token abc12 here");
    }

    #[test]
    fn json_is_scrubbed_through_and_through() {
        let v = json!({"code": format!("print('{KEY}')"), "list": [KEY, 1, null], "nested": {format!("k-{KEY}"): true}});
        assert!(holds_key(&v, KEY));
        let clean = scrub_json(&v, KEY);
        assert!(!clean.to_string().contains("51Hq8ZpL"), "{clean}");
        assert!(!holds_key(&clean, KEY));
        assert!(!holds_key(&json!({"a": "nothing", "b": [1, 2]}), KEY));
        assert!(!holds_key(&json!({"a": KEY}), ""));
        // Only values can hold the key: a name is not looked at (what is
        // shown has it taken out all the same).
        let named = json!({KEY: 1, "deep": [{format!("x{KEY}"): "plain"}]});
        assert!(!holds_key(&named, KEY));
        assert!(!scrub_json(&named, KEY).to_string().contains("51Hq8ZpL"));
        assert!(holds_key(
            &json!({"deep": [{"a": {"b": format!("x {KEY}")}}]}),
            KEY
        ));
    }

    // A key a person made up, as for a gateway or a local server.
    const WORDS: &str = "gis-connection-2026";

    #[test]
    fn a_key_made_of_words_is_looked_for_whole_only() {
        assert!(!looks_random(WORDS));
        let text = "The tracts of the tiger connection: connection-2026 is ready (gis-connection).";
        assert!(
            matches!(scrub(text, WORDS), Cow::Borrowed(_)),
            "{}",
            scrub(text, WORDS)
        );
        assert_eq!(
            scrub(&format!("key {WORDS}, twice {WORDS}."), WORDS),
            format!("key {REMOVED}, twice {REMOVED}.")
        );
        // Tool calls: Columbus's own argument names and everyday values.
        let call = json!({"connection": "tiger", "dataset": "tracts", "where": "NAME = 'connection-2026'"});
        assert!(!holds_key(&call, WORDS));
        assert_eq!(scrub_json(&call, WORDS), call);
        assert!(holds_key(
            &json!({"where": format!("NAME = '{WORDS}'")}),
            WORDS
        ));
        assert!(!holds_key(&json!({WORDS: "a name is not a value"}), WORDS));
    }

    #[test]
    fn which_keys_look_random() {
        // As services issue them (all made up here).
        for key in [
            KEY,
            "sk-proj-Tn4BcY81rvoa3Hc8Vb1NxK6mQw72RtLp9Zs4YdGfJe0A",
            "AIzaSyD4q7Lp2Qs9xTn4BcY81-rv_3Hc8Vb1NxK6",
            "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08",
            "3f2b8c1e-9d4a-4f6b-a1c7-5e8d2b9f0a3c",
            "rvgw-7Lp2Qs9xTn4BcY81",
        ] {
            assert!(looks_random(key), "{key}");
        }
        // Made up by a person, short, or a number.
        for key in [
            WORDS,
            "MyCompanyGateway2026",
            "abc-team-llm-2026-prod",
            "Tr0ub4dor-horse-battery-staple",
            "open-sesame-open-sesame-123",
            "RoadNameService-2026-11-11",
            "20261111123456789012",
            // (Half of it is one long number: numbers are in everyday text.)
            "123e4567-e89b-12d3-a456-426614174000",
            "tk-gw-9Lp2Qs7x",
            "ключ-ключ-ключ-ключ-ключ",
        ] {
            assert!(!looks_random(key), "{key}");
        }
        // What reads like ordinary writing, and what does not.
        for piece in [
            "connecti", "gis-conn", "RoadName", "COLUMBUS", "yGateway", "2026-11-", "34567890",
            "12:30:00",
        ] {
            assert!(ordinary(piece.as_bytes()), "{piece}");
        }
        for piece in ["ion-2026", "tk51Hq8Z", "aBcDeFgH", "abc_123x", "pL0vXn3T"] {
            assert!(!ordinary(piece.as_bytes()), "{piece}");
        }
    }

    #[test]
    fn a_word_inside_a_random_key_stays_readable() {
        // A key with a company's name in front, and the public start of a
        // service's keys: those words are in everyday text.
        let key = "AcmeRoads-sk-live-7Lp2Qs9xTn4BcY81rv3Hc8Vb1NxK6mQw72";
        assert!(looks_random(key));
        let text = "AcmeRoads keeps its roads here; the key starts with sk-live-.";
        assert!(
            matches!(scrub(text, key), Cow::Borrowed(_)),
            "{}",
            scrub(text, key)
        );
        // Quoted whole, and in part: the random part goes.
        assert_eq!(
            scrub(&format!("key: {key}."), key),
            format!("key: {REMOVED}.")
        );
        let t = scrub("Incorrect key: AcmeRoads-sk-live-7Lp2Qs9x*****mQw72", key).into_owned();
        assert!(!t.contains("7Lp2Qs9x") && t.contains(REMOVED), "{t}");
        // A date and a number that are also in the key are not the key.
        let key = "x9Kq2026-11-11Zp4LmW8vTn3BcY7rQe5d";
        assert!(looks_random(key));
        assert_eq!(scrub("Read on 2026-11-11.", key), "Read on 2026-11-11.");
    }

    #[test]
    fn a_services_error_is_read_in_every_shape() {
        let read = |v: Value| Said::read(&v, b"", KEY);
        // OpenAI and those that follow it.
        let s = read(
            json!({"error": {"message": "The model `gpt-9` does not exist", "type": "invalid_request_error", "param": null, "code": "model_not_found"}}),
        );
        assert_eq!(s.message, "The model `gpt-9` does not exist");
        assert!(s.unknown_model(404) && s.unknown_model(400) && !s.no_credit(404));
        // Anthropic.
        let s = read(
            json!({"type": "error", "error": {"type": "not_found_error", "message": "model: claude-x"}}),
        );
        assert!(s.unknown_model(404), "{s:?}");
        // Gemini's endpoint: a list, with a number for a code.
        let s = read(
            json!([{"error": {"code": 400, "message": "API key not valid.", "status": "INVALID_ARGUMENT"}}]),
        );
        assert_eq!(s.message, "API key not valid.");
        assert!(s.codes.contains("400") && s.codes.contains("invalid_argument"));
        // A bare string, a `message`, a `detail`.
        assert_eq!(
            read(json!({"error": "model \"x\" not found"})).message,
            "model \"x\" not found"
        );
        assert_eq!(read(json!({"message": "no"})).message, "no");
        assert_eq!(
            read(json!({"detail": "Not authenticated"})).message,
            "Not authenticated"
        );
        // Not JSON at all: its first words, in one line.
        let s = Said::read(&Value::Null, b"<html>\n<h1>502 Bad\tGateway</h1>", KEY);
        assert_eq!(s.message, "<html> <h1>502 Bad Gateway</h1>");
        // The key is taken out here already.
        let s = read(json!({"error": {"message": format!("Incorrect API key provided: {KEY}")}}));
        assert_eq!(s.message, format!("Incorrect API key provided: {REMOVED}"));
        // Money: by status, by code, by words.
        assert!(read(json!({"error": {"message": "x"}})).no_credit(402));
        assert!(read(json!({"error": {"message": "You exceeded your current quota", "code": "insufficient_quota"}})).no_credit(429));
        assert!(read(json!({"error": {"type": "invalid_request_error", "message": "Your credit balance is too low to access the Anthropic API."}})).no_credit(400));
        assert!(!read(json!({"error": {"message": "Rate limit reached for requests", "code": "rate_limit_exceeded"}})).no_credit(429));
    }

    #[test]
    fn refusals_name_the_service_in_plain_words() {
        let said = |m: &str, c: &str| Said {
            message: m.into(),
            codes: c.into(),
        };
        let none = Said::default();
        assert_eq!(
            refusal(
                "OpenAI",
                "gpt-6-luna",
                401,
                &said("Incorrect API key", ""),
                true
            ),
            "OpenAI refused the key (Incorrect API key). Check it in Settings › Assistant"
        );
        assert!(
            refusal("LM Studio on this computer", "m", 401, &none, false)
                .contains("asks for a key")
        );
        assert!(refusal("xAI (Grok)", "grok-4.7", 403, &none, true)
            .contains("the key may not use the model grok-4.7"));
        let t = refusal(
            "Ollama on this computer",
            "llama9",
            404,
            &said(
                "model \"llama9\" not found, try pulling it first",
                "not_found_error",
            ),
            false,
        );
        assert!(
            t.starts_with("Ollama on this computer does not know the model llama9")
                && t.contains("try pulling it first"),
            "{t}"
        );
        assert!(
            refusal("My gateway", "m", 404, &said("page not found", ""), true)
                .contains("nothing at the address")
        );
        let t = refusal(
            "OpenAI",
            "m",
            429,
            &said("You exceeded your current quota", "insufficient_quota"),
            true,
        );
        assert!(
            t.contains("no credit left") && t.contains("did not try again"),
            "{t}"
        );
        assert!(refusal(
            "OpenAI",
            "m",
            429,
            &said("Rate limit reached", "rate_limit_exceeded"),
            true
        )
        .contains("limiting how much"));
        assert!(refusal("Google Gemini", "m", 503, &none, true).contains("could not answer (503)"));
        assert_eq!(
            refusal("OpenAI", "m", 400, &said("bad field", ""), true),
            "OpenAI refused the request (bad field)"
        );
        assert_eq!(
            refusal("OpenAI", "m", 418, &none, true),
            "OpenAI answered 418"
        );
    }

    #[test]
    fn a_busy_service_may_say_how_long_to_wait() {
        let mut h = reqwest::header::HeaderMap::new();
        assert_eq!(retry_after(&h), None);
        h.insert("retry-after", "2".parse().unwrap());
        assert_eq!(retry_after(&h), Some(Duration::from_secs(2)));
        h.insert("retry-after", "0.5".parse().unwrap());
        assert_eq!(retry_after(&h), Some(Duration::from_millis(500)));
        h.insert("retry-after-ms", "1500".parse().unwrap());
        assert_eq!(retry_after(&h), Some(Duration::from_millis(1500)));
        // A date, or nonsense, is not a wait Columbus keeps.
        let mut h = reqwest::header::HeaderMap::new();
        h.insert(
            "retry-after",
            "Wed, 21 Oct 2026 07:28:00 GMT".parse().unwrap(),
        );
        assert_eq!(retry_after(&h), None);
        h.insert("retry-after", "-3".parse().unwrap());
        assert_eq!(retry_after(&h), None);
        assert!(!told_not_to_retry(&h));
        h.insert("x-should-retry", "false".parse().unwrap());
        assert!(told_not_to_retry(&h));
    }

    #[test]
    fn events_end_at_a_blank_line() {
        assert_eq!(find_event_end(b"data: 1\n\ndata: 2\n\n"), Some(9));
        assert_eq!(find_event_end(b"data: 1\r\n\r\nrest"), Some(11));
        assert_eq!(find_event_end(b"data: 1\n"), None);
    }
}
