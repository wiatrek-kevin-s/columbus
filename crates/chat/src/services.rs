//! The AI services the app's chat can use (build brief 3, D-22, F-19).
//!
//! A service is an address, one of three request formats, and how its key
//! is sent. The ones Columbus knows have fixed addresses here, in code; a
//! service the person adds brings its own. One function, [`check_address`],
//! decides which addresses a key may be sent to (R-25), for the engine and
//! for the app alike.

use reqwest::Url;

/// The request format a service speaks. A chat never changes format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// Anthropic Messages (`POST <address>/v1/messages`).
    Messages,
    /// OpenAI Responses (`POST <address>/responses`).
    Responses,
    /// OpenAI Chat Completions (`POST <address>/chat/completions`).
    Chat,
}

impl Format {
    /// The name the app's settings file uses (F-19).
    pub fn id(self) -> &'static str {
        match self {
            Format::Messages => "messages",
            Format::Responses => "responses",
            Format::Chat => "chat",
        }
    }

    pub fn from_id(id: &str) -> Option<Format> {
        match id {
            "messages" => Some(Format::Messages),
            "responses" => Some(Format::Responses),
            "chat" => Some(Format::Chat),
            _ => None,
        }
    }

    /// The format in a few plain words, for a page.
    pub fn words(self) -> &'static str {
        match self {
            Format::Messages => "Anthropic Messages",
            Format::Responses => "OpenAI Responses",
            Format::Chat => "OpenAI Chat Completions",
        }
    }

    /// Where a chat request goes, after the service's address.
    pub(crate) fn path(self) -> &'static str {
        match self {
            Format::Messages => "/v1/messages",
            Format::Responses => "/responses",
            Format::Chat => "/chat/completions",
        }
    }

    /// Where the service lists its models, after its address.
    pub(crate) fn models_path(self) -> &'static str {
        match self {
            Format::Messages => "/v1/models",
            Format::Responses | Format::Chat => "/models",
        }
    }
}

/// How a service takes its key (F-19 `key_header`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyHeader {
    /// `Authorization: Bearer <key>`.
    Bearer,
    /// `api-key: <key>` (Azure OpenAI).
    ApiKey,
    /// `x-api-key: <key>` (Anthropic).
    XApiKey,
}

impl KeyHeader {
    pub fn id(self) -> &'static str {
        match self {
            KeyHeader::Bearer => "bearer",
            KeyHeader::ApiKey => "api-key",
            KeyHeader::XApiKey => "x-api-key",
        }
    }

    pub fn from_id(id: &str) -> Option<KeyHeader> {
        match id {
            "bearer" => Some(KeyHeader::Bearer),
            "api-key" => Some(KeyHeader::ApiKey),
            "x-api-key" => Some(KeyHeader::XApiKey),
            _ => None,
        }
    }

    /// The header's name and the value it carries for `key`.
    pub(crate) fn header(self, key: &str) -> (&'static str, String) {
        match self {
            KeyHeader::Bearer => ("authorization", format!("Bearer {key}")),
            KeyHeader::ApiKey => ("api-key", key.to_string()),
            KeyHeader::XApiKey => ("x-api-key", key.to_string()),
        }
    }
}

/// Whether a service wants a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyRule {
    /// No chat without one.
    Needed,
    /// Sent when the person saved one (a local server with sign-in on).
    Optional,
    /// The service takes none; none is ever sent.
    None,
}

/// A service as a chat uses it: what the engine needs besides the address
/// and the key (which are in [`crate::Config`]).
#[derive(Debug, Clone)]
pub struct Service {
    /// `anthropic`, `openai`, …, or the id of one the person added.
    pub id: String,
    /// The name errors and notices say ("OpenAI", "Ollama on this computer").
    pub name: String,
    pub format: Format,
    pub key: KeyRule,
    pub key_header: KeyHeader,
    /// Whether its models are sent pictures (a map snapshot). Off, the
    /// model gets a line of text in the picture's place.
    pub pictures: bool,
    /// The models offered (id, label); the first is the default.
    pub models: Vec<(String, String)>,
    /// Models no longer offered that a chat may still run on, with their
    /// labels (one chosen before it left the list).
    pub earlier_models: Vec<(String, String)>,
    /// Whether a chat may also use a model id that is not listed (one the
    /// person typed, or one the service's own list named).
    pub other_models: bool,
    /// The thinking levels it takes (id, label); "" is the model's own.
    pub efforts: Vec<(String, String)>,
}

