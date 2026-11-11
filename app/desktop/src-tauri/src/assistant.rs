//! The AI service the app's chat uses (build brief 3, D-22, F-19): which
//! one new chats start on, the model chosen for each, each one's key, and
//! the services the person added by address.
//!
//! * A service Columbus knows has its address in Columbus's own code
//!   (`columbus_chat::services`). One the person adds is kept in the app's
//!   settings (`services`), and adding it, or changing where it points, is
//!   confirmed by the person at the keyboard: it changes where the chat's
//!   messages and what the assistant reads are sent. What was confirmed is
//!   recorded in the credential store, so a program that only rewrites the
//!   settings file cannot point a service, and its key, somewhere else.
//! * One key for each service, in the operating system's credential store
//!   (service `columbus-app`; account `anthropic-api-key` for Anthropic, as
//!   before, and `ai-key:<id>` for the others). A page can set or clear a
//!   key and never read one back.
//! * The organization may name the only hosts the chat may talk to
//!   (`assistant.hosts`). It is checked here, in the app's own code, when a
//!   service is chosen, when a chat starts and before each message.
//!
//! Only the main window's own pages may call these commands.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use columbus_chat::services::{self, check_address, Format, KeyHeader, KeyRule, Service};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, State};

use crate::chat::{self, ChatState, Settings};

/// The service the app keeps its own secrets under (see `chat.rs`).
const KEY_SERVICE: &str = "columbus-app";
/// Anthropic's key keeps the account it always had: a workflow's AI review
/// reads it by this name in another program.
const ANTHROPIC_ACCOUNT: &str = "anthropic-api-key";
/// The service new chats use until the person chooses another.
pub(crate) const DEFAULT_SERVICE: &str = "anthropic";
/// Most services a person may add.
const MAX_ADDED: usize = 20;

/// A service the person added (F-19), as `settings.json` keeps it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Added {
    pub id: String,
    pub name: String,
    /// `https://host/path`; plain `http://` only for this computer.
    pub address: String,
    /// `messages`, `responses` or `chat`.
    pub format: String,
    /// `bearer`, `api-key` or `x-api-key`.
    pub key_header: String,
    /// Whether its models are sent pictures.
    #[serde(default)]
    pub pictures: bool,
}

/// `services` read from the settings file: an entry that does not read is
/// left out, and the rest of the settings are kept.
pub(crate) fn lenient<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<Added>, D::Error> {
    let raw = Vec::<serde_json::Value>::deserialize(d).unwrap_or_default();
    Ok(raw
        .into_iter()
        .filter_map(|v| serde_json::from_value(v).ok())
        .take(MAX_ADDED)
        .collect())
}

fn id_ok(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 40
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && services::builtin(id).is_none()
}

/// An id for a new service, from its name: lower-case letters, digits and
/// `-`, unlike every other service's.
fn new_id(name: &str, taken: &[Added]) -> String {
    let mut base: String = name
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    while base.contains("--") {
        base = base.replace("--", "-");
    }
    let base = base.trim_matches('-');
    let base: String = if base.is_empty() { "service" } else { base }
        .chars()
        .take(30)
        .collect();
    let free = |id: &str| id_ok(id) && !taken.iter().any(|a| a.id == id);
    if free(&base) {
        return base;
    }
    (2..)
        .map(|n| format!("{base}-{n}"))
        .find(|id| free(id))
        .unwrap_or(base)
}

/// A service ready to use: what the chat's engine needs, and where it is.
pub(crate) struct Resolved {
    pub service: Service,
    /// The address requests go to.
    pub address: String,
    /// Its host, as the organization's `assistant.hosts` names it.
    pub host: String,
    /// Whether it runs on this computer.
    pub local: bool,
    /// One line for the person: where a key comes from, who bills it.
    pub about: String,
    /// The entry of one the person added.
    pub added: Option<Added>,
}

fn resolve_added(a: &Added) -> Result<Resolved, String> {
    if !id_ok(&a.id) {
        return Err(format!("{:?} is not a service id", a.id));
    }
    let name = a.name.trim();
    if name.is_empty() || name.chars().count() > 60 || name.chars().any(char::is_control) {
        return Err("give the service a name of at most 60 characters".into());
    }
    let format = Format::from_id(&a.format)
        .ok_or("choose one of the three formats: messages, responses or chat")?;
    let key_header = KeyHeader::from_id(&a.key_header)
        .ok_or("choose how the key is sent: bearer, api-key or x-api-key")?;
    let at = check_address(&a.address)?;
    Ok(Resolved {
        service: services::added(&a.id, name, format, key_header, a.pictures),
        address: at.url,
        host: at.host,
        local: at.local,
        about: if at.local {
            "A service on this computer that you added.".into()
        } else {
            format!("A service you added. What the chat reads is sent to {}, and that service bills its use.", a.address.trim())
        },
        added: Some(a.clone()),
    })
}

/// The service with this id: one Columbus knows, or one the person added.
pub(crate) fn resolve(s: &Settings, id: &str) -> Result<Resolved, String> {
    if let Some(b) = services::builtin(id) {
        let at = check_address(b.address)?;
        return Ok(Resolved {
            service: b.service(),
            address: at.url,
            host: at.host,
            local: at.local,
            about: b.about.to_string(),
            added: None,
        });
    }
    let a = s
        .services
        .iter()
        .find(|a| a.id == id)
        .ok_or("That AI service is no longer set up. Choose one in Settings › Assistant.")?;
    resolve_added(a)
}

/// The service new chats use: the one chosen while it is still set up,
/// else Anthropic.
pub(crate) fn chosen(s: &Settings) -> String {
    s.service
        .as_deref()
        .filter(|id| services::builtin(id).is_some() || s.services.iter().any(|a| a.id == *id))
        .unwrap_or(DEFAULT_SERVICE)
        .to_string()
}

/// The model new chats on a service start with: the one chosen for it
/// while the service takes it, else its first.
pub(crate) fn model_for(s: &Settings, r: &Resolved) -> Option<String> {
    let id = r.service.id.as_str();
    s.models
        .get(id)
        .cloned()
        // What earlier versions kept for Anthropic.
        .or_else(|| (id == DEFAULT_SERVICE).then(|| s.model.clone()).flatten())
        .filter(|m| r.service.model_ok(m))
        .or_else(|| r.service.default_model().map(String::from))
}

/// The thinking level new chats start with, where the service takes it.
pub(crate) fn effort_for(s: &Settings, r: &Resolved) -> String {
    s.effort
        .clone()
        .filter(|e| r.service.effort_ok(e))
        .unwrap_or_default()
}

// ---------- The organization's say ----------

