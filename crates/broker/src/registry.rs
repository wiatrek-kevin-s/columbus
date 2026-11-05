//! The set of connected servers for one gateway session: which manifests
//! load under organization policy, the live MCP client session for each,
//! and the reviewed state of every tool they list.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use columbus_shared::policy::OrgPolicy;
use rmcp::model::{CallToolRequestParams, CallToolResult, JsonObject, Tool};
use rmcp::service::RunningService;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::{StreamableHttpClientTransport, TokioChildProcess};
use rmcp::{RoleClient, ServiceExt};
use serde::Serialize;
use tokio::sync::OnceCell;

use crate::manifest::{self, Auth, Level, Loaded, Source, ToolRule, Transport, Trust};
use crate::review::{self, Review, ReviewedTool, ToolState};
use crate::BrokerError;

/// Reads a secret from the gateway's credential store by entry name.
/// Also where OAuth tokens for remote servers are kept.
pub type SecretSource = Arc<dyn crate::oauth::SecretStore>;

/// Environment variables a local server may inherit; everything else,
/// including every COLUMBUS_* variable and any secrets in the gateway's
/// environment, is withheld.
const ENV_ALLOW: &[&str] = &[
    "PATH",
    "PATHEXT",
    "SystemRoot",
    "SystemDrive",
    "windir",
    "COMSPEC",
    "HOME",
    "USERPROFILE",
    "LOCALAPPDATA",
    "APPDATA",
    "LANG",
    "LC_ALL",
    "NUMBER_OF_PROCESSORS",
    "PROCESSOR_ARCHITECTURE",
];

pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Status {
    /// Connected; reviewed tools are usable.
    Ready,
    /// Connected but some tools changed or appeared since review; those
    /// are off until a person reviews them again.
    NeedsReview {
        changed: Vec<String>,
        new: Vec<String>,
        removed: Vec<String>,
    },
    /// Connected, never approved: trial mode, no tools enabled.
    Trial,
    /// Not started: organization policy or a missing capability.
    Blocked { reason: String },
    /// Needs a sign-in Columbus cannot do yet.
    SignInRequired { reason: String },
    /// Could not connect.
    Error { reason: String },
}

pub struct Provider {
    pub loaded: Loaded,
    blocked: Option<Status>,
    /// How a local server is started: through the launcher at this level,
    /// or (only where policy allows unconfined pilots and this machine
    /// cannot confine) directly.
    confine: Option<Confine>,
    conn: OnceCell<Result<Connected, String>>,
}

#[derive(Debug, Clone)]
struct Confine {
    launcher: PathBuf,
    level: columbus_confine::Level,
    memory_mb: u64,
}

pub struct Connected {
    pub client: RunningService<RoleClient, ()>,
    pub review: Review,
    pub server_version: Option<String>,
}

impl Provider {
    pub fn id(&self) -> &str {
        &self.loaded.manifest.id
    }

    /// For a local server: its confinement level, or "unconfined" for a
    /// pilot. None for a remote server.
    pub fn confinement(&self) -> Option<String> {
        match self.loaded.manifest.transport {
            Transport::Http { .. } => None,
            Transport::Stdio { .. } => Some(
                self.confine
                    .as_ref()
                    .map(|c| c.level.as_str().to_string())
                    .unwrap_or_else(|| "unconfined".into()),
            ),
        }
    }
}

/// A local server's own folder in the session workspace: the only place
/// it may write, and where the gateway puts the files it is given.
pub fn server_dir(workspace: &std::path::Path, id: &str) -> PathBuf {
    let safe: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    workspace.join("servers").join(safe)
}

pub struct Broker {
    providers: Vec<Arc<Provider>>,
    workspace: PathBuf,
    secrets: SecretSource,
    /// Namespaces of every provider, for cross-reference checks in review.
    namespaces: BTreeSet<String>,
    pub notes: Vec<String>,
}

fn level_c_allowed(policy: &OrgPolicy, l: &Loaded) -> bool {
    l.manifest.trust == Trust::Trusted || policy.servers.allow.contains(&l.manifest.id)
}