impl Service {
    /// Whether a chat on this service may use `model`.
    pub fn model_ok(&self, model: &str) -> bool {
        self.models.iter().any(|(id, _)| id == model)
            || self.earlier_models.iter().any(|(id, _)| id == model)
            || (self.other_models && model_id_ok(model))
    }

    pub fn effort_ok(&self, effort: &str) -> bool {
        self.efforts.iter().any(|(id, _)| id == effort)
    }

    /// The model a new chat starts with when none was chosen.
    pub fn default_model(&self) -> Option<&str> {
        self.models.first().map(|(id, _)| id.as_str())
    }

    /// A model's label, if Columbus has one.
    pub fn model_label(&self, model: &str) -> Option<&str> {
        self.models
            .iter()
            .chain(&self.earlier_models)
            .find(|(id, _)| id == model)
            .map(|(_, l)| l.as_str())
    }
}

/// A service Columbus knows: its address is fixed here, in code.
#[derive(Debug, Clone, Copy)]
pub struct Builtin {
    pub id: &'static str,
    pub name: &'static str,
    pub format: Format,
    pub address: &'static str,
    pub key: KeyRule,
    pub key_header: KeyHeader,
    pub pictures: bool,
    pub models: &'static [(&'static str, &'static str)],
    pub earlier_models: &'static [(&'static str, &'static str)],
    pub efforts: &'static [(&'static str, &'static str)],
    /// One line for the person: where a key comes from and who bills it,
    /// or what must be running on this computer.
    pub about: &'static str,
}

/// The models the app offers for Anthropic, the default first.
pub const MODELS: &[(&str, &str)] = &[
    ("claude-sonnet-5-5", "Claude Sonnet 5.5 (balanced)"),
    ("claude-opus-5-5", "Claude Opus 5.5 (most capable Opus)"),
    (
        "claude-fable-5-2",
        "Claude Fable 5.2 (Mythos tier, most capable)",
    ),
    ("claude-haiku-5-5", "Claude Haiku 5.5 (fastest)"),
];

/// Offered until 0.15: still taken where it was already chosen.
const EARLIER_MODELS: &[(&str, &str)] = &[("claude-fable-5-1", "Claude Fable 5.1 (Mythos tier)")];

/// How hard an Anthropic model thinks (the API's `output_config.effort`);
/// "" leaves the model's own default. Every model in `MODELS` takes all of
/// these.
pub const EFFORTS: &[(&str, &str)] = &[
    ("", "Model's default"),
    ("low", "Low (quickest, cheapest)"),
    ("medium", "Medium"),
    ("high", "High"),
    ("xhigh", "Extra high"),
    ("max", "Max (slowest, most thorough)"),
];

/// OpenAI's reasoning levels offered (`reasoning: {effort}`).
const OPENAI_EFFORTS: &[(&str, &str)] = &[
    ("", "Model's default"),
    ("low", "Low (quickest, cheapest)"),
    ("medium", "Medium"),
    ("high", "High"),
];

/// A service whose thinking level Columbus does not set.
const DEFAULT_ONLY: &[(&str, &str)] = &[("", "Model's default")];

