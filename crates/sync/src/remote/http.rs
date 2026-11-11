//! One HTTP client for one connected server: its address, its token, and
//! the rules every request keeps.
//!
//! * HTTPS only, and the server's certificate is always checked against
//!   the operating system's trust store, plus the company certificate file
//!   the person may have given for this server. There is no way to turn
//!   the check off.
//! * The token is sent to this server's host and nowhere else. A redirect
//!   to another host (or to plain http) is not followed: the request ends
//!   there, and the person is told.
//! * Every request has a time limit, and every error is put in plain
//!   words. No error, and nothing else this module builds, holds the
//!   token: text that comes back from a server is scrubbed of it first.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, LOCATION, USER_AGENT};
use reqwest::{Method, Url};
use serde_json::Value;

use super::{Address, Kind};
use crate::Error;

/// How long one request may take.
const TIMEOUT: Duration = Duration::from_secs(90);
/// How long reaching the server may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
/// The most bytes read from one answer (a list of 100,000 files is 7 MB on
/// GitHub; a 5 MB file is 7 MB as base64).
const MAX_BODY: usize = 48 * 1024 * 1024;
/// Same-host redirects followed for one request (a renamed repository).
const MAX_REDIRECTS: usize = 5;
/// The most pages read of one list.
const MAX_PAGES: usize = 200;

/// Says who is asking, without naming the person or their computer.
const AGENT: &str = concat!("columbus-sync/", env!("CARGO_PKG_VERSION"));

#[derive(Clone)]
pub(crate) struct Http {
    client: reqwest::Client,
    api: Url,
    pub(crate) host: String,
    pub(crate) kind: Kind,
    auth: HeaderValue,
    token: Arc<str>,
}

/// An answer, whatever its status (redirects to elsewhere excepted).
pub(crate) struct Reply {
    pub status: u16,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

impl Http {
    pub(crate) fn new(
        address: &Address,
        token: &str,
        certificate: Option<&Path>,
    ) -> Result<Http, Error> {
        let token = token.trim();
        let plain =
            !token.is_empty() && token.len() <= 512 && token.bytes().all(|b| b.is_ascii_graphic());
        if !plain {
            return Err(Error::Refused(
                "That does not look like a token: it is one run of letters, digits and signs, without spaces.".into(),
            ));
        }
        let scheme = match address.kind {
            Kind::GitHub => "Bearer",
            Kind::Gitea => "token",
        };
        let mut auth = HeaderValue::from_str(&format!("{scheme} {token}"))
            .map_err(|_| Error::Refused("That does not look like a token.".into()))?;
        // Never shown when a request is printed for debugging.
        auth.set_sensitive(true);

        let origin = address.api.clone();
        let mut b = reqwest::Client::builder()
            .user_agent(AGENT)
            .timeout(TIMEOUT)
            .connect_timeout(CONNECT_TIMEOUT)
            // Only to the same scheme, host and port; never anywhere else.
            .redirect(reqwest::redirect::Policy::custom(move |attempt| {
                let next = attempt.url();
                let same = next.scheme() == origin.scheme()
                    && next.host_str() == origin.host_str()
                    && next.port_or_known_default() == origin.port_or_known_default();
                if same && attempt.previous().len() <= MAX_REDIRECTS {
                    attempt.follow()
                } else {
                    attempt.stop()
                }
            }));
        if address.is_loopback() {
            // A stand-in server on this computer (tests): not through a proxy.
            b = b.no_proxy();
        }
        if let Some(file) = certificate {
            b = b.tls_certs_merge(read_certificates(file)?);
        }
        let client = b.build().map_err(|e| {
            Error::Network(format!(
                "Columbus could not set up a secure connection: {e}"
            ))
        })?;
        Ok(Http {
            client,
            api: address.api.clone(),
            host: address.host.clone(),
            kind: address.kind,
            auth,
            token: token.into(),
        })
    }

    /// Take the token out of text that came from elsewhere.
    pub(crate) fn scrub(&self, text: &str) -> String {
        text.replace(&*self.token, "[token]")
    }

    fn url(&self, segments: &[&str], query: &[(&str, String)]) -> Result<Url, Error> {
        let mut url = self.api.clone();
        url.path_segments_mut()
            .map_err(|()| Error::Refused("the server's address cannot be used".into()))?
            .pop_if_empty()
            .extend(segments);
        if !query.is_empty() {
            url.query_pairs_mut()
                .extend_pairs(query.iter().map(|(k, v)| (*k, v.as_str())));
        }
        Ok(url)
    }

