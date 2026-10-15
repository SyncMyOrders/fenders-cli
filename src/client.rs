//! Thin blocking client for the Fenders customer API.
//!
//! Every response is an untyped `Record` in the spec, so we deal in
//! `serde_json::Value`. Every mutation gets its own `Idempotency-Key`, reused
//! only when that same request is retried, so a retry never runs twice while
//! distinct requests (upload → deploy, version → release) stay distinct.

use std::time::{Duration, Instant};

use anyhow::Result;
use reqwest::Method;
use reqwest::blocking::Response;
use serde_json::Value;

const MAX_ATTEMPTS: u32 = 3;
pub const MAX_WASM_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Debug)]
pub struct ApiError {
    pub status: u16,
    pub code: String,
    pub message: String,
    pub request_id: Option<String>,
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} ({}, HTTP {})",
            humanize(&self.message),
            crate::ui::sanitize(&self.code),
            self.status
        )
    }
}

impl std::error::Error for ApiError {}

fn humanize(s: &str) -> String {
    let s = crate::ui::sanitize(s).replace('_', " ");
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => s,
    }
}

impl ApiError {
    /// A suggestion for what to do next.
    pub fn hint(&self) -> Option<String> {
        Some(match self.status {
            401 => "Your API key is missing, expired or revoked. Run `fenders auth login` or set FENDERS_API_KEY.".into(),
            403 => "This API key lacks the permission (or project scope) for this action. Workspace owners manage keys in the console under Settings → API keys.".into(),
            404 => "Check the ids involved — `fenders context` shows the current workspace and project.".into(),
            409 | 412 => "The resource changed since it was read (revision mismatch). Re-run the command to use the latest revision.".into(),
            413 => "The upload is too large. WASI components may be at most 16 MiB.".into(),
            429 => "Rate limited. Wait a moment and try again.".into(),
            s if s >= 500 => "The Fenders API had a problem. Retry shortly; include the request id if you contact support.".into(),
            _ => return None,
        })
    }
}

pub enum Body {
    None,
    Json(Value),
    Wasm(Vec<u8>),
}

pub struct Client {
    http: reqwest::blocking::Client,
    base: String,
    token: Option<String>,
    verbose: bool,
    retry_base: Duration,
}

/// Percent-encode a single path segment.
pub fn seg(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

impl Client {
    pub fn new(base: &str, token: Option<String>, verbose: bool) -> Result<Self> {
        let http = reqwest::blocking::Client::builder()
            .user_agent(concat!("fenders-cli/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(300))
            .build()?;
        Ok(Self {
            http,
            base: base.trim_end_matches('/').to_string(),
            token,
            verbose,
            retry_base: Duration::from_millis(500),
        })
    }

    #[cfg(test)]
    pub fn with_retry_base(mut self, d: Duration) -> Self {
        self.retry_base = d;
        self
    }

    fn url(&self, path: &str) -> String {
        if path.starts_with("http://") || path.starts_with("https://") {
            path.to_string()
        } else if path.starts_with('/') {
            format!("{}{}", self.base, path)
        } else {
            format!("{}/{}", self.base, path)
        }
    }

    pub fn get(&self, path: &str, query: &[(String, String)]) -> Result<Value> {
        self.send(Method::GET, path, query, Body::None)
    }

    pub fn post(&self, path: &str, body: Value) -> Result<Value> {
        self.send(Method::POST, path, &[], Body::Json(body))
    }

    pub fn put(&self, path: &str, body: Value) -> Result<Value> {
        self.send(Method::PUT, path, &[], Body::Json(body))
    }

    pub fn patch(&self, path: &str, body: Value) -> Result<Value> {
        self.send(Method::PATCH, path, &[], Body::Json(body))
    }

    pub fn delete(&self, path: &str) -> Result<Value> {
        self.send(Method::DELETE, path, &[], Body::None)
    }

    pub fn send(
        &self,
        method: Method,
        path: &str,
        query: &[(String, String)],
        body: Body,
    ) -> Result<Value> {
        let Some(token) = &self.token else {
            return Err(ApiError {
                status: 401,
                code: "not_logged_in".into(),
                message: "not logged in".into(),
                request_id: None,
            }
            .into());
        };
        let url = self.url(path);
        let idempotency_key = uuid::Uuid::new_v4().simple().to_string();
        let mut attempt = 0;
        loop {
            attempt += 1;
            let mut req = self
                .http
                .request(method.clone(), &url)
                .bearer_auth(token)
                .header("Accept", "application/json");
            if !query.is_empty() {
                req = req.query(query);
            }
            if method != Method::GET {
                req = req.header("Idempotency-Key", &idempotency_key);
            }
            req = match &body {
                Body::None => req,
                Body::Json(v) => req.json(v),
                Body::Wasm(b) => req
                    .header("Content-Type", "application/wasm")
                    .body(b.clone()),
            };
            if self.verbose {
                let qs: Vec<String> = query.iter().map(|(k, v)| format!("{k}={v}")).collect();
                let q = if qs.is_empty() {
                    String::new()
                } else {
                    format!("?{}", qs.join("&"))
                };
                eprintln!(
                    "{}",
                    crate::ui::dim(&format!("→ {method} {url}{q}"), crate::ui::To::Err)
                );
            }
            let started = Instant::now();
            let result = req.send();
            let retry_after = match result {
                Ok(resp) => {
                    let status = resp.status().as_u16();
                    if self.verbose {
                        eprintln!(
                            "{}",
                            crate::ui::dim(
                                &format!("← {status} ({} ms)", started.elapsed().as_millis()),
                                crate::ui::To::Err
                            )
                        );
                    }
                    let transient = matches!(status, 429 | 502 | 503 | 504);
                    if !transient || attempt >= MAX_ATTEMPTS {
                        return parse(resp);
                    }
                    resp.headers()
                        .get("retry-after")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.parse::<u64>().ok())
                        .map(|s| Duration::from_secs(s.min(10)))
                }
                Err(e) => {
                    let transient = e.is_connect() || e.is_timeout() || e.is_request();
                    if !transient || attempt >= MAX_ATTEMPTS {
                        return Err(
                            anyhow::Error::new(e).context(format!("request to {url} failed"))
                        );
                    }
                    None
                }
            };
            std::thread::sleep(retry_after.unwrap_or(self.retry_base * 2u32.pow(attempt - 1)));
        }
    }
}

fn parse(resp: Response) -> Result<Value> {
    let status = resp.status();
    let header_rid = resp
        .headers()
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let text = resp.text()?;
    let json: Option<Value> = if text.trim().is_empty() {
        None
    } else {
        serde_json::from_str(&text).ok()
    };
    if status.is_success() {
        return Ok(json.unwrap_or(if text.is_empty() {
            Value::Null
        } else {
            Value::String(text)
        }));
    }
    let field = |k: &str| {
        json.as_ref()
            .and_then(|j| j.get(k))
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    let reason = status
        .canonical_reason()
        .unwrap_or("request failed")
        .to_lowercase();
    Err(ApiError {
        status: status.as_u16(),
        code: field("code").unwrap_or_else(|| reason.replace(' ', "_")),
        message: field("error").unwrap_or_else(|| {
            let snippet: String = text.chars().take(200).collect();
            if snippet.trim().is_empty() {
                reason
            } else {
                snippet
            }
        }),
        request_id: field("request_id").or(header_rid),
    }
    .into())
}
