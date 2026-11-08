//! `columbus-mcp notes check '{}'`: what in the person's notes needs them,
//! for the app's Documents > Proposals inbox.
//!
//! - **Changed since drafted:** for each note about a dataset, its fields as
//!   the database lists them now (through Columbus, with the rules in force:
//!   `list_datasets` in an in-process session, audited like any client's
//!   call), against the fields the note names: fields added, gone (or now
//!   hidden), or of another type, and datasets no longer there or allowed.
//!   A note no one has edited since it was drafted can simply be drafted
//!   again; an edited or reviewed one is the person's to update.
//! - **Due for review:** notes whose `last_confirmed` is older than the
//!   notes' review interval.
//!
//! Read only: nothing in the notes or the databases changes. Prints
//! `{"ok": true, "changed": [...], "due": [...], "problems": [...]}`.

use std::collections::{BTreeMap, BTreeSet};

use chrono::NaiveDate;
use serde_json::{json, Value};

use crate::context::Pack;
use crate::server::State;

pub async fn check(request: &str) -> anyhow::Result<()> {
    #[cfg(target_os = "linux")]
    unsafe {
        libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0);
    }
    let out = match run(request).await {
        Ok(v) => v,
        Err(e) => json!({"ok": false, "error": e}),
    };
    println!("{out}");
    Ok(())
}

struct DatasetNote {
    id: String,
    title: String,
    path: String,
    status: String,
    untouched: bool,
    connection: String,
    dataset: String,
    fields: BTreeMap<String, Option<String>>,
}

/// Most connections checked at once.
const MAX_CONNECTIONS: usize = 50;

/// The name in `names` that is `want`: the same name, or else the one name
/// that differs from it only in case (some databases fold case). None when
/// there's no such name, or several.
fn matching<'a>(want: &str, names: &[&'a str]) -> Option<&'a str> {
    if let Some(n) = names.iter().find(|n| **n == want) {
        return Some(n);
    }
    let mut folded = names.iter().filter(|n| n.eq_ignore_ascii_case(want));
    match (folded.next(), folded.next()) {
        (Some(n), None) => Some(n),
        _ => None,
    }
}

fn months_between(a: NaiveDate, b: NaiveDate) -> i64 {
    use chrono::Datelike;
    (b.year() as i64 - a.year() as i64) * 12 + (b.month() as i64 - a.month() as i64)
        - i64::from(b.day() < a.day())
}

