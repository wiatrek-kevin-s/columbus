//! The person's own notes: context entries Columbus drafts from a
//! connection's database, which the person (or an AI, with the person's
//! approval each time) fills in with what the data means.
//!
//! Principle 10 of the design: harvest facts, curate meaning. A draft holds
//! only what the database says about itself: datasets, fields and types,
//! geometry and CRS, row counts, keys, relationships, indexes, the comments
//! people wrote in the database, and the codes short fields hold. What a
//! dataset is for, what a code means and how to read it is for people.
//!
//! The notes live in a folder of the person's own (`notes/`, beside the
//! session workspaces, or `COLUMBUS_NOTES`), in the context pack format,
//! so the context tools read them beside the organization's pack. They are
//! never the organization's rules: nothing in them reaches `whoami`'s
//! always-on rules, and every entry is served as information, not
//! instruction. They are served through the rules in force *now*: a note
//! about a connection that is gone, a dataset no longer allowed or a field
//! now hidden is not shown (or shows without that field).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use columbus_shared::approvals::Risk;
use columbus_shared::audit::AuditRecord;
use columbus_shared::policy::{Connection, EffectiveAccess};
use columbus_shared::sqlcheck::{check_select, SqlDialect};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::service::RequestContext;
use rmcp::{tool, tool_router, Json, RoleServer};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::approval_flow::{Ask, Sought};
use crate::context::{Entry, Pack};
use crate::server::{Check, Gateway, Output, Severity, State};

/// Most datasets one draft covers (the assistant can name more, in turns).
const MAX_DATASETS: usize = 200;
/// From the app, which drafts a whole connection at once.
const APP_MAX_DATASETS: usize = 1000;
/// Most fields of one dataset whose values are counted.
const MAX_PROFILED_FIELDS: usize = 25;
/// A field with more distinct values than this is not a code list.
const MAX_CODES: usize = 30;
/// A code longer than this is not a code.
const MAX_CODE_CHARS: usize = 60;
/// Datasets with more rows than this are not profiled (counting values
/// would read them whole).
const MAX_PROFILED_ROWS: u64 = 2_000_000;

/// Where the person's notes live.
pub(crate) fn notes_dir(paths: &crate::config::Paths) -> PathBuf {
    columbus_shared::paths::env_path("COLUMBUS_NOTES").unwrap_or_else(|| {
        paths
            .workspaces
            .parent()
            .unwrap_or(&paths.workspaces)
            .join("notes")
    })
}

/// A slug for ids and file names: lower-case letters, digits and `-`.
fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let t = out.trim_matches('-');
    let t: String = t.chars().take(60).collect();
    let t = t.trim_end_matches('-').to_string();
    // Names Windows keeps for devices are not file names there.
    let reserved = matches!(t.as_str(), "con" | "prn" | "aux" | "nul")
        || ((t.starts_with("com") || t.starts_with("lpt"))
            && t.len() == 4
            && t[3..].chars().all(|c| c.is_ascii_digit()));
    if t.is_empty() {
        "x".into()
    } else if reserved {
        format!("{t}-x")
    } else {
        t
    }
}

/// The hash a draft carries of itself, so a later draft can tell whether
/// anyone has edited it since (then it is left alone).
fn draft_hash(text: &str) -> String {
    let blanked: String = text
        .lines()
        .map(|l| {
            if l.trim_start().starts_with("draft_sha256:") {
                "draft_sha256:"
            } else {
                l
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let mut h = Sha256::new();
    h.update(blanked.as_bytes());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// Fill in the one line that is exactly `draft_sha256:` (text from the
/// database elsewhere in the note can hold the same words).
fn with_hash(text: &str) -> String {
    let h = draft_hash(text);
    let mut done = false;
    let mut out: Vec<String> = Vec::new();
    for l in text.split('\n') {
        if !done && l == "draft_sha256:" {
            out.push(format!("draft_sha256: {h}"));
            done = true;
        } else {
            out.push(l.to_string());
        }
    }
    out.join("\n")
}

/// Whether `path` is a draft Columbus wrote that no one has changed.
fn untouched_draft(path: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    let Some(line) = text
        .lines()
        .find(|l| l.trim_start().starts_with("draft_sha256:"))
    else {
        return false;
    };
    let given = line.trim_start().trim_start_matches("draft_sha256:").trim();
    !given.is_empty() && given == draft_hash(&text)
}

/// Write a file in the notes folder: never through a link, whole, then
/// moved into place.
/// A note's file and the folders to it are plain (no links, no
/// devices), checked before the person is asked and again when written.
fn plain_place(root: &Path, rel: &Path) -> Result<(), String> {
    let mut at = root.to_path_buf();
    for c in rel.parent().into_iter().flat_map(|p| p.components()) {
        at.push(c);
        if std::fs::symlink_metadata(&at).is_ok_and(|m| m.file_type().is_symlink() || !m.is_dir()) {
            return Err(format!("{} is not a plain folder", at.display()));
        }
    }
    let path = root.join(rel);
    if std::fs::symlink_metadata(&path).is_ok_and(|m| !m.is_file()) {
        return Err(format!("{} is not a plain file", path.display()));
    }
    Ok(())
}

fn write_note(root: &Path, rel: &Path, text: &str) -> Result<(), String> {
    let path = root.join(rel);
    let mut at = root.to_path_buf();
    for c in rel.parent().into_iter().flat_map(|p| p.components()) {
        at.push(c);
        match std::fs::symlink_metadata(&at) {
            Ok(m) if m.file_type().is_symlink() || !m.is_dir() => {
                return Err(format!("{} is not a plain folder", at.display()))
            }
            Ok(_) => {}
            Err(_) => std::fs::create_dir(&at).map_err(|e| format!("{}: {e}", at.display()))?,
        }
    }
    if std::fs::symlink_metadata(&path).is_ok_and(|m| !m.is_file()) {
        return Err(format!("{} is not a plain file", path.display()));
    }
    let tmp = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4().simple()));
    std::fs::write(&tmp, text).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("{}: {e}", path.display())
    })
}

/// The notes folder is the person's, never inside the organization's pack.
fn notes_root(paths: &crate::config::Paths) -> Result<PathBuf, String> {
    let root = notes_dir(paths);
    let canon = |p: &Path| {
        let c = p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
        if cfg!(windows) {
            PathBuf::from(c.to_string_lossy().to_lowercase())
        } else {
            c
        }
    };
    let (r, c) = (canon(&root), canon(&paths.context));
    if r.starts_with(&c) || c.starts_with(&r) {
        return Err(format!(
            "the notes folder {} is inside the organization's context folder; set COLUMBUS_NOTES elsewhere",
            root.display()
        ));
    }
    Ok(root)
}

fn ensure_pack(root: &Path) -> Result<(), String> {
    std::fs::create_dir_all(root).map_err(|e| format!("{}: {e}", root.display()))?;
    if std::fs::symlink_metadata(root.join("pack.yaml")).is_err() {
        write_note(
            root,
            Path::new("pack.yaml"),
            "# Your own notes about your data, drafted by Columbus and filled in by you.\n\
             name: My notes\nformat_version: 1\n",
        )?;
    }
    Ok(())
}

/// A YAML string: JSON-quoted, with control and line-break characters
/// (C0, DEL, C1, U+2028, U+2029) taken out first, so text from a database
/// can never end a line, a comment or the front matter.
fn yaml_str(s: &str) -> String {
    let clean: String = s
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, '\u{2028}' | '\u{2029}'))
        .collect();
    serde_json::to_string(&clean).unwrap_or_else(|_| "\"\"".into())
}

/// A name from a database that can be written down as it is: no control or
/// line-break characters, not empty, not absurdly long.
fn name_ok(s: &str) -> bool {
    !s.is_empty()
        && s.chars().count() <= 128
        && !s
            .chars()
            .any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}'))
}

fn short_hash(s: &str) -> String {
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    h.finalize()
        .iter()
        .take(3)
        .map(|b| format!("{b:02x}"))
        .collect()
}

