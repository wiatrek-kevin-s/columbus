//! OAuth sign-in for remote connected servers (design change 1, section 8,
//! "Remote server sign-in").
//!
//! * OAuth 2.1 with PKCE, following the MCP authorization specification:
//!   discovery from the server, then a pre-registered client id from the
//!   manifest or dynamic client registration as a native application.
//! * The person signs in in their own browser; the redirect comes back to a
//!   one-time listener on 127.0.0.1.
//! * Tokens are kept by the gateway in the credential store (Windows
//!   Credential Manager), one entry per server, refreshed by the gateway,
//!   and never given to the agent, the app's web view or another server.
//! * Scopes are the manifest's, never wider: a grant with any other scope
//!   is discarded.
//!
//! The protocol work (discovery, registration, PKCE, refresh) is rmcp's;
//! this module supplies the store, the browser hand-off and the scope rule.

use std::sync::Arc;
use std::time::Duration;

use rmcp::transport::auth::{
    AuthClient, AuthError, AuthorizationManager, AuthorizationRequest, CredentialStore, OAuthState,
    StoredCredentials,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// The credential store as the broker sees it: entries by name.
pub trait SecretStore: Send + Sync {
    fn get(&self, key: &str) -> Result<Option<String>, String>;
    fn set(&self, key: &str, value: &str) -> Result<(), String>;
    fn delete(&self, key: &str) -> Result<(), String>;
}

/// Read-only stores: any `Fn(name) -> secret`, as tests and tools that
/// only read pass.
impl<F> SecretStore for F
where
    F: Fn(&str) -> Result<Option<String>, String> + Send + Sync,
{
    fn get(&self, key: &str) -> Result<Option<String>, String> {
        self(key)
    }
    fn set(&self, _key: &str, _value: &str) -> Result<(), String> {
        Err("this credential store is read-only".into())
    }
    fn delete(&self, _key: &str) -> Result<(), String> {
        Err("this credential store is read-only".into())
    }
}

/// The credential-store entry holding a server's OAuth tokens.
pub fn entry_name(server_id: &str) -> String {
    format!("oauth:{server_id}")
}

/// rmcp's credential store, kept in the gateway's credential store.
struct ServerTokens {
    key: String,
    store: Arc<dyn SecretStore>,
}

#[async_trait::async_trait]
impl CredentialStore for ServerTokens {
    async fn load(&self) -> Result<Option<StoredCredentials>, AuthError> {
        match self.store.get(&self.key) {
            Ok(Some(text)) => serde_json::from_str(&text)
                .map(Some)
                .map_err(|e| AuthError::InternalError(format!("stored sign-in unreadable: {e}"))),
            Ok(None) => Ok(None),
            Err(e) => Err(AuthError::InternalError(e)),
        }
    }
    async fn save(&self, credentials: StoredCredentials) -> Result<(), AuthError> {
        let text = serde_json::to_string(&credentials)
            .map_err(|e| AuthError::InternalError(e.to_string()))?;
        self.store
            .set(&self.key, &text)
            .map_err(AuthError::InternalError)
    }
    async fn clear(&self) -> Result<(), AuthError> {
        self.store
            .delete(&self.key)
            .map_err(AuthError::InternalError)
    }
}

/// Whether a sign-in is stored for this server.
pub fn signed_in(store: &dyn SecretStore, server_id: &str) -> bool {
    matches!(store.get(&entry_name(server_id)), Ok(Some(_)))
}

pub fn sign_out(store: &dyn SecretStore, server_id: &str) -> Result<(), String> {
    store.delete(&entry_name(server_id))
}

async fn manager(
    url: &str,
    server_id: &str,
    store: Arc<dyn SecretStore>,
) -> Result<AuthorizationManager, String> {
    let mut m = AuthorizationManager::new(url)
        .await
        .map_err(|e| format!("OAuth discovery for {url}: {e}"))?;
    m.set_credential_store(ServerTokens {
        key: entry_name(server_id),
        store,
    });
    Ok(m)
}

/// The HTTP client a connection uses: it adds the stored access token to
/// every request and refreshes it when it expires. Fails when no sign-in is
/// stored.
pub async fn authorized_client(
    url: &str,
    server_id: &str,
    store: Arc<dyn SecretStore>,
) -> Result<AuthClient<reqwest::Client>, String> {
    let mut m = manager(url, server_id, store).await?;
    let ok = m
        .initialize_from_store()
        .await
        .map_err(|e| format!("stored sign-in for {server_id}: {e}"))?;
    if !ok {
        return Err(format!(
            "not signed in; run `columbus-mcp servers signin {server_id}`"
        ));
    }
    Ok(AuthClient::new(reqwest::Client::new(), m))
}

/// What a finished sign-in granted.
#[derive(Debug)]
pub struct SignedIn {
    pub scopes: Vec<String>,
}

const CALLBACK_WAIT: Duration = Duration::from_secs(300);

/// Sign in to a remote server: open the authorization page in the person's
/// browser (`open`), wait for the redirect on 127.0.0.1, exchange the code
/// and store the tokens. `scopes` are the manifest's; a grant outside them
/// is discarded.
pub async fn sign_in(
    url: &str,
    server_id: &str,
    scopes: &[String],
    client_id: Option<&str>,
    store: Arc<dyn SecretStore>,
    open: impl FnOnce(&str) -> Result<(), String>,
) -> Result<SignedIn, String> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| format!("could not listen for the sign-in redirect: {e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let redirect = format!("http://127.0.0.1:{port}/callback");

    let m = manager(url, server_id, store.clone()).await?;
    let mut state = OAuthState::Unauthorized(m);
    let mut request = AuthorizationRequest::new(redirect.clone())
        .with_client_name("Columbus")
        .with_application_type("native");
    if !scopes.is_empty() {
        request = request.with_scopes(scopes.iter().cloned());
    }
    if let Some(id) = client_id {
        request = request.with_preregistered_client(id);
    }
    state
        .start_authorization(request)
        .await
        .map_err(|e| format!("starting sign-in: {e}"))?;
    let auth_url = state
        .get_authorization_url()
        .await
        .map_err(|e| format!("starting sign-in: {e}"))?;
    open(&auth_url)?;

    let callback = tokio::time::timeout(CALLBACK_WAIT, wait_for_callback(&listener))
        .await
        .map_err(|_| "no sign-in within 5 minutes".to_string())??;
    state
        .handle_callback_url(&format!("http://127.0.0.1:{port}{callback}"))
        .await
        .map_err(|e| format!("completing sign-in: {e}"))?;

    // Scopes are the manifest's, never wider.
    let creds = ServerTokens {
        key: entry_name(server_id),
        store: store.clone(),
    }
    .load()
    .await
    .map_err(|e| e.to_string())?
    .ok_or("the sign-in stored no tokens")?;
    let granted = creds.granted_scopes.clone();
    let extra: Vec<&String> = granted.iter().filter(|g| !scopes.contains(g)).collect();
    if !scopes.is_empty() && !extra.is_empty() {
        let _ = store.delete(&entry_name(server_id));
        return Err(format!(
            "the server granted scopes the manifest does not list ({}); sign-in discarded. Review the manifest's scopes",
            extra.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
        ));
    }
    Ok(SignedIn { scopes: granted })
}

/// Accept one request on the listener and return its path and query
/// (`/callback?code=…&state=…`), answering the browser with a page that
/// says it can be closed. Other paths get a 404 and are ignored.
async fn wait_for_callback(listener: &tokio::net::TcpListener) -> Result<String, String> {
    loop {
        let (mut sock, _) = listener.accept().await.map_err(|e| e.to_string())?;
        let mut buf = vec![0u8; 8192];
        let n = tokio::time::timeout(Duration::from_secs(10), sock.read(&mut buf))
            .await
            .map_err(|_| "the browser sent nothing".to_string())?
            .map_err(|e| e.to_string())?;
        let head = String::from_utf8_lossy(&buf[..n]);
        let target = head
            .lines()
            .next()
            .and_then(|l| l.strip_prefix("GET "))
            .and_then(|l| l.split_whitespace().next())
            .unwrap_or("")
            .to_string();
        if !target.starts_with("/callback?") {
            let _ = sock
                .write_all(
                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await;
            continue;
        }
        let failed = target.contains("error=");
        let body = if failed {
            "<!doctype html><meta charset=utf-8><title>Columbus</title><p>Sign-in was not completed. You can close this tab and try again.</p>"
        } else {
            "<!doctype html><meta charset=utf-8><title>Columbus</title><p>Signed in. You can close this tab and return to Columbus.</p>"
        };
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = sock.write_all(resp.as_bytes()).await;
        if failed {
            return Err("the authorization server reported an error; sign-in not completed".into());
        }
        return Ok(target);
    }
}
