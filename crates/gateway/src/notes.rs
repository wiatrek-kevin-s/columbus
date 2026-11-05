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

/// Most datasets one draft covers.
const MAX_DATASETS: usize = 200;
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
    if t.is_empty() {
        "x".into()
    } else {
        t.trim_end_matches('-').to_string()
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

fn with_hash(text: &str) -> String {
    let h = draft_hash(text);
    text.replacen("draft_sha256:", &format!("draft_sha256: {h}"), 1)
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

fn yaml_str(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
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
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct UpdateArgs {
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
    ) -> Result<Drafted, String> {
        let (access, conn) = self.access(conn_id).map_err(|e| e.to_string())?;
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
        if names.len() > MAX_DATASETS {
            return Err(format!(
                "this connection allows {} datasets; draft at most {MAX_DATASETS} at a time (name them in datasets)",
                names.len()
            ));
        }
        let root = notes_dir(&self.state.paths);
        ensure_pack(&root)?;
        let values_on = self.state.policy().notes.values;
        let mut out = Drafted {
            folder: root.display().to_string(),
            ..Default::default()
        };
        let today = chrono::Local::now().date_naive().to_string();
        let cslug = slug(&conn.id);
        // Relationships pointing at each dataset, for "referenced by".
        let mut drafts: Vec<(String, Value, Value)> = Vec::new();
        for ds in &names {
            match self.dataset_facts(&access, &conn, ds, values_on).await {
                Ok((info, cat)) => drafts.push((ds.clone(), info, cat)),
                Err(e) => out.problems.push(format!("{ds}: {e}")),
            }
        }
        let mut incoming: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (ds, _, cat) in &drafts {
            for fk in cat["foreign_keys"].as_array().into_iter().flatten() {
                if let Some(t) = fk["references"]["dataset"].as_str() {
                    incoming
                        .entry(t.to_ascii_lowercase())
                        .or_default()
                        .push(format!(
                            "{ds} ({})",
                            fk["columns"]
                                .as_array()
                                .map(|c| c
                                    .iter()
                                    .filter_map(Value::as_str)
                                    .collect::<Vec<_>>()
                                    .join(", "))
                                .unwrap_or_default()
                        ));
                }
            }
        }
        for (ds, info, cat) in &drafts {
            let dslug = slug(ds);
            let id = format!("{cslug}-{dslug}");
            let rel = PathBuf::from("datasets")
                .join(&cslug)
                .join(format!("{dslug}.md"));
            let path = root.join(&rel);
            if path.exists() && !(refresh && untouched_draft(&path)) {
                out.kept.push(id);
                continue;
            }
            // Code lists first: the dataset entry links to them.
            let mut domains: BTreeMap<String, String> = BTreeMap::new();
            let catalog_domains = cat["domains"].as_object().cloned().unwrap_or_default();
            let mut fields_with_values: BTreeMap<String, Vec<(String, u64)>> = BTreeMap::new();
            for (field, v) in cat["values"].as_object().into_iter().flatten() {
                let Some(rows) = v.as_array() else { continue };
                if rows.len() > MAX_CODES {
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
                    if code.chars().count() > MAX_CODE_CHARS || code.contains(['\n', '\r']) {
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
            domain_fields.extend(catalog_domains.keys().cloned());
            for field in &domain_fields {
                let did = format!("{id}-{}", slug(field));
                let drel = PathBuf::from("domains")
                    .join(&cslug)
                    .join(format!("{dslug}-{}.yaml", slug(field)));
                let dpath = root.join(&drel);
                domains.insert(field.clone(), did.clone());
                if dpath.exists() && !(refresh && untouched_draft(&dpath)) {
                    out.kept.push(did);
                    continue;
                }
                let labelled = catalog_domains.get(field);
                let mut text = format!(
                    "# Drafted by Columbus from {conn_id} on {today}: the codes {field} holds{}.\n\
                     # Add what each code means; set status: reviewed when it is right.\n\
                     id: {did}\nkind: domain\ntitle: {}\nsummary: {}\nconnection: {}\ndataset: {}\nfield: {}\ntype: coded\nvalues:\n",
                    if labelled.is_some() { ", with the labels the GeoPackage gives them" } else { ", with how many rows have each" },
                    yaml_str(&format!("{field} codes in {ds}")),
                    yaml_str(&format!("The values {field} holds in {ds}.")),
                    yaml_str(&conn.id),
                    yaml_str(ds),
                    yaml_str(field),
                    conn_id = conn.id,
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
                match write_note(&root, &drel, &with_hash(&text)) {
                    Ok(()) => out.written.push(did),
                    Err(e) => out.problems.push(e),
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
            );
            match write_note(&root, &rel, &with_hash(&text)) {
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
        values_on: bool,
    ) -> Result<(Value, Value), String> {
        let info = self.dataset_info(access, conn, ds, false).await?;
        let dialect = SqlDialect::for_connection_type(&conn.kind);
        let mut value_sqls = Vec::new();
        let rows = info["rows"].as_u64().unwrap_or(u64::MAX);
        if values_on && rows <= MAX_PROFILED_ROWS {
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
                match check_select(&sql, access, (MAX_CODES + 1) as u64, &cols) {
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
        for key in ["columns"] {
            if let Some(m) = cat["comments"][key].as_object_mut() {
                m.retain(|k, _| !access.field_hidden(k));
            }
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
        let root = notes_dir(&self.state.paths);
        root.join("pack.yaml")
            .is_file()
            .then(|| Pack::load(&root).ok())
            .flatten()
    }

    /// A note as the rules in force now let it be shown: None when it is
    /// about a connection that is gone or a dataset no longer allowed, and
    /// without fields now hidden.
    pub(crate) fn visible_note(&self, e: &Entry) -> Option<Entry> {
        let Some(conn_id) = e.str_field("connection") else {
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
        let hidden = |v: &serde_yaml::Value| v.as_str().is_some_and(|n| access.field_hidden(n));
        let key = |k: &str| serde_yaml::Value::from(k);
        if let Some(serde_yaml::Value::Mapping(m)) = e.card.get_mut(key("fields")) {
            m.retain(|k, _| !hidden(k));
        }
        if let Some(serde_yaml::Value::Sequence(s)) = e.card.get_mut(key("keys")) {
            s.retain(|k| !hidden(k));
        }
        for list in ["relationships", "indexes"] {
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
                        .and_then(|d| d.as_str())
                        .map(String::from);
                    !cols.iter().any(hidden) && target.is_none_or(|t| access.dataset_allowed(&t))
                });
            }
        }
        Some(e)
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
    incoming: Option<&Vec<String>>,
    owner: &str,
    today: &str,
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
            "{}{}{}.",
            rows.map(|r| format!("{r} rows"))
                .unwrap_or_else(|| "A table".into()),
            geom.map(|g| format!(", {g}")).unwrap_or_default(),
            crs.map(|c| format!(" in {c}")).unwrap_or_default(),
        ));
        summary.push_str(" What it is for is not written down yet.");
    }
    let mut t = format!(
        "---\n# Drafted by Columbus from {} on {today}: facts from the database only.\n\
         # Fill in what it means (or ask your assistant to, from your documents), then set status: reviewed.\n\
         id: {id}\nkind: dataset\ntitle: {}\nsummary: {}\nconnection: {}\nphysical: {}\n",
        conn.id,
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
    let links: Vec<&String> = domains.values().collect();
    if !links.is_empty() {
        t.push_str(&format!(
            "links: [{}]\n",
            links
                .iter()
                .map(|l| l.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    t.push_str(&format!(
        "owner: {}\nlast_confirmed: {today}\nstatus: draft\ndrafted_by: {}\ndraft_sha256:\n---\n\n",
        yaml_str(owner),
        yaml_str(&format!("columbus {}", env!("CARGO_PKG_VERSION")))
    ));
    t.push_str(&format!("# {ds}\n\n"));
    if let Some(c) = comment {
        t.push_str(&format!("The database describes it as: {c}\n\n"));
    }
    if let Some(inc) = incoming {
        t.push_str("Other datasets point at this one: ");
        t.push_str(&inc.join("; "));
        t.push_str(".\n\n");
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
        let folder = notes_dir(&self.state.paths);
        let what = match &args.datasets {
            Some(d) => format!("{} dataset(s)", d.len()),
            None => "every dataset it allows".into(),
        };
        let summary = format!(
            "Write draft notes about {what} on {} to your notes folder ({}): field names and types, row counts, keys, relationships, indexes, the database's own comments{}. Fields your organization hides, and tables this connection refuses, are left out.",
            args.connection,
            folder.display(),
            if self.state.policy().notes.values {
                ", and up to 30 codes of each short field"
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
            approval_id: None,
            every_time: false,
            code: None,
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
            .draft_notes_for(&args.connection, args.datasets.as_deref(), args.refresh)
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
        description = "Record what the person's data means in one of their notes (from context_search or draft_notes): a better title or summary, field aliases and notes, labels for a code list's codes, or text added to the note (what it is for, how to read it, where the information came from: the person, or the document you read). The person sees exactly what changes and approves every update. Only the person's own notes can be changed, never the organization's pack, and a note stays a draft until the person marks it reviewed.",
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
        let request = serde_json::to_value(json!({
            "id": args.id, "title": args.title, "summary": args.summary, "add_to_body": args.add_to_body,
            "fields": args.fields.as_ref().map(|m| m.iter().map(|(k, v)| (k.clone(), json!({"alias": v.alias, "note": v.note}))).collect::<serde_json::Map<_, _>>()),
            "labels": args.labels,
        }))
        .unwrap_or(Value::Null);
        let rec = self.gate(&ctx, "update_notes", &request)?;
        let Some(pack) = self.notes_pack() else {
            return Err(self.deny(
                &ctx,
                rec,
                "there are no notes yet: draft some with draft_notes".to_string(),
            ));
        };
        let Some(entry) = pack.get(&args.id).and_then(|e| self.visible_note(e)) else {
            return Err(self.deny(
                &ctx,
                rec,
                format!("no note {:?} among the person's notes", args.id),
            ));
        };
        if entry.str_field("status").as_deref() != Some("draft") {
            return Err(self.deny(
                &ctx,
                rec,
                format!(
                    "{} is marked {}: only the person changes it now",
                    args.id,
                    entry.str_field("status").unwrap_or_default()
                ),
            ));
        }
        let clean = |s: &str, max: usize| -> Result<String, String> {
            if s.chars().count() > max {
                return Err(format!("at most {max} characters"));
            }
            Ok(s.replace('\r', ""))
        };
        let mut card = entry.card.clone();
        let mut lines: Vec<String> = Vec::new();
        let k = |s: &str| serde_yaml::Value::from(s);
        let r = (|| -> Result<(), String> {
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
                if entry.kind != "dataset" {
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
                if entry.kind != "domain" {
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
            Ok(())
        })();
        if let Err(e) = r {
            return Err(self.deny(&ctx, rec, e));
        }
        let mut body = entry.body.clone();
        if let Some(add) = &args.add_to_body {
            let add = match clean(add, 8000) {
                Ok(a) => a,
                Err(e) => return Err(self.deny(&ctx, rec, format!("add_to_body: {e}"))),
            };
            lines.push(format!("added to the note:\n{add}"));
            if entry.kind == "domain" {
                card.insert(k("notes"), k(&add));
            } else {
                body = format!("{}\n\n{}\n", body.trim_end(), add.trim());
            }
        }
        if lines.is_empty() {
            return Err(self.deny(&ctx, rec, "nothing to change".to_string()));
        }
        card.insert(
            k("edited_by"),
            k(&format!(
                "{} for {} on {}",
                crate::server::client_name(&ctx),
                self.state.user,
                chrono::Local::now().date_naive()
            )),
        );
        card.remove(k("draft_sha256"));
        card.remove(k("drafted_by"));
        let summary = format!("Change your note {}:\n{}", args.id, lines.join("\n"));
        let ask = Ask {
            tool: "update_notes",
            label: "a change to your notes",
            request: &request,
            risk: Risk::Edit,
            summary,
            detail: Some(json!({"arguments": request, "changes": lines})),
            approval_id: None,
            every_time: true,
            code: None,
        };
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
        // The file is written whole: changed card, same (or longer) body.
        let root = notes_dir(&self.state.paths);
        let rel = entry
            .file
            .strip_prefix(&root)
            .unwrap_or(&entry.file)
            .to_path_buf();
        let yaml = serde_yaml::to_string(&card).map_err(|e| e.to_string())?;
        let text = if entry.file.extension().and_then(|x| x.to_str()) == Some("md") {
            format!("---\n{yaml}---\n\n{}", body.trim_start())
        } else {
            yaml
        };
        if let Err(e) = write_note(&root, &rel, &text) {
            return Err(self.deny(&ctx, rec, e));
        }
        self.audit(&ctx, rec)?;
        Ok(Json(Output {
            result: json!({"status": "updated", "id": args.id, "changes": lines}),
            checks: vec![],
            provenance: None,
        }))
    }
}

/// For the app's Settings window: draft notes for a connection, as the
/// person's own action (no approval asked; audited as the app's).
pub(crate) async fn draft_from_app(state: std::sync::Arc<State>, connection: String) -> Value {
    let gw = Gateway::with_state(state.clone());
    gw.reload().await;
    if let Some(why) = state.problem() {
        return json!({"ok": false, "message": why});
    }
    let r = gw.draft_notes_for(&connection, None, true).await;
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