impl Broker {
    /// Load manifests from the organization folder and, if policy allows,
    /// the personal folder, and decide which may start. Connecting happens
    /// on first use.
    pub fn load(
        policy: &OrgPolicy,
        org_dir: &Path,
        personal_dir: &Path,
        workspace: &Path,
        secrets: SecretSource,
        native_namespaces: &[&str],
    ) -> Result<Self, BrokerError> {
        let mut notes = Vec::new();
        let mut loaded = manifest::load_dir(org_dir, Source::Organization)?;
        let personal = manifest::load_dir(personal_dir, Source::Personal)?;
        if !personal.is_empty() {
            if policy.servers.allow_personal {
                for p in personal {
                    if loaded.iter().any(|o| o.manifest.id == p.manifest.id) {
                        notes.push(format!(
                            "Personal manifest {:?} ignored: an organization manifest uses that id.",
                            p.manifest.id
                        ));
                    } else {
                        loaded.push(p);
                    }
                }
            } else {
                notes.push(format!(
                    "{} personal server manifest(s) ignored: organization policy allows only organization-managed servers.",
                    personal.len()
                ));
            }
        }
        loaded.sort_by(|a, b| a.manifest.id.cmp(&b.manifest.id));

        let mut namespaces: BTreeSet<String> =
            native_namespaces.iter().map(|s| s.to_string()).collect();
        namespaces.extend(loaded.iter().map(|l| l.manifest.id.clone()));

        let providers = loaded
            .into_iter()
            .map(|l| {
                let mut blocked = Self::gate(policy, &l, secrets.as_ref());
                let mut confine = None;
                if blocked.is_none() && matches!(l.manifest.transport, Transport::Stdio { .. }) {
                    match Self::confinement(policy, &l) {
                        Ok(c) => confine = c,
                        Err(reason) => blocked = Some(Status::Blocked { reason }),
                    }
                    if confine.is_none() && blocked.is_none() {
                        notes.push(format!(
                            "{} runs unconfined: this machine cannot confine it and organization policy allows unconfined pilots.",
                            l.manifest.id
                        ));
                    }
                }
                Arc::new(Provider {
                    loaded: l,
                    blocked,
                    confine,
                    conn: OnceCell::new(),
                })
            })
            .collect();
        Ok(Broker {
            providers,
            workspace: workspace.to_path_buf(),
            secrets,
            namespaces,
            notes,
        })
    }

    /// The confinement a local server starts under (Rule 5): the manifest's
    /// level (A when it names none), if policy allows it and this machine
    /// can provide it. `Ok(None)` only for an unconfined pilot.
    fn confinement(policy: &OrgPolicy, l: &Loaded) -> Result<Option<Confine>, String> {
        let level = l
            .manifest
            .confinement
            .as_ref()
            .and_then(|c| c.level)
            .unwrap_or(Level::A);
        let level = match level {
            Level::A => columbus_confine::Level::A,
            Level::B => columbus_confine::Level::B,
            Level::C => columbus_confine::Level::C,
        };
        if !policy
            .servers
            .levels
            .iter()
            .any(|x| x.eq_ignore_ascii_case(level.as_str()))
        {
            return Err(format!(
                "confinement level {} is not allowed for local servers by organization policy",
                level.as_str()
            ));
        }
        match columbus_confine::ready(level) {
            Ok(launcher) => Ok(Some(Confine {
                launcher,
                level,
                memory_mb: policy.servers.memory_mb,
            })),
            Err(_) if policy.servers.allow_unconfined_local => Ok(None),
            Err(e) => Err(format!(
                "local servers run confined (level {}), and {e}",
                level.as_str()
            )),
        }
    }

