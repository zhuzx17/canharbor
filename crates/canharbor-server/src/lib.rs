// SPDX-License-Identifier: AGPL-3.0-or-later
//! Local transport and versioned DTOs for the M0 virtual workbench.
pub mod capture;
mod diagnostic;
pub mod dto;
pub mod local;
mod periodic;
pub mod preferences;
mod stress;
mod stress_coord;
mod stress_recovery;
mod stress_store;
mod tx;
use axum::{
    Extension, Json, Router,
    extract::{
        DefaultBodyLimit, Path, Request, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{HeaderMap, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use canharbor_core::runtime::{Runtime, RuntimeError};
use dto::{CreateRequest, PairRequest, Subscribe, Tx};
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;

use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::{
    sync::{Mutex, Semaphore, broadcast},
    time::Instant,
};
use uuid::Uuid;

#[derive(Clone, Copy)]
struct Native(bool);
#[derive(Clone)]
struct Identity {
    revoked: Arc<std::sync::atomic::AtomicBool>,
    id: String,
    cookie: Option<String>,
    csrf: Option<String>,
}
struct BrowserSession {
    identity: Identity,
    created: Instant,
    touched: Instant,
}
struct PairCode {
    until: Instant,
}
#[derive(Default)]
struct Auth {
    pairs: HashMap<String, PairCode>,
    browsers: HashMap<String, BrowserSession>,
}
pub struct AppState {
    pub runtime: Arc<Runtime>,
    pub origin: String,
    pub instance: String,
    pub captures: capture::Manager,
    web_enabled: bool,
    cookie_name: String,
    auth: Mutex<Auth>,
    transmissions: Arc<tx::Manager>,
    periodic: Arc<periodic::Manager>,
    stress: stress::Manager,
    ws_slots: Arc<Semaphore>,
    preferences: preferences::Store,
}
fn secret() -> String {
    format!(
        "{}{}{}",
        Uuid::new_v4().simple(),
        Uuid::new_v4().simple(),
        Uuid::new_v4().simple()
    )
}
impl AppState {
    pub fn new(runtime: Arc<Runtime>, origin: String) -> Arc<Self> {
        Self::with_web(runtime, origin, true)
    }
    pub fn with_web(runtime: Arc<Runtime>, origin: String, web_enabled: bool) -> Arc<Self> {
        Self::with_capture_dir(runtime, origin, web_enabled, None)
    }
    pub fn with_capture_dir(
        runtime: Arc<Runtime>,
        origin: String,
        web_enabled: bool,
        root: Option<std::path::PathBuf>,
    ) -> Arc<Self> {
        Self::with_preferences_file(runtime, origin, web_enabled, root, None)
    }
    pub fn with_preferences_file(
        runtime: Arc<Runtime>,
        origin: String,
        web_enabled: bool,
        root: Option<std::path::PathBuf>,
        preferences_file: Option<std::path::PathBuf>,
    ) -> Arc<Self> {
        let instance = Uuid::new_v4().simple().to_string();
        let capture_root = root
            .unwrap_or_else(|| std::env::temp_dir().join(format!("canharbor-captures-{instance}")));
        let transmissions = tx::Manager::new();
        let periodic = periodic::Manager::new(runtime.clone(), transmissions.clone());
        Arc::new(Self {
            stress: stress::Manager::new(runtime.clone(), capture_root.join("stress")),
            periodic,
            captures: capture::Manager::new(
                runtime.clone(),
                capture_root.clone(),
                instance.clone(),
            ),
            runtime,
            origin,
            cookie_name: format!("canharbor_{instance}"),
            instance,
            web_enabled,
            auth: Mutex::new(Auth::default()),
            transmissions,
            ws_slots: Arc::new(Semaphore::new(16)),
            preferences: preferences::Store::new(preferences_file),
        })
    }
    async fn issue_pair(&self) -> Result<String, ApiError> {
        if !self.web_enabled {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "web_disabled",
                "Web UI is disabled for this service",
            ));
        }
        let mut a = self.auth.lock().await;
        a.pairs.retain(|_, p| p.until > Instant::now());
        if a.pairs.len() >= 16 {
            return Err(ApiError::limit());
        }
        let code = secret();
        a.pairs.insert(
            code.clone(),
            PairCode {
                until: Instant::now() + Duration::from_secs(60),
            },
        );
        Ok(format!("{}/#pair={code}", self.origin))
    }
    async fn browser_valid(&self, token: &str) -> bool {
        let mut a = self.auth.lock().await;
        let now = Instant::now();
        a.browsers.retain(|_, s| {
            now.duration_since(s.created) < Duration::from_secs(8 * 3600)
                && now.duration_since(s.touched) < Duration::from_secs(1800)
        });
        a.browsers.contains_key(token)
    }
}
#[derive(Clone, Debug)]
pub struct ApiError(StatusCode, &'static str, String, Option<String>, bool);
impl ApiError {
    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self(status, code, message.into(), None, false)
    }
    fn limit() -> Self {
        Self::new(
            StatusCode::TOO_MANY_REQUESTS,
            "resource_limit",
            "resource limit reached",
        )
    }
    fn forbidden() -> Self {
        Self::new(
            StatusCode::FORBIDDEN,
            "forbidden",
            "request source or credentials rejected",
        )
    }
}
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut headers = HeaderMap::new();
        if let Some(receipt) = &self.3 {
            headers.insert("x-canharbor-receipt", receipt.parse().unwrap());
        }
        (
            self.0,
            headers,
            Json(dto::ApiErrorBody {
                error: dto::ErrorDetail {
                    code: self.1.into(),
                    message: self.2,
                    retryable: false,
                    request_id: self.3.clone().unwrap_or_else(|| Uuid::new_v4().to_string()),
                    receipt: self.3,
                    outcome_unknown: self.4,
                },
            }),
        )
            .into_response()
    }
}
impl From<RuntimeError> for ApiError {
    fn from(e: RuntimeError) -> Self {
        let (status, code) = match e {
            RuntimeError::PreparationFailed => {
                (StatusCode::SERVICE_UNAVAILABLE, "preparation_failed")
            }
            RuntimeError::SubmissionUnknown => (StatusCode::SERVICE_UNAVAILABLE, "outcome_unknown"),
            RuntimeError::RevisionConflict => {
                (StatusCode::PRECONDITION_FAILED, "revision_conflict")
            }
            RuntimeError::Closed => (StatusCode::SERVICE_UNAVAILABLE, "service_closing"),
            RuntimeError::NotFound => (StatusCode::NOT_FOUND, "not_found"),
            RuntimeError::Busy => (StatusCode::CONFLICT, "device_busy"),
            RuntimeError::LeaseConflict => (StatusCode::CONFLICT, "lease_conflict"),
            RuntimeError::LeaseExpired => (StatusCode::CONFLICT, "lease_expired"),
            RuntimeError::Limit => (StatusCode::TOO_MANY_REQUESTS, "resource_limit"),
            RuntimeError::ListenOnly | RuntimeError::InvalidState => {
                (StatusCode::CONFLICT, "invalid_state")
            }
            RuntimeError::Backend(canharbor_core::Error::Fault(f)) => (
                StatusCode::SERVICE_UNAVAILABLE,
                match f {
                    canharbor_core::BackendFault::Disconnected => "device_disconnected",
                    canharbor_core::BackendFault::BusOff => "bus_off",
                    canharbor_core::BackendFault::ReceiveOverflow => "receive_overflow",
                    canharbor_core::BackendFault::TransmitFailed => "transmit_failed",
                    canharbor_core::BackendFault::StopFailed => "stop_failed",
                },
            ),
            RuntimeError::Backend(canharbor_core::Error::UnknownChannel) => {
                (StatusCode::NOT_FOUND, "channel_not_found")
            }
            RuntimeError::Backend(canharbor_core::Error::QueueFull) => {
                (StatusCode::TOO_MANY_REQUESTS, "queue_full")
            }
            RuntimeError::Backend(canharbor_core::Error::InvalidState) => {
                (StatusCode::CONFLICT, "invalid_state")
            }
            RuntimeError::Backend(canharbor_core::Error::ControlExpired) => {
                (StatusCode::CONFLICT, "lease_expired")
            }
            RuntimeError::Backend(canharbor_core::Error::Unsupported) => {
                (StatusCode::UNPROCESSABLE_ENTITY, "unsupported")
            }
            RuntimeError::Backend(_) => (StatusCode::UNPROCESSABLE_ENTITY, "invalid_frame"),
        };
        Self::new(status, code, e.to_string())
    }
}
fn text_header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name)?.to_str().ok()
}
fn generation(headers: &HeaderMap) -> Result<u64, ApiError> {
    text_header(headers, "x-canharbor-lease-generation")
        .and_then(|s| s.parse().ok())
        .ok_or_else(ApiError::forbidden)
}

