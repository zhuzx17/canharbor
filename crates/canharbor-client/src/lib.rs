// SPDX-License-Identifier: AGPL-3.0-or-later
//! Same-user local HTTP client (Unix socket or Windows Named Pipe). No implicit acquisition or TX retry.
pub use canharbor_api as api;
mod periodic;
mod stream;
mod transport;
pub use periodic::{AmbiguousTask, TaskStartUnknown};
use serde_json::Value;
use std::path::PathBuf;
pub use stream::{EventStream, StreamUnavailable};
use uuid::Uuid;
/// Structured error returned by an authenticated service response.
#[derive(Debug)]
pub struct ApiFailure {
    pub status: u16,
    pub detail: Option<api::ErrorDetail>,
    /// Whether outcome metadata was explicitly present in the response.
    pub outcome_unknown: Option<bool>,
}
impl ApiFailure {
    /// A server error without a complete explicit outcome cannot establish
    /// whether a submitted mutation executed.
    pub fn submission_is_unknown(&self) -> bool {
        if self.outcome_unknown == Some(true) {
            return true;
        }
        if matches!(
            self.status,
            400 | 401 | 403 | 404 | 405 | 409 | 412 | 413 | 414 | 415 | 422 | 428 | 429 | 431
        ) {
            return false;
        }
        !(self.detail.is_some() && self.outcome_unknown == Some(false))
    }
}
impl std::fmt::Display for ApiFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(detail) = &self.detail {
            write!(
                f,
                "HTTP {}: {}: {}",
                self.status, detail.code, detail.message
            )?;
            if let Some(receipt) = &detail.receipt {
                write!(f, "; receipt {receipt}")?;
            }
            if detail.outcome_unknown {
                write!(f, "; outcome unknown; do not resend automatically")?;
            }
            Ok(())
        } else {
            write!(f, "HTTP {} from local service", self.status)
        }
    }
}
impl std::error::Error for ApiFailure {}
const JSON_LIMIT: usize = 1024 * 1024;
/// The local service could not be reached, or its transport ended early.
#[derive(Debug)]
pub struct ServiceUnavailable(pub transport::Error);
impl std::fmt::Display for ServiceUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "cannot contact local service; run canharbor serve first: {}",
            self.0
        )
    }
}
impl std::error::Error for ServiceUnavailable {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.0.as_ref())
    }
}
#[derive(Debug)]
pub struct InvalidArgument(pub &'static str);
impl std::fmt::Display for InvalidArgument {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for InvalidArgument {}
#[derive(Debug)]
pub struct ResourceLimit(pub &'static str);
impl std::fmt::Display for ResourceLimit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for ResourceLimit {}
/// Cloning a client shares its native identity; `new` creates an independent one.
#[derive(Clone)]
pub struct LocalClient {
    http: transport::Client,
    dir: PathBuf,
    id: String,
}
impl LocalClient {
    pub fn new(dir: PathBuf) -> Result<Self, Box<dyn std::error::Error>> {
        Ok(Self {
            http: transport::Client::new(dir.clone()),
            dir,
            id: Uuid::new_v4().to_string(),
        })
    }
    pub fn client_id(&self) -> &str {
        &self.id
    }
    pub async fn request(
        &self,
        method: &str,
        path: &str,
        body: Value,
        g: Option<&str>,
    ) -> Result<Value, Box<dyn std::error::Error>> {
        self.request_with_query(method, path, body, g, &[]).await
    }
    /// Encode query values separately from the validated API-relative path.
    pub async fn request_with_query(
        &self,
        method: &str,
        path: &str,
        body: Value,
        g: Option<&str>,
        query: &[(&str, &str)],
    ) -> Result<Value, Box<dyn std::error::Error>> {
        validate_api_path(path)?;
        if query.len() > 16
            || query
                .iter()
                .try_fold(0usize, |n, (key, value)| {
                    n.checked_add(key.len())?.checked_add(value.len())
                })
                .is_none_or(|n| n > 4096)
        {
            return Err(Box::new(InvalidArgument("query exceeds client limit")));
        }
        let mut req = self
            .http
            .request(method.parse()?, format!("http://localhost/api/v1{path}"))
            .header("x-canharbor-client", &self.id)
            .json(&body);
        if !query.is_empty() {
            req = req.query(query);
        }
        if let Some(g) = g {
            req = req.header("x-canharbor-lease-generation", g)
        }
        let mut response = req.send().await.map_err(ServiceUnavailable)?;
        let status = response.status();
        if status == reqwest::StatusCode::NO_CONTENT {
            return Ok(Value::Null);
        }
        if response
            .content_length()
            .is_some_and(|length| length > JSON_LIMIT as u64)
        {
            return Err(Box::new(ResourceLimit("API response exceeds JSON limit")));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(ServiceUnavailable)? {
            if chunk.len() > JSON_LIMIT.saturating_sub(bytes.len()) {
                return Err(Box::new(ResourceLimit("API response exceeds JSON limit")));
            }
            bytes.extend_from_slice(&chunk);
        }
        let parsed = serde_json::from_slice::<Value>(&bytes);
        if !status.is_success() {
            let value = parsed.ok();
            let outcome_unknown = value
                .as_ref()
                .and_then(|value| value.get("error"))
                .and_then(|error| error.get("outcome_unknown"))
                .and_then(Value::as_bool);
            let detail = value
                .and_then(|value| serde_json::from_value::<api::ApiErrorBody>(value).ok())
                .map(|body| body.error);
            return Err(Box::new(ApiFailure {
                status: status.as_u16(),
                detail,
                outcome_unknown,
            }));
        }
        Ok(parsed?)
    }
    /// Send one request without retrying it. Unknown response fields are ignored
    /// by the shared response DTOs; malformed responses return decoding errors.
    pub async fn request_typed<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        path: &str,
        body: Value,
        generation: Option<&str>,
    ) -> Result<T, Box<dyn std::error::Error>> {
        Ok(serde_json::from_value(
            self.request(method, path, body, generation).await?,
        )?)
    }
    pub async fn version(&self) -> Result<api::Version, Box<dyn std::error::Error>> {
        self.request_typed("GET", "/version", Value::Null, None)
            .await
    }
    pub async fn devices(&self) -> Result<api::Devices, Box<dyn std::error::Error>> {
        self.request_typed("GET", "/devices", Value::Null, None)
            .await
    }
    pub async fn create(
        &self,
        request: &api::CreateRequest,
    ) -> Result<api::Session, Box<dyn std::error::Error>> {
        self.request_typed("POST", "/sessions", serde_json::to_value(request)?, None)
            .await
    }
    pub async fn acquire(&self, id: &str) -> Result<api::Session, Box<dyn std::error::Error>> {
        session_id(id)?;
        self.request_typed(
            "POST",
            &format!("/sessions/{id}/lease"),
            serde_json::json!({}),
            None,
        )
        .await
    }
    pub async fn running(
        &self,
        id: &str,
        generation: &str,
        running: bool,
    ) -> Result<api::Session, Box<dyn std::error::Error>> {
        session_id(id)?;
        self.request_typed(
            "POST",
            &format!("/sessions/{id}/{}", if running { "start" } else { "stop" }),
            serde_json::json!({}),
            Some(generation),
        )
        .await
    }
    pub async fn submit(
        &self,
        id: &str,
        generation: &str,
        request: &api::Tx,
    ) -> Result<api::TxReceipt, Box<dyn std::error::Error>> {
        session_id(id)?;
        self.request_typed(
            "POST",
            &format!("/sessions/{id}/tx"),
            serde_json::to_value(request)?,
            Some(generation),
        )
        .await
    }
    pub async fn receipt(
        &self,
        session: &str,
        receipt: Uuid,
    ) -> Result<api::TxReceipt, Box<dyn std::error::Error>> {
        session_id(session)?;
        self.request_typed(
            "GET",
            &format!("/sessions/{session}/tx/{receipt}"),
            Value::Null,
            None,
        )
        .await
    }
    pub async fn receipt_by_request(
        &self,
        session: &str,
        request: &str,
        generation: Option<&str>,
    ) -> Result<api::TxReceipt, Box<dyn std::error::Error>> {
        session_id(session)?;
        if !api::valid_client_request_id(request) {
            return Err(Box::new(InvalidArgument(
                "request ID must be 1..128 Unicode characters",
            )));
        }
        let mut query = vec![("client_request_id", request)];
        if let Some(g) = generation {
            if !g.parse::<u64>().is_ok_and(|value| value.to_string() == g) {
                return Err(Box::new(InvalidArgument(
                    "generation must be a canonical u64 decimal string",
                )));
            }
            query.push(("generation", g));
        }
        Ok(serde_json::from_value(
            self.request_with_query(
                "GET",
                &format!("/sessions/{session}/tx"),
                Value::Null,
                None,
                &query,
            )
            .await?,
        )?)
    }
    pub async fn download(
        &self,
        id: Uuid,
        output: PathBuf,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use tokio::io::AsyncWriteExt;
        let mut response = self
            .http
            .get(format!("http://localhost/api/v1/captures/{id}/download"))
            .header("x-canharbor-client", &self.id)
            .send()
            .await
            .map_err(ServiceUnavailable)?;
        if !response.status().is_success() {
            return Err(Box::new(ApiFailure {
                status: response.status().as_u16(),
                detail: None,
                outcome_unknown: None,
            }));
        }
        let limit = 64 * 1024 * 1024u64;
        let expected = response.content_length().ok_or("download has no length")?;
        if expected > limit {
            return Err(Box::new(ResourceLimit("capture exceeds download limit")));
        }
        let parent = output
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(std::path::Path::new("."));
        let temporary = parent.join(format!(".canharbor-{}.part", Uuid::new_v4()));
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }
        let file = canharbor_platform::private::create_new(&temporary)?;
        let cleanup = Cleanup(temporary);
        let mut file = tokio::fs::File::from_std(file);
        let mut received = 0u64;
        while let Some(chunk) = response.chunk().await.map_err(ServiceUnavailable)? {
            received = received
                .checked_add(chunk.len() as u64)
                .ok_or("download length overflow")?;
            if received > expected || received > limit {
                return Err("capture download exceeds declared length".into());
            }
            file.write_all(&chunk).await?;
        }
        if received != expected {
            return Err("incomplete capture download".into());
        }
        file.sync_all().await?;
        drop(file);
        // Create the destination atomically without replacing an existing user file.
        tokio::fs::hard_link(&cleanup.0, &output).await?;
        Ok(())
    }
}

fn session_id(id: &str) -> Result<(), Box<dyn std::error::Error>> {
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(Box::new(InvalidArgument("invalid session ID")));
    }
    Ok(())
}

/// A path rejected locally, before constructing or submitting an HTTP request.
#[derive(Debug, Clone, Copy)]
pub struct InvalidPath;
impl std::fmt::Display for InvalidPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("expected an absolute API-relative path without query or fragment")
    }
}
impl std::error::Error for InvalidPath {}
pub fn validate_api_path(path: &str) -> Result<(), InvalidPath> {
    if !path.starts_with('/')
        || path.contains(['?', '#', '\\'])
        || path.split('/').any(|part| part == "." || part == "..")
    {
        return Err(InvalidPath);
    }
    Ok(())
}