/// Whether the organization lets the app's chat use a service at `host`
/// (`assistant.hosts`), and if not, the sentence for the person. A policy
/// file that is there but cannot be read allows no service: the limit it
/// may hold cannot be checked.
pub(crate) fn org_allows(name: &str, host: &str) -> Result<(), String> {
    let path = columbus_shared::paths::policy_file();
    let policy = if path.exists() {
        columbus_shared::policy::OrgPolicy::load(&path).map_err(|_| {
            "Your organization's settings file cannot be read, so the app cannot check which AI services its chat may use. Settings › Organization rules shows what is wrong with the file.".to_string()
        })?
    } else {
        columbus_shared::policy::OrgPolicy::default()
    };
    if policy.assistant.allows(host) {
        Ok(())
    } else {
        Err(format!(
            "Your organization's rules do not let the app's chat use {name}: its address, {host}, is not on the list of AI services allowed (assistant.hosts: {}).",
            policy.assistant.hosts.join(", ")
        ))
    }
}

/// The hosts the organization names; empty means the person chooses.
fn org_hosts() -> Vec<String> {
    crate::settings::policy_now().assistant.hosts
}

// ---------- Keys, and what was confirmed ----------

/// The credential-store account that holds a service's key (F-19).
pub(crate) fn key_account(service: &str) -> String {
    if service == DEFAULT_SERVICE {
        ANTHROPIC_ACCOUNT.to_string()
    } else {
        format!("ai-key:{service}")
    }
}

/// Where the address the person confirmed for an added service is kept.
fn confirmed_account(service: &str) -> String {
    format!("ai-service:{service}")
}

/// What this app has read from or put in the credential store since it
/// started, by account: each item is read once per run. On a Mac every
/// read can ask for the person's password (until they choose Always Allow,
/// and again after each update), so pages that ask often must not read
/// again.
static READ: Mutex<Option<HashMap<String, Option<String>>>> = Mutex::new(None);

fn cache<R>(f: impl FnOnce(&mut HashMap<String, Option<String>>) -> R) -> R {
    let mut read = READ.lock().unwrap_or_else(|e| e.into_inner());
    f(read.get_or_insert_with(HashMap::new))
}

/// Tests only (debug builds, `COLUMBUS_APP_TEST_KEYS=memory`): keys are
/// kept in this program's memory instead, so the end-to-end script can
/// save one where there is no credential store, and never touches a real
/// one. Nothing is written anywhere. Release builds do not read the
/// variable.
#[cfg(debug_assertions)]
fn in_memory() -> bool {
    std::env::var("COLUMBUS_APP_TEST_KEYS").as_deref() == Ok("memory")
}

#[cfg(not(debug_assertions))]
fn in_memory() -> bool {
    false
}

/// Whether a key can be kept on this system at all, and if not, why.
pub(crate) fn can_keep() -> Result<(), String> {
    if in_memory() {
        return Ok(());
    }
    chat::ensure_store()
}

fn entry(account: &str) -> Result<keyring_core::Entry, String> {
    chat::ensure_store()?;
    keyring_core::Entry::new(KEY_SERVICE, account).map_err(|e| e.to_string())
}

/// What the credential store holds under `account` (read once per run; a
/// refusal counts, so the app does not ask again this run).
pub(crate) fn secret(account: &str) -> Option<String> {
    if let Some(known) = cache(|c| c.get(account).cloned()) {
        return known;
    }
    let found = if in_memory() {
        None
    } else {
        entry(account).ok().and_then(|e| e.get_password().ok())
    };
    cache(|c| c.insert(account.to_string(), found.clone()));
    found
}

/// Whether this run already knows what `account` holds, without reading.
fn known(account: &str) -> Option<Option<String>> {
    cache(|c| c.get(account).cloned())
}

pub(crate) fn set_secret(account: &str, value: &str) -> Result<(), String> {
    if !in_memory() {
        entry(account)?
            .set_password(value)
            .map_err(|e| e.to_string())?;
    }
    cache(|c| c.insert(account.to_string(), Some(value.to_string())));
    Ok(())
}

pub(crate) fn delete_secret(account: &str) -> Result<(), String> {
    if !in_memory() {
        match entry(account).and_then(|e| match e.delete_credential() {
            Ok(()) | Err(keyring_core::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.to_string()),
        }) {
            Ok(()) => {}
            // No store at all: then nothing is kept in it either.
            Err(_) if chat::ensure_store().is_err() => {}
            Err(e) => return Err(e),
        }
    }
    cache(|c| c.insert(account.to_string(), None));
    Ok(())
}

/// A service's key. Development and tests only: `COLUMBUS_APP_API_KEY`
/// stands for the key of the service new chats use.
pub(crate) fn read_key(s: &Settings, service: &str) -> Option<String> {
    #[cfg(debug_assertions)]
    if chosen(s) == service {
        if let Ok(k) = std::env::var("COLUMBUS_APP_API_KEY") {
            return Some(k);
        }
    }
    let _ = s;
    secret(&key_account(service)).filter(|k| !k.is_empty())
}

/// A key's last four characters, so the person can tell which key it is.
fn hint(key: &str) -> String {
    let n = key.chars().count();
    key.chars().skip(n.saturating_sub(4)).collect()
}

/// What the person confirmed for an added service: its format, how its key
/// is sent, and its address.
fn confirmation(a: &Added) -> Result<String, String> {
    let at = check_address(&a.address)?;
    Ok(format!("{}|{}|{}", a.format, a.key_header, at.url))
}

/// Whether an added service is as the person confirmed it at the keyboard.
fn as_confirmed(a: &Added) -> Result<(), String> {
    let now = confirmation(a)?;
    if secret(&confirmed_account(&a.id)).as_deref() == Some(now.as_str()) {
        Ok(())
    } else {
        Err(format!(
            "The address or the format of {} is not the one you confirmed in this app, so it is not used. Open Settings › Assistant, choose Change for it and save it again.",
            a.name
        ))
    }
}

/// A service ready for a chat or a look at its models: set up, as the
/// person confirmed it, and allowed by the organization.
pub(crate) fn usable(s: &Settings, id: &str) -> Result<Resolved, String> {
    let r = resolve(s, id)?;
    if let Some(a) = &r.added {
        as_confirmed(a)?;
    }
    org_allows(&r.service.name, &r.host)?;
    Ok(r)
}

// ---------- What the pages are told ----------