/// Keep extractor failures within the same generated error envelope.
struct WireJson<T>(T);
impl<S, T> axum::extract::FromRequest<S> for WireJson<T>
where
    S: Send + Sync,
    T: serde::de::DeserializeOwned + Send,
{
    type Rejection = ApiError;
    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        let Json(value) = Json::<T>::from_request(req, state)
            .await
            .map_err(|e| ApiError::new(e.status(), "invalid_request", e.body_text()))?;
        Ok(Self(value))
    }
}
/// Native identity is available only on the separately constructed, UID-checked UDS router.
async fn guard(
    State(s): State<Arc<AppState>>,
    Extension(native): Extension<Native>,
    mut req: Request,
    next: Next,
) -> Response {
    let path = req.uri().path().to_owned();
    let headers = req.headers().clone();
    let method = req.method().clone();
    let result: Result<Option<Identity>, ApiError> = async {
        if native.0 {
            let id = text_header(&headers, "x-canharbor-client")
                .and_then(|s| Uuid::parse_str(s).ok())
                .ok_or_else(ApiError::forbidden)?;
            return Ok(Some(Identity {
                revoked: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                id: format!("native:{id}"),
                cookie: None,
                csrf: None,
            }));
        }
        let expected = s.origin.strip_prefix("http://").unwrap();
        if text_header(&headers, "host") != Some(expected) {
            return Err(ApiError::forbidden());
        }
        let origin = text_header(&headers, "origin");
        if origin.is_some_and(|o| o != s.origin) {
            return Err(ApiError::forbidden());
        }
        if path.starts_with("/api/v1/native/") {
            return Err(ApiError::forbidden());
        }
        let unsafe_method = !matches!(
            method,
            axum::http::Method::GET | axum::http::Method::HEAD | axum::http::Method::OPTIONS
        );
        if unsafe_method
            && (origin != Some(s.origin.as_str())
                || text_header(&headers, "content-type") != Some("application/json"))
        {
            return Err(ApiError::forbidden());
        }
        if path == "/api/v1/version" || path == "/api/v1/auth/pair" || !path.starts_with("/api/") {
            return Ok(None);
        }
        let cookie = text_header(&headers, "cookie")
            .and_then(|h| {
                h.split(';')
                    .map(str::trim)
                    .find_map(|p| p.strip_prefix(&format!("{}=", s.cookie_name)))
            })
            .ok_or_else(|| {
                ApiError::new(
                    StatusCode::UNAUTHORIZED,
                    "unauthorized",
                    "pair this browser first",
                )
            })?
            .to_owned();
        if !s.browser_valid(&cookie).await {
            return Err(ApiError::new(
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "browser session expired",
            ));
        }
        let mut auth = s.auth.lock().await;
        let b = auth
            .browsers
            .get_mut(&cookie)
            .ok_or_else(ApiError::forbidden)?;
        if unsafe_method && text_header(&headers, "x-canharbor-csrf") != b.identity.csrf.as_deref()
        {
            return Err(ApiError::forbidden());
        }
        b.touched = Instant::now();
        Ok(Some(b.identity.clone()))
    }
    .await;
    match result {
        Err(e) => e.into_response(),
        Ok(identity) => {
            if let Some(i) = identity {
                req.extensions_mut().insert(i);
            }
            let mut res = next.run(req).await;
            let h = res.headers_mut();
            h.insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
            h.insert("x-content-type-options", "nosniff".parse().unwrap());
            h.insert("referrer-policy", "no-referrer".parse().unwrap());
            h.insert("content-security-policy",format!("default-src 'self'; script-src 'self'; style-src 'self'; connect-src 'self' {}; object-src 'none'; base-uri 'none'; frame-ancestors 'none'",s.origin.replace("http:","ws:")).parse().unwrap());
            res
        }
    }
}
pub fn router(s: Arc<AppState>, native: bool) -> Router {
    Router::new()
        .route("/api/v1/version", get(version))
        .route("/api/v1/native/pair", post(native_pair))
        .route("/api/v1/auth/pair", post(pair))
        .route("/api/v1/auth/session", get(auth_session).delete(logout))
        .route(
            "/api/v1/preferences",
            get(get_preferences).put(set_preferences),
        )
        .route("/api/v1/devices", get(devices))
        .route("/api/v1/diagnostics/devices", get(diagnostic::report))
        .route("/api/v1/channels/{id}/capabilities", get(capabilities))
        .route(
            "/api/v1/sessions/{id}/config",
            axum::routing::patch(configure),
        )
        .route("/api/v1/sessions", get(sessions).post(create))
        .route("/api/v1/sessions/{id}", get(session).delete(close))
        .route("/api/v1/sessions/{id}/lease", post(acquire).delete(release))
        .route("/api/v1/sessions/{id}/lease/renew", post(renew))
        .route("/api/v1/sessions/{id}/start", post(start))
        .route("/api/v1/sessions/{id}/stop", post(stop))
        .route(
            "/api/v1/sessions/{id}/tx",
            post(transmit).get(transmission_lookup),
        )
        .route("/api/v1/sessions/{id}/tx/{receipt}", get(transmission))
        .route("/api/v1/sessions/{id}/stream", get(stream))
        .route("/api/v1/sessions/{id}/tasks", post(periodic_start))
        .route("/api/v1/stress/requests/{request}", get(stress::by_request))
        .route("/api/v1/stress/coordinate", post(stress_coord::action))
        .route("/api/v1/stress/validate", post(stress::validate))
        .route("/api/v1/stress/runs", post(stress::start).get(stress::list))
        .route(
            "/api/v1/stress/runs/{id}",
            get(stress::get_run).delete(stress::delete),
        )
        .route("/api/v1/stress/runs/{id}/events", get(stress::events))
        .route("/api/v1/stress/runs/{id}/report", get(stress::get_run))
        .route("/api/v1/stress/runs/{id}/artifacts", get(stress::artifacts))
        .route(
            "/api/v1/stress/runs/{id}/artifacts/{name}",
            get(stress::download),
        )
        .route("/api/v1/stress/runs/{id}/stop", post(stress::stop))
        .route(
            "/api/v1/stress/runs/{id}/flows/{name}/stop",
            post(stress::stop_flow),
        )
        .route("/api/v1/tasks", get(periodic_list))
        .route("/api/v1/tasks/{id}", get(periodic_get))
        .route("/api/v1/tasks/{id}/cancel", post(periodic_cancel))
        .route("/api/v1/diagnostics", get(diagnostics))
        .route("/api/v1/legal", get(legal))
        .route("/api/v1/captures", get(captures).post(capture_start))
        .route(
            "/api/v1/captures/{id}",
            get(capture_status)
                .patch(capture_rename)
                .delete(capture_delete),
        )
        .route("/api/v1/captures/{id}/stop", post(capture_stop))
        .route("/api/v1/captures/{id}/download", get(capture_download))
        .fallback(static_file)
        .layer(DefaultBodyLimit::max(65536))
        .layer(middleware::from_fn_with_state(s.clone(), guard))
        .layer(Extension(Native(native)))
        .with_state(s)
}
async fn version(State(s): State<Arc<AppState>>) -> Json<dto::Version> {
    Json(dto::Version {
        version: env!("CARGO_PKG_VERSION").into(),
        api_version: "1.0".into(),
        server_instance_id: s.instance.clone(),
        stage: "m0-virtual".into(),
        web_ui: cfg!(feature = "web-ui"),
    })
}
async fn native_pair(State(s): State<Arc<AppState>>) -> Result<Json<dto::PairLink>, ApiError> {
    Ok(Json(dto::PairLink {
        url: s.issue_pair().await?,
    }))
}
async fn pair(
    State(s): State<Arc<AppState>>,
    WireJson(input): WireJson<PairRequest>,
) -> Result<Response, ApiError> {
    let mut a = s.auth.lock().await;
    let p = a
        .pairs
        .remove(&input.code)
        .ok_or_else(ApiError::forbidden)?;
    if p.until <= Instant::now() {
        return Err(ApiError::forbidden());
    }
    a.browsers.retain(|_, b| {
        b.created.elapsed() < Duration::from_secs(8 * 3600)
            && b.touched.elapsed() < Duration::from_secs(1800)
    });
    if a.browsers.len() >= 8 {
        return Err(ApiError::limit());
    }
    let token = secret();
    let id = Identity {
        revoked: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        id: format!("browser:{}", Uuid::new_v4()),
        cookie: Some(token.clone()),
        csrf: Some(secret()),
    };
    a.browsers.insert(
        token.clone(),
        BrowserSession {
            identity: id.clone(),
            created: Instant::now(),
            touched: Instant::now(),
        },
    );
    let mut response = Json(dto::Auth {
        client_id: id.id,
        csrf_token: id.csrf,
    })
    .into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        format!(
            "{}={token}; HttpOnly; SameSite=Strict; Path=/",
            s.cookie_name
        )
        .parse()
        .unwrap(),
    );
    Ok(response)
}
async fn auth_session(Extension(i): Extension<Identity>) -> Json<dto::Auth> {
    Json(dto::Auth {
        client_id: i.id,
        csrf_token: i.csrf,
    })
}
async fn logout(State(s): State<Arc<AppState>>, Extension(i): Extension<Identity>) -> StatusCode {
    i.revoked.store(true, std::sync::atomic::Ordering::Release);
    s.stress.stop_owner(capture_owner(&i));
    if let Some(token) = i.cookie {
        s.auth.lock().await.browsers.remove(&token);
    }
    s.periodic.cancel_owner(&i.id);
    s.runtime.revoke(&i.id).await;
    StatusCode::NO_CONTENT
}
async fn get_preferences(State(s): State<Arc<AppState>>) -> Json<preferences::Preferences> {
    Json(s.preferences.get())
}
async fn set_preferences(
    State(s): State<Arc<AppState>>,
    WireJson(next): WireJson<preferences::Preferences>,
) -> Result<Json<preferences::Preferences>, ApiError> {
    let saved = tokio::task::spawn_blocking(move || {
        s.preferences.set(next)?;
        Ok::<_, std::io::Error>(s.preferences.get())
    })
    .await
    .map_err(|_| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "preferences_io_error",
            "Could not save language preference",
        )
    })?
    .map_err(|_| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "preferences_io_error",
            "Could not save language preference",
        )
    })?;
    Ok(Json(saved))
}
async fn devices(State(s): State<Arc<AppState>>) -> Json<dto::Devices> {
    let mut devices: std::collections::BTreeMap<String, dto::Device> =
        std::collections::BTreeMap::new();
    for channel in s.runtime.channels() {
        let Ok(caps) = s.runtime.capabilities(&channel.id) else {
            continue;
        };
        let device = devices
            .entry(caps.device_id.clone())
            .or_insert_with(|| dto::Device {
                id: caps.device_id.clone(),
                label: if caps.simulated {
                    "Virtual CAN bus".into()
                } else {
                    caps.device_id
                },
                backend: caps.backend,
                simulated: caps.simulated,
                verification: if caps.simulated {
                    "simulation_only"
                } else {
                    "hardware_unverified"
                }
                .into(),
                channels: Vec::new(),
            });
        device.channels.push(dto::Channel {
            id: channel.id,
            label: channel.label,
            fd: caps.fd,
            listen_only: caps.listen_only,
        });
    }
    Json(dto::Devices {
        devices: devices.into_values().collect(),
    })
}