    /// One request. `auth` false sends no token (Gitea's version needs none).
    pub(crate) async fn call(
        &self,
        method: Method,
        segments: &[&str],
        query: &[(&str, String)],
        body: Option<&Value>,
        auth: bool,
    ) -> Result<Reply, Error> {
        let url = self.url(segments, query)?;
        let mut req = self.client.request(method, url).header(USER_AGENT, AGENT);
        req = match self.kind {
            Kind::GitHub => req
                .header(ACCEPT, "application/vnd.github+json")
                .header("X-GitHub-Api-Version", "2022-11-28"),
            Kind::Gitea => req.header(ACCEPT, "application/json"),
        };
        if auth {
            req = req.header(AUTHORIZATION, self.auth.clone());
        }
        if let Some(b) = body {
            req = req.json(b);
        }
        let mut resp = req.send().await.map_err(|e| self.unreachable(&e))?;
        let status = resp.status().as_u16();
        if (300..400).contains(&status) && status != 304 {
            let to = resp
                .headers()
                .get(LOCATION)
                .and_then(|v| v.to_str().ok())
                .and_then(|l| Url::parse(l).ok())
                .and_then(|u| u.host_str().map(str::to_string))
                .unwrap_or_else(|| "another address".into());
            return Err(Error::Redirected(format!(
                "{} answered by sending Columbus on to {}. Columbus does not follow that: your token is sent to {} only. Check the server's address in Settings > Git servers.",
                self.host,
                self.scrub(&to),
                self.host
            )));
        }
        let headers = resp.headers().clone();
        let mut bytes = Vec::new();
        while let Some(chunk) = resp.chunk().await.map_err(|e| self.unreachable(&e))? {
            if bytes.len() + chunk.len() > MAX_BODY {
                return Err(Error::Server(format!(
                    "{} sent more than Columbus reads in one answer",
                    self.host
                )));
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(Reply {
            status,
            headers,
            body: bytes,
        })
    }

    pub(crate) async fn get(
        &self,
        segments: &[&str],
        query: &[(&str, String)],
    ) -> Result<Reply, Error> {
        self.call(Method::GET, segments, query, None, true).await
    }

    pub(crate) async fn post(&self, segments: &[&str], body: &Value) -> Result<Reply, Error> {
        self.call(Method::POST, segments, &[], Some(body), true)
            .await
    }

    /// Every item of a list the server gives a page at a time. `size` is
    /// the page-size parameter's name and the size asked for; a server may
    /// give fewer (Gitea gives 50 at most), so the end is an empty page, or
    /// a page that says there is no next one.
    pub(crate) async fn list(
        &self,
        segments: &[&str],
        query: &[(&str, String)],
        size: (&str, usize),
    ) -> Result<Vec<Value>, Error> {
        let mut out = Vec::new();
        for page in 1..=MAX_PAGES {
            let mut q = query.to_vec();
            q.push((size.0, size.1.to_string()));
            q.push(("page", page.to_string()));
            let reply = self.get(segments, &q).await?;
            if reply.status != 200 {
                return Err(self.failed(&reply));
            }
            let Value::Array(items) = reply.json(self)? else {
                return Err(Error::Server(format!(
                    "{} did not answer with a list",
                    self.host
                )));
            };
            if items.is_empty() {
                break;
            }
            out.extend(items);
            let link = reply.headers.get("link").and_then(|v| v.to_str().ok());
            if link.is_some_and(|l| !l.contains("rel=\"next\"")) {
                break;
            }
        }
        Ok(out)
    }

    /// The error for an answer Columbus cannot use, by its status, in plain
    /// words.
    pub(crate) fn failed(&self, reply: &Reply) -> Error {
        let said = reply.message(self);
        let because = if said.is_empty() {
            String::new()
        } else {
            format!(" The server said: {said}")
        };
        let host = &self.host;
        match reply.status {
            401 => Error::Token(format!(
                "{host} did not accept the token. It may have expired or been removed: connect the server again with a new token in Settings > Git servers."
            )),
            403 | 429 if reply.rate_limited() => Error::RateLimited(format!(
                "{host} asks Columbus to wait before more requests ({}). Nothing was lost; try again then.",
                reply.wait()
            )),
            403 => Error::Permission(format!(
                "The token is not allowed to do this on {host}.{because}"
            )),
            404 => Error::NotFound(format!(
                "{host} has nothing at that address, or the token may not see it.{because}"
            )),
            413 => Error::Server(format!(
                "{host} refused what Columbus sent as too large. Sync fewer or smaller files at a time, or ask whoever runs the server to allow larger requests."
            )),
            s if s >= 500 => Error::Server(format!(
                "{host} had a problem of its own (HTTP {s}). Try again in a moment.{because}"
            )),
            s => Error::Server(format!("{host} answered HTTP {s}.{because}")),
        }
    }

    fn unreachable(&self, e: &reqwest::Error) -> Error {
        let host = &self.host;
        // The whole chain of causes, without the address (which holds no
        // secret, but is long) and never the token.
        let mut chain = String::new();
        let mut source: Option<&dyn std::error::Error> = Some(e);
        while let Some(s) = source {
            chain.push_str(&s.to_string());
            chain.push_str("; ");
            source = s.source();
        }
        let lower = chain.to_lowercase();
        if e.is_timeout() {
            return Error::Network(format!(
                "{host} did not answer within {} seconds. Check your network, then try again.",
                TIMEOUT.as_secs()
            ));
        }
        if lower.contains("certificate")
            || lower.contains("unknownissuer")
            || lower.contains("handshake")
        {
            return Error::Network(format!(
                "The certificate of {host} could not be checked, so nothing was sent. If your organization uses a certificate of its own, add its file (.pem) for this server in Settings > Git servers."
            ));
        }
        Error::Network(format!(
            "Columbus could not reach {host}. Check the address and your network, then try again."
        ))
    }
}

impl Reply {
    pub(crate) fn json(&self, http: &Http) -> Result<Value, Error> {
        serde_json::from_slice(&self.body).map_err(|_| {
            Error::Server(format!(
                "{} did not answer the way a {} server does. Check the server's address in Settings > Git servers.",
                http.host,
                http.kind.name()
            ))
        })
    }

    /// What the server said went wrong: its `message` (and GitHub's list of
    /// `errors`), as one short line, scrubbed of the token.
    pub(crate) fn message(&self, http: &Http) -> String {
        let Ok(v) = serde_json::from_slice::<Value>(&self.body) else {
            return String::new();
        };
        let mut said = v["message"].as_str().unwrap_or_default().to_string();
        if let Some(errors) = v["errors"].as_array() {
            for e in errors {
                let more = e["message"].as_str().or(e.as_str()).unwrap_or_default();
                if !more.is_empty() {
                    said.push_str(if said.is_empty() { "" } else { ": " });
                    said.push_str(more);
                }
            }
        }
        let one_line: String = said
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .take(300)
            .collect();
        http.scrub(one_line.trim())
    }

    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }

    fn rate_limited(&self) -> bool {
        self.status == 429
            || self.header("retry-after").is_some()
            || self.header("x-ratelimit-remaining") == Some("0")
            || String::from_utf8_lossy(&self.body)
                .to_lowercase()
                .contains("rate limit")
    }

    /// How long the server asks to wait, in words.
    fn wait(&self) -> String {
        if let Some(s) = self
            .header("retry-after")
            .and_then(|v| v.trim().parse::<u64>().ok())
        {
            return format!("about {} seconds", s.max(1));
        }
        if let Some(at) = self
            .header("x-ratelimit-reset")
            .and_then(|v| v.trim().parse::<i64>().ok())
            .and_then(|t| chrono::DateTime::from_timestamp(t, 0))
        {
            let local = at.with_timezone(&chrono::Local);
            return format!("until {}", local.format("%H:%M"));
        }
        "a few minutes".into()
    }
}

/// The certificates in a company's certificate file (PEM). A file with a
/// private key in it is refused: only the certificate is wanted here.
fn read_certificates(file: &Path) -> Result<Vec<reqwest::Certificate>, Error> {
    let meta = std::fs::metadata(file).map_err(|e| Error::io(file, e))?;
    if !meta.is_file() || meta.len() > 1024 * 1024 {
        return Err(Error::Refused(format!(
            "{} is not a certificate file",
            file.display()
        )));
    }
    let bytes = std::fs::read(file).map_err(|e| Error::io(file, e))?;
    let text = String::from_utf8_lossy(&bytes);
    if text.contains("PRIVATE KEY-----") {
        return Err(Error::Refused(format!(
            "{} holds a private key. Choose the file with the certificate only (.pem or .crt).",
            file.display()
        )));
    }
    let certs = reqwest::Certificate::from_pem_bundle(&bytes).unwrap_or_default();
    if certs.is_empty() {
        return Err(Error::Refused(format!(
            "{} holds no certificate (a .pem file starts with -----BEGIN CERTIFICATE-----)",
            file.display()
        )));
    }
    Ok(certs)
}