/// The services built in, in the order a page lists them. Addresses,
/// formats and model ids as read on 2026-11-11 (build brief 3, A-11).
pub const BUILTIN: &[Builtin] = &[
    Builtin {
        id: "anthropic",
        name: "Anthropic",
        format: Format::Messages,
        address: "https://api.anthropic.com",
        key: KeyRule::Needed,
        key_header: KeyHeader::XApiKey,
        pictures: true,
        models: MODELS,
        earlier_models: EARLIER_MODELS,
        efforts: EFFORTS,
        about: "Create a key at console.anthropic.com. Anthropic bills it per use, separately from a Claude subscription.",
    },
    Builtin {
        id: "openai",
        name: "OpenAI",
        format: Format::Responses,
        address: "https://api.openai.com/v1",
        key: KeyRule::Needed,
        key_header: KeyHeader::Bearer,
        pictures: true,
        models: &[
            ("gpt-6.1-sol", "GPT-6.1 Sol (balanced)"),
            ("gpt-6-astra", "GPT-6 Astra (most capable)"),
            ("gpt-6.1-terra", "GPT-6.1 Terra (fast)"),
            ("gpt-6-luna", "GPT-6 Luna (fastest)"),
        ],
        earlier_models: &[],
        efforts: OPENAI_EFFORTS,
        about: "Create a key at platform.openai.com, under API keys. OpenAI bills it per use, separately from a ChatGPT subscription.",
    },
    Builtin {
        id: "xai",
        name: "xAI (Grok)",
        format: Format::Responses,
        address: "https://api.x.ai/v1",
        key: KeyRule::Needed,
        key_header: KeyHeader::Bearer,
        pictures: true,
        models: &[
            ("grok-4.6", "Grok 4.6"),
            ("grok-4.7", "Grok 4.7 (newest)"),
            ("grok-4.3", "Grok 4.3 (lower cost)"),
        ],
        earlier_models: &[],
        efforts: DEFAULT_ONLY,
        about: "Create a key at console.x.ai. xAI bills it per use.",
    },
    Builtin {
        id: "gemini",
        name: "Google Gemini",
        format: Format::Chat,
        address: "https://generativelanguage.googleapis.com/v1beta/openai",
        key: KeyRule::Needed,
        key_header: KeyHeader::Bearer,
        pictures: true,
        models: &[
            ("gemini-3.8-flash", "Gemini 3.8 Flash (fast)"),
            ("gemini-4-argon", "Gemini 4 Argon (most capable)"),
            ("gemini-3.6-flash", "Gemini 3.6 Flash"),
        ],
        earlier_models: &[],
        efforts: DEFAULT_ONLY,
        about: "Create a key at aistudio.google.com. Google bills its use under the Gemini API's terms.",
    },
    Builtin {
        id: "openrouter",
        name: "OpenRouter",
        format: Format::Chat,
        address: "https://openrouter.ai/api/v1",
        key: KeyRule::Needed,
        key_header: KeyHeader::Bearer,
        pictures: false,
        models: &[],
        earlier_models: &[],
        efforts: DEFAULT_ONLY,
        about: "Create a key at openrouter.ai. OpenRouter bills it per use and passes your chat on to the model's own company.",
    },
    Builtin {
        id: "ollama",
        name: "Ollama on this computer",
        format: Format::Chat,
        address: "http://localhost:11434/v1",
        key: KeyRule::None,
        key_header: KeyHeader::Bearer,
        pictures: false,
        models: &[],
        earlier_models: &[],
        efforts: DEFAULT_ONLY,
        about: "No key and no bill: the model runs on this computer. Ollama must be running, with a model pulled that can call tools.",
    },
    Builtin {
        id: "lmstudio",
        name: "LM Studio on this computer",
        format: Format::Chat,
        address: "http://localhost:1234/v1",
        key: KeyRule::Optional,
        key_header: KeyHeader::Bearer,
        pictures: false,
        models: &[],
        earlier_models: &[],
        efforts: DEFAULT_ONLY,
        about: "No bill: the model runs on this computer. LM Studio's local server must be running, with a model loaded that can call tools. A key is needed only if you turned on its sign-in.",
    },
];

pub fn builtin(id: &str) -> Option<&'static Builtin> {
    BUILTIN.iter().find(|b| b.id == id)
}

impl Builtin {
    /// This service as a chat uses it. A chat may also run on a model id
    /// the person typed or the service's own list named.
    pub fn service(&self) -> Service {
        let own = |l: &[(&str, &str)]| {
            l.iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect()
        };
        Service {
            id: self.id.to_string(),
            name: self.name.to_string(),
            format: self.format,
            key: self.key,
            key_header: self.key_header,
            pictures: self.pictures,
            models: own(self.models),
            earlier_models: own(self.earlier_models),
            other_models: true,
            efforts: own(self.efforts),
        }
    }
}

/// Anthropic with the models Columbus lists and no other: the service of
/// [`crate::Chat::start`], as the chat was before it took other services.
pub fn anthropic_listed() -> Service {
    let mut s = BUILTIN[0].service();
    s.other_models = false;
    s
}

/// A service the person added (F-19): any model id they type, the model's
/// own thinking level.
pub fn added(
    id: &str,
    name: &str,
    format: Format,
    key_header: KeyHeader,
    pictures: bool,
) -> Service {
    Service {
        id: id.to_string(),
        name: name.to_string(),
        format,
        key: KeyRule::Optional,
        key_header,
        pictures,
        models: vec![],
        earlier_models: vec![],
        other_models: true,
        efforts: DEFAULT_ONLY
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect(),
    }
}

/// A model id as a person may type it, or a service may list it: letters,
/// digits and `. _ : / @ + -`, at most 200 characters.
pub fn model_id_ok(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 200
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._:/@+-".contains(&b))
}

/// An address a key may be sent to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Address {
    /// As requests use it: scheme, host, port and path, no trailing `/`.
    pub url: String,
    /// The host's name in lower case, without a port (what the
    /// organization's `assistant.hosts` lists).
    pub host: String,
    /// Whether it is this computer (`localhost`, `127.0.0.0/8`, `::1`).
    pub local: bool,
}