async fn capabilities(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<dto::Capabilities>, ApiError> {
    let caps = s.runtime.capabilities(&id).map_err(RuntimeError::from)?;
    Ok(Json(dto::Capabilities {
        channel_id: id,
        backend: caps.backend,
        simulated: caps.simulated,
        fd: caps.fd,
        listen_only: caps.listen_only,
        hardware_timestamps: caps.timestamps,
        nominal_timing: caps.nominal_timing.map(Into::into),
        data_timing: caps.data_timing.map(Into::into),
        fixed_nominal_bitrates: caps.fixed_nominal_bitrates,
        fixed_data_bitrates: caps.fixed_data_bitrates,
    }))
}
fn session_with_etag(view: canharbor_core::runtime::SessionView) -> Response {
    let dto = dto::session(view);
    let etag = serde_json::to_string(&dto.revision).unwrap();
    let mut response = Json(dto).into_response();
    response
        .headers_mut()
        .insert(header::ETAG, etag.parse().unwrap());
    response
}
async fn configure(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
    Extension(i): Extension<Identity>,
    headers: HeaderMap,
    WireJson(config): WireJson<dto::Config>,
) -> Result<Response, ApiError> {
    let etag = text_header(&headers, "if-match").ok_or_else(|| {
        ApiError::new(
            StatusCode::PRECONDITION_REQUIRED,
            "revision_required",
            "If-Match is required",
        )
    })?;
    let revision = etag
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| format!("\"{v}\"") == etag)
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_revision",
                "If-Match must contain a quoted revision",
            )
        })?;
    let session = s.runtime.get(&id).await?;
    let core = validated_config(&s.runtime, &session.channel, config)?;
    Ok(session_with_etag(
        s.runtime
            .configure(&id, &i.id, generation(&headers)?, revision, core)
            .await?,
    ))
}
fn validated_config(
    runtime: &Runtime,
    channel: &str,
    config: dto::Config,
) -> Result<canharbor_core::Config, ApiError> {
    let caps = runtime.capabilities(channel).map_err(RuntimeError::from)?;
    if config.fd.as_ref().is_some_and(|fd| !fd.enabled || !fd.iso)
        || config.fd.is_some() && !caps.fd
        || config.mode == dto::SessionMode::ListenOnly && !caps.listen_only
    {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "unsupported",
            "requested mode is unsupported",
        ));
    }
    let data_bitrate = config.fd.map(|fd| fd.data_bitrate);
    Ok(canharbor_core::Config {
        nominal_bitrate: config.nominal_bitrate,
        data_bitrate,
        listen_only: config.mode == dto::SessionMode::ListenOnly,
    })
}
async fn sessions(State(s): State<Arc<AppState>>) -> Json<dto::Sessions> {
    Json(dto::Sessions {
        sessions: s
            .runtime
            .list()
            .await
            .into_iter()
            .map(dto::session)
            .collect(),
    })
}
async fn session(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    Ok(session_with_etag(s.runtime.get(&id).await?))
}
async fn create(
    State(s): State<Arc<AppState>>,
    WireJson(r): WireJson<CreateRequest>,
) -> Result<(StatusCode, Json<dto::Session>), ApiError> {
    let config = validated_config(&s.runtime, &r.channel_id, r.config)?;
    Ok((
        StatusCode::CREATED,
        Json(dto::session(
            s.runtime.create_configured(&r.channel_id, config).await?,
        )),
    ))
}

