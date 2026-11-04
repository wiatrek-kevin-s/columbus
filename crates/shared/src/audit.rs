//! Hash-chained audit log (design section 3, "Audit log").
//!
//! One JSON line per tool call in `YYYY-MM.jsonl`. Each line carries the
//! hash of the previous line (`prev`) and its own hash (`hash`, computed over
//! the canonical record without `hash`), so removing, reordering or editing a
//! line breaks the chain and `verify` reports where.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::{DateTime, FixedOffset, Local, Utc};
use serde::Serialize;
use serde_json::{Map, Value};

use crate::canon::{canonical_json, sha256_hex};

/// Hash used as `prev` for the first line of a file.
pub const GENESIS: &str = "0000000000000000000000000000000000000000000000000000000000000000";

#[derive(Debug, Clone, Serialize, Default)]
pub struct AuditRecord {
    pub user: String,
    pub client: String,
    pub tool: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connection: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dataset: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rows: Option<u64>,
    /// `allowed`, `denied`, `approval_required` or `error`.
    pub decision: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub approval: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prov: Option<String>,
    /// Hash of the exact request, so a line can be matched to an approval.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request: Option<String>,
    /// Connected server or engine plugin that served the call (change 1).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_version: Option<String>,
    /// Hash of the provider's approved tool set at the time of the call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools_hash: Option<String>,
    /// Data labels in play (e.g. `connection:roads-prod`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    /// `allow`, `approve` or `deny`, from the flow rules.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flow_decision: Option<String>,
}

pub struct AuditLog {
    dir: PathBuf,
    state: Mutex<Option<(PathBuf, String)>>,
}

#[derive(Debug, PartialEq)]
pub enum VerifyError {
    Io(String),
    /// 1-based line number and what is wrong with it.
    Broken(usize, String),
}

impl AuditLog {
    pub fn new(dir: impl Into<PathBuf>) -> std::io::Result<Self> {
        let dir = dir.into();
        std::fs::create_dir_all(&dir)?;
        Ok(AuditLog {
            dir,
            state: Mutex::new(None),
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn file_for(&self, ts: DateTime<Utc>) -> PathBuf {
        self.dir.join(format!("{}.jsonl", ts.format("%Y-%m")))
    }

    /// Append one record. Returns the line's hash.
    pub fn append(&self, record: &AuditRecord) -> std::io::Result<String> {
        self.append_at(record, Utc::now())
    }

    pub fn append_at(&self, record: &AuditRecord, now: DateTime<Utc>) -> std::io::Result<String> {
        let mut state = self.state.lock().expect("audit log poisoned");
        let path = self.file_for(now);
        let prev = match &*state {
            Some((p, h)) if *p == path => h.clone(),
            _ => last_hash(&path)?,
        };

        let local: DateTime<FixedOffset> = now.with_timezone(&Local).fixed_offset();
        let mut map = match serde_json::to_value(record).expect("record serializes") {
            Value::Object(m) => m,
            _ => unreachable!(),
        };
        map.insert(
            "ts".into(),
            Value::String(local.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
        );
        map.insert("prev".into(), Value::String(prev));
        let hash = line_hash(&map);
        map.insert("hash".into(), Value::String(hash.clone()));

        let mut f = OpenOptions::new().create(true).append(true).open(&path)?;
        let mut line = serde_json::to_string(&Value::Object(map)).expect("serializes");
        line.push('\n');
        f.write_all(line.as_bytes())?;
        f.sync_data()?;
        *state = Some((path, hash.clone()));
        Ok(hash)
    }
}

fn line_hash(map: &Map<String, Value>) -> String {
    let mut m = map.clone();
    m.remove("hash");
    sha256_hex(canonical_json(&Value::Object(m)).as_bytes())
}

fn last_hash(path: &Path) -> std::io::Result<String> {
    if !path.exists() {
        return Ok(GENESIS.into());
    }
    let mut last = GENESIS.to_string();
    for line in BufReader::new(File::open(path)?).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(Value::Object(m)) = serde_json::from_str::<Value>(&line) {
            if let Some(Value::String(h)) = m.get("hash") {
                last = h.clone();
            }
        }
    }
    Ok(last)
}

/// Check every line of one month's file. Returns the number of lines.
pub fn verify(path: &Path) -> Result<usize, VerifyError> {
    let f = File::open(path).map_err(|e| VerifyError::Io(e.to_string()))?;
    let mut prev = GENESIS.to_string();
    let mut n = 0;
    for (i, line) in BufReader::new(f).lines().enumerate() {
        let line = line.map_err(|e| VerifyError::Io(e.to_string()))?;
        if line.trim().is_empty() {
            continue;
        }
        let ln = i + 1;
        let map = match serde_json::from_str::<Value>(&line) {
            Ok(Value::Object(m)) => m,
            _ => return Err(VerifyError::Broken(ln, "not a JSON object".into())),
        };
        match map.get("prev") {
            Some(Value::String(p)) if *p == prev => {}
            _ => {
                return Err(VerifyError::Broken(
                    ln,
                    "prev does not match the previous line".into(),
                ))
            }
        }
        let stated = match map.get("hash") {
            Some(Value::String(h)) => h.clone(),
            _ => return Err(VerifyError::Broken(ln, "missing hash".into())),
        };
        if line_hash(&map) != stated {
            return Err(VerifyError::Broken(
                ln,
                "content does not match its hash".into(),
            ));
        }
        prev = stated;
        n += 1;
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(tool: &str) -> AuditRecord {
        AuditRecord {
            user: "CORP\\analyst1".into(),
            client: "test".into(),
            tool: tool.into(),
            decision: "allowed".into(),
            ..Default::default()
        }
    }

    #[test]
    fn chain_verifies_and_survives_restart() {
        let d = tempfile::tempdir().unwrap();
        let now = Utc::now();
        {
            let log = AuditLog::new(d.path()).unwrap();
            log.append_at(&rec("whoami"), now).unwrap();
            log.append_at(&rec("list_connections"), now).unwrap();
        }
        // A new process continues the same chain.
        let log = AuditLog::new(d.path()).unwrap();
        log.append_at(&rec("sql_select"), now).unwrap();
        let file = log.file_for(now);
        assert_eq!(verify(&file).unwrap(), 3);
    }

    #[test]
    fn edits_and_deletions_are_detected() {
        let d = tempfile::tempdir().unwrap();
        let now = Utc::now();
        let log = AuditLog::new(d.path()).unwrap();
        for t in ["a", "b", "c"] {
            log.append_at(&rec(t), now).unwrap();
        }
        let file = log.file_for(now);
        let original = std::fs::read_to_string(&file).unwrap();

        std::fs::write(&file, original.replace("\"tool\":\"b\"", "\"tool\":\"x\"")).unwrap();
        assert!(matches!(verify(&file), Err(VerifyError::Broken(2, _))));

        let lines: Vec<&str> = original.lines().collect();
        std::fs::write(&file, format!("{}\n{}\n", lines[0], lines[2])).unwrap();
        assert!(matches!(verify(&file), Err(VerifyError::Broken(2, _))));
    }
}