/// The one rule for where the chat may send a key and a conversation
/// (R-25): HTTPS; plain HTTP only to this computer; no user name or
/// password in the address, and nothing after a `?` or a `#` (a key belongs
/// in the credential store, never in an address kept in a file).
pub fn check_address(address: &str) -> Result<Address, String> {
    let given = address.trim();
    if given.is_empty() {
        return Err("give the service's address, starting with https://".into());
    }
    if given.len() > 500 || given.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("that is not an address Columbus can use".into());
    }
    let url = Url::parse(given)
        .map_err(|_| "that is not an address Columbus can use: write it as https://host/path")?;
    let scheme = url.scheme();
    if scheme != "https" && scheme != "http" {
        return Err("the address must start with https://".into());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(
            "the address must not hold a user name or a password: the key is saved apart from it"
                .into(),
        );
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(
            "the address must end with its path: nothing after a ? or a #, and no key in it".into(),
        );
    }
    // The host as the address parser wrote it: a name in lower case, an
    // IPv4 address in its usual form, or an IPv6 address in brackets.
    let written = url.host_str().unwrap_or("");
    let v6 = written
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .and_then(|h| h.parse::<std::net::Ipv6Addr>().ok());
    let v4 = written.parse::<std::net::Ipv4Addr>().ok();
    let host = match v6 {
        Some(ip) => ip.to_string(),
        None => written.to_ascii_lowercase(),
    };
    let local = match (v4, v6) {
        (Some(ip), _) => ip.is_loopback(),
        (_, Some(ip)) => ip.is_loopback(),
        _ => host == "localhost",
    };
    if host.is_empty() {
        return Err("the address names no host".into());
    }
    if scheme == "http" && !local {
        return Err(format!(
            "{host} is not this computer, so its address must start with https:// (plain http would send the key and the chat unprotected)"
        ));
    }
    let mut out = format!("{scheme}://{written}");
    if let Some(p) = url.port() {
        out.push_str(&format!(":{p}"));
    }
    out.push_str(url.path().trim_end_matches('/'));
    Ok(Address {
        url: out,
        host,
        local,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_goes_over_https_or_to_this_computer_only() {
        // HTTPS anywhere; the address is kept without a trailing slash.
        let a = check_address("https://api.openai.com/v1/").unwrap();
        assert_eq!(
            (a.url.as_str(), a.host.as_str(), a.local),
            ("https://api.openai.com/v1", "api.openai.com", false)
        );
        let a = check_address(" https://My-Resource.OpenAI.Azure.com/openai/v1 ").unwrap();
        assert_eq!(a.host, "my-resource.openai.azure.com");
        assert_eq!(a.url, "https://my-resource.openai.azure.com/openai/v1");
        // Plain HTTP only to this computer, by name or by address.
        for (given, host) in [
            ("http://localhost:11434/v1", "localhost"),
            ("http://LOCALHOST:1234/v1", "localhost"),
            ("http://127.0.0.1:8080", "127.0.0.1"),
            ("http://127.8.9.10/v1", "127.8.9.10"),
            ("http://[::1]:11434/v1", "::1"),
        ] {
            let a = check_address(given).unwrap_or_else(|e| panic!("{given}: {e}"));
            assert!(a.local, "{given}");
            assert_eq!(a.host, host, "{given}");
        }
        assert_eq!(
            check_address("http://[::1]:11434/v1/").unwrap().url,
            "http://[::1]:11434/v1"
        );
        for bad in [
            "http://api.openai.com/v1",
            "http://192.168.1.20:11434/v1",
            "http://10.0.0.5/v1",
            "http://localhost.example.org/v1",
            "http://127.0.0.1.example.org/v1",
            "http://0.0.0.0:11434/v1",
            "http://[::ffff:10.0.0.5]/v1",
        ] {
            let e = check_address(bad).unwrap_err();
            assert!(e.contains("https://"), "{bad}: {e}");
        }
        // No user name, password, query or fragment; no other scheme.
        for bad in [
            "https://user@example.org/v1",
            "https://user:pass@example.org/v1",
            "https://:pass@example.org/v1",
            "https://example.org/v1?api-key=abc",
            "https://example.org/v1#frag",
            "ftp://example.org/v1",
            "file:///etc/passwd",
            "example.org/v1",
            "https://",
            "",
            "https://exa mple.org",
            "https://exa\tmple.org/v1",
            "https://[not-an-address]/v1",
        ] {
            assert!(check_address(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn every_builtin_service_has_an_address_the_rule_takes() {
        let mut ids = std::collections::HashSet::new();
        for b in BUILTIN {
            assert!(ids.insert(b.id), "{} is listed twice", b.id);
            let a = check_address(b.address).unwrap_or_else(|e| panic!("{}: {e}", b.id));
            assert_eq!(a.url, b.address, "{} is kept as written", b.id);
            // Only the two that run on this computer use plain HTTP.
            assert_eq!(a.local, matches!(b.id, "ollama" | "lmstudio"), "{}", b.id);
            for (m, label) in b.models.iter().chain(b.earlier_models) {
                assert!(model_id_ok(m), "{m}");
                assert!(!label.is_empty());
            }
            assert_eq!(b.efforts[0].0, "", "{}: the model's own level first", b.id);
            assert!(!b.about.is_empty());
            let s = b.service();
            assert!(s.other_models && s.effort_ok(""));
        }
        assert_eq!(BUILTIN[0].id, "anthropic");
        assert_eq!(
            builtin("openai").map(|b| (b.format, b.address)),
            Some((Format::Responses, "https://api.openai.com/v1"))
        );
        assert_eq!(builtin("xai").map(|b| b.format), Some(Format::Responses));
        for id in ["gemini", "openrouter", "ollama", "lmstudio"] {
            assert_eq!(builtin(id).map(|b| b.format), Some(Format::Chat), "{id}");
        }
        assert_eq!(builtin("ollama").map(|b| b.key), Some(KeyRule::None));
        assert_eq!(builtin("lmstudio").map(|b| b.key), Some(KeyRule::Optional));
        // Pictures go to the four whose models are known to take them.
        let with: Vec<&str> = BUILTIN
            .iter()
            .filter(|b| b.pictures)
            .map(|b| b.id)
            .collect();
        assert_eq!(with, ["anthropic", "openai", "xai", "gemini"]);
    }

    #[test]
    fn the_newer_model_is_offered_and_the_earlier_one_still_taken() {
        let a = builtin("anthropic").unwrap().service();
        assert!(a.models.iter().any(|(id, _)| id == "claude-fable-5-2"));
        assert!(!a.models.iter().any(|(id, _)| id == "claude-fable-5-1"));
        assert!(a.model_ok("claude-fable-5-1") && a.model_ok("claude-fable-5-2"));
        assert_eq!(a.default_model(), Some("claude-sonnet-5-5"));
        // As the chat was before: the listed models and no other.
        let strict = anthropic_listed();
        assert!(strict.model_ok("claude-opus-5-5") && strict.model_ok("claude-fable-5-1"));
        assert!(!strict.model_ok("claude-opus-9"));
        // In the app a person may type a newer model's id.
        assert!(a.model_ok("claude-opus-9"));
        assert!(!a.model_ok("claude opus 9") && !a.model_ok(""));
    }

    #[test]
    fn model_ids_are_plain() {
        for ok in [
            "gpt-6.1-sol",
            "openai/gpt-6-luna",
            "models/gemini-3.8-flash",
            "llama3.3:70b-instruct-q4_K_M",
            "anthropic/claude-sonnet-5.5@20261001",
            "qwen3+tools",
        ] {
            assert!(model_id_ok(ok), "{ok}");
        }
        let long = "m".repeat(201);
        for bad in [
            "",
            "gpt 6",
            "gpt\n6",
            "gpt\"6",
            "<script>",
            "modèle",
            "a?b",
            "a#b",
            "a\\b",
            long.as_str(),
        ] {
            assert!(!model_id_ok(bad), "{bad}");
        }
        assert!(model_id_ok(&"m".repeat(200)));
    }

    #[test]
    fn formats_and_key_headers_have_the_names_the_settings_file_uses() {
        for f in [Format::Messages, Format::Responses, Format::Chat] {
            assert_eq!(Format::from_id(f.id()), Some(f));
        }
        assert_eq!(Format::from_id("completions"), None);
        for k in [KeyHeader::Bearer, KeyHeader::ApiKey, KeyHeader::XApiKey] {
            assert_eq!(KeyHeader::from_id(k.id()), Some(k));
        }
        assert_eq!(KeyHeader::from_id("cookie"), None);
        assert_eq!(
            KeyHeader::Bearer.header("k"),
            ("authorization", "Bearer k".to_string())
        );
        assert_eq!(KeyHeader::ApiKey.header("k"), ("api-key", "k".to_string()));
        assert_eq!(
            KeyHeader::XApiKey.header("k"),
            ("x-api-key", "k".to_string())
        );
    }
}