async fn acquire(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
    Extension(i): Extension<Identity>,
) -> Result<Json<dto::Session>, ApiError> {
    Ok(Json(dto::session(s.runtime.acquire(&id, &i.id).await?)))
}
async fn renew(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
    Extension(i): Extension<Identity>,
    h: HeaderMap,
) -> Result<Json<dto::Session>, ApiError> {
    Ok(Json(dto::session(
        s.runtime.renew(&id, &i.id, generation(&h)?).await?,
    )))
}
async fn start(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
    Extension(i): Extension<Identity>,
    h: HeaderMap,
) -> Result<Json<dto::Session>, ApiError> {
    Ok(Json(dto::session(
        s.runtime.running(&id, &i.id, generation(&h)?, true).await?,
    )))
}
async fn stop(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
    Extension(i): Extension<Identity>,
    h: HeaderMap,
) -> Result<Json<dto::Session>, ApiError> {
    Ok(Json(dto::session(
        s.runtime
            .running(&id, &i.id, generation(&h)?, false)
            .await?,
    )))
}
async fn release(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
    Extension(i): Extension<Identity>,
    h: HeaderMap,
) -> Result<StatusCode, ApiError> {
    s.runtime.release(&id, &i.id, generation(&h)?).await?;
    Ok(StatusCode::NO_CONTENT)
}
async fn close(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
    Extension(i): Extension<Identity>,
    h: HeaderMap,
) -> Result<StatusCode, ApiError> {
    s.runtime.close(&id, &i.id, generation(&h)?).await?;
    Ok(StatusCode::NO_CONTENT)
}
async fn transmit(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
    Extension(i): Extension<Identity>,
    h: HeaderMap,
    WireJson(r): WireJson<Tx>,
) -> Result<(StatusCode, Json<dto::TxReceipt>), ApiError> {
    let g = generation(&h)?;
    if !dto::valid_client_request_id(&r.client_request_id) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "request ID must be 1..128 Unicode characters",
        ));
    }
    let frame = r
        .frame
        .into_frame()
        .map_err(|e| ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid_frame", e))?;
    let payload = serde_json::to_string(&dto::Frame::from(&frame)).unwrap();
    let key = tx::Key {
        owner: i.id.clone(),
        session: id.clone(),
        generation: g,
        request: r.client_request_id,
    };
    if let Some(receipt) = s.transmissions.existing(&key, &payload)? {
        return Ok((StatusCode::ACCEPTED, Json(receipt.wait().await?)));
    }
    let prepared = s.runtime.prepare_send(&id, &i.id, g, frame).await?;
    let session = s.runtime.get(&id).await?;
    let simulated = s
        .runtime
        .capabilities(&session.channel)
        .map_err(RuntimeError::from)?
        .simulated;
    let runtime = s.runtime.clone();
    let receipt = s
        .transmissions
        .submit(key, payload, simulated, async move {
            runtime.submit_prepared(prepared).await
        })?;
    Ok((StatusCode::ACCEPTED, Json(receipt.wait().await?)))
}
async fn transmission_lookup(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
    Extension(i): Extension<Identity>,
    Extension(Native(native)): Extension<Native>,
    query: Result<
        axum::extract::Query<dto::ReceiptLookup>,
        axum::extract::rejection::QueryRejection,
    >,
) -> Result<Json<dto::TxReceipt>, ApiError> {
    let axum::extract::Query(query) = query.map_err(|_| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "invalid receipt lookup",
        )
    })?;
    if !dto::valid_client_request_id(&query.client_request_id) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "request ID must be 1..128 Unicode characters",
        ));
    }
    let generation = query.generation.map(|g| g.as_str().parse::<u64>().unwrap());
    Ok(Json(s.transmissions.by_request(
        if native { None } else { Some(i.id.as_str()) },
        &id,
        &query.client_request_id,
        generation,
    )?))
}
async fn transmission(
    State(s): State<Arc<AppState>>,
    Path((id, receipt)): Path<(String, String)>,
    Extension(i): Extension<Identity>,
    Extension(Native(native)): Extension<Native>,
) -> Result<Json<dto::TxReceipt>, ApiError> {
    Ok(Json(if native {
        s.transmissions.get_native(&id, &receipt)?
    } else {
        s.transmissions.get(&i.id, &id, &receipt)?
    }))
}
async fn diagnostics(State(s): State<Arc<AppState>>) -> Json<dto::Diagnostics> {
    Json(dto::Diagnostics {
        stage: "m1".into(),
        hardware_backends: s
            .runtime
            .channels()
            .into_iter()
            .filter_map(|channel| s.runtime.capabilities(&channel.id).ok())
            .filter(|caps| !caps.simulated)
            .map(|caps| caps.backend)
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect(),
        session_count: s.runtime.list().await.len() as u32,
        stream_capacity: 256,
        ws_slots_available: s.ws_slots.available_permits() as u32,
    })
}
async fn legal() -> Json<dto::Legal> {
    Json(dto::Legal {
        license: "AGPL-3.0-or-later".into(),
        source_url: "https://github.com/soulteary/canharbor".into(),
        stage: "development".into(),
        exact_source_archive: None,
        notice: "Development build; exact build source packaging is not yet implemented. Third-party licenses remain in THIRD_PARTY_NOTICES and LICENSES.".into(),
    })
}
async fn stream(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
    Extension(i): Extension<Identity>,
    h: HeaderMap,
    ws: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    if i.cookie.is_some() && text_header(&h, "origin") != Some(s.origin.as_str()) {
        return Err(ApiError::forbidden());
    }
    let input = s.runtime.subscribe_all(&id).await?;
    let permit = s
        .ws_slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| ApiError::limit())?;
    Ok(ws
        .max_message_size(8192)
        .on_upgrade(move |socket| async move {
            let _permit = permit;
            stream_loop(socket, s, id, i, input).await
        }))
}
async fn stream_loop(
    socket: WebSocket,
    s: Arc<AppState>,
    id: String,
    identity: Identity,
    mut input: broadcast::Receiver<canharbor_core::runtime::Notice>,
) {
    let (mut tx, mut rx) = socket.split();
    let subscribe = match tokio::time::timeout(Duration::from_secs(5), rx.next()).await {
        Ok(Some(Ok(Message::Text(t)))) => serde_json::from_str::<Subscribe>(&t).ok(),
        _ => None,
    };
    let Some(sub) = subscribe else { return };
    if sub.protocol_version != "1.0"
        || sub.csrf_token != identity.csrf
        || sub.filter.ids.len() > 256
    {
        return;
    }
    if sub.after_sequence.is_some() {
        let _ = tx
            .send(Message::Text(
                serde_json::to_string(&dto::StreamMessage::ResumeUnavailable)
                    .unwrap()
                    .into(),
            ))
            .await;
        return;
    }
    let hello = dto::StreamMessage::Hello {
        api_version: "1.0".into(),
        server_instance_id: s.instance.clone(),
        session_epoch: id.clone(),
        capacity: 256,
    };
    if tx
        .send(Message::Text(serde_json::to_string(&hello).unwrap().into()))
        .await
        .is_err()
    {
        return;
    }
    let mut ticker = tokio::time::interval(Duration::from_millis(20));
    let mut heartbeat = tokio::time::interval(Duration::from_secs(10));
    let mut last_pong = Instant::now();
    loop {
        tokio::select! {
            _ = heartbeat.tick() => {
                if last_pong.elapsed() > Duration::from_secs(30) || s.runtime.get(&id).await.is_err() { break; }
                if let Some(c) = &identity.cookie && !s.browser_valid(c).await { break; }
                if !matches!(tokio::time::timeout(Duration::from_secs(2), tx.send(Message::Ping(vec![].into()))).await, Ok(Ok(()))) { break; }
            }
            incoming = rx.next() => {
                match incoming {
                    Some(Ok(Message::Pong(_))) => last_pong = Instant::now(),
                    Some(Ok(Message::Ping(p))) => {
                        if !matches!(tokio::time::timeout(Duration::from_secs(2), tx.send(Message::Pong(p))).await, Ok(Ok(()))) { break; }
                    }
                    Some(Ok(Message::Text(t))) if t == "ping" => last_pong = Instant::now(),
                    _ => break,
                }
            }
            _ = ticker.tick() => {
                let mut events = Vec::new();
                let mut lagged = 0;
                let mut closed = false;
                for _ in 0..256 {
                    match input.try_recv() {
                        Ok(e) => {
                            if !visible_notice(&e,&sub.filter){continue;}
                            // Diagnostic loss/error events are never hidden by data filters.
                            events.push(dto::notice(e));
                        }
                        Err(broadcast::error::TryRecvError::Lagged(n)) => lagged += n,
                        Err(broadcast::error::TryRecvError::Closed) => {
                            closed = true;
                            break;
                        },
                        Err(broadcast::error::TryRecvError::Empty) => break,
                    }
                }
                if lagged > 0 {
                    let gap = dto::StreamMessage::Gap{cause:dto::GapCause::SubscriberLag,count:lagged.into()};
                    if !send_ws(&mut tx, gap).await { break; }
                }
                if !events.is_empty() && !send_ws(&mut tx, dto::StreamMessage::Events{server_instance_id:s.instance.clone(),session_epoch:id.clone(),events}).await { break; }
                if closed {
                            if let Ok(view)=s.runtime.get(&id).await && let Some(fault)=view.fault {
                                let state=if fault==canharbor_core::BackendFault::Disconnected {dto::SessionState::Disconnected}else{dto::SessionState::Faulted};
                                let _=send_ws(&mut tx,dto::StreamMessage::SessionFault{state,code:fault.into()}).await;
                            }
                            // Complete a close handshake instead of dropping the
                            // transport while a client may be replying to a ping.
                            let _ = tokio::time::timeout(Duration::from_secs(1), tx.send(Message::Close(None))).await;
                            let _ = tokio::time::timeout(Duration::from_secs(1), async {
                                while let Some(Ok(message)) = rx.next().await {
                                    match message {
                                        Message::Close(_) => break,
                                        Message::Ping(p) => { if tx.send(Message::Pong(p)).await.is_err() { break; } }
                                        _ => {}
                                    }
                                }
                            }).await;
                            return;
                }
            }
        }
    }
}