    /// Policy checks made before any connection is attempted.
    fn gate(
        policy: &OrgPolicy,
        l: &Loaded,
        secrets: &dyn crate::oauth::SecretStore,
    ) -> Option<Status> {
        let m = &l.manifest;
        let blocked = |reason: String| Some(Status::Blocked { reason });
        if !policy.servers.allow.is_empty() && !policy.servers.allow.contains(&m.id) {
            return blocked("not on the organization's server allow list".into());
        }
        match &m.transport {
            Transport::Http { .. } => {
                let host = m.host().unwrap_or_default();
                if !policy.host_allowed(&host) {
                    return blocked(format!(
                        "host {host:?} is not on the organization's allowed host list"
                    ));
                }
            }
            Transport::Stdio { .. } => {
                if let Some(c) = &m.confinement {
                    if c.level == Some(Level::C) && !level_c_allowed(policy, l) {
                        return blocked(
                            "confinement level C needs trust: trusted or an organization allow-list entry".into(),
                        );
                    }
                }
            }
        }
        if let Auth::Oauth { .. } = m.auth {
            if !matches!(m.transport, Transport::Http { .. }) {
                return blocked("OAuth sign-in applies to remote servers only".into());
            }
            if !crate::oauth::signed_in(secrets, &m.id) {
                return Some(Status::SignInRequired {
                    reason: format!(
                        "not signed in; run `columbus-mcp servers signin {}` and sign in in the browser",
                        m.id
                    ),
                });
            }
        }
        if let Auth::Secret {
            header: Some(h), ..
        } = &m.auth
        {
            if !h.eq_ignore_ascii_case("authorization") {
                return blocked(format!(
                    "custom auth header {h:?} is not supported yet; use Authorization (bearer)"
                ));
            }
        }
        None
    }

    pub fn providers(&self) -> &[Arc<Provider>] {
        &self.providers
    }

    pub fn get(&self, id: &str) -> Option<&Arc<Provider>> {
        self.providers.iter().find(|p| p.id() == id)
    }