#[derive(Serialize)]
pub struct ServiceView {
    id: String,
    name: String,
    /// The request format, in plain words, and by its name.
    format: String,
    format_id: String,
    host: String,
    /// Whether it runs on this computer.
    local: bool,
    /// Whether Columbus knows it (its address is not the person's to change).
    builtin: bool,
    /// `needed`, `optional` or `none`.
    key: &'static str,
    /// Whether a key is saved, where the app has looked this run (always
    /// for the service new chats use), and that key's last four characters.
    /// The key itself never reaches a page.
    key_saved: Option<bool>,
    key_hint: Option<String>,
    models: Vec<(String, String)>,
    /// The model chosen for it, which may be one the person typed.
    model: Option<String>,
    /// Models its own list named, when the person asked for it this run.
    listed: Vec<String>,
    efforts: Vec<(String, String)>,
    pictures: bool,
    about: String,
    /// Whether the organization allows it, and the sentence if not.
    allowed: bool,
    why_not: Option<String>,
    /// For one the person added: what its form shows again.
    address: Option<String>,
    key_header: Option<String>,
    /// Why it cannot be used as it is (changed outside the app).
    problem: Option<String>,
}

fn key_rule(k: KeyRule) -> &'static str {
    match k {
        KeyRule::Needed => "needed",
        KeyRule::Optional => "optional",
        KeyRule::None => "none",
    }
}

/// One service, for a page. `look` reads its key from the credential store
/// if this run has not yet (done for the service in use only).
fn view(s: &Settings, id: &str, look: bool, listed: &HashMap<String, Vec<String>>) -> ServiceView {
    let builtin = services::builtin(id).is_some();
    match resolve(s, id) {
        Ok(r) => {
            let account = key_account(id);
            let key = if r.service.key == KeyRule::None {
                Some(None)
            } else if look {
                Some(read_key(s, id))
            } else {
                known(&account)
            };
            let allowed = org_allows(&r.service.name, &r.host);
            let problem = r.added.as_ref().and_then(|a| as_confirmed_quietly(a).err());
            ServiceView {
                id: id.to_string(),
                name: r.service.name.clone(),
                format: r.service.format.words().to_string(),
                format_id: r.service.format.id().to_string(),
                host: r.host.clone(),
                local: r.local,
                builtin,
                key: key_rule(r.service.key),
                key_saved: key
                    .as_ref()
                    .map(|k| k.as_ref().is_some_and(|k| !k.is_empty())),
                key_hint: key.flatten().filter(|k| !k.is_empty()).map(|k| hint(&k)),
                models: r.service.models.clone(),
                model: model_for(s, &r),
                listed: listed.get(id).cloned().unwrap_or_default(),
                efforts: r.service.efforts.clone(),
                pictures: r.service.pictures,
                about: r.about.clone(),
                allowed: allowed.is_ok(),
                why_not: allowed.err(),
                address: r.added.as_ref().map(|a| a.address.clone()),
                key_header: r.added.as_ref().map(|a| a.key_header.clone()),
                problem,
            }
        }
        // An entry the app cannot use as it is written: shown, so the
        // person can change or remove it.
        Err(e) => {
            let a = s.services.iter().find(|a| a.id == id);
            ServiceView {
                id: id.to_string(),
                name: a.map(|a| a.name.clone()).unwrap_or_else(|| id.to_string()),
                format: String::new(),
                format_id: a.map(|a| a.format.clone()).unwrap_or_default(),
                host: String::new(),
                local: false,
                builtin,
                key: "optional",
                key_saved: None,
                key_hint: None,
                models: vec![],
                model: None,
                listed: vec![],
                efforts: vec![(String::new(), "Model's default".into())],
                pictures: a.is_some_and(|a| a.pictures),
                about: String::new(),
                allowed: true,
                why_not: None,
                address: a.map(|a| a.address.clone()),
                key_header: a.map(|a| a.key_header.clone()),
                problem: Some(e),
            }
        }
    }
}

/// `as_confirmed`, without a read of the credential store this run has
/// not made yet: a page that only lists services must not make the system
/// ask for a password. Unknown counts as fine here; a chat checks again.
fn as_confirmed_quietly(a: &Added) -> Result<(), String> {
    match known(&confirmed_account(&a.id)) {
        Some(_) => as_confirmed(a),
        None => Ok(()),
    }
}

/// What the settings view says of the services: the one new chats use,
/// and every service there is.
#[derive(Serialize)]
pub struct Services {
    /// The service new chats use.
    pub service: String,
    pub service_name: String,
    pub services: Vec<ServiceView>,
    /// The hosts the organization names (`assistant.hosts`); empty: the
    /// person chooses.
    pub hosts: Vec<String>,
    /// Why no key can be kept on this system, if so.
    pub no_store: Option<String>,
}

pub(crate) fn services_view(s: &Settings, listed: &HashMap<String, Vec<String>>) -> Services {
    let now = chosen(s);
    let mut all: Vec<ServiceView> = services::BUILTIN
        .iter()
        .map(|b| view(s, b.id, b.id == now, listed))
        .collect();
    all.extend(
        s.services
            .iter()
            .map(|a| view(s, &a.id, a.id == now, listed)),
    );
    Services {
        service_name: all
            .iter()
            .find(|v| v.id == now)
            .map(|v| v.name.clone())
            .unwrap_or_default(),
        service: now,
        services: all,
        hosts: org_hosts(),
        no_store: can_keep().err().map(|e| {
            format!("A key cannot be kept safely on this computer ({e}), so only a service that needs no key can be used here.")
        }),
    }
}

/// The chosen service as the rest of the settings view needs it: whether
/// a key is saved (and its last four characters), whether a chat could
/// start now, and its models and levels.
pub(crate) struct Now {
    pub has_key: bool,
    pub key_hint: Option<String>,
    pub key_needed: bool,
    pub ready: bool,
    pub model: String,
    pub models: Vec<(String, String)>,
    pub effort: String,
    pub efforts: Vec<(String, String)>,
}

pub(crate) fn now(s: &Settings, listed: &HashMap<String, Vec<String>>) -> Now {
    let id = chosen(s);
    let Ok(r) = resolve(s, &id) else {
        return Now {
            has_key: false,
            key_hint: None,
            key_needed: false,
            ready: false,
            model: String::new(),
            models: vec![],
            effort: String::new(),
            efforts: vec![(String::new(), "Model's default".into())],
        };
    };
    let key = if r.service.key == KeyRule::None {
        None
    } else {
        read_key(s, &id)
    };
    let model = model_for(s, &r);
    let mut models = r.service.models.clone();
    // A model the person typed, and the ones the service listed, are
    // offered beside Columbus's own.
    let listed = listed.get(&id).cloned().unwrap_or_default();
    for m in model.iter().chain(&listed) {
        if !models.iter().any(|(id, _)| id == m) {
            let label = r.service.model_label(m).unwrap_or(m).to_string();
            models.push((m.clone(), label));
        }
    }
    let key_needed = r.service.key == KeyRule::Needed;
    let usable = r
        .added
        .as_ref()
        .is_none_or(|a| as_confirmed_quietly(a).is_ok())
        && org_allows(&r.service.name, &r.host).is_ok();
    Now {
        has_key: key.is_some(),
        key_hint: key.as_deref().map(hint),
        key_needed,
        ready: (key.is_some() || !key_needed) && model.is_some() && usable,
        model: model.unwrap_or_default(),
        models,
        effort: effort_for(s, &r),
        efforts: r.service.efforts.clone(),
    }
}