fn visible_notice(notice: &canharbor_core::runtime::Notice, filter: &dto::Filter) -> bool {
    match notice {
        canharbor_core::runtime::Notice::Diagnostic { .. } => true,
        canharbor_core::runtime::Notice::Frame(frame) => {
            (filter.ids.is_empty() || filter.ids.contains(&frame.frame.id().value()))
                && filter
                    .extended
                    .is_none_or(|extended| extended == frame.frame.id().extended())
                && filter
                    .direction
                    .as_ref()
                    .is_none_or(|direction| match direction {
                        dto::Direction::Rx => frame.direction == "rx",
                        dto::Direction::Tx => frame.direction == "tx",
                    })
        }
    }
}
async fn send_ws<T: Serialize>(
    tx: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    value: T,
) -> bool {
    matches!(
        tokio::time::timeout(
            Duration::from_secs(2),
            tx.send(Message::Text(serde_json::to_string(&value).unwrap().into()))
        )
        .await,
        Ok(Ok(()))
    )
}
#[cfg(feature = "web-ui")]
#[derive(rust_embed::RustEmbed)]
#[folder = "../../web/dist/"]
struct WebAssets;
async fn static_file(State(_state): State<Arc<AppState>>, req: Request) -> Response {
    if req.method() != axum::http::Method::GET || req.uri().path().starts_with("/api/") {
        return StatusCode::NOT_FOUND.into_response();
    }
    #[cfg(feature = "web-ui")]
    {
        let path = req.uri().path().trim_start_matches('/');
        let path = if path.is_empty() { "index.html" } else { path };
        if let Some(file) = WebAssets::get(path) {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            let data = if path == "index.html" {
                String::from_utf8_lossy(&file.data)
                    .replacen(
                        "data-ui-locale=\"auto\"",
                        &format!(
                            "data-ui-locale=\"{}\"",
                            _state.preferences.get().locale.as_str()
                        ),
                        1,
                    )
                    .into_bytes()
            } else {
                file.data.to_vec()
            };
            return ([(header::CONTENT_TYPE, mime.as_ref())], data).into_response();
        }
        StatusCode::NOT_FOUND.into_response()
    }
    #[cfg(not(feature = "web-ui"))]
    {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "Web UI was not included in this build. Build web/ and enable the web-ui feature.",
        )
            .into_response()
    }
}

