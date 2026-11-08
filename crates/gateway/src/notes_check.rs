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
    let interval = if pack.info.review_interval_months == 0 {
        12
    } else {
        pack.info.review_interval_months
    };
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
        if status != "retired" {
            if let Some(d) = e
                .str_field("last_confirmed")
                .and_then(|d| NaiveDate::parse_from_str(&d, "%Y-%m-%d").ok())
            {
                let m = months_between(d, today);
                if m >= i64::from(interval) {
                    due.push(
                        json!({"id": e.id, "title": title, "path": rel(e), "status": status,
                        "last_confirmed": d.to_string(), "months": m}),
                    );
                }
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
    let connections: BTreeSet<String> = notes.iter().map(|n| n.connection.clone()).collect();
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
    let mut problems = Vec::new();
    for (c, r) in &listed {
        if let Err(e) = r {
            problems.push(json!({"connection": c, "error": e}));
        }
    }
    for n in &notes {
        let Some(Ok(datasets)) = listed.get(&n.connection) else {
            continue;
        };
        let norm = |s: &str| s.to_ascii_lowercase();
        let found = datasets.iter().find(|d| {
            d["name"]
                .as_str()
                .is_some_and(|x| norm(x) == norm(&n.dataset))
        });
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
            // As drafting does: names with control or line-break characters
            // are never written to notes, so they are no change.
            .filter(|f| {
                f["name"]
                    .as_str()
                    .is_some_and(|n| !n.chars().any(|c| c.is_control()))
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
        let key =
            |m: &BTreeMap<String, Option<String>>| -> BTreeMap<String, (String, Option<String>)> {
                m.iter()
                    .map(|(k, v)| (k.to_ascii_lowercase(), (k.clone(), v.clone())))
                    .collect()
            };
        let (was, is) = (key(&n.fields), key(&now));
        let added: Vec<&String> = is
            .iter()
            .filter(|(k, _)| !was.contains_key(*k))
            .map(|(_, (name, _))| name)
            .collect();
        let removed: Vec<&String> = was
            .iter()
            .filter(|(k, _)| !is.contains_key(*k))
            .map(|(_, (name, _))| name)
            .collect();
        let retyped: Vec<Value> = was
            .iter()
            .filter_map(|(k, (name, t))| {
                let (_, now_t) = is.get(k)?;
                match (t, now_t) {
                    (Some(a), Some(b)) if !a.eq_ignore_ascii_case(b) => {
                        Some(json!({"field": name, "was": a, "now": b}))
                    }
                    _ => None,
                }
            })
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