// ---------- Commands ----------

/// Use this service for new chats. Refused where the organization does
/// not list its host.
#[tauri::command]
pub async fn chat_use_service(app: AppHandle, service: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let mut s = chat::load(&app);
        usable(&s, &service)?;
        s.service = Some(service);
        chat::save(&app, &s)
    })
    .await
    .map_err(|e| e.to_string())?
}

fn key_ok(key: &str, builtin_remote: bool) -> bool {
    let least = if builtin_remote { 20 } else { 8 };
    key.len() >= least && key.len() <= 4096 && key.chars().all(|c| c.is_ascii_graphic())
}

/// Save a service's key (the service new chats use, unless one is named).
#[tauri::command]
pub async fn chat_set_key(
    app: AppHandle,
    key: String,
    service: Option<String>,
) -> Result<(), String> {
    let key = key.trim().to_string();
    tauri::async_runtime::spawn_blocking(move || {
        let s = chat::load(&app);
        let id = service.unwrap_or_else(|| chosen(&s));
        let r = resolve(&s, &id)?;
        if r.service.key == KeyRule::None {
            return Err(format!("{} takes no key", r.service.name));
        }
        if !key_ok(&key, r.added.is_none() && !r.local) {
            return Err("that does not look like an API key".into());
        }
        can_keep()?;
        set_secret(&key_account(&id), &key).map_err(|e| format!("could not save the key: {e}"))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn chat_clear_key(app: AppHandle, service: Option<String>) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let s = chat::load(&app);
        let id = service.unwrap_or_else(|| chosen(&s));
        delete_secret(&key_account(&id))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// What the form for another service sends.
#[derive(Deserialize)]
pub struct ServiceForm {
    /// The id of the one being changed; none for a new one.
    #[serde(default)]
    id: Option<String>,
    name: String,
    address: String,
    format: String,
    key_header: String,
    #[serde(default)]
    pictures: bool,
}

/// Add a service by its address, or change one. A new one, and any change
/// to where a service points or how it is spoken to, is confirmed by the
/// person at the keyboard first: from then on the chat's messages and what
/// the assistant reads go there.
#[tauri::command]
pub async fn chat_service_save(app: AppHandle, service: ServiceForm) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let mut s = chat::load(&app);
        let old = match &service.id {
            Some(id) => Some(
                s.services
                    .iter()
                    .find(|a| a.id == *id)
                    .cloned()
                    .ok_or("that service is no longer there")?,
            ),
            None => None,
        };
        if old.is_none() && s.services.len() >= MAX_ADDED {
            return Err(format!("at most {MAX_ADDED} services can be added"));
        }
        let name = service.name.split_whitespace().collect::<Vec<_>>().join(" ");
        let taken = |n: &str| {
            services::BUILTIN.iter().any(|b| b.name.eq_ignore_ascii_case(n))
                || s.services
                    .iter()
                    .any(|a| a.name.eq_ignore_ascii_case(n) && Some(&a.id) != service.id.as_ref())
        };
        if taken(&name) {
            return Err(format!("there is already a service named {name}; give this one another name"));
        }
        let a = Added {
            id: old
                .as_ref()
                .map(|o| o.id.clone())
                .unwrap_or_else(|| new_id(&name, &s.services)),
            name,
            address: service.address.trim().to_string(),
            format: service.format,
            key_header: service.key_header,
            pictures: service.pictures,
        };
        // Checked as it will be used; the address is kept as checked.
        let r = resolve_added(&a)?;
        let a = Added {
            address: r.address.clone(),
            ..a
        };
        org_allows(&r.service.name, &r.host)?;
        let now = confirmation(&a)?;
        let confirmed_before = old.is_some()
            && secret(&confirmed_account(&a.id)).as_deref() == Some(now.as_str());
        if !confirmed_before {
            can_keep().map_err(|e| {
                format!("A service you add is confirmed and recorded in this computer's credential store, which this system does not have ({e}).")
            })?;
            crate::settings::confirm(&format!(
                "use {} as an AI service for the app's chat: the chat's messages, and what your assistant reads from your data, will be sent to {}",
                a.name, r.host
            ))?;
            set_secret(&confirmed_account(&a.id), &now)
                .map_err(|e| format!("the service could not be recorded in this computer's credential store: {e}"))?;
            // A key saved for the address as it was is not sent to a new one.
            if old.as_ref().is_some_and(|o| confirmation(o).ok().as_deref() != Some(now.as_str())) {
                delete_secret(&key_account(&a.id))?;
            }
        }
        match s.services.iter_mut().find(|x| x.id == a.id) {
            Some(x) => *x = a.clone(),
            None => s.services.push(a.clone()),
        }
        chat::save(&app, &s)?;
        Ok(a.id)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Remove a service the person added: its entry, its key and what was
/// confirmed for it. New chats go back to Anthropic if it was in use.
#[tauri::command]
pub async fn chat_service_remove(
    app: AppHandle,
    st: State<'_, ChatState>,
    id: String,
) -> Result<(), String> {
    st.listed.lock().expect("listed").remove(&id);
    tauri::async_runtime::spawn_blocking(move || {
        let mut s = chat::load(&app);
        if !s.services.iter().any(|a| a.id == id) {
            return Err("that service is no longer there".into());
        }
        delete_secret(&key_account(&id))?;
        delete_secret(&confirmed_account(&id))?;
        s.services.retain(|a| a.id != id);
        s.models.remove(&id);
        if s.service.as_deref() == Some(id.as_str()) {
            s.service = None;
        }
        chat::save(&app, &s)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Ask a service for its own list of models (the service new chats use,
/// unless one is named). Ids only; they are offered beside Columbus's own
/// until the app closes.
#[tauri::command]
pub async fn chat_models(
    app: AppHandle,
    st: State<'_, ChatState>,
    service: Option<String>,
) -> Result<Vec<String>, String> {
    let app2 = app.clone();
    let (id, r, key, address) = tauri::async_runtime::spawn_blocking(move || {
        let s = chat::load(&app2);
        let id = service.unwrap_or_else(|| chosen(&s));
        let r = usable(&s, &id)?;
        let key = if r.service.key == KeyRule::None {
            None
        } else {
            read_key(&s, &id)
        };
        if r.service.key == KeyRule::Needed && key.is_none() {
            return Err(format!(
                "Save a key for {} first: its list is asked for with the key.",
                r.service.name
            ));
        }
        let address = chat::address_in_use(&r.address, r.added.is_none());
        Ok::<_, String>((id, r, key.unwrap_or_default(), address))
    })
    .await
    .map_err(|e| e.to_string())??;
    let models = columbus_chat::list_models(&address, &key, &r.service)
        .await
        .map_err(|e| e.to_string())?;
    st.listed.lock().expect("listed").insert(id, models.clone());
    Ok(models)
}

// ---------- Other AI apps (REQ-50) ----------

/// How to connect one AI app to Columbus: the file to change and the exact
/// entry, with this computer's own path to `columbus-mcp`.
#[derive(Serialize)]
pub struct OtherApp {
    id: &'static str,
    name: &'static str,
    /// Where the entry goes.
    file: String,
    /// The entry, exactly as it is written there.
    entry: String,
    /// A command that adds the same entry, where the app has one.
    command: Option<String>,
    /// What else to know, each a sentence or two.
    notes: Vec<String>,
}

#[derive(Serialize)]
pub struct OtherApps {
    /// The Columbus server's place on this computer, if it was found.
    server: Option<String>,
    apps: Vec<OtherApp>,
    /// How a request for approval reaches the person in any of them.
    approvals: &'static str,
}

/// How approvals reach the person in another AI app, as the reference
/// says it (docs/reference.md, "Connected MCP servers").
const APPROVALS: &str = "When a call needs your approval, Columbus asks you in the AI app's own prompt; your assistant never sees or answers that prompt. When the app cannot ask, Columbus opens its local approvals page in your browser, and the call waits briefly (45 seconds by default) for your decision. If none comes in time, your assistant is told an approval is waiting, tells you, and tries again once you have decided.";

/// JSON for a person to read and paste: its keys stay in the order they
/// are written here (the command first), which a map would sort.
enum J {
    S(String),
    A(Vec<J>),
    O(Vec<(String, J)>),
}

fn s(v: &str) -> J {
    J::S(v.to_string())
}

fn o(pairs: Vec<(&str, J)>) -> J {
    J::O(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

fn pretty(j: &J) -> String {
    fn write(j: &J, depth: usize, out: &mut String) {
        let pad = |n: usize| "  ".repeat(n);
        let quote = |t: &str| serde_json::to_string(t).unwrap_or_default();
        match j {
            J::S(t) => out.push_str(&quote(t)),
            J::A(items) if items.is_empty() => out.push_str("[]"),
            J::A(items) => {
                // (Only short lists of words are written.)
                let each: Vec<String> = items
                    .iter()
                    .map(|i| {
                        let mut t = String::new();
                        write(i, depth, &mut t);
                        t
                    })
                    .collect();
                out.push_str(&format!("[{}]", each.join(", ")));
            }
            J::O(pairs) if pairs.is_empty() => out.push_str("{}"),
            J::O(pairs) => {
                out.push_str("{\n");
                for (n, (k, v)) in pairs.iter().enumerate() {
                    out.push_str(&format!("{}{}: ", pad(depth + 1), quote(k)));
                    write(v, depth + 1, out);
                    out.push_str(if n + 1 < pairs.len() { ",\n" } else { "\n" });
                }
                out.push_str(&format!("{}}}", pad(depth)));
            }
        }
    }
    let mut out = String::new();
    write(j, 0, &mut out);
    out
}

/// A path as one word of a command line.
fn quoted(p: &str) -> String {
    if p.chars().any(|c| c.is_whitespace() || c == '"') {
        format!("\"{}\"", p.replace('"', "\\\""))
    } else {
        p.to_string()
    }
}

/// A TOML string holding `p` exactly: as written where it can be (a
/// Windows path keeps its backslashes), else with its marks escaped.
fn toml_string(p: &str) -> String {
    if !p.contains('\'') && !p.chars().any(char::is_control) {
        format!("'{p}'")
    } else {
        serde_json::to_string(p).unwrap_or_default()
    }
}

/// Every app's entry for the server at `server`, with `env` beside it
/// where the server would not find its GIS engine by itself.
pub(crate) fn other_apps(server: &str, env: &[(String, String)]) -> Vec<OtherApp> {
    let all_tools = || J::A(vec![s("*")]);
    // One server named columbus, under the key the app's file uses: its
    // type where the app names one, the program, no arguments, then the
    // environment only when something belongs in it (every one of these
    // files takes an entry without it), then what the app adds after.
    let entry = |top: &str, kind: Option<&str>, after: Vec<(&str, J)>| {
        let mut fields: Vec<(&str, J)> = kind.map(|k| ("type", s(k))).into_iter().collect();
        fields.push(("command", s(server)));
        fields.push(("args", J::A(vec![])));
        if !env.is_empty() {
            fields.push((
                "env",
                J::O(env.iter().map(|(k, v)| (k.clone(), s(v))).collect()),
            ));
        }
        fields.extend(after);
        pretty(&o(vec![(top, o(vec![("columbus", o(fields))]))]))
    };
    let home = if cfg!(windows) {
        "%USERPROFILE%\\"
    } else {
        "~/"
    };
    let sep = if cfg!(windows) { "\\" } else { "/" };
    let in_home = |parts: &[&str]| format!("{home}{}", parts.join(sep));
    let word = quoted(server);
    let env_note = (!env.is_empty()).then(|| {
        "The entry also names the GIS engine's Python (COLUMBUS_PYTHON), because it is not in the folder the server looks in by itself.".to_string()
    });
    let with_env = |mut notes: Vec<String>| {
        notes.extend(env_note.clone());
        notes
    };
    let toml_env = if env.is_empty() {
        String::new()
    } else {
        format!(
            "\nenv = {{ {} }}",
            env.iter()
                .map(|(k, v)| format!("{k} = {}", toml_string(v)))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    vec![
        OtherApp {
            id: "claude-desktop",
            name: "Claude desktop",
            file: if cfg!(windows) {
                "%APPDATA%\\Claude\\claude_desktop_config.json".into()
            } else if cfg!(target_os = "macos") {
                "~/Library/Application Support/Claude/claude_desktop_config.json".into()
            } else {
                "~/.config/Claude/claude_desktop_config.json".into()
            },
            entry: entry("mcpServers", None, vec![]),
            command: None,
            notes: with_env(vec![
                "Restart Claude desktop after you save the file.".into(),
                "Columbus's releases also come as a .mcpb file, which Claude desktop installs by a double-click; the install guide shows that way.".into(),
            ]),
        },
        OtherApp {
            id: "copilot-vscode",
            name: "GitHub Copilot in VS Code",
            file: format!("{} (every workspace), or .mcp.json at the top of one workspace", in_home(&[".copilot", "mcp-config.json"])),
            entry: entry("mcpServers", Some("stdio"), vec![("tools", all_tools())]),
            command: None,
            notes: with_env(vec![
                "The Copilot CLI reads the same file: one entry serves both.".into(),
                format!("The older file, .vscode{sep}mcp.json in a workspace, is still read. There the same entry goes under \"servers\" in place of \"mcpServers\", without the \"tools\" line."),
                "On Copilot Business or Enterprise, your organization must have turned on the policy \"MCP servers in Copilot\".".into(),
            ]),
        },
        OtherApp {
            id: "copilot-cli",
            name: "GitHub Copilot CLI",
            file: in_home(&[".copilot", "mcp-config.json"]),
            // The file is the one Copilot in VS Code reads, so the entry is
            // the same one. (The CLI takes "local" or "stdio" for the type,
            // "local" where none is written; VS Code's page writes "stdio".)
            entry: entry("mcpServers", Some("stdio"), vec![("tools", all_tools())]),
            command: Some(format!("copilot mcp add columbus -- {word}")),
            notes: with_env(vec![
                "Copilot in VS Code reads the same file: one entry serves both. The CLI also takes \"local\" in place of \"stdio\".".into(),
            ]),
        },
        OtherApp {
            id: "visual-studio",
            name: "Visual Studio",
            file: "%USERPROFILE%\\.mcp.json (every solution), or .mcp.json in a solution's folder".into(),
            entry: entry("servers", Some("stdio"), vec![]),
            command: None,
            notes: with_env(vec!["Visual Studio 2022 version 17.14 or later, or Visual Studio 2026.".into()]),
        },
        OtherApp {
            id: "jetbrains",
            name: "JetBrains IDEs (GitHub Copilot)",
            file: "the mcp.json the Copilot plugin opens: Copilot icon, Open Chat, Agent mode, the tools icon, Add MCP Tools".into(),
            entry: entry("servers", None, vec![]),
            command: None,
            notes: with_env(vec![]),
        },
        OtherApp {
            id: "cursor",
            name: "Cursor",
            file: format!("{} (every project), or .cursor{sep}mcp.json in one project", in_home(&[".cursor", "mcp.json"])),
            entry: entry("mcpServers", Some("stdio"), vec![]),
            command: None,
            notes: with_env(vec![]),
        },
        OtherApp {
            id: "codex",
            name: "Codex and ChatGPT desktop",
            file: in_home(&[".codex", "config.toml"]),
            entry: format!("[mcp_servers.columbus]\ncommand = {}\nargs = []{toml_env}", toml_string(server)),
            command: Some(format!("codex mcp add columbus -- {word}")),
            notes: with_env(vec![
                "Codex reads this file. OpenAI's documentation says the ChatGPT desktop app shares it (Settings, MCP servers); Columbus has not been tried with either.".into(),
            ]),
        },
        OtherApp {
            id: "gemini-cli",
            name: "Gemini CLI",
            file: format!("{} (yours), or .gemini{sep}settings.json in one project", in_home(&[".gemini", "settings.json"])),
            entry: entry("mcpServers", None, vec![]),
            command: Some(format!("gemini mcp add -s user columbus {word}")),
            notes: with_env(vec![]),
        },
        OtherApp {
            id: "claude-code",
            name: "Claude Code",
            file: ".mcp.json at the top of a project (or use the command, which keeps it for you)".into(),
            entry: entry("mcpServers", None, vec![]),
            command: Some(format!("claude mcp add --transport stdio columbus -- {word}")),
            notes: with_env(vec![]),
        },
        OtherApp {
            id: "lm-studio",
            name: "LM Studio",
            file: in_home(&[".lmstudio", "mcp.json"]),
            entry: entry("mcpServers", None, vec![]),
            command: None,
            notes: with_env(vec!["LM Studio reads the file again when it is saved.".into()]),
        },
    ]
}

/// A path as a person writes it in another app's settings: Windows hands
/// back `\\?\C:\…` for a file it was asked the real place of, which is the
/// same file as `C:\…`. (A path to another computer is left as it is.)
fn plain_path(p: &str) -> String {
    match p.strip_prefix(r"\\?\") {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => rest.to_string(),
        _ => p.to_string(),
    }
}

/// How to connect other AI apps to the Columbus server on this computer.
#[tauri::command]
pub async fn chat_other_apps(app: AppHandle) -> Result<OtherApps, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let s = chat::load(&app);
        let found: Option<PathBuf> = chat::server(&s);
        let env = found
            .as_deref()
            .map(|srv: &Path| chat::server_env(&s, srv))
            .unwrap_or_default();
        let shown = found.as_ref().map(|p| plain_path(&p.display().to_string()));
        OtherApps {
            apps: other_apps(shown.as_deref().unwrap_or("columbus-mcp"), &env),
            server: shown,
            approvals: APPROVALS,
        }
    })
    .await
    .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn added(id: &str, address: &str) -> Added {
        Added {
            id: id.into(),
            name: "My gateway".into(),
            address: address.into(),
            format: "chat".into(),
            key_header: "bearer".into(),
            pictures: false,
        }
    }

    #[test]
    fn settings_from_before_keep_working() {
        // What 0.14 wrote: a model and a level, for Anthropic.
        let s: Settings = serde_json::from_str(
            r#"{"model": "claude-opus-5-5", "effort": "high", "server": null, "engine": null}"#,
        )
        .unwrap();
        assert_eq!(chosen(&s), "anthropic");
        let r = resolve(&s, "anthropic").unwrap();
        assert_eq!(model_for(&s, &r).as_deref(), Some("claude-opus-5-5"));
        assert_eq!(effort_for(&s, &r), "high");
        assert_eq!(
            (r.address.as_str(), r.host.as_str(), r.local),
            ("https://api.anthropic.com", "api.anthropic.com", false)
        );
        // A model that left the offered list is still the chat's own where it was chosen.
        let s: Settings = serde_json::from_str(r#"{"model": "claude-fable-5-1"}"#).unwrap();
        assert_eq!(
            model_for(&s, &resolve(&s, "anthropic").unwrap()).as_deref(),
            Some("claude-fable-5-1")
        );
        // Nothing saved: Anthropic's first model, the model's own level.
        let s = Settings::default();
        let r = resolve(&s, "anthropic").unwrap();
        assert_eq!(model_for(&s, &r).as_deref(), Some("claude-sonnet-5-5"));
        assert_eq!(effort_for(&s, &r), "");
        // A level the chosen service does not take is not sent to it.
        let s: Settings = serde_json::from_str(r#"{"service": "openai", "effort": "xhigh", "models": {"openai": "gpt-6-luna", "ollama": "llama3.3:70b"}}"#).unwrap();
        assert_eq!(chosen(&s), "openai");
        let r = resolve(&s, "openai").unwrap();
        assert_eq!(model_for(&s, &r).as_deref(), Some("gpt-6-luna"));
        assert_eq!(effort_for(&s, &r), "");
        // A service with no list of its own: the model the person typed, or none.
        let r = resolve(&s, "ollama").unwrap();
        assert_eq!(model_for(&s, &r).as_deref(), Some("llama3.3:70b"));
        assert!(r.local);
        assert_eq!(model_for(&Settings::default(), &r), None);
        // A chosen service that is gone: back to Anthropic.
        let s: Settings = serde_json::from_str(r#"{"service": "gone"}"#).unwrap();
        assert_eq!(chosen(&s), "anthropic");
    }

    #[test]
    fn an_added_service_is_read_and_checked() {
        let s: Settings = serde_json::from_str(
            r#"{"service": "my-gateway", "models": {"my-gateway": "deployment-1"}, "services": [
                {"id": "my-gateway", "name": "My gateway", "address": "https://ai.example.org/openai/v1/", "format": "responses", "key_header": "api-key", "pictures": true},
                {"id": "broken"},
                "not an entry",
                {"id": "local", "name": "Local", "address": "http://localhost:8080/v1", "format": "chat", "key_header": "bearer"}]}"#,
        )
        .unwrap();
        // An entry that does not read is left out; the others stay.
        assert_eq!(
            s.services.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(),
            ["my-gateway", "local"]
        );
        assert_eq!(chosen(&s), "my-gateway");
        let r = resolve(&s, "my-gateway").unwrap();
        assert_eq!(r.address, "https://ai.example.org/openai/v1");
        assert_eq!(r.host, "ai.example.org");
        assert_eq!(r.service.format, Format::Responses);
        assert_eq!(r.service.key_header, KeyHeader::ApiKey);
        assert!(r.service.pictures && !r.local);
        assert_eq!(model_for(&s, &r).as_deref(), Some("deployment-1"));
        assert!(resolve(&s, "local").unwrap().local);
        // What the settings file says is checked every time it is used.
        for (address, format, header) in [
            ("http://ai.example.org/v1", "chat", "bearer"),
            ("https://user:pw@ai.example.org/v1", "chat", "bearer"),
            ("https://ai.example.org/v1?key=abc", "chat", "bearer"),
            ("https://ai.example.org/v1", "completions", "bearer"),
            ("https://ai.example.org/v1", "chat", "cookie"),
        ] {
            let a = Added {
                format: format.into(),
                key_header: header.into(),
                ..added("x", address)
            };
            assert!(resolve_added(&a).is_err(), "{address} {format} {header}");
        }
        // An id may not be one of Columbus's own services, or anything but a plain word.
        for id in ["openai", "anthropic", "Has Space", "", "ünï"] {
            assert!(
                resolve_added(&added(id, "https://ai.example.org/v1")).is_err(),
                "{id}"
            );
        }
        assert!(resolve(&s, "nobody").is_err());
    }

    #[test]
    fn a_new_service_gets_an_id_of_its_own() {
        assert_eq!(new_id("My Gateway", &[]), "my-gateway");
        assert_eq!(new_id("  Azure OpenAI (west)  ", &[]), "azure-openai-west");
        assert_eq!(
            new_id("OpenAI", &[]),
            "openai-2",
            "never one of Columbus's own ids"
        );
        assert_eq!(new_id("日本", &[]), "service");
        let taken = [
            added("my-gateway", "https://a.example/v1"),
            added("my-gateway-2", "https://b.example/v1"),
        ];
        assert_eq!(new_id("My gateway", &taken), "my-gateway-3");
        assert!(new_id(&"x".repeat(100), &[]).len() <= 30);
    }

    #[test]
    fn keys_are_kept_by_service_and_anthropics_where_it_always_was() {
        assert_eq!(key_account("anthropic"), "anthropic-api-key");
        assert_eq!(key_account("openai"), "ai-key:openai");
        assert_eq!(key_account("my-gateway"), "ai-key:my-gateway");
        assert_eq!(hint("sk-abcdefgh1234"), "1234");
        assert_eq!(hint("abc"), "abc");
        assert!(key_ok("0123456789abcdefghij", true));
        assert!(!key_ok("0123456789", true) && key_ok("0123456789", false));
        assert!(!key_ok("has a space in it 0123456789", true));
        assert!(
            !key_ok("ключ-ключ-ключ-ключ-ключ", true),
            "a header takes plain characters only"
        );
        assert!(!key_ok("short", false));
    }

    #[test]
    fn what_was_confirmed_is_the_format_the_key_header_and_the_address() {
        let a = added("x", "https://ai.example.org/v1/");
        assert_eq!(
            confirmation(&a).unwrap(),
            "chat|bearer|https://ai.example.org/v1"
        );
        // A change of name or of pictures is no change of where things go.
        let renamed = Added {
            name: "Another name".into(),
            pictures: true,
            ..a.clone()
        };
        assert_eq!(confirmation(&renamed).unwrap(), confirmation(&a).unwrap());
        for changed in [
            Added {
                address: "https://ai.example.com/v1".into(),
                ..a.clone()
            },
            Added {
                format: "responses".into(),
                ..a.clone()
            },
            Added {
                key_header: "api-key".into(),
                ..a.clone()
            },
        ] {
            assert_ne!(confirmation(&changed).unwrap(), confirmation(&a).unwrap());
        }
        assert!(confirmation(&added("x", "http://ai.example.org/v1")).is_err());
    }

    #[test]
    fn each_other_app_gets_its_own_file_and_entry_with_this_computers_path() {
        let server = if cfg!(windows) {
            r"C:\Users\Ana Maria\Columbus\columbus-mcp.exe"
        } else {
            "/Users/ana maria/Columbus/columbus-mcp"
        };
        let apps = other_apps(server, &[]);
        let ids: Vec<&str> = apps.iter().map(|a| a.id).collect();
        assert_eq!(
            ids,
            [
                "claude-desktop",
                "copilot-vscode",
                "copilot-cli",
                "visual-studio",
                "jetbrains",
                "cursor",
                "codex",
                "gemini-cli",
                "claude-code",
                "lm-studio"
            ]
        );
        let by = |id: &str| apps.iter().find(|a| a.id == id).unwrap();
        // The entry is the file's own format, and holds the path exactly.
        for a in &apps {
            assert!(!a.file.is_empty() && !a.name.is_empty(), "{}", a.id);
            if a.id == "codex" {
                continue;
            }
            let v: serde_json::Value =
                serde_json::from_str(&a.entry).unwrap_or_else(|e| panic!("{}: {e}", a.id));
            let top = if ["visual-studio", "jetbrains"].contains(&a.id) {
                "servers"
            } else {
                "mcpServers"
            };
            assert_eq!(v[top]["columbus"]["command"], server, "{}", a.id);
            assert_eq!(v.as_object().unwrap().len(), 1, "{}", a.id);
            assert_eq!(
                v[top]["columbus"]["args"],
                serde_json::json!([]),
                "{}",
                a.id
            );
            // Nothing belongs in the environment here, so there is no line for it.
            assert!(
                v[top]["columbus"].get("env").is_none() && !a.entry.contains("env"),
                "{}: {}",
                a.id,
                a.entry
            );
            assert!(
                !a.notes.iter().any(|n| n.contains("COLUMBUS_PYTHON")),
                "{}",
                a.id
            );
        }
        // As read on 2026-11-11: the type each app names, and its tools line.
        let entry = |id: &str| serde_json::from_str::<serde_json::Value>(&by(id).entry).unwrap();
        assert_eq!(
            entry("copilot-vscode")["mcpServers"]["columbus"]["type"],
            "stdio"
        );
        assert_eq!(
            entry("copilot-vscode")["mcpServers"]["columbus"]["tools"],
            serde_json::json!(["*"])
        );
        // The Copilot CLI reads the file Copilot in VS Code reads: the
        // entry is the same one, and each says so.
        assert_eq!(by("copilot-cli").entry, by("copilot-vscode").entry);
        assert!(by("copilot-vscode")
            .file
            .starts_with(&by("copilot-cli").file));
        assert!(by("copilot-cli").notes[0].contains("VS Code reads the same file"));
        assert!(by("copilot-vscode").notes[0].contains("Copilot CLI reads the same file"));
        assert_eq!(
            entry("visual-studio")["servers"]["columbus"]["type"],
            "stdio"
        );
        assert_eq!(entry("cursor")["mcpServers"]["columbus"]["type"], "stdio");
        assert!(entry("jetbrains")["servers"]["columbus"]
            .get("type")
            .is_none());
        assert!(entry("claude-desktop")["mcpServers"]["columbus"]
            .get("type")
            .is_none());
        assert!(
            by("copilot-vscode").file.contains("mcp-config.json")
                && by("copilot-vscode").file.contains(".mcp.json")
        );
        assert!(
            by("copilot-vscode")
                .notes
                .iter()
                .any(|n| n.contains("mcp.json") && n.contains("\"servers\"")),
            "the older file is named"
        );
        assert!(by("claude-desktop")
            .file
            .ends_with("claude_desktop_config.json"));
        // The command is the first thing the person reads in the entry.
        let lines: Vec<&str> = by("claude-desktop").entry.lines().collect();
        assert_eq!(
            lines[0..3],
            ["{", "  \"mcpServers\": {", "    \"columbus\": {"]
        );
        assert!(lines[3].starts_with("      \"command\": "), "{}", lines[3]);
        assert_eq!(lines[4..], ["      \"args\": []", "    }", "  }", "}"]);
        assert!(by("copilot-vscode").entry.contains("\"tools\": [\"*\"]"));
        // Codex: TOML, the path as it is written on this system.
        let codex = by("codex");
        assert!(codex.file.ends_with("config.toml"));
        assert_eq!(
            codex.entry,
            format!("[mcp_servers.columbus]\ncommand = '{server}'\nargs = []")
        );
        // A command line takes the path as one word.
        assert_eq!(
            by("claude-code").command.as_deref(),
            Some(format!("claude mcp add --transport stdio columbus -- \"{server}\"").as_str())
        );
        assert_eq!(
            by("codex").command.as_deref(),
            Some(format!("codex mcp add columbus -- \"{server}\"").as_str())
        );
        assert_eq!(
            by("gemini-cli").command.as_deref(),
            Some(format!("gemini mcp add -s user columbus \"{server}\"").as_str())
        );
        assert_eq!(
            by("copilot-cli").command.as_deref(),
            Some(format!("copilot mcp add columbus -- \"{server}\"").as_str())
        );
        assert_eq!(
            quoted("/opt/columbus/columbus-mcp"),
            "/opt/columbus/columbus-mcp"
        );
        // An engine the server would not find by itself is named beside it.
        let apps = other_apps(
            server,
            &[("COLUMBUS_PYTHON".into(), "/opt/gis/bin/python3".into())],
        );
        for a in apps.iter().filter(|a| a.id != "codex") {
            let v: serde_json::Value = serde_json::from_str(&a.entry).unwrap();
            let top = if ["visual-studio", "jetbrains"].contains(&a.id) {
                "servers"
            } else {
                "mcpServers"
            };
            assert_eq!(
                v[top]["columbus"]["env"],
                serde_json::json!({"COLUMBUS_PYTHON": "/opt/gis/bin/python3"}),
                "{}",
                a.id
            );
            assert!(
                a.notes.iter().any(|n| n.contains("COLUMBUS_PYTHON")),
                "{}",
                a.id
            );
        }
        // After the arguments, before what the app adds of its own.
        let lines: Vec<&str> = apps[1].entry.lines().map(str::trim).collect();
        let at = |start: &str| lines.iter().position(|l| l.starts_with(start)).unwrap();
        assert!(at("\"args\"") < at("\"env\"") && at("\"env\"") < at("\"tools\""));
        assert!(apps
            .iter()
            .find(|a| a.id == "codex")
            .unwrap()
            .entry
            .ends_with("env = { COLUMBUS_PYTHON = '/opt/gis/bin/python3' }"));
        // Windows' long form of a path is written the usual way.
        assert_eq!(
            plain_path(r"\\?\C:\Columbus\columbus-mcp.exe"),
            r"C:\Columbus\columbus-mcp.exe"
        );
        assert_eq!(
            plain_path(r"C:\Columbus\columbus-mcp.exe"),
            r"C:\Columbus\columbus-mcp.exe"
        );
        assert_eq!(
            plain_path(r"\\?\UNC\host\share\columbus-mcp.exe"),
            r"\\?\UNC\host\share\columbus-mcp.exe"
        );
        assert_eq!(
            plain_path("/opt/columbus/columbus-mcp"),
            "/opt/columbus/columbus-mcp"
        );
        // A path with a quote mark in it is still one TOML string.
        assert_eq!(toml_string("it's"), "\"it's\"");
        // Nothing is promised of an app that was not confirmed.
        assert!(APPROVALS.contains("local approvals page") && APPROVALS.contains("45 seconds"));
    }
}