impl Gateway {
    /// How many codes a field's code list may hold under the rules in force:
    /// none unless codes are on, the assistant could read rows anyway (a row
    /// tool allowed) and a call may return rows; then at most that many.
    pub(crate) fn codes_cap(&self, access: &EffectiveAccess) -> Option<u64> {
        let p = self.state.policy();
        let row_tools = ["sql_select", "query_features", "sample_rows"]
            .iter()
            .any(|t| p.tool_allowed(t));
        (p.notes.values && row_tools && access.return_rows > 0)
            .then(|| (MAX_CODES as u64).min(access.return_rows))
    }
}

/// What decided a draft's facts: the filters on the dataset, the fields
/// hidden, and whether codes were listed. When the rules in force differ,
/// the counts and codes in the note are no longer shown.
fn drafted_under(access: &EffectiveAccess, ds: &str, codes: Option<u64>) -> String {
    let mut hidden = access.hide_fields.clone();
    hidden.sort();
    let v = json!([access.filters_for(ds), hidden, codes]);
    short_hash(&v.to_string()) + &short_hash(&format!("{v}#"))
}

/// The card a note file holds (front matter, or the whole YAML file).
fn card_of(path: &Path) -> Option<serde_yaml::Mapping> {
    let text = std::fs::read_to_string(path).ok()?;
    let yaml = if path.extension().and_then(|x| x.to_str()) == Some("md") {
        crate::context::front_matter(&text)?.0.to_string()
    } else {
        text
    };
    serde_yaml::from_str(&yaml).ok()
}

/// The file a note goes in: the plain name, unless a note about something
/// else already has it (two names with the same slug), then the name with a
/// short hash of the exact names.
fn note_path(
    root: &Path,
    plain: PathBuf,
    hashed: PathBuf,
    want: &[(&str, &str)],
) -> Option<PathBuf> {
    let ours = |rel: &Path| {
        let path = root.join(rel);
        !path.exists()
            || card_of(&path).is_some_and(|c| {
                want.iter().all(|(k, v)| {
                    c.get(serde_yaml::Value::from(*k))
                        .and_then(|x| x.as_str())
                        .is_some_and(|x| x == *v)
                })
            })
    };
    if ours(&plain) {
        Some(plain)
    } else if ours(&hashed) {
        Some(hashed)
    } else {
        None
    }
}

/// The written note reads back as the note it should be: its own id and
/// kind, about this connection and dataset (and field). Anything else (text
/// from the database that changed its structure) is removed, not kept.
fn reads_back(root: &Path, rel: &Path, want: &[(&str, &str)]) -> Result<(), String> {
    let ok = card_of(&root.join(rel)).is_some_and(|c| {
        want.iter().all(|(k, v)| {
            c.get(serde_yaml::Value::from(*k))
                .and_then(|x| x.as_str())
                .is_some_and(|x| x == *v)
        })
    });
    if ok {
        Ok(())
    } else {
        let _ = std::fs::remove_file(root.join(rel));
        Err(format!(
            "{}: did not read back as written; left out",
            rel.display()
        ))
    }
}

/// Short text-like or integer fields are worth counting; ids, geometry,
/// dates and measures are not.
fn profilable(name: &str, kind: &str) -> bool {
    let t = kind.to_ascii_lowercase();
    let n = name.to_ascii_lowercase();
    let idish = [
        "id", "guid", "globalid", "objectid", "oid", "fid", "gid", "uuid", "shape",
    ]
    .iter()
    .any(|w| n == *w || n.ends_with(&format!("_{w}")));
    if idish {
        return false;
    }
    let texty = ["char", "text", "string", "varchar", "nvarchar", "clob"]
        .iter()
        .any(|k| t.contains(k))
        && !t.contains("clob");
    let inty = [
        "int", "smallint", "tinyint", "integer", "number(1", "number(2", "number(3", "bool",
    ]
    .iter()
    .any(|k| t.contains(k))
        && !t.contains("bigint");
    texty || inty
}

