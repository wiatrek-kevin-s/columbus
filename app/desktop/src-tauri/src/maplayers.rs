//! Connections › Map layers: the person's web map layers (build brief
//! part 2, Part 3: REQ-31, REQ-33, F-13, F-14; T-25). The server side is
//! T-24's `columbus-mcp map-source inspect|save|test|remove|list`, which
//! these commands run, as Sign in runs `arcgis signin`.
//!
//! Saving a layer lets the person's map (never the assistant) ask that
//! service for tiles of the area in view. When the organization lists no
//! `map.hosts`, saving is the person's approval of the service's host and
//! the hosts its capabilities name, so the save is confirmed at the
//! keyboard first (Touch ID or the account password on macOS, Windows
//! Hello on Windows: `settings::confirm`), unless an Edit keeps to hosts
//! already approved. With `map.hosts` listed, the server refuses any other
//! host itself. Testing and removing need no confirmation. Only the app's
//! own main window may call these (capabilities/main.json); there is no
//! MCP tool for any of it.

use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Value};
use tauri::AppHandle;

use crate::settings::{confirm, ONE_AT_A_TIME};

/// Reading capabilities can take the server's 30 s per request, plus a
/// margin; a Test may read capabilities and one tile.
const LIMIT: Duration = Duration::from_secs(90);

#[derive(Deserialize, Debug)]
pub struct LayerForm {
    /// Set when editing a saved layer (its id cannot change).
    #[serde(default)]
    existing: bool,
    id: String,
    title: String,
    /// One of the six F-13 kinds, or empty to tell from the address.
    #[serde(default)]
    kind: Option<String>,
    url: String,
    #[serde(default)]
    layer: Option<String>,
    #[serde(default)]
    auth_from: Option<String>,
    #[serde(default)]
    attribution: Option<String>,
}

fn trimmed(v: &Option<String>) -> Option<String> {
    v.as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
}

/// `columbus-mcp map-source …`: its JSON answer, or its plain message.
fn cli(app: &AppHandle, args: &[&str]) -> Result<Value, String> {
    let (server, engine) = crate::registry::program(app)?;
    let mut all = vec!["map-source"];
    all.extend_from_slice(args);
    let (_, out, errs) = crate::registry::run_raw(&server, engine.as_deref(), &all, LIMIT)?;
    let v: Value = serde_json::from_str(out.trim()).map_err(|_| {
        let last = errs
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("Columbus's answer could not be read");
        last.trim().to_string()
    })?;
    if v["ok"] == true {
        Ok(v)
    } else {
        Err(v["message"]
            .as_str()
            .unwrap_or("Columbus could not do that")
            .to_string())
    }
}

/// The host of an address (lower case, no port or user), as far as the
/// app needs it: to tell whether an Edit keeps to approved hosts.
pub(crate) fn host_of(url: &str) -> Option<String> {
    let rest = url.trim().split_once("://")?.1;
    let auth = rest.split(['/', '?', '#']).next().unwrap_or("");
    let auth = auth.rsplit('@').next().unwrap_or(auth);
    let host = if let Some(v6) = auth.strip_prefix('[') {
        v6.split(']').next().unwrap_or("")
    } else {
        auth.split(':').next().unwrap_or("")
    };
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    (!host.is_empty()).then_some(host)
}

fn inspect_args<'a>(
    url: &'a str,
    kind: &'a Option<String>,
    auth_from: &'a Option<String>,
) -> Vec<&'a str> {
    let mut args = vec!["inspect", url];
    if let Some(k) = kind.as_deref().filter(|k| !k.is_empty()) {
        args.extend(["--kind", k]);
    }
    if let Some(a) = auth_from.as_deref().filter(|a| !a.is_empty()) {
        args.extend(["--auth-from", a]);
    }
    args
}

/// Every web layer: the person's (with the hosts approved when each was
/// saved) and the organization's, and whether saving asks to confirm.
#[tauri::command]
pub async fn map_layers_list(app: AppHandle) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || cli(&app, &["list"]))
        .await
        .map_err(|e| e.to_string())?
}

/// Read a pasted address's capabilities and offer its layers. Nothing is
/// saved and only the pasted host is asked.
#[tauri::command]
pub async fn map_layer_inspect(
    app: AppHandle,
    url: String,
    kind: Option<String>,
    auth_from: Option<String>,
) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let url = url.trim().to_string();
        let kind = trimmed(&kind);
        let auth_from = trimmed(&auth_from);
        cli(&app, &inspect_args(&url, &kind, &auth_from))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Add (or, with `existing`, edit) a web layer: its capabilities are read