    /// Connect (once) and review. Errors are cached for the session.
    pub async fn connected<'a>(&self, p: &'a Provider) -> Result<&'a Connected, Status> {
        if let Some(s) = &p.blocked {
            return Err(s.clone());
        }
        let r = p
            .conn
            .get_or_init(|| async {
                match tokio::time::timeout(CONNECT_TIMEOUT, self.connect(p)).await {
                    Ok(r) => r,
                    Err(_) => Err(format!("no answer within {} s", CONNECT_TIMEOUT.as_secs())),
                }
            })
            .await;
        r.as_ref().map_err(|e| Status::Error { reason: e.clone() })
    }

    async fn connect(&self, p: &Provider) -> Result<Connected, String> {
        let m = &p.loaded.manifest;
        let secret = match &m.auth {
            Auth::Secret { credential, .. } => match self.secrets.get(credential) {
                Ok(Some(s)) => Some(s),
                Ok(None) => return Err(format!("no secret stored for {credential:?}")),
                Err(e) => return Err(e),
            },
            _ => None,
        };
        let client = match &m.transport {
            Transport::Stdio { command, args, cwd } => {
                let mut cmd = match &p.confine {
                    Some(c) => {
                        // Its own folder only: never the workspace, where
                        // governed extracts and Columbus's results live.
                        let mut rw = vec![server_dir(&self.workspace, &m.id)];
                        for f in m.confinement.iter().flat_map(|c| &c.folders) {
                            let f = PathBuf::from(f);
                            if !f.is_absolute() {
                                return Err(format!(
                                    "declared folder {} is not an absolute path",
                                    f.display()
                                ));
                            }
                            let ws_root = self.workspace.parent().unwrap_or(&self.workspace);
                            // Compared as resolved, since the sandbox grants
                            // what a link or another spelling points at.
                            let real = |p: &std::path::Path| {
                                p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
                            };
                            let (rf, rws) = (real(&f), real(ws_root));
                            if f.starts_with(ws_root)
                                || ws_root.starts_with(&f)
                                || rf.starts_with(&rws)
                                || rws.starts_with(&rf)
                            {
                                return Err(format!(
                                    "declared folder {} overlaps Columbus's workspaces",
                                    f.display()
                                ));
                            }
                            rw.push(f);
                        }
                        let spec = columbus_confine::Spec {
                            level: c.level,
                            name: format!("server.{}", m.id),
                            read_write: rw,
                            read_only: cwd.iter().map(PathBuf::from).collect(),
                            memory_mb: Some(c.memory_mb),
                            processes: Some(32),
                        };
                        let mut cmd = tokio::process::Command::new(&c.launcher);
                        cmd.args(spec.launcher_args(command, args));
                        cmd
                    }
                    None => {
                        let mut cmd = tokio::process::Command::new(command);
                        cmd.args(args);
                        cmd
                    }
                };
                cmd.env_clear();
                for k in ENV_ALLOW {
                    if let Some(v) = std::env::var_os(k) {
                        cmd.env(k, v);
                    }
                }
                let own = server_dir(&self.workspace, &m.id);
                let tmp = own.join("tmp");
                let _ = std::fs::create_dir_all(&tmp);
                cmd.env("TEMP", &tmp).env("TMP", &tmp).env("TMPDIR", &tmp);
                if let (Some(s), Auth::Secret { env: Some(var), .. }) = (&secret, &m.auth) {
                    cmd.env(var, s);
                }
                cmd.current_dir(
                    cwd.as_ref()
                        .map(PathBuf::from)
                        .unwrap_or_else(|| own.clone()),
                );
                // The server's (and the launcher's) error output goes to a log
                // outside its own folder (where it could swap the file for a
                // link the gateway would then read), so a failed start can
                // say why.
                let logs = self.workspace.join("logs");
                let _ = std::fs::create_dir_all(&logs);
                let log = logs.join(format!(
                    "server-{}.stderr.log",
                    server_dir(&self.workspace, &m.id)
                        .file_name()
                        .map(|f| f.to_string_lossy().into_owned())
                        .unwrap_or_default()
                ));
                let _ = std::fs::remove_file(&log);
                let stderr = std::fs::File::create(&log)
                    .map(Stdio::from)
                    .unwrap_or_else(|_| Stdio::null());
                let (transport, _) = TokioChildProcess::builder(cmd)
                    .stderr(stderr)
                    .spawn()
                    .map_err(|e| format!("could not start {command:?}: {e}"))?;
                match ().serve(transport).await {
                    Ok(c) => c,
                    Err(e) => {
                        let tail = std::fs::read_to_string(&log)
                            .map(|t| {
                                let lines: Vec<&str> = t.lines().rev().take(5).collect();
                                lines.into_iter().rev().collect::<Vec<_>>().join(" | ")
                            })
                            .unwrap_or_default();
                        return Err(if tail.trim().is_empty() {
                            format!("MCP handshake failed: {e}")
                        } else {
                            format!("MCP handshake failed: {e}; the server said: {tail}")
                        });
                    }
                }
            }
            Transport::Http { url } => {
                let mut cfg = StreamableHttpClientTransportConfig::with_uri(url.clone());
                if let Auth::Oauth { .. } = &m.auth {
                    // Tokens stay in the gateway: the client adds the access
                    // token to each request and refreshes it when needed.
                    let client =
                        crate::oauth::authorized_client(url, &m.id, self.secrets.clone()).await?;
                    let transport = StreamableHttpClientTransport::with_client(client, cfg);
                    ().serve(transport)
                        .await
                        .map_err(|e| format!("MCP handshake failed: {e}"))?
                } else {
                    if let Some(s) = secret {
                        cfg = cfg.auth_header(s);
                    }
                    let transport = StreamableHttpClientTransport::from_config(cfg);
                    ().serve(transport)
                        .await
                        .map_err(|e| format!("MCP handshake failed: {e}"))?
                }
            }
        };
        let server_version = client
            .peer_info()
            .and_then(|i| i.server_info.as_ref().map(|s| s.version.clone()));
        let tools = client
            .list_all_tools()
            .await
            .map_err(|e| format!("listing tools failed: {e}"))?;
        let others: BTreeSet<String> = self
            .namespaces
            .iter()
            .filter(|n| *n != &m.id)
            .cloned()
            .collect();
        let review = review::review(m, &tools, &others);
        Ok(Connected {
            client,
            review,
            server_version,
        })
    }

    pub fn status_of(conn: &Result<&Connected, Status>) -> Status {
        match conn {
            Err(s) => s.clone(),
            Ok(c) => {
                let pick = |st: ToolState| -> Vec<String> {
                    c.review
                        .tools
                        .iter()
                        .filter(|t| t.state == st)
                        .map(|t| t.name.clone())
                        .collect()
                };
                if c.review
                    .tools
                    .iter()
                    .all(|t| t.state == ToolState::Unreviewed)
                    && !c.review.tools.is_empty()
                {
                    Status::Trial
                } else if c.review.drifted {
                    Status::NeedsReview {
                        changed: pick(ToolState::Changed),
                        new: pick(ToolState::New),
                        removed: c.review.removed.clone(),
                    }
                } else {
                    Status::Ready
                }
            }
        }
    }

    /// An enabled tool with its manifest rule, or why it is unavailable.
    pub async fn enabled_tool<'a>(
        &self,
        p: &'a Provider,
        tool: &str,
    ) -> Result<(&'a ReviewedTool, ToolRule), String> {
        let c = self
            .connected(p)
            .await
            .map_err(|s| describe_status(p.id(), &s))?;
        let t = c
            .review
            .tools
            .iter()
            .find(|t| t.name == tool)
            .ok_or_else(|| format!("{} has no tool named {tool:?}", p.id()))?;
        let why = match t.state {
            ToolState::Enabled => return Ok((t, p.loaded.manifest.rule_for(tool).expect("enabled implies a rule"))),
            ToolState::Unreviewed => "the server has not been reviewed yet",
            ToolState::New => "it appeared after the server was reviewed",
            ToolState::Changed => "its definition changed since it was reviewed; it stays off until a person reviews it again",
            ToolState::NotAllowed => "the manifest does not allow it",
        };
        Err(format!("{}.{tool} is disabled: {why}", p.id()))
    }

    pub async fn call(
        &self,
        p: &Provider,
        tool: &str,
        args: JsonObject,
        timeout: Duration,
    ) -> Result<CallToolResult, String> {
        let c = self
            .connected(p)
            .await
            .map_err(|s| describe_status(p.id(), &s))?;
        let params = CallToolRequestParams::new(tool.to_string()).with_arguments(args);
        match tokio::time::timeout(timeout, c.client.call_tool(params)).await {
            Ok(Ok(r)) => Ok(r),
            Ok(Err(e)) => Err(format!("{}.{tool} failed: {e}", p.id())),
            Err(_) => Err(format!(
                "{}.{tool} took longer than {} s",
                p.id(),
                timeout.as_secs()
            )),
        }
    }

    /// Tools a provider lists with their review state (for review tooling).
    pub async fn reviewed(&self, p: &Provider) -> Result<Vec<ReviewedTool>, Status> {
        Ok(self.connected(p).await?.review.tools.clone())
    }

    pub fn namespaces(&self) -> &BTreeSet<String> {
        &self.namespaces
    }

    /// Enabled tools across ready providers, namespaced, for discovery.
    pub async fn enabled_tools(&self) -> BTreeMap<String, (Arc<Provider>, Tool, ToolRule)> {
        let mut out = BTreeMap::new();
        for p in &self.providers {
            if let Ok(c) = self.connected(p).await {
                for t in c
                    .review
                    .tools
                    .iter()
                    .filter(|t| t.state == ToolState::Enabled)
                {
                    if let Some(rule) = p.loaded.manifest.rule_for(&t.name) {
                        out.insert(
                            format!("{}.{}", p.id(), t.name),
                            (p.clone(), t.definition.clone(), rule),
                        );
                    }
                }
            }
        }
        out
    }
}

pub fn describe_status(id: &str, s: &Status) -> String {
    match s {
        Status::Ready => format!("{id} is ready"),
        Status::Trial => {
            format!("{id} has not been reviewed yet, so none of its tools are enabled")
        }
        Status::NeedsReview { .. } => {
            format!("{id} changed since review; its changed tools are off")
        }
        Status::Blocked { reason } => format!("{id} is blocked: {reason}"),
        Status::SignInRequired { reason } => format!("{id} needs sign-in: {reason}"),
        Status::Error { reason } => format!("{id} is unavailable: {reason}"),
    }
}
