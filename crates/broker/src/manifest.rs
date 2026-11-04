//! Columbus manifests for connected MCP servers (design change 1, section 4).
//!
//! The manifest, written or approved by a person or IT, decides what a
//! server may do. Nothing the server says about itself (its annotations,
//! its descriptions) is trusted.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::BrokerError;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Transport {
    /// A local child process speaking MCP over stdin/stdout.
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
    },
    /// A remote server over streamable HTTP.
    Http { url: String },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Auth {
    #[default]
    None,
    /// OAuth 2.1 with PKCE; tokens held by the gateway (milestone 2).
    Oauth {
        #[serde(default)]
        scopes: Vec<String>,
        /// A client id registered with the server in advance; without one,
        /// Columbus registers itself (dynamic client registration).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        client_id: Option<String>,
    },
    /// A secret from the credential store, handed to the server as an
    /// environment variable (local) or a header (remote).
    Secret {
        credential: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        env: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        header: Option<String>,
    },
}

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum Trust {
    Trusted,
    Standard,
    #[default]
    Untrusted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub enum Level {
    A,
    B,
    C,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Confinement {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<Level>,
    #[serde(default)]
    pub folders: Vec<String>,
    #[serde(default)]
    pub hosts: Vec<String>,
    #[serde(default)]
    pub loopback: Vec<u16>,
}

/// Recorded when a person approves the server's tool definitions.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Pinned {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// `sha256:<hex>` over all approved tool definitions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools_hash: Option<String>,
    /// Per-tool `sha256:<hex>`, so a change disables only what changed.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tools: BTreeMap<String, String>,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum Risk {
    Read,
    Write,
    Destructive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ArgRole {
    Path,
    Url,
    Dataset,
    FileHandle,
}

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum OutputMode {
    #[default]
    Untrusted,
    ToWorkspace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalRule {
    Never,
    OnWrite,
    Always,
    Deny,
}

/// The reviewer's settings for one tool. Also used for `tools.default`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolRule {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk: Option<Risk>,
    #[serde(default)]
    pub egress: bool,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub args: BTreeMap<String, ArgRole>,
    #[serde(default)]
    pub output: OutputMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval: Option<ApprovalRule>,
}

impl ToolRule {
    /// Risk as reviewed; a tool nobody rated is treated as a write.
    pub fn effective_risk(&self) -> Risk {
        self.risk.unwrap_or(Risk::Write)
    }

    /// Whether a call can send data out or change something elsewhere.
    pub fn outbound(&self) -> bool {
        self.egress || self.effective_risk() >= Risk::Write
    }

    pub fn effective_approval(&self) -> ApprovalRule {
        match (self.approval, self.effective_risk()) {
            // Destructive tools always ask, whatever the manifest lowers it to.
            (Some(ApprovalRule::Deny), _) => ApprovalRule::Deny,
            (_, Risk::Destructive) => ApprovalRule::Always,
            (Some(a), _) => a,
            (None, _) => ApprovalRule::OnWrite,
        }
    }

    /// Whether this call needs a person's approval before it runs, before
    /// any data-flow rule is considered.
    pub fn needs_approval(&self) -> bool {
        match self.effective_approval() {
            ApprovalRule::Never | ApprovalRule::Deny => false,
            ApprovalRule::Always => true,
            ApprovalRule::OnWrite => self.effective_risk() >= Risk::Write,
        }
    }
}

/// Where a manifest was found. Organization manifests come from
/// `%ProgramData%`; personal ones from the user's profile, if policy allows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Organization,
    Personal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerManifest {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publisher: Option<String>,
    pub transport: Transport,
    #[serde(default)]
    pub auth: Auth,
    #[serde(default)]
    pub trust: Trust,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confinement: Option<Confinement>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned: Option<Pinned>,
    /// Per-tool rules; the key `default` applies to tools not listed.
    #[serde(default)]
    pub tools: BTreeMap<String, ToolRule>,
}

/// Namespaces the agent sees that a server id must not take.
const RESERVED: &[&str] = &[
    "columbus",
    "core",
    "native",
    "workspace",
    "ws",
    "connection",
    "server",
    "default",
];

impl ServerManifest {
    pub fn parse(text: &str, origin: &Path) -> Result<Self, BrokerError> {
        let m: ServerManifest = serde_yaml::from_str(text)
            .map_err(|e| BrokerError::Manifest(origin.into(), e.to_string()))?;
        m.validate()
            .map_err(|e| BrokerError::Manifest(origin.into(), e))?;
        Ok(m)
    }

    fn validate(&self) -> Result<(), String> {
        let ok_id = !self.id.is_empty()
            && self.id.len() <= 32
            && self.id.starts_with(|c: char| c.is_ascii_lowercase())
            && self
                .id
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
        if !ok_id {
            return Err(format!(
                "id {:?} must be 1-32 characters of a-z, 0-9 and '-', starting with a letter",
                self.id
            ));
        }
        if RESERVED.contains(&self.id.as_str()) {
            return Err(format!("id {:?} is reserved", self.id));
        }
        if let Transport::Http { url } = &self.transport {
            if !url.starts_with("https://") && !is_loopback_url(url) {
                return Err("remote servers must use https:// (http:// only on loopback)".into());
            }
        }
        Ok(())
    }

    /// The rule for a tool, or `None` if the manifest does not allow it.
    pub fn rule_for(&self, tool: &str) -> Option<ToolRule> {
        if tool == "default" {
            return None;
        }
        match self.tools.get(tool) {
            Some(r) if r.allow == Some(false) => None,
            Some(r) if r.approval == Some(ApprovalRule::Deny) => None,
            Some(r) => Some(r.clone()),
            None => {
                let d = self.tools.get("default").cloned().unwrap_or_default();
                if d.allow == Some(true) && d.approval != Some(ApprovalRule::Deny) {
                    Some(d)
                } else {
                    None
                }
            }
        }
    }

    pub fn display_name(&self) -> String {
        self.name.clone().unwrap_or_else(|| self.id.clone())
    }

    pub fn is_local(&self) -> bool {
        matches!(self.transport, Transport::Stdio { .. })
    }

    pub fn host(&self) -> Option<String> {
        match &self.transport {
            Transport::Http { url } => url_host(url),
            Transport::Stdio { .. } => None,
        }
    }
}

pub fn url_host(url: &str) -> Option<String> {
    let rest = url.split_once("://")?.1;
    let authority = rest.split(['/', '?', '#']).next()?;
    let host_port = authority.rsplit('@').next()?;
    let host = if let Some(stripped) = host_port.strip_prefix('[') {
        stripped.split(']').next()?.to_string()
    } else {
        host_port.split(':').next()?.to_string()
    };
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

fn is_loopback_url(url: &str) -> bool {
    url.starts_with("http://")
        && matches!(
            url_host(url).as_deref(),
            Some("127.0.0.1") | Some("localhost") | Some("::1")
        )
}

/// A manifest with where it came from.
#[derive(Debug, Clone)]
pub struct Loaded {
    pub manifest: ServerManifest,
    pub path: PathBuf,
    pub source: Source,
}

/// Load `*.yaml` manifests from one folder.
pub fn load_dir(dir: &Path, source: Source) -> Result<Vec<Loaded>, BrokerError> {
    let mut out = Vec::new();
    if !dir.exists() {
        return Ok(out);
    }
    let entries = std::fs::read_dir(dir).map_err(|e| BrokerError::Io(dir.into(), e.to_string()))?;
    for entry in entries {
        let path = entry
            .map_err(|e| BrokerError::Io(dir.into(), e.to_string()))?
            .path();
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if ext != "yaml" && ext != "yml" {
            continue;
        }
        let text = std::fs::read_to_string(&path)
            .map_err(|e| BrokerError::Io(path.clone(), e.to_string()))?;
        out.push(Loaded {
            manifest: ServerManifest::parse(&text, &path)?,
            path,
            source,
        });
    }
    out.sort_by(|a, b| a.manifest.id.cmp(&b.manifest.id));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DRIVE: &str = r#"
id: drive
name: Google Drive
transport: { type: http, url: https://drivemcp.googleapis.com/mcp/v1 }
auth: { type: oauth, scopes: [drive.readonly, drive.file] }
trust: standard
pinned: { tools_hash: "sha256:3b9c" }
tools:
  search_files:          { risk: read }
  get_file_metadata:     { risk: read }
  read_file_content:     { risk: read, output: untrusted }
  download_file_content: { risk: read, output: to_workspace }
  create_file:           { risk: write, egress: true, args: { content: file_handle }, approval: always }
  default:               { allow: false }
"#;

    const OBSIDIAN: &str = r#"
id: obsidian
name: Obsidian vault
transport: { type: stdio, command: uvx, args: [mcp-obsidian] }
auth: { type: secret, credential: columbus/obsidian-rest-key }
trust: untrusted
confinement: { level: C, loopback: [27124], hosts: [] }
pinned: { version: 0.2.x, tools_hash: "sha256:9a41" }
tools:
  list_files_in_vault:   { risk: read }
  get_file_contents:     { risk: read, output: untrusted }
  search:                { risk: read }
  append_content:        { risk: write, approval: on_write }
  delete_file:           { approval: deny }
  default:               { allow: false }
"#;

    fn parse(t: &str) -> ServerManifest {
        ServerManifest::parse(t, Path::new("test.yaml")).unwrap()
    }

    #[test]
    fn the_change_documents_examples_parse() {
        let d = parse(DRIVE);
        assert_eq!(d.host().as_deref(), Some("drivemcp.googleapis.com"));
        let create = d.rule_for("create_file").unwrap();
        assert!(create.outbound() && create.needs_approval());
        assert_eq!(create.args["content"], ArgRole::FileHandle);
        assert_eq!(
            d.rule_for("download_file_content").unwrap().output,
            OutputMode::ToWorkspace
        );
        assert!(!d.rule_for("search_files").unwrap().needs_approval());
        assert!(
            d.rule_for("delete_everything").is_none(),
            "unlisted tools are off by default"
        );

        let o = parse(OBSIDIAN);
        assert!(o.is_local());
        assert_eq!(o.confinement.as_ref().unwrap().level, Some(Level::C));
        assert!(
            o.rule_for("delete_file").is_none(),
            "approval: deny removes the tool"
        );
        assert!(o.rule_for("append_content").unwrap().needs_approval());
    }

    #[test]
    fn unrated_tools_count_as_writes_and_destructive_always_asks() {
        let r = ToolRule::default();
        assert_eq!(r.effective_risk(), Risk::Write);
        assert!(r.outbound() && r.needs_approval());
        let r = ToolRule {
            risk: Some(Risk::Destructive),
            approval: Some(ApprovalRule::Never),
            ..Default::default()
        };
        assert!(r.needs_approval());
    }

    #[test]
    fn bad_ids_and_plain_http_are_refused() {
        for bad in ["Drive", "1drive", "columbus", "drive.files", ""] {
            let t = DRIVE.replace("id: drive", &format!("id: {bad:?}"));
            assert!(ServerManifest::parse(&t, Path::new("t")).is_err(), "{bad}");
        }
        let t = DRIVE.replace("https://drivemcp", "http://drivemcp");
        assert!(ServerManifest::parse(&t, Path::new("t")).is_err());
        let t = DRIVE.replace(
            "https://drivemcp.googleapis.com/mcp/v1",
            "http://127.0.0.1:8080/mcp",
        );
        assert!(ServerManifest::parse(&t, Path::new("t")).is_ok());
    }

    #[test]
    fn url_hosts() {
        assert_eq!(
            url_host("https://user@Example.com:8443/x?y").as_deref(),
            Some("example.com")
        );
        assert_eq!(url_host("http://[::1]:9/").as_deref(), Some("::1"));
        assert_eq!(url_host("nonsense"), None);
    }
}

#[cfg(test)]
mod example_tests {
    use super::*;

    #[test]
    fn shipped_examples_parse() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/servers");
        let loaded = load_dir(&dir, Source::Organization).unwrap();
        let ids: Vec<&str> = loaded.iter().map(|l| l.manifest.id.as_str()).collect();
        assert_eq!(ids, ["drive", "obsidian"]);
        let policy = columbus_shared::policy::OrgPolicy::load(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/policy.yaml"),
        )
        .unwrap();
        assert_eq!(policy.flows.len(), 2);
        assert!(!policy.servers.allow_personal);
    }
}