/// again here (never taken from the page), the person confirms the hosts
/// it will reach when the organization lists none, then it is saved.
#[tauri::command]
pub async fn map_layer_save(app: AppHandle, form: LayerForm) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || save(&app, form))
        .await
        .map_err(|e| e.to_string())?
}

fn save(app: &AppHandle, form: LayerForm) -> Result<Value, String> {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let id = form.id.trim().to_string();
    let title = form.title.trim().to_string();
    let url = form.url.trim().to_string();
    if url.is_empty() {
        return Err("paste the service's address first".into());
    }
    let kind = trimmed(&form.kind);
    let auth_from = trimmed(&form.auth_from);
    let layer = trimmed(&form.layer);
    let found = cli(app, &inspect_args(&url, &kind, &auth_from))?;
    let kind = found["kind"].as_str().unwrap_or_default().to_string();
    let host = found["host"].as_str().unwrap_or_default().to_string();
    // The hosts this layer's requests go to: its own, and those its
    // capabilities name for the chosen layer.
    let mut hosts = vec![host.clone()];
    if let Some(l) = &layer {
        if let Some(o) = found["layers"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|o| o["layer"].as_str() == Some(l.as_str()))
        {
            for h in o["hosts"].as_array().into_iter().flatten() {
                if let Some(h) = h.as_str() {
                    let h = h.to_ascii_lowercase();
                    if !hosts.contains(&h) {
                        hosts.push(h);
                    }
                }
            }
        }
    }
    if found["confirm"] == true {
        // An Edit that keeps to the hosts already approved for this layer
        // widens nothing.
        let known: Vec<String> = if form.existing {
            let list = cli(app, &["list"])?;
            list["sources"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|s| s["id"].as_str() == Some(id.as_str()) && s["origin"] == "saved")
                .map(|s| {
                    let mut k: Vec<String> =
                        s["url"].as_str().and_then(host_of).into_iter().collect();
                    k.extend(
                        s["approved_hosts"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|h| h.as_str().map(|h| h.to_ascii_lowercase())),
                    );
                    k
                })
                .unwrap_or_default()
        } else {
            vec![]
        };
        if !hosts.iter().all(|h| known.contains(h)) {
            confirm(&format!(
                "show the web layer \"{title}\" on your maps from {}; its service will see which area you are viewing",
                hosts.join(" and ")
            ))?;
        }
    }
    let mut entry = json!({"id": id, "title": title, "kind": kind, "url": url});
    if let Some(l) = layer {
        entry["layer"] = json!(l);
    }
    if let Some(a) = auth_from {
        entry["auth_from"] = json!(a);
    }
    if let Some(a) = trimmed(&form.attribution) {
        entry["attribution"] = json!(a);
    }
    let text = entry.to_string();
    let mut args = vec!["save", text.as_str()];
    if form.existing {
        args.push("--replace");
    }
    cli(app, &args)
}

/// Is it reachable, as the map would ask? `{ok, message}`; a failure is
/// an answer here, not an error.
#[tauri::command]
pub async fn map_layer_test(app: AppHandle, id: String) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || match cli(&app, &["test", id.trim()]) {
        Ok(v) => Ok(v),
        Err(e) => Ok(json!({"ok": false, "message": e})),
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Remove a saved web layer: its file, its approved hosts and its tiles go,
/// and every chat's map drops it.
#[tauri::command]
pub async fn map_layer_remove(app: AppHandle, id: String) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
        cli(&app, &["remove", id.trim()])
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::host_of;

    #[test]
    fn hosts_of_addresses() {
        assert_eq!(
            host_of("https://Imagery.Example.gov/wmts/1.0.0/WMTSCapabilities.xml").as_deref(),
            Some("imagery.example.gov")
        );
        assert_eq!(
            host_of("http://127.0.0.1:8080/web/xyz/{z}/{x}/{y}.png").as_deref(),
            Some("127.0.0.1")
        );
        assert_eq!(
            host_of("https://user@tiles.example.com:443?x=1").as_deref(),
            Some("tiles.example.com")
        );
        assert_eq!(host_of("https://[::1]:8443/a").as_deref(), Some("::1"));
        assert_eq!(host_of("tiles.example.com/a"), None);
        assert_eq!(host_of("https:///a"), None);
    }
}