fn capture_owner(identity: &Identity) -> &str {
    if identity.cookie.is_none() {
        "native"
    } else {
        &identity.id
    }
}
impl From<capture::Error> for ApiError {
    fn from(error: capture::Error) -> Self {
        let (status, code) = match error {
            capture::Error::Missing => (StatusCode::NOT_FOUND, "capture_not_found"),
            capture::Error::Limit => (StatusCode::TOO_MANY_REQUESTS, "resource_limit"),
            capture::Error::State => (StatusCode::CONFLICT, "invalid_state"),
            capture::Error::Conflict => (StatusCode::CONFLICT, "request_conflict"),
            capture::Error::Closing => (StatusCode::SERVICE_UNAVAILABLE, "service_closing"),
            capture::Error::DiskFull => (StatusCode::SERVICE_UNAVAILABLE, "disk_full"),
            capture::Error::Storage => (StatusCode::SERVICE_UNAVAILABLE, "capture_io_error"),
            capture::Error::WorkerFailed => {
                (StatusCode::SERVICE_UNAVAILABLE, "capture_worker_failed")
            }
        };
        Self::new(status, code, error.to_string())
    }
}
async fn captures(
    State(s): State<Arc<AppState>>,
    Extension(identity): Extension<Identity>,
) -> Result<Json<dto::Captures>, ApiError> {
    Ok(Json(dto::Captures {
        captures: s
            .captures
            .list(capture_owner(&identity))
            .await?
            .into_iter()
            .map(dto::capture)
            .collect(),
    }))
}
async fn capture_start(
    State(s): State<Arc<AppState>>,
    Extension(identity): Extension<Identity>,
    WireJson(request): WireJson<dto::CaptureRequest>,
) -> Result<(StatusCode, Json<dto::Capture>), ApiError> {
    if !dto::valid_capture_name(&request.name)
        || !dto::valid_client_request_id(&request.client_request_id)
        || request.session_id.is_empty()
        || request.session_id.chars().take(129).count() > 128
        || !(1..=86400).contains(&request.max_duration_seconds)
    {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "capture request has invalid name, IDs or duration",
        ));
    }
    Ok((
        StatusCode::CREATED,
        Json(dto::capture(
            s.captures
                .start_named(
                    &request.session_id,
                    capture_owner(&identity),
                    &request.client_request_id,
                    request.max_duration_seconds,
                    &request.name,
                )
                .await?,
        )),
    ))
}
async fn capture_status(
    State(s): State<Arc<AppState>>,
    Extension(identity): Extension<Identity>,
    Path(id): Path<String>,
) -> Result<Json<dto::Capture>, ApiError> {
    Ok(Json(dto::capture(
        s.captures.get(&id, capture_owner(&identity)).await?,
    )))
}
async fn capture_rename(
    State(s): State<Arc<AppState>>,
    Extension(identity): Extension<Identity>,
    Path(id): Path<String>,
    WireJson(request): WireJson<dto::CaptureRename>,
) -> Result<Json<dto::Capture>, ApiError> {
    if !dto::valid_capture_name(&request.name) || !dto::valid_capture_name(&request.expected_name) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "Recording names must be printable, at most 120 characters, and have no surrounding whitespace.",
        ));
    }
    Ok(Json(dto::capture(
        s.captures
            .rename(
                &id,
                capture_owner(&identity),
                &request.expected_name,
                &request.name,
            )
            .await?,
    )))
}
async fn capture_stop(
    State(s): State<Arc<AppState>>,
    Extension(identity): Extension<Identity>,
    Path(id): Path<String>,
) -> Result<Json<dto::Capture>, ApiError> {
    Ok(Json(dto::capture(
        s.captures.stop(&id, capture_owner(&identity)).await?,
    )))
}
async fn capture_delete(
    State(s): State<Arc<AppState>>,
    Extension(identity): Extension<Identity>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    s.captures.delete(&id, capture_owner(&identity)).await?;
    Ok(StatusCode::NO_CONTENT)
}
async fn capture_download(
    State(s): State<Arc<AppState>>,
    Extension(identity): Extension<Identity>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    use futures_util::StreamExt;
    use tokio::io::AsyncReadExt;
    let info = s.captures.get(&id, capture_owner(&identity)).await?;
    let disposition = capture::download_disposition(&info);
    let (file, size, permit) = s.captures.download(&id, capture_owner(&identity)).await?;

    let stream = tokio_util::io::ReaderStream::new(tokio::fs::File::from_std(file).take(size)).map(
        move |chunk| {
            let _ = &permit;
            chunk
        },
    );
    let mut response = axum::body::Body::from_stream(stream).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        "application/x-ndjson".parse().unwrap(),
    );
    response
        .headers_mut()
        .insert(header::CONTENT_LENGTH, size.to_string().parse().unwrap());
    response
        .headers_mut()
        .insert(header::CONTENT_DISPOSITION, disposition.parse().unwrap());
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    Ok(response)
}