async fn run(request: &str) -> Result<Value, String> {
    if request.len() > 4096 {
        return Err("the request is too long".into());
    }
    let _: Value = serde_json::from_str(request).map_err(|e| format!("not a request: {e}"))?;
    let state = State::load(crate::config::Paths::from_env()).map_err(|e| e.to_string())?;
    let workspace = state.sandbox.root().to_path_buf();
    let root = crate::notes::notes_root(&state.paths)?;
    let today = chrono::Local::now().date_naive();
    if !root.join("pack.yaml").is_file() {
        let _ = std::fs::remove_dir(&workspace);
        return Ok(json!({"ok": true, "changed": [], "due": [], "problems": [], "notes": 0}));
    }
    let pack = Pack::load(&root)?;
    let interval = pack.info.review_interval_months;
    let rel = |e: &crate::context::Entry| {
        e.file
            .strip_prefix(&root)
            .unwrap_or(&e.file)
            .display()
            .to_string()
    };
    let mut due = Vec::new();
    let mut notes = Vec::new();
    for e in &pack.entries {
        let title = e.str_field("title").unwrap_or_else(|| e.id.clone());
        let status = e.str_field("status").unwrap_or_default();
        if status == "retired" {
            continue;
        }
        // Due as the context tools call an entry stale, so the person and
        // their assistant agree.
        if e.stale(interval, today) == Some(true) {
            if let Some(d) = e
                .str_field("last_confirmed")
                .and_then(|d| NaiveDate::parse_from_str(&d, "%Y-%m-%d").ok())
            {
                due.push(
                    json!({"id": e.id, "title": title, "path": rel(e), "status": status,
                    "last_confirmed": d.to_string(), "months": months_between(d, today).max(0)}),
                );
            }
        }
        let (Some(connection), Some(dataset)) = (
            e.str_field("connection"),
            e.str_field("physical").or_else(|| e.str_field("dataset")),
        ) else {
            continue;
        };
        if e.kind != "dataset" {
            continue;
        }
        let mut fields = BTreeMap::new();
        if let Some(serde_yaml::Value::Mapping(m)) = e.card.get(serde_yaml::Value::from("fields")) {
            for (k, v) in m {
                if let Some(k) = k.as_str() {
                    let ty = v
                        .get("type")
                        .and_then(|t| t.as_str())
                        .map(|t| t.to_string());
                    fields.insert(k.to_string(), ty);
                }
            }
        }
        if fields.is_empty() {
            continue; // a note that names no fields has none to compare
        }
        notes.push(DatasetNote {
            id: e.id.clone(),
            title,
            path: rel(e),
            untouched: crate::notes::untouched_draft(&e.file),
            status,
            connection,
            dataset,
            fields,
        });
    }
    let mut connections: BTreeSet<String> = notes.iter().map(|n| n.connection.clone()).collect();
    let mut problems = Vec::new();
    if connections.len() > MAX_CONNECTIONS {
        let rest: Vec<String> = connections.iter().skip(MAX_CONNECTIONS).cloned().collect();
        for c in &rest {
            connections.remove(c);
        }
        problems.push(
            json!({"connection": format!("{} more connections", rest.len()),
            "error": format!("only the first {MAX_CONNECTIONS} connections are checked at once")}),
        );
    }
    let calls: Vec<(String, serde_json::Map<String, Value>)> = connections
        .iter()
        .map(|c| {
            let mut m = serde_json::Map::new();
            m.insert("connection".into(), json!(c));
            ("list_datasets".to_string(), m)
        })
        .collect();
    let answers = crate::registry_cli::calls(state, calls).await;
    let _ = std::fs::remove_dir(&workspace);
    let mut listed: BTreeMap<String, Result<Vec<Value>, String>> = BTreeMap::new();
    for (c, a) in connections.iter().zip(answers) {
        listed.insert(
            c.clone(),
            a.map(|v| {
                v["result"]["datasets"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
            }),
        );
    }
    let mut changed = Vec::new();
    for (c, r) in &listed {
        if let Err(e) = r {
            problems.push(json!({"connection": c, "error": e}));
        }
    }
    for n in &notes {
        let Some(Ok(datasets)) = listed.get(&n.connection) else {
            continue;
        };
        let names: Vec<&str> = datasets.iter().filter_map(|d| d["name"].as_str()).collect();
        let found = matching(&n.dataset, &names)
            .and_then(|name| datasets.iter().find(|d| d["name"].as_str() == Some(name)));
        let base = json!({"id": n.id, "title": n.title, "path": n.path, "status": n.status,
            "untouched": n.untouched, "connection": n.connection, "dataset": n.dataset});
        let Some(d) = found else {
            let mut v = base;
            v["gone"] = json!(true);
            changed.push(v);
            continue;
        };
        let now: BTreeMap<String, Option<String>> = d["fields"]
            .as_array()
            .into_iter()
            .flatten()
            // As drafting does: names it would never write to a note are no
            // change; nor, here, are names with characters a person can't
            // see, which would otherwise reach a chat through "Ask my
            // assistant".
            .filter(|f| {
                f["name"].as_str().is_some_and(|n| {
                    crate::notes::name_ok(n) && !n.chars().any(columbus_shared::canon::hidden_char)
                })
            })
            .filter_map(|f| {
                f["name"]
                    .as_str()
                    .map(|name| (name.to_string(), f["type"].as_str().map(|t| t.to_string())))
            })
            .collect();
        if now.is_empty() {
            continue; // the listing doesn't say (a raster folder, say)
        }
        let was_names: Vec<&str> = n.fields.keys().map(String::as_str).collect();
        let now_names: Vec<&str> = now.keys().map(String::as_str).collect();
        let mut seen_now = BTreeSet::new();
        let mut removed: Vec<&str> = Vec::new();
        let mut retyped: Vec<Value> = Vec::new();
        for (name, t) in &n.fields {
            let Some(m) = matching(name, &now_names) else {
                removed.push(name);
                continue;
            };
            seen_now.insert(m);
            if let (Some(a), Some(Some(b))) = (t, now.get(m)) {
                if !a.eq_ignore_ascii_case(b) {
                    retyped.push(json!({"field": name, "was": a, "now": b}));
                }
            }
        }
        let added: Vec<&str> = now_names
            .iter()
            .copied()
            .filter(|k| !seen_now.contains(k) && matching(k, &was_names).is_none())
            .collect();
        if added.is_empty() && removed.is_empty() && retyped.is_empty() {
            continue;
        }
        let mut v = base;
        v["added"] = json!(added);
        v["removed"] = json!(removed);
        v["retyped"] = json!(retyped);
        changed.push(v);
    }
    Ok(
        json!({"ok": true, "changed": changed, "due": due, "problems": problems,
        "notes": pack.entries.len(), "interval_months": interval}),
    )
}