/// What a draft wrote, for the answer and the audit line.
#[derive(Default)]
pub(crate) struct Drafted {
    pub written: Vec<String>,
    pub kept: Vec<String>,
    pub problems: Vec<String>,
    pub folder: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DraftArgs {
    /// The connection id (see list_connections).
    pub connection: String,
    /// Datasets to draft notes for (as list_datasets names them); all the
    /// connection allows when left out (at most 200).
    #[serde(default)]
    pub datasets: Option<Vec<String>>,
    /// Redraft notes Columbus drafted before and no one has edited since.
    /// Notes anyone edited, or marked reviewed, are never replaced.
    #[serde(default)]
    pub refresh: bool,
    /// The approval the person gave on the approvals page, if asked there.
    #[serde(default)]
    pub approval_id: Option<String>,
}

/// One note's changes.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct NoteChange {
    /// The note's id (from context_search or draft_notes).
    pub id: String,
    /// A better title.
    #[serde(default)]
    pub title: Option<String>,
    /// What it is, in at most two sentences.
    #[serde(default)]
    pub summary: Option<String>,
    /// Text to add at the end of the note's body (Markdown): what it is for,
    /// how to read it, gotchas, where the information came from.
    #[serde(default)]
    pub add_to_body: Option<String>,
    /// A dataset's fields: name -> {alias?, note?}.
    #[serde(default)]
    pub fields: Option<BTreeMap<String, FieldUpdate>>,
    /// A code list's labels: code -> label.
    #[serde(default)]
    pub labels: Option<BTreeMap<String, String>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct UpdateArgs {
    /// Every change, to as many notes as needed, in one call: the person
    /// approves them together.
    #[serde(default)]
    pub changes: Vec<NoteChange>,
    /// One note's changes, as an alternative to `changes`.
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub add_to_body: Option<String>,
    #[serde(default)]
    pub fields: Option<BTreeMap<String, FieldUpdate>>,
    #[serde(default)]
    pub labels: Option<BTreeMap<String, String>>,
    /// The approval the person gave on the approvals page, if asked there.
    #[serde(default)]
    pub approval_id: Option<String>,
}

/// The most text one approval of note changes may show the person.
const MAX_NOTES_SUMMARY: usize = 20_000;
/// Most notes one update_notes call changes.
const MAX_NOTE_CHANGES: usize = 200;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct FieldUpdate {
    #[serde(default)]
    pub alias: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
}

impl Gateway {
    /// Draft notes for `datasets` (all allowed when None) of a connection.
    /// The caller has already decided this may happen (the person's
    /// approval, or the person's own click in the app).
    pub(crate) async fn draft_notes_for(
        &self,
        conn_id: &str,
        datasets: Option<&[String]>,
        refresh: bool,
        max_datasets: usize,
    ) -> Result<Drafted, String> {
        let generation = self.state.generation();
        let (access, conn) = self.access(conn_id).map_err(|e| e.to_string())?;
        // What is drafted is read from the connection, and its data labels
        // the session like any other read (flow rules apply).
        self.note_read(&conn.id, None, None, None, "draft_notes");
        if !Self::engine_ready(&conn.kind) || crate::raster_tools::is_rasters(&conn.kind) {
            return Err(format!(
                "notes can be drafted for databases and GeoPackages, not {} connections",
                conn.kind
            ));
        }
        let timeout = access.limits.seconds.unwrap_or(120);
        let handle = self.handle(&conn)?;
        let listed = self
            .state
            .worker
            .core(
                "list_datasets",
                json!({"connection": handle, "timeout_s": timeout, "schemas": Self::listed_schemas(&conn)}),
                Duration::from_secs(timeout + 10),
            )
            .await
            .map_err(|e| e.to_string())?;
        let mut names: Vec<String> = listed["datasets"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|d| d["name"].as_str().map(String::from))
            .filter(|n| access.dataset_allowed(n))
            .collect();
        if let Some(want) = datasets {
            let want: BTreeSet<String> = want.iter().map(|w| w.to_ascii_lowercase()).collect();
            names.retain(|n| want.contains(&n.to_ascii_lowercase()));
            if names.is_empty() {
                return Err("none of those datasets is on this connection and allowed".into());
            }
        }
        if names.len() > max_datasets {
            return Err(format!(
                "this connection allows {} datasets; draft at most {max_datasets} at a time (name them in datasets)",
                names.len()
            ));
        }
        let root = notes_root(&self.state.paths)?;
        ensure_pack(&root)?;
        // Checked again now that it exists, as the file system resolves it.
        let root = notes_root(&self.state.paths)?;
        // Codes only where the assistant could read rows anyway, and no more
        // of them than a call may return.
        let codes_cap = self.codes_cap(&access);
        // Ids the notes already use, and whose they are: a name clash never
        // replaces another dataset's note.
        let mut owners: BTreeMap<String, (String, String, String)> = BTreeMap::new();
        if let Ok(p) = Pack::load(&root) {
            for e in &p.entries {
                owners.insert(
                    e.id.clone(),
                    (
                        e.str_field("connection").unwrap_or_default(),
                        e.str_field("physical")
                            .or_else(|| e.str_field("dataset"))
                            .unwrap_or_default(),
                        e.str_field("field").unwrap_or_default(),
                    ),
                );
            }
        }
        let mut claim = |id: &str, owner: (String, String, String)| -> bool {
            match owners.get(id) {
                Some(o) if *o != owner => false,
                _ => {
                    owners.insert(id.to_string(), owner);
                    true
                }
            }
        };
        let mut out = Drafted {
            folder: root.display().to_string(),
            ..Default::default()
        };
        let today = chrono::Local::now().date_naive().to_string();
        let cslug = slug(&conn.id);
        // Relationships pointing at each dataset, for "referenced by".
        let mut drafts: Vec<(String, Value, Value)> = Vec::new();
        for ds in &names {
            if !name_ok(ds) {
                out.problems
                    .push("a dataset whose name cannot be written down was left out".into());
                continue;
            }
            // The rules changed while drafting: stop, rather than write some
            // notes under one set of rules and some under another.
            if self.state.generation() != generation {
                return Err(
                    "Columbus's settings changed during the draft, so it stopped; draft again"
                        .into(),
                );
            }
            match self.dataset_facts(&access, &conn, ds, codes_cap).await {
                Ok((info, cat)) => drafts.push((ds.clone(), info, cat)),
                Err(e) => out.problems.push(format!("{ds}: {e}")),
            }
        }
        let mut incoming: BTreeMap<String, Vec<Value>> = BTreeMap::new();
        for (ds, _, cat) in &drafts {
            for fk in cat["foreign_keys"].as_array().into_iter().flatten() {
                if let Some(t) = fk["references"]["dataset"].as_str() {
                    incoming
                        .entry(t.to_ascii_lowercase())
                        .or_default()
                        .push(json!({"dataset": ds, "columns": fk["columns"]}));
                }
            }
        }
        for (ds, info, cat) in &drafts {
            let under = drafted_under(&access, ds, codes_cap);
            let want = [("connection", conn.id.as_str()), ("physical", ds.as_str())];
            let hash = short_hash(&format!("{}\u{0}{ds}", conn.id));
            let Some(rel) = note_path(
                &root,
                PathBuf::from("datasets")
                    .join(&cslug)
                    .join(format!("{}.md", slug(ds))),
                PathBuf::from("datasets")
                    .join(&cslug)
                    .join(format!("{}-{hash}.md", slug(ds))),
                &want,
            ) else {
                out.problems.push(format!(
                    "{ds}: another dataset's note has its name; left out"
                ));
                continue;
            };
            let dslug = rel
                .file_stem()
                .and_then(|x| x.to_str())
                .unwrap_or("x")
                .to_string();
            // `--` between parts: a slug never holds two dashes together.
            let id = format!("{cslug}--{dslug}");
            if !claim(&id, (conn.id.clone(), ds.clone(), String::new())) {
                out.problems
                    .push(format!("{ds}: another note has the id {id}; left out"));
                continue;
            }
            let path = root.join(&rel);
            if path.exists() && !(refresh && untouched_draft(&path)) {
                out.kept.push(id);
                continue;
            }
            // Code lists first: the dataset entry links to them.
            let mut domains: BTreeMap<String, String> = BTreeMap::new();
            // The GeoPackage's own code lists, as codes are: only when codes
            // are listed at all, coded ones, and no longer than the cap.
            let catalog_domains: serde_json::Map<String, Value> = cat["domains"]
                .as_object()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter(|(_, d)| {
                    d["type"] == "coded"
                        && codes_cap.is_some_and(|cap| {
                            d["values"]
                                .as_object()
                                .is_some_and(|v| v.len() as u64 <= cap)
                        })
                })
                .collect();
            let mut fields_with_values: BTreeMap<String, Vec<(String, u64)>> = BTreeMap::new();
            let cap = codes_cap.unwrap_or(0) as usize;
            for (field, v) in cat["values"].as_object().into_iter().flatten() {
                let Some(rows) = v.as_array() else { continue };
                if rows.len() > cap || !name_ok(field) {
                    continue;
                }
                let mut codes = Vec::new();
                let mut ok = true;
                for r in rows {
                    let code = match &r[0] {
                        Value::Null => continue,
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    if code.chars().count() > MAX_CODE_CHARS || !name_ok(&code) {
                        ok = false;
                        break;
                    }
                    codes.push((code, r[1].as_u64().unwrap_or(0)));
                }
                if ok && !codes.is_empty() {
                    fields_with_values.insert(field.clone(), codes);
                }
            }
            let mut domain_fields: BTreeSet<String> = fields_with_values.keys().cloned().collect();
            domain_fields.extend(catalog_domains.keys().filter(|k| name_ok(k)).cloned());
            for field in &domain_fields {
                let dwant = [
                    ("connection", conn.id.as_str()),
                    ("dataset", ds.as_str()),
                    ("field", field.as_str()),
                ];
                let fslug = slug(field);
                let fhash = short_hash(&format!("{}\u{0}{ds}\u{0}{field}", conn.id));
                let Some(drel) = note_path(
                    &root,
                    PathBuf::from("domains")
                        .join(&cslug)
                        .join(format!("{dslug}--{fslug}.yaml")),
                    PathBuf::from("domains")
                        .join(&cslug)
                        .join(format!("{dslug}--{fslug}-{fhash}.yaml")),
                    &dwant,
                ) else {
                    out.problems.push(format!(
                        "{ds}.{field}: another code list has its name; left out"
                    ));
                    continue;
                };
                let fpart = drel
                    .file_stem()
                    .and_then(|x| x.to_str())
                    .unwrap_or("x")
                    .trim_start_matches(&format!("{dslug}--"))
                    .to_string();
                let did = format!("{id}--{fpart}");
                if !claim(&did, (conn.id.clone(), ds.clone(), field.clone())) {
                    out.problems.push(format!(
                        "{ds}.{field}: another note has the id {did}; left out"
                    ));
                    continue;
                }
                let dpath = root.join(&drel);
                domains.insert(field.clone(), did.clone());
                if dpath.exists() && !(refresh && untouched_draft(&dpath)) {
                    out.kept.push(did);
                    continue;
                }
                let labelled = catalog_domains.get(field);
                let mut text = format!(
                    "# Drafted by Columbus on {today}: the codes one field holds{}.\n\
                     # Add what each code means; set status: reviewed when it is right.\n\
                     id: {did}\nkind: domain\ntitle: {}\nsummary: {}\nconnection: {}\ndataset: {}\nfield: {}\ntype: coded\ndrafted_under: {under}\nvalues:\n",
                    if labelled.is_some() { ", with the labels the GeoPackage gives them" } else { ", with how many rows have each" },
                    yaml_str(&format!("{field} codes in {ds}")),
                    yaml_str(&format!("The values {field} holds in {ds}.")),
                    yaml_str(&conn.id),
                    yaml_str(ds),
                    yaml_str(field),
                );
                let mut counts: BTreeMap<String, u64> = BTreeMap::new();
                for (c, n) in fields_with_values.get(field).into_iter().flatten() {
                    counts.insert(c.clone(), *n);
                }
                let mut codes: Vec<String> = counts.keys().cloned().collect();
                if let Some(d) = labelled.and_then(|d| d["values"].as_object()) {
                    for k in d.keys() {
                        if !codes.contains(k) {
                            codes.push(k.clone());
                        }
                    }
                }
                for code in &codes {
                    let label = labelled
                        .and_then(|d| d["values"][code].as_str())
                        .filter(|s| !s.is_empty())
                        .map(String::from)
                        .unwrap_or_default();
                    text.push_str(&format!("  {}: {}\n", yaml_str(code), yaml_str(&label)));
                }
                if !counts.is_empty() {
                    text.push_str("rows_with:\n");
                    for (c, n) in &counts {
                        text.push_str(&format!("  {}: {n}\n", yaml_str(c)));
                    }
                }
                text.push_str(&format!(
                    "owner: {}\nlast_confirmed: {today}\nstatus: draft\ndrafted_by: {}\ndraft_sha256:\n",
                    yaml_str(&self.state.user),
                    yaml_str(&format!("columbus {}", env!("CARGO_PKG_VERSION")))
                ));
                let mut dwant_id = dwant.to_vec();
                dwant_id.push(("id", did.as_str()));
                dwant_id.push(("kind", "domain"));
                match write_note(&root, &drel, &with_hash(&text))
                    .and_then(|()| reads_back(&root, &drel, &dwant_id))
                {
                    Ok(()) => out.written.push(did),
                    Err(e) => {
                        domains.remove(field);
                        out.problems.push(e)
                    }
                }
            }
            let text = dataset_note(
                &id,
                &conn,
                ds,
                info,
                cat,
                &domains,
                incoming.get(&ds.to_ascii_lowercase()),
                &self.state.user,
                &today,
                &under,
            );
            let mut want_id = want.to_vec();
            want_id.push(("id", id.as_str()));
            want_id.push(("kind", "dataset"));
            match write_note(&root, &rel, &with_hash(&text))
                .and_then(|()| reads_back(&root, &rel, &want_id))
            {
                Ok(()) => out.written.push(id),
                Err(e) => out.problems.push(e),
            }
        }
        Ok(out)
    }

    /// One dataset's facts: describe_dataset's answer (hidden fields already
    /// left out, the row count under the row filter) and the catalog's keys,
    /// relationships, indexes, comments and codes, with anything the
    /// connection does not allow taken out.
    async fn dataset_facts(
        &self,
        access: &EffectiveAccess,
        conn: &Connection,
        ds: &str,
        codes_cap: Option<u64>,
    ) -> Result<(Value, Value), String> {
        let mut info = self.dataset_info(access, conn, ds, false).await?;
        if let Some(f) = info["fields"].as_array_mut() {
            f.retain(|f| f["name"].as_str().is_some_and(name_ok));
        }
        let dialect = SqlDialect::for_connection_type(&conn.kind);
        let mut value_sqls = Vec::new();
        let rows = info["rows"].as_u64().unwrap_or(u64::MAX);
        if let Some(cap) = codes_cap.filter(|_| rows <= MAX_PROFILED_ROWS) {
            let cols = self.table_columns(access, conn).await?;
            let from = crate::gis_tools::dataset_sql(ds, dialect)?;
            let geom = info["geometry_column"].as_str().unwrap_or("");
            let mut n = 0;
            for f in info["fields"].as_array().into_iter().flatten() {
                let (Some(name), kind) = (f["name"].as_str(), f["type"].as_str().unwrap_or(""))
                else {
                    continue;
                };
                if n >= MAX_PROFILED_FIELDS || name.eq_ignore_ascii_case(geom) || f["pk"] == true {
                    continue;
                }
                if access.field_hidden(name) || !profilable(name, kind) {
                    continue;
                }
                let q = crate::gis_tools::quote(name, dialect);
                let sql = format!("SELECT {q} AS v, COUNT(*) AS n FROM {from} GROUP BY {q} ORDER BY COUNT(*) DESC");
                match check_select(&sql, access, cap + 1, &cols) {
                    Ok(c) => {
                        value_sqls
                            .push(json!({"field": name, "sql": c.sql, "max_rows": c.max_rows}));
                        n += 1;
                    }
                    Err(_) => continue,
                }
            }
        }
        let timeout = access.limits.seconds.unwrap_or(120);
        let mut cat = self
            .state
            .worker
            .core(
                "catalog_details",
                json!({"connection": self.handle(conn)?, "dataset": ds, "timeout_s": timeout, "value_sqls": value_sqls}),
                Duration::from_secs(timeout * (1 + value_sqls.len() as u64).min(10) + 10),
            )
            .await
            .map_err(|e| e.to_string())?;
        // Nothing hidden, and no name of a table this connection refuses.
        let hidden = |c: &Value| c.as_str().is_some_and(|n| access.field_hidden(n));
        if let Some(pk) = cat["primary_key"].as_array_mut() {
            pk.retain(|c| !hidden(c));
        }
        if let Some(fks) = cat["foreign_keys"].as_array_mut() {
            fks.retain(|fk| !fk["columns"].as_array().into_iter().flatten().any(hidden));
            for fk in fks.iter_mut() {
                if let Some(t) = fk["references"]["columns"].as_array_mut() {
                    t.retain(|c| !hidden(c));
                }
                let target = fk["references"]["dataset"]
                    .as_str()
                    .unwrap_or("")
                    .to_string();
                if !access.dataset_allowed(&target) {
                    fk["references"] = json!({"dataset": null, "columns": []});
                }
            }
        }
        if let Some(ix) = cat["indexes"].as_array_mut() {
            ix.retain(|i| !i["columns"].as_array().into_iter().flatten().any(hidden));
        }
        if let Some(m) = cat["comments"]["columns"].as_object_mut() {
            m.retain(|k, _| !access.field_hidden(k));
        }
        for key in ["domains", "values"] {
            if let Some(m) = cat[key].as_object_mut() {
                m.retain(|k, _| !access.field_hidden(k));
            }
        }
        Ok((info, cat))
    }

    /// The person's notes as a pack, if there are any.
    pub(crate) fn notes_pack(&self) -> Option<Pack> {
        let root = notes_root(&self.state.paths).ok()?;
        let mut pack = root
            .join("pack.yaml")
            .is_file()
            .then(|| Pack::load(&root).ok())
            .flatten()?;
        // Two notes with one id (a copied file, say): neither is served, so
        // what is shown and what its reader takes on can never differ.
        let mut count: BTreeMap<String, usize> = BTreeMap::new();
        for e in &pack.entries {
            *count.entry(e.id.clone()).or_default() += 1;
        }
        pack.entries.retain(|e| count.get(&e.id) == Some(&1));
        Some(pack)
    }

    /// A note as the rules in force now let it be shown: None when it is
    /// about a connection that is gone or a dataset no longer allowed, and
    /// without fields now hidden.
    pub(crate) fn visible_note(&self, e: &Entry) -> Option<Entry> {
        let key = |k: &str| serde_yaml::Value::from(k);
        let Some(conn_id) = e.str_field("connection") else {
            // A connection that is there but not plain text: fail closed.
            if e.card.contains_key(key("connection")) {
                return None;
            }
            return Some(e.clone());
        };
        let (access, _) = self.access(&conn_id).ok()?;
        let dataset = e.str_field("physical").or_else(|| e.str_field("dataset"))?;
        if !access.dataset_allowed(&dataset) {
            return None;
        }
        if let Some(f) = e.str_field("field") {
            if access.field_hidden(&f) {
                return None;
            }
        }
        let mut e = e.clone();
        // Drafted under other rules (or before notes recorded them): its
        // counts and codes may come from rows or fields the rules no longer
        // allow, so they are not shown; text in it was written then too.
        let drafted = [
            "drafted_by",
            "drafted_under",
            "edited_by",
            "rows",
            "rows_with",
        ]
        .iter()
        .any(|k| e.card.contains_key(key(k)));
        if drafted {
            let now = drafted_under(&access, &dataset, self.codes_cap(&access));
            if e.str_field("drafted_under").as_deref() != Some(now.as_str()) {
                if e.kind == "domain" {
                    return None;
                }
                for k in ["rows", "referenced_by"] {
                    e.card.remove(key(k));
                }
                // Which fields had a code list says how few values they had.
                if let Some(serde_yaml::Value::Mapping(m)) = e.card.get_mut(key("fields")) {
                    for (_, f) in m.iter_mut() {
                        if let serde_yaml::Value::Mapping(f) = f {
                            f.remove(key("domain"));
                        }
                    }
                }
                e.card.insert(
                    key("redraft"),
                    key("The rules for this connection changed since this note was drafted: its counts and code lists are not shown, and its text was written under the earlier rules, so confirm it with the person. If no one has edited it, drafting again (refresh) brings it up to date."),
                );
            }
        }
        e.card.remove(key("drafted_under"));
        e.card.remove(key("carries"));
        let hidden = |v: &serde_yaml::Value| v.as_str().is_some_and(|n| access.field_hidden(n));
        if let Some(serde_yaml::Value::Mapping(m)) = e.card.get_mut(key("fields")) {
            m.retain(|k, _| !hidden(k));
        }
        if let Some(serde_yaml::Value::Sequence(s)) = e.card.get_mut(key("keys")) {
            s.retain(|k| !hidden(k));
        }
        e.card.remove(key("links"));
        for list in ["relationships", "indexes", "referenced_by"] {
            if let Some(serde_yaml::Value::Sequence(s)) = e.card.get_mut(key(list)) {
                s.retain(|r| {
                    let cols = r
                        .get("columns")
                        .and_then(|c| c.as_sequence())
                        .cloned()
                        .unwrap_or_default();
                    let target = r
                        .get("references")
                        .and_then(|t| t.get("dataset"))
                        .or_else(|| r.get("dataset"))
                        .and_then(|d| d.as_str())
                        .map(String::from);
                    !cols.iter().any(hidden) && target.is_none_or(|t| access.dataset_allowed(&t))
                });
                for r in s.iter_mut() {
                    if let Some(serde_yaml::Value::Sequence(t)) =
                        r.get_mut("references").and_then(|t| t.get_mut("columns"))
                    {
                        t.retain(|c| !hidden(c));
                    }
                }
            }
        }
        Some(e)
    }

    /// A note about a connection was served: its data labels the session,
    /// as reading the connection itself would (flow rules apply).
    pub(crate) fn note_served(&self, e: &Entry) {
        if let Some(c) = e.str_field("connection") {
            self.state.flow.add(columbus_flow::connection_label(&c));
        }
        // And what was written into it from other data.
        if let Some(serde_yaml::Value::Sequence(l)) = e.card.get(serde_yaml::Value::from("carries"))
        {
            for x in l.iter().filter_map(|x| x.as_str()) {
                if x.starts_with("connection:") || x.starts_with("server:") {
                    self.state.flow.add(x.to_string());
                }
            }
        }
    }
}

/// The text of a drafted dataset note.
#[allow(clippy::too_many_arguments)]
fn dataset_note(
    id: &str,
    conn: &Connection,
    ds: &str,
    info: &Value,
    cat: &Value,
    domains: &BTreeMap<String, String>,
    incoming: Option<&Vec<Value>>,
    owner: &str,
    today: &str,
    under: &str,
) -> String {
    let comment = cat["comments"]["table"].as_str();
    let rows = info["rows"].as_u64();
    let geom = info["geometry_type"].as_str();
    let crs = info["crs"]["code"].as_str();
    let mut summary = String::new();
    if let Some(c) = comment {
        summary.push_str(c.lines().next().unwrap_or(c));
    } else {
        summary.push_str(&format!(
            "A table{}{}.",
            geom.map(|g| format!(" of {g}")).unwrap_or_default(),
            crs.map(|c| format!(" in {c}")).unwrap_or_default(),
        ));
        summary.push_str(" What it is for is not written down yet.");
    }
    let mut t = format!(
        "---\n# Drafted by Columbus on {today}: facts from the database only.\n\
         # Fill in what it means (or ask your assistant to, from your documents), then set status: reviewed.\n\
         id: {id}\nkind: dataset\ntitle: {}\nsummary: {}\nconnection: {}\nphysical: {}\ndrafted_under: {under}\n",
        yaml_str(ds),
        yaml_str(&summary),
        yaml_str(&conn.id),
        yaml_str(ds),
    );
    if let Some(g) = geom {
        t.push_str(&format!("geometry: {}\n", yaml_str(g)));
    }
    if let Some(c) = crs {
        t.push_str(&format!("crs: {}\n", yaml_str(c)));
    }
    if let Some(r) = rows {
        t.push_str(&format!(
            "rows: {r}   # on {today}, within the connection's row filter\n"
        ));
    }
    let pk: Vec<String> = cat["primary_key"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().map(yaml_str))
        .collect();
    if !pk.is_empty() {
        t.push_str(&format!("keys: [{}]\n", pk.join(", ")));
    }
    let comments = cat["comments"]["columns"].as_object();
    t.push_str("fields:\n");
    for f in info["fields"].as_array().into_iter().flatten() {
        let Some(name) = f["name"].as_str() else {
            continue;
        };
        let mut parts = Vec::new();
        if let Some(ty) = f["type"].as_str() {
            parts.push(format!("type: {}", yaml_str(ty)));
        }
        if let Some(d) = domains.get(name) {
            parts.push(format!("domain: {d}"));
        }
        if let Some(c) = comments.and_then(|m| m.get(name)).and_then(Value::as_str) {
            parts.push(format!("note: {}", yaml_str(c)));
        }
        if f["not_null"] == true {
            parts.push("required: true".into());
        }
        t.push_str(&format!(
            "  {}: {{ {} }}\n",
            yaml_str(name),
            parts.join(", ")
        ));
    }
    let fks: Vec<&Value> = cat["foreign_keys"]
        .as_array()
        .into_iter()
        .flatten()
        .collect();
    if !fks.is_empty() {
        t.push_str("relationships:\n");
        for fk in fks {
            let cols: Vec<String> = fk["columns"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|c| c.as_str().map(yaml_str))
                .collect();
            let target = fk["references"]["dataset"].as_str();
            let tcols: Vec<String> = fk["references"]["columns"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|c| c.as_str().map(yaml_str))
                .collect();
            t.push_str(&format!(
                "  - {{ columns: [{}], references: {{ dataset: {}, columns: [{}] }} }}\n",
                cols.join(", "),
                target.map(yaml_str).unwrap_or_else(|| "null".into()),
                tcols.join(", ")
            ));
        }
    }
    let ix: Vec<&Value> = cat["indexes"].as_array().into_iter().flatten().collect();
    if !ix.is_empty() {
        t.push_str("indexes:\n");
        for i in ix {
            let cols: Vec<String> = i["columns"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|c| c.as_str().map(yaml_str))
                .collect();
            t.push_str(&format!(
                "  - {{ columns: [{}], unique: {} }}\n",
                cols.join(", "),
                i["unique"] == true
            ));
        }
    }
    if let Some(inc) = incoming.filter(|i| !i.is_empty()) {
        t.push_str("referenced_by:\n");
        for r in inc {
            let cols: Vec<String> = r["columns"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|c| c.as_str().map(yaml_str))
                .collect();
            t.push_str(&format!(
                "  - {{ dataset: {}, columns: [{}] }}\n",
                yaml_str(r["dataset"].as_str().unwrap_or("")),
                cols.join(", ")
            ));
        }
    }
    t.push_str(&format!(
        "owner: {}\nlast_confirmed: {today}\nstatus: draft\ndrafted_by: {}\ndraft_sha256:\n---\n\n",
        yaml_str(owner),
        yaml_str(&format!("columbus {}", env!("CARGO_PKG_VERSION")))
    ));
    t.push_str(&format!("# {}\n\n", ds.replace(['\n', '\r'], " ")));
    if let Some(c) = comment {
        t.push_str(&format!(
            "The database describes it as: {}\n\n",
            c.replace("\n---", "\n- - -")
        ));
    }
    t.push_str(
        "## To fill in\n\n\
         - What this dataset is for, who maintains it, and how often it changes.\n\
         - What each field means, its units, and what its codes stand for (in the linked code lists).\n\
         - How to read it: the filters people always apply, how it joins to other data, its gotchas.\n",
    );
    t
}

#[tool_router(router = notes_router, vis = "pub(crate)")]
impl Gateway {
    #[tool(
        description = "Draft notes about a connection's datasets from the database itself, into the person's own notes (read by context_search and context_get, and attached by describe_dataset): each dataset's fields and types, geometry and CRS, row count, primary key, relationships to other tables, indexes, the comments written in the database, and for short code fields the codes they hold (as code lists to label). Facts only, all marked draft; nothing hidden or refused is written. Then help the person say what the data means: ask them, or read the documents they give you, and record it with update_notes. Needs the person's approval; writes nothing to the database.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn draft_notes(
        &self,
        Parameters(args): Parameters<DraftArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<Json<Output<Value>>, String> {
        let request = json!({"connection": args.connection, "datasets": args.datasets, "refresh": args.refresh});
        let mut rec = self.gate(&ctx, "draft_notes", &request)?;
        rec.connection = Some(args.connection.clone());
        if let Err(e) = self.access(&args.connection) {
            return Err(self.deny(&ctx, rec, e));
        }
        let folder = match notes_root(&self.state.paths) {
            Ok(f) => f,
            Err(e) => return Err(self.deny(&ctx, rec, e)),
        };
        let what = match &args.datasets {
            Some(d) => format!("{} dataset(s)", d.len()),
            None => "every dataset it allows".into(),
        };
        let summary = format!(
            "Write draft notes about {what} on {} to your notes folder ({}): field names and types, row counts, keys, relationships, indexes, the database's own comments{}. Fields your organization hides, and tables this connection refuses, are left out.",
            args.connection,
            folder.display(),
            if self.access(&args.connection).ok().and_then(|(a, _)| self.codes_cap(&a)).is_some() {
                ", and the codes of each short field"
            } else {
                ""
            }
        );
        let ask = Ask {
            tool: "draft_notes",
            label: "drafting notes",
            request: &request,
            risk: Risk::Export,
            summary,
            detail: Some(json!({"arguments": request, "destination": folder})),
            approval_id: args.approval_id.as_deref(),
            every_time: false,
            code: None,
            standing: None,
            always_covers: None,
        };
        match self.seek_approval(&ctx, ask).await {
            Ok(Sought::Granted(id)) => rec.approval = Some(id),
            Ok(Sought::Declined(m)) => return Err(self.deny(&ctx, rec, m)),
            Ok(Sought::Pending(needed)) => {
                rec.approval = Some(needed.id.clone());
                self.audit(
                    &ctx,
                    AuditRecord {
                        decision: "approval_required".into(),
                        ..rec
                    },
                )?;
                return Ok(Json(Output {
                    result: json!({"status": "approval_required", "approval": needed}),
                    checks: vec![],
                    provenance: None,
                }));
            }
            Err(e) => return Err(self.deny(&ctx, rec, e)),
        }
        let d = match self
            .draft_notes_for(
                &args.connection,
                args.datasets.as_deref(),
                args.refresh,
                MAX_DATASETS,
            )
            .await
        {
            Ok(d) => d,
            Err(e) => return Err(self.deny(&ctx, rec, e)),
        };
        rec.reason = Some(format!(
            "{} note(s) written, {} kept as they were, to {}",
            d.written.len(),
            d.kept.len(),
            d.folder
        ));
        self.audit(&ctx, rec)?;
        let mut checks = vec![Check {
            id: "notes.drafted".into(),
            severity: Severity::Info,
            message: format!(
                "Drafted {} note(s); {} already there were kept (edited or reviewed notes are never replaced)",
                d.written.len(),
                d.kept.len()
            ),
            detail: None,
            fix: Some("Ask the person what the datasets and codes mean, or read their documents, then record it with update_notes".into()),
        }];
        if !d.problems.is_empty() {
            checks.push(Check {
                id: "notes.problems".into(),
                severity: Severity::Warn,
                message: format!("{} dataset(s) could not be drafted", d.problems.len()),
                detail: Some(json!(d.problems)),
                fix: None,
            });
        }
        Ok(Json(Output {
            result: json!({"status": "drafted", "folder": d.folder, "written": d.written, "kept": d.kept}),
            checks,
            provenance: None,
        }))
    }