fn task_id(id: &str) -> Result<Uuid, ApiError> {
    id.parse()
        .map_err(|_| ApiError::new(StatusCode::BAD_REQUEST, "invalid_task", "invalid task ID"))
}
async fn periodic_start(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
    Extension(i): Extension<Identity>,
    h: HeaderMap,
    WireJson(r): WireJson<dto::PeriodicRequest>,
) -> Result<(StatusCode, Json<dto::PeriodicTask>), ApiError> {
    let g = generation(&h)?;
    if !dto::valid_client_request_id(&r.client_request_id)
        || r.count == 0
        || r.count > 6000
        || !(10..=60000).contains(&r.interval_ms)
        || u64::from(r.count.saturating_sub(1)) * u64::from(r.interval_ms) > 60000
    {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_plan",
            "periodic plan exceeds its bounds",
        ));
    }
    let frame = r
        .frame
        .into_frame()
        .map_err(|e| ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid_frame", e))?;
    let payload = serde_json::to_string(&serde_json::json!({
        "frame": dto::Frame::from(&frame),
        "interval_ms": r.interval_ms,
        "count": r.count,
    }))
    .unwrap();
    let key = periodic::Key {
        owner: i.id.clone(),
        session: id.clone(),
        generation: g,
        request: r.client_request_id,
    };
    if let Some(existing) = s.periodic.existing(&key, &payload)? {
        return Ok((StatusCode::ACCEPTED, Json(existing)));
    }
    let observation = s.runtime.observe(&id).await?;
    drop(s.runtime.prepare_send(&id, &i.id, g, frame.clone()).await?);
    let plan = canharbor_tasks::PeriodicPlan {
        frame,
        interval: Duration::from_millis(r.interval_ms.into()),
        count: r.count,
    };
    Ok((
        StatusCode::ACCEPTED,
        Json(
            s.periodic.start(
                key,
                payload,
                plan,
                observation.epoch(),
                s.runtime
                    .capabilities(&observation.channel)
                    .map_err(RuntimeError::from)?
                    .simulated,
            )?,
        ),
    ))
}
async fn periodic_list(
    State(s): State<Arc<AppState>>,
    Extension(i): Extension<Identity>,
    Extension(native): Extension<Native>,
) -> Json<dto::PeriodicTasks> {
    Json(s.periodic.list((!native.0).then_some(i.id.as_str())))
}
async fn periodic_get(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
    Extension(i): Extension<Identity>,
    Extension(native): Extension<Native>,
) -> Result<Json<dto::PeriodicTask>, ApiError> {
    Ok(Json(s.periodic.get(
        (!native.0).then_some(i.id.as_str()),
        task_id(&id)?,
    )?))
}
async fn periodic_cancel(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
    Extension(i): Extension<Identity>,
    Extension(native): Extension<Native>,
) -> Result<Json<dto::PeriodicTask>, ApiError> {
    Ok(Json(s.periodic.cancel(
        (!native.0).then_some(i.id.as_str()),
        task_id(&id)?,
    )?))
}
