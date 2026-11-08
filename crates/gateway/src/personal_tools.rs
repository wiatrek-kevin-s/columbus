//! The person's own tools, live: engine plugins the assistant proposed with
//! `propose_tool` (propose_tool.rs) and the person installed from Settings
//! in the Columbus app (columbus_shared::tools).
//!
//! Unlike organization plugins, which a session sets up once when it starts,
//! personal tools are read again whenever their folder changes (checked at
//! every call, as the settings files are). That is safe because each tool
//! stands alone: it is one provider that only reads the workspace files it
//! is given and writes its own outputs, confined at level A, and it is on
//! only while `approved.yaml` holds the hash of exactly its files, which
//! only the person's confirmed install writes. Adding one adds one provider
//! and nothing else; removing one, a changed file, or the organization's
//! switch going off takes it away from the next call. A run already going
//! when its tool is removed finishes (its worker ends with it).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use columbus_shared::policy::OrgPolicy;
use sha2::{Digest, Sha256};

use crate::plugins::Plugins;

pub struct PersonalTools {
    root: PathBuf,
    python: PathBuf,
    host_script: PathBuf,
    workspace: PathBuf,
    taken: std::collections::BTreeSet<String>,
    /// The organization's tools folder rather than the person's own.
    org: bool,
    inner: Mutex<(Option<[u8; 32]>, Arc<Plugins>)>,
}

impl PersonalTools {
    pub fn new(
        root: PathBuf,
        python: PathBuf,
        host_script: PathBuf,
        workspace: PathBuf,
        taken: std::collections::BTreeSet<String>,
        org: bool,
    ) -> Self {
        let off = OrgPolicy {
            allow_org_tools: false,
            ..OrgPolicy::default()
        };
        let empty = Plugins::load_personal_tools(
            &off,
            &root,
            python.clone(),
            host_script.clone(),
            workspace.clone(),
            &taken,
            &[],
            org,
        );
        PersonalTools {
            root,
            python,
            host_script,
            workspace,
            taken,
            org,
            inner: Mutex::new((None, Arc::new(empty))),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn host_script(&self) -> &Path {
        &self.host_script
    }

    /// Ids other providers use, which no personal tool may take.
    pub fn taken(&self) -> &std::collections::BTreeSet<String> {
        &self.taken
    }

    /// The tools in force now.
    pub fn current(&self) -> Arc<Plugins> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .1
            .clone()
    }

    /// Read the folder again if it (or what the policy says about personal
    /// tools) changed since last time.
    pub fn refresh(&self, policy: &OrgPolicy) {
        let digest = self.digest(policy);
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if inner.0 == Some(digest) {
            return;
        }
        let reuse: Vec<_> = inner.1.all().to_vec();
        let loaded = Plugins::load_personal_tools(
            policy,
            &self.root,
            self.python.clone(),
            self.host_script.clone(),
            self.workspace.clone(),
            &self.taken,
            &reuse,
            self.org,
        );
        for n in &loaded.notes {
            tracing::warn!("{n}");
        }
        *inner = (Some(digest), Arc::new(loaded));
    }

    /// A hash of everything a load depends on: the policy's switch and
    /// plugin settings, `approved.yaml`, and each tool folder's two files.
    fn digest(&self, policy: &OrgPolicy) -> [u8; 32] {
        let mut h = Sha256::new();
        h.update(
            serde_json::json!({
                "on": if self.org { policy.allow_org_tools } else { policy.allow_personal_tools },
                "levels": policy.plugins.levels,
                "memory": policy.plugins.memory_mb,
            })
            .to_string(),
        );
        let file = |h: &mut Sha256, p: &Path| match std::fs::read(p) {
            Ok(b) => {
                h.update((b.len() as u64).to_le_bytes());
                h.update(&b);
            }
            Err(_) => h.update(b"-"),
        };
        file(&mut h, &self.root.join(columbus_shared::tools::APPROVED));
        let mut names: Vec<String> = std::fs::read_dir(&self.root)
            .map(|d| {
                d.flatten()
                    .filter_map(|e| e.file_name().to_str().map(String::from))
                    .filter(|n| columbus_shared::tools::id_ok(n))
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        for n in names {
            h.update(n.as_bytes());
            h.update([0]);
            let dir = self.root.join(&n);
            file(&mut h, &dir.join(columbus_shared::tools::MANIFEST));
            file(&mut h, &dir.join(columbus_shared::tools::ENTRY));
        }
        h.finalize().into()
    }
}