    #[tool(
        description = "Record what the person's data means in their notes (from context_search or draft_notes): better titles or summaries, field aliases and notes, labels for a code list's codes, or text added to a note (what it is for, how to read it, where the information came from: the person, or the document you read). Send every change in one call, as `changes` (one entry per note, up to 200): the person sees them all and approves them together, and may allow note changes for the rest of the session. Only the person's own notes about a connection can be changed, never the organization's pack, and a note stays a draft until the person marks it reviewed.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn update_notes(
        &self,
        Parameters(args): Parameters<UpdateArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<Json<Output<Value>>, String> {
        let mut changes = args.changes.clone();
        let loose = args.title.is_some()
            || args.summary.is_some()
            || args.add_to_body.is_some()
            || args.fields.is_some()
            || args.labels.is_some();
        if let Some(id) = &args.id {
            changes.push(NoteChange {
                id: id.clone(),
                title: args.title.clone(),
                summary: args.summary.clone(),
                add_to_body: args.add_to_body.clone(),
                fields: args.fields.clone(),
                labels: args.labels.clone(),
            });
        }
        let asked: Vec<Value> = changes.iter().map(change_json).collect();
        let rec = self.gate(&ctx, "update_notes", &json!({"changes": asked}))?;
        if loose && args.id.is_none() {
            return Err(self.deny(
                &ctx,
                rec,
                "title, summary, add_to_body, fields and labels belong in an entry of `changes`, with the note's id".to_string(),
            ));
        }
        if changes.is_empty() {
            return Err(self.deny(&ctx, rec, "nothing to change: give `changes`".to_string()));
        }
        if changes.len() > MAX_NOTE_CHANGES {
            return Err(self.deny(
                &ctx,
                rec,
                format!("at most {MAX_NOTE_CHANGES} notes in one call"),
            ));
        }
        let mut seen_ids = BTreeSet::new();
        for c in &changes {
            if !seen_ids.insert(c.id.clone()) {
                return Err(self.deny(
                    &ctx,
                    rec,
                    format!(
                        "{} appears twice: put all of a note's changes in one entry",
                        c.id
                    ),
                ));
            }
        }
        let Some(pack) = self.notes_pack() else {
            return Err(self.deny(
                &ctx,
                rec,
                "there are no notes yet: draft some with draft_notes".to_string(),
            ));
        };
        let Ok(root) = notes_root(&self.state.paths) else {
            return Err(self.deny(&ctx, rec, "the notes folder is not usable".to_string()));
        };
        // Everything is checked before the person is asked; nothing is
        // written unless all of it can be. Reading these notes adds their
        // labels first, so what the person is shown is what the session
        // holds once they are read.
        for c in &changes {
            if let Some(raw) = pack.get(&c.id) {
                if self.visible_note(raw).is_some() {
                    self.note_served(raw);
                }
            }
        }
        let session = self.state.flow.session_labels();
        let policy = self.state.policy();
        let mut rule_asks: BTreeSet<usize> = BTreeSet::new();
        let mut planned: Vec<Planned> = Vec::new();
        for c in &changes {
            match self.plan_change(&pack, &root, c, &session, &policy.flows, &ctx) {
                Ok((p, asks)) => {
                    if let Some(r) = asks {
                        rule_asks.insert(r);
                    }
                    planned.push(p);
                }
                Err(e) => return Err(self.deny(&ctx, rec, e)),
            }
        }
        // Where data this session read goes: into notes about which
        // connection, from where.
        let flows: BTreeSet<(String, String)> = planned
            .iter()
            .flat_map(|p| p.carries.iter().map(|l| (p.connection.clone(), l.clone())))
            .collect();
        let flow_text = |(into, from): &(String, String)| {
            format!(
                "data from {} into notes about {into}",
                from.trim_start_matches("connection:")
            )
        };
        let request = json!({
            "changes": asked,
            "files": planned.iter().map(|p| p.file_sha.clone()).collect::<Vec<_>>(),
            "flows": flows.iter().map(flow_text).collect::<Vec<_>>(),
        });
        // "Allow for this session" covers changes to the person's draft
        // notes, and each flow of data shown here; a later change that
        // takes data anywhere else asks again.
        const NOTES_SCOPE: &str = "the person's own notes";
        let scopes: Vec<Value> = std::iter::once(json!({"scope": NOTES_SCOPE}))
            .chain(
                flows
                    .iter()
                    .map(|(into, from)| json!({"scope": NOTES_SCOPE, "into": into, "from": from})),
            )
            .collect();
        let count = planned.iter().map(|p| p.lines.len()).sum::<usize>();
        let mut summary = format!(
            "{count} change{} to {} of your notes:\n",
            if count == 1 { "" } else { "s" },
            planned.len()
        );
        for p in &planned {
            summary.push_str(&format!("\n{}:\n", p.id));
            for l in &p.lines {
                // Every line of the AI's text indented, so it cannot pass
                // for the summary's own lines.
                summary.push_str(&format!("  {}\n", l.replace('\n', "\n    | ")));
            }
        }
        if summary.chars().count() > MAX_NOTES_SUMMARY {
            return Err(self.deny(
                &ctx,
                rec,
                format!("these changes are too long to show the person in one approval (more than {MAX_NOTES_SUMMARY} characters): send them in smaller batches"),
            ));
        }
        if !flows.is_empty() {
            summary.push_str(&format!(
                "\nThis session has read data that these notes will be treated as holding: {}.",
                flows.iter().map(flow_text).collect::<Vec<_>>().join("; ")
            ));
        }
        for r in &rule_asks {
            summary.push_str(&format!(
                "\nOrganization flow rule {} asks for your approval for this.",
                r + 1
            ));
        }
        let always_covers = format!(
            "\"Allow for this session\" lets the assistant make further changes to any of your draft notes, without asking, until its session ends{}. It asks again before a change would put data from anywhere else into your notes. Every change is recorded, and notes stay drafts until you mark them reviewed.",
            if flows.is_empty() {
                String::new()
            } else {
                format!(
                    ", including putting {}",
                    flows.iter().map(flow_text).collect::<Vec<_>>().join("; ")
                )
            }
        );
        let ask = Ask {
            tool: "update_notes",
            label: "changes to your notes",
            request: &request,
            risk: Risk::Notes,
            summary,
            detail: Some(
                json!({"changes": planned.iter().map(|p| json!({"id": p.id, "changes": p.lines})).collect::<Vec<_>>()}),
            ),
            approval_id: args.approval_id.as_deref(),
            every_time: policy.notes.ask_each_change || !rule_asks.is_empty(),
            code: None,
            standing: Some(&scopes),
            always_covers: Some(always_covers),
        };
        drop(policy);
        let mut rec = rec;
        match self.seek_approval(&ctx, ask).await {
            Ok(Sought::Granted(id)) => rec.approval = Some(id),
            Ok(Sought::Declined(m)) => return Err(self.deny(&ctx, rec, m)),
            Ok(Sought::Pending(needed)) => {
                rec.approval = Some(needed.id.clone());
                self.audit(
                    &ctx,
                    AuditRecord {
                        decision: "approval_required".into(),
                        ..rec
                    },
                )?;
                return Ok(Json(Output {
                    result: json!({"status": "approval_required", "approval": needed}),
                    checks: vec![],
                    provenance: None,
                }));
            }
            Err(e) => return Err(self.deny(&ctx, rec, e)),
        }
        // Any of them changed while the person decided: none is written.
        for p in &planned {
            if std::fs::read(root.join(&p.rel)).ok().as_deref() != Some(p.before.as_slice()) {
                return Err(self.deny(&ctx, rec, format!("{} changed while the person was deciding; nothing was written. Read the notes again and propose the changes again", p.id)));
            }
        }
        let mut written: Vec<&Planned> = Vec::new();
        for p in &planned {
            if let Err(e) = write_note(&root, &p.rel, &p.text) {
                // All or nothing: the notes already written go back as
                // they were.
                let mut kept = Vec::new();
                for w in &written {
                    if write_note(&root, &w.rel, &String::from_utf8_lossy(&w.before)).is_err() {
                        kept.push(w.id.clone());
                    }
                }
                let why = if kept.is_empty() {
                    format!("{}: {e}; nothing was changed", p.id)
                } else {
                    format!(
                        "{}: {e}; these could not be put back and keep the change: {}",
                        p.id,
                        kept.join(", ")
                    )
                };
                return Err(self.deny(&ctx, rec, why));
            }
            written.push(p);
        }
        let written: Vec<String> = written.iter().map(|p| p.id.clone()).collect();
        rec.reason = Some(format!(
            "{} note(s) changed: {}",
            written.len(),
            written.join(", ")
        ));
        self.audit(&ctx, rec)?;
        Ok(Json(Output {
            result: json!({
                "status": "updated",
                "notes": planned.iter().map(|p| json!({"id": p.id, "changes": p.lines})).collect::<Vec<_>>(),
            }),
            checks: vec![],
            provenance: None,
        }))
    }
}

/// One note's change, checked and ready to write.
struct Planned {
    id: String,
    /// The connection the note is about.
    connection: String,
    rel: PathBuf,
    before: Vec<u8>,
    file_sha: String,
    text: String,
    lines: Vec<String>,
    carries: BTreeSet<String>,
}

fn change_json(c: &NoteChange) -> Value {
    json!({
        "id": c.id, "title": c.title, "summary": c.summary, "add_to_body": c.add_to_body,
        "fields": c.fields.as_ref().map(|m| m.iter().map(|(k, v)| (k.clone(), json!({"alias": v.alias, "note": v.note}))).collect::<serde_json::Map<_, _>>()),
        "labels": c.labels,
    })
}

impl Gateway {
    /// Check one note's change against the rules and the file as it is now,
    /// and make the text to write; the flow rule asking for approval, if any.
    fn plan_change(
        &self,
        pack: &Pack,
        root: &Path,
        c: &NoteChange,
        session: &BTreeSet<String>,
        flows: &[columbus_shared::policy::FlowRule],
        ctx: &RequestContext<RoleServer>,
    ) -> Result<(Planned, Option<usize>), String> {
        let missing = || format!("no note {:?} among the person's notes", c.id);
        let raw = pack.get(&c.id).cloned().ok_or_else(missing)?;
        let visible = self.visible_note(&raw).ok_or_else(missing)?;
        // Only notes about a connection: what is written into one is that
        // connection's, and its readers carry its label.
        let conn_id = raw.str_field("connection").ok_or_else(|| {
            format!(
                "{} is not about a connection; only the person changes it",
                c.id
            )
        })?;
        if raw.str_field("status").as_deref() != Some("draft") {
            return Err(format!(
                "{} is marked {}: only the person changes it now",
                c.id,
                raw.str_field("status").unwrap_or_default()
            ));
        }
        let rel = raw
            .file
            .strip_prefix(root)
            .map(|r| r.to_path_buf())
            .map_err(|_| format!("{} is not in the notes folder", c.id))?;
        plain_place(root, &rel).map_err(|e| format!("{}: {e}", c.id))?;
        // The approval covers this exact file as it is now.
        let before =
            std::fs::read(&raw.file).map_err(|e| format!("{}: {e}", raw.file.display()))?;
        let file_sha = {
            let mut h = Sha256::new();
            h.update(&before);
            h.finalize()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        };
        // The note is the file as it is now, not as it was when the notes
        // were loaded: a change since then is not overwritten.
        let md = raw.file.extension().and_then(|x| x.to_str()) == Some("md");
        let (now_card, now_body) = {
            let text = String::from_utf8_lossy(&before).to_string();
            let (yaml, body) = if md {
                match crate::context::front_matter(&text) {
                    Some((y, b)) => (Some(y.to_string()), b.to_string()),
                    None => (None, String::new()),
                }
            } else {
                (Some(text.clone()), String::new())
            };
            (
                yaml.and_then(|y| serde_yaml::from_str::<serde_yaml::Mapping>(&y).ok()),
                body,
            )
        };
        if now_card.as_ref() != Some(&raw.card) || now_body != raw.body {
            return Err(format!(
                "{} changed just now; read it again and propose the change again",
                c.id
            ));
        }
        // What this session has read goes into the note with the words: a
        // flow rule that keeps that data from this connection applies, and
        // later readers of the note carry its labels.
        let mut labels = session.clone();
        labels.remove(&columbus_flow::connection_label(&conn_id));
        let decision = columbus_flow::evaluate_to(flows, &labels, &format!("connection:{conn_id}"));
        if let columbus_flow::FlowDecision::Deny { labels, rule } = &decision {
            return Err(format!(
                "organization flow rule {} forbids writing data from {} into notes about {conn_id} ({})",
                rule + 1,
                labels.join(", "),
                c.id
            ));
        }
        let asks = match &decision {
            columbus_flow::FlowDecision::Approve { rule: Some(r), .. } => Some(*r),
            _ => None,
        };
        // Checked on the note as the rules let it be seen (a field now hidden
        // cannot be named), then made to the file as it is, so nothing the
        // rules hide today is lost from it.
        let mut seen = visible.card.clone();
        let lines =
            apply_changes(&mut seen, &visible.kind, c).map_err(|e| format!("{}: {e}", c.id))?;
        if lines.is_empty() {
            return Err(format!("{}: nothing to change", c.id));
        }
        let mut card = raw.card.clone();
        apply_changes(&mut card, &raw.kind, c).map_err(|e| format!("{}: {e}", c.id))?;
        let mut body = raw.body.clone();
        if let Some(add) = &c.add_to_body {
            if raw.kind != "domain" {
                body = format!("{}\n\n{}\n", body.trim_end(), add.replace('\r', "").trim());
            }
        }
        let k = |s: &str| serde_yaml::Value::from(s);
        card.insert(
            k("edited_by"),
            k(&format!(
                "{} for {} on {}",
                crate::server::client_name(ctx),
                self.state.user,
                chrono::Local::now().date_naive()
            )),
        );
        card.remove(k("draft_sha256"));
        card.remove(k("drafted_by"));
        if !labels.is_empty() {
            let mut carries: BTreeSet<String> = match card.get(k("carries")) {
                Some(serde_yaml::Value::Sequence(l)) => l
                    .iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect(),
                _ => BTreeSet::new(),
            };
            carries.extend(labels.iter().cloned());
            card.insert(
                k("carries"),
                serde_yaml::Value::Sequence(carries.into_iter().map(|x| k(&x)).collect()),
            );
        }
        let yaml = serde_yaml::to_string(&card).map_err(|e| e.to_string())?;
        let text = if md {
            format!("---\n{yaml}---\n\n{}", body.trim_start())
        } else {
            yaml
        };
        Ok((
            Planned {
                id: c.id.clone(),
                connection: conn_id.clone(),
                rel,
                before,
                file_sha,
                text,
                lines,
                carries: labels,
            },
            asks,
        ))
    }
}

/// Make `args`' changes to a note's card; the lines that say what changed.
fn apply_changes(
    card: &mut serde_yaml::Mapping,
    kind: &str,
    args: &NoteChange,
) -> Result<Vec<String>, String> {
    let clean = |s: &str, max: usize| -> Result<String, String> {
        if s.chars().count() > max {
            return Err(format!("at most {max} characters"));
        }
        Ok(s.replace('\r', ""))
    };
    let k = |s: &str| serde_yaml::Value::from(s);
    let mut lines: Vec<String> = Vec::new();
    if let Some(t) = &args.title {
        let t = clean(t, 200)?.replace('\n', " ");
        lines.push(format!("title: {t}"));
        card.insert(k("title"), k(&t));
    }
    if let Some(s) = &args.summary {
        let s = clean(s, 500)?.replace('\n', " ");
        lines.push(format!("summary: {s}"));
        card.insert(k("summary"), k(&s));
    }
    if let Some(fields) = &args.fields {
        if kind != "dataset" {
            return Err("fields belong to dataset notes".into());
        }
        let Some(serde_yaml::Value::Mapping(m)) = card.get_mut(k("fields")) else {
            return Err("this note lists no fields".into());
        };
        for (name, u) in fields {
            let Some(serde_yaml::Value::Mapping(f)) = m.get_mut(k(name)) else {
                return Err(format!("{name} is not one of this note's fields"));
            };
            if let Some(a) = &u.alias {
                let a = clean(a, 120)?.replace('\n', " ");
                lines.push(format!("{name} alias: {a}"));
                f.insert(k("alias"), k(&a));
            }
            if let Some(n) = &u.note {
                let n = clean(n, 1000)?.replace('\n', " ");
                lines.push(format!("{name} note: {n}"));
                f.insert(k("note"), k(&n));
            }
        }
    }
    if let Some(labels) = &args.labels {
        if kind != "domain" {
            return Err("labels belong to code lists".into());
        }
        let Some(serde_yaml::Value::Mapping(m)) = card.get_mut(k("values")) else {
            return Err("this code list has no codes".into());
        };
        for (code, label) in labels {
            let label = clean(label, 200)?.replace('\n', " ");
            let key = m
                .keys()
                .find(|c| match c {
                    serde_yaml::Value::String(s) => s == code,
                    serde_yaml::Value::Number(n) => n.to_string() == *code,
                    _ => false,
                })
                .cloned()
                .ok_or_else(|| format!("{code:?} is not one of this list's codes"))?;
            lines.push(format!("{code} = {label}"));
            m.insert(key, k(&label));
        }
    }
    if let Some(add) = &args.add_to_body {
        let add = clean(add, 8000).map_err(|e| format!("add_to_body: {e}"))?;
        lines.push(format!("added to the note:\n{add}"));
        if kind == "domain" {
            card.insert(k("notes"), k(&add));
        }
    }
    Ok(lines)
}

/// For the app's Settings window: draft notes for a connection, as the
/// person's own action (no approval asked; audited as the app's).
pub(crate) async fn draft_from_app(state: std::sync::Arc<State>, connection: String) -> Value {
    let gw = Gateway::with_state(state.clone());
    gw.reload_settled().await;
    if let Some(why) = state.problem() {
        return json!({"ok": false, "message": why});
    }
    // The organization can turn drafting off entirely, for the app too.
    if !state.policy().tool_allowed("draft_notes") {
        return json!({"ok": false, "message": "Your organization does not allow drafting notes (draft_notes)."});
    }
    let r = gw
        .draft_notes_for(&connection, None, true, APP_MAX_DATASETS)
        .await;
    let (decision, reason, out) = match r {
        Ok(d) => {
            let msg = format!(
                "Drafted {} note(s) in {}{}{}",
                d.written.len(),
                d.folder,
                if d.kept.is_empty() {
                    String::new()
                } else {
                    format!(
                        "; kept {} you or someone had edited or reviewed",
                        d.kept.len()
                    )
                },
                if d.problems.is_empty() {
                    String::new()
                } else {
                    format!(
                        "; {} could not be drafted: {}",
                        d.problems.len(),
                        d.problems.join("; ")
                    )
                },
            );
            (
                "allowed",
                msg.clone(),
                json!({"ok": true, "message": msg, "folder": d.folder, "written": d.written.len()}),
            )
        }
        Err(e) => ("error", e.clone(), json!({"ok": false, "message": e})),
    };
    let _ = state.audit.append(&AuditRecord {
        user: state.user.clone(),
        client: "columbus-app (settings)".into(),
        tool: "settings.draft_notes".into(),
        connection: Some(connection),
        decision: decision.into(),
        reason: Some(reason),
        ..Default::default()
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_are_plain() {
        assert_eq!(slug("ROADS.ROADWAY_INVENTORY"), "roads-roadway-inventory");
        assert_eq!(slug("../etc/passwd"), "etc-passwd");
        assert_eq!(slug("..."), "x");
    }

    #[test]
    fn a_draft_knows_when_it_was_edited() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("n.md");
        std::fs::write(&p, with_hash("---\nid: a\ndraft_sha256:\n---\nbody\n")).unwrap();
        assert!(untouched_draft(&p));
        let t = std::fs::read_to_string(&p)
            .unwrap()
            .replace("body", "edited");
        std::fs::write(&p, t).unwrap();
        assert!(!untouched_draft(&p));
    }

    #[test]
    fn ids_and_geometry_are_not_counted() {
        assert!(profilable("FUNC_CLASS", "SMALLINT"));
        assert!(profilable("STATUS", "CHAR(1)"));
        assert!(!profilable("ROUTE_ID", "VARCHAR2(20)"));
        assert!(!profilable("OBJECTID", "INTEGER"));
        assert!(!profilable("NOTES", "CLOB"));
        assert!(!profilable("LENGTH_MI", "FLOAT"));
    }
}
