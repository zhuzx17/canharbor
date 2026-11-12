// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use crate::stress_store::{self, Identity as DiskIdentity};
use canharbor_stress::Request as Start;
use canharbor_stress::{
    Artifacts, EvidenceSink, Executor, MAX_FPS, MEMORY_LIMIT, Plan, QUANTUM_NS, Report, Run, Runs,
    Validation,
};
use std::{
    path::PathBuf,
    sync::{
        Mutex as SyncMutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};
pub(super) struct Entry {
    pub(super) owner: String,
    request: String,
    pub(super) plan: Plan,
    pub(super) run: Arc<Run>,
    pub(super) writing: Arc<AtomicBool>,
    downloads: Arc<std::sync::atomic::AtomicUsize>,
}
type RecoveryResult = Option<Arc<Result<stress_store::RecoveryBatch, String>>>;
type RecoveryReceiver = tokio::sync::watch::Receiver<RecoveryResult>;
pub(super) struct Manager {
    pub(super) closing: AtomicBool,
    retired_unknown: AtomicU64,
    pub(super) executor: Executor,
    pub(super) entries: SyncMutex<Vec<Entry>>,
    pub(super) recovery_issues: SyncMutex<Vec<canharbor_stress::RecoveryIssue>>,
    root: PathBuf,
    initialized: AtomicBool,
    initialization: SyncMutex<Option<RecoveryReceiver>>,
    pub(super) admission: Mutex<()>,
    downloads: Arc<Semaphore>,
    starts: Arc<Semaphore>,
    pub(super) coordinated: SyncMutex<Option<crate::stress_coord::Prepared>>,
    pub(super) coord_operations: Mutex<()>,
    pub(super) coord_commands: Arc<Semaphore>,
}
pub(super) fn terminal(state: &str) -> bool {
    matches!(state, "completed" | "failed" | "aborted" | "interrupted")
}
fn uncertain(r: &Report) -> bool {
    r.adaptive
        .as_ref()
        .is_some_and(|a| a.steps.iter().any(|s| uncertain(&s.report)))
        || r.lifecycle.inflight_sequence.is_some()
        || r.group
            .as_ref()
            .is_some_and(|g| g.flows.iter().any(|f| uncertain(&f.report)))
        || r.unknown > 0
        || r.rtt_report
            .as_ref()
            .is_some_and(|t| t.responder_unknown > 0)
        || (r.state == "interrupted" && !r.counters_complete)
}
fn storage(error: impl ToString) -> ApiError {
    // Startup storage failures precede executor admission; ApiError::new carries
    // the explicit outcome_unknown=false metadata used by recovery clients.
    ApiError::new(
        StatusCode::INSUFFICIENT_STORAGE,
        "stress_evidence",
        error.to_string(),
    )
}

fn missing() -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, "stress_not_found", "run not found")
}
impl Manager {
    pub fn new(runtime: Arc<Runtime>, root: PathBuf) -> Self {
        Self {
            closing: AtomicBool::new(false),
            retired_unknown: AtomicU64::new(0),
            executor: Executor::new(runtime),
            entries: SyncMutex::new(vec![]),
            recovery_issues: SyncMutex::new(vec![]),
            root,
            initialized: AtomicBool::new(false),
            initialization: SyncMutex::new(None),
            admission: Mutex::new(()),
            downloads: Arc::new(Semaphore::new(2)),
            starts: Arc::new(Semaphore::new(8)),
            coordinated: SyncMutex::new(None),
            coord_operations: Mutex::new(()),
            coord_commands: Arc::new(Semaphore::new(8)),
        }
    }
    pub(super) async fn ensure(&self) -> Result<(), ApiError> {
        if self.initialized.load(Ordering::Acquire) {
            return Ok(());
        }
        // The recovery worker belongs to the service, not to an HTTP waiter.
        let mut receiver = {
            let mut initialization = self.initialization.lock().unwrap();
            if let Some(receiver) = initialization.as_ref() {
                receiver.clone()
            } else {
                let (sender, receiver) = tokio::sync::watch::channel(None);
                let root = self.root.clone();
                tokio::spawn(async move {
                    let result =
                        tokio::task::spawn_blocking(move || stress_store::recover_registry(&root))
                            .await
                            .map_err(|e| e.to_string())
                            .and_then(|v| v);
                    sender.send_replace(Some(Arc::new(result)));
                });
                *initialization = Some(receiver.clone());
                receiver
            }
        };
        let result = loop {
            let value = receiver.borrow().clone();
            if let Some(value) = value {
                break value;
            }
            receiver.changed().await.map_err(storage)?;
        };
        let recovered = result.as_ref().as_ref().map_err(storage)?;
        let mut entries = self.entries.lock().unwrap();
        if !self.initialized.load(Ordering::Acquire) {
            *self.recovery_issues.lock().unwrap() = recovered.issues.clone();
            for disk in &recovered.runs {
                entries.push(Entry {
                    owner: disk.owner.clone(),
                    request: disk.request.clone(),
                    plan: disk.report.plan.clone(),
                    run: Run::recovered(disk.report.clone()),
                    writing: Arc::new(AtomicBool::new(false)),
                    downloads: Default::default(),
                });
            }
            self.initialized.store(true, Ordering::Release);
        }
        Ok(())
    }
    pub fn stop_all(&self) {
        self.closing.store(true, Ordering::Release);
        for e in self.entries.lock().unwrap().iter() {
            e.run.stop();
        }
    }
    pub fn stop_owner(&self, owner: &str) {
        for e in self
            .entries
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e.owner == owner)
        {
            e.run.stop();
        }
    }
    pub async fn shutdown(&self) -> u64 {
        self.stop_all();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let busy = self
                .entries
                .lock()
                .unwrap()
                .iter()
                .any(|e| !terminal(&e.run.state()) || e.writing.load(Ordering::Acquire));
            if !busy || Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        self.pending()
    }
    pub fn pending(&self) -> u64 {
        self.retired_unknown.load(Ordering::Acquire)
            + self
                .entries
                .lock()
                .unwrap()
                .iter()
                .map(|e| {
                    let r = e.run.snapshot();
                    u64::from(
                        uncertain(&r) || !terminal(&r.state) || e.writing.load(Ordering::Acquire),
                    )
                })
                .sum::<u64>()
    }
    pub(super) fn find(
        &self,
        owner: &str,
        id: &str,
    ) -> Result<(Arc<Run>, Arc<AtomicBool>), ApiError> {
        uuid::Uuid::parse_str(id).map_err(|_| missing())?;
        self.entries
            .lock()
            .unwrap()
            .iter()
            .find(|e| (owner == "native" || e.owner == owner) && e.run.id() == id)
            .map(|e| (e.run.clone(), e.writing.clone()))
            .ok_or_else(missing)
    }
}
fn invalid(e: impl ToString) -> ApiError {
    ApiError::new(
        StatusCode::UNPROCESSABLE_ENTITY,
        "invalid_stress_plan",
        e.to_string(),
    )
}
pub(super) async fn validate(
    State(s): State<Arc<AppState>>,
    WireJson(p): WireJson<Plan>,
) -> Result<Json<Validation>, ApiError> {
    p.validate(&s.runtime).map_err(invalid)?;
    let compiled = p.compile().map_err(invalid)?;
    Ok(Json(Validation {
        load_budget: p.load_budget().map_err(invalid)?,
        valid: true,
        version: 11,
        group_budget: if p.flows.is_empty(){None}else{let mut r=Report::initial(&p,true,"validation".into()).map_err(invalid)?;r.capture_sources(&s.runtime);r.group.map(|g|g.budget)},
        plan_hash: p.fingerprint(),
        tracking_bytes: compiled.tracking_bytes,
        plan: p,
        max_active_runs: 1,
        max_fps: MAX_FPS,
        memory_limit_bytes: MEMORY_LIMIT,
        scheduling_quantum_ns: QUANTUM_NS,
        measurement: "profile_declared_integrity; quantized_host_schedule; RTT is initiator host observation; no cross-host one-way latency".into(),
    }))
}
pub(super) async fn start(
    State(s): State<Arc<AppState>>,
    Extension(i): Extension<Identity>,
    WireJson(p): WireJson<Start>,
) -> Result<(StatusCode, Json<Report>), ApiError> {
    if !dto::valid_client_request_id(&p.client_request_id) || !p.active {
        return Err(invalid("valid request ID and explicit active required"));
    }
    if p.plan_hash != p.plan.fingerprint() {
        return Err(invalid("plan hash mismatch; validate exact plan first"));
    }
    p.plan.validate(&s.runtime).map_err(invalid)?;
    let slot = s
        .stress
        .starts
        .clone()
        .try_acquire_owned()
        .map_err(|_| ApiError::limit())?;
    tokio::spawn(async move {
        let _slot = slot;
        start_owned(s, i, p, false).await
    })
    .await
    .map_err(|e| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "stress_start_accounting",
            e.to_string(),
        )
    })?
}
pub(super) async fn start_owned(
    s: Arc<AppState>,
    i: Identity,
    p: Start,
    coordinated: bool,
) -> Result<(StatusCode, Json<Report>), ApiError> {
    if i.revoked.load(Ordering::Acquire) {
        return Err(ApiError::forbidden());
    }
    s.stress.ensure().await?;
    let _admission = s.stress.admission.lock().await;
    if !coordinated && p.plan.coordination.is_some() {
        return Err(invalid(
            "coordinated plans require two-phase native coordinator",
        ));
    }
    if !coordinated
        && s.stress
            .coordinated
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|c| c.run.is_none() && Instant::now() < c.expires)
    {
        return Err(invalid("coordinator holds prepared admission"));
    }
    let owner = capture_owner(&i).to_owned();
    {
        let entries = s.stress.entries.lock().unwrap();
        if let Some(e) = entries
            .iter()
            .find(|e| e.owner == owner && e.request == p.client_request_id)
        {
            if e.plan != p.plan {
                return Err(ApiError::new(
                    StatusCode::CONFLICT,
                    "request_conflict",
                    "request ID used with another plan",
                ));
            }
            return Ok((StatusCode::ACCEPTED, Json(e.run.snapshot())));
        }
        if s.stress.closing.load(Ordering::Acquire) {
            return Err(ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "service_closing",
                "service is closing",
            ));
        }
        if !s.stress.recovery_issues.lock().unwrap().is_empty() {
            return Err(storage(
                "quarantined evidence requires native operator repair before new admission",
            ));
        }
        if entries.len() >= 32 {
            return Err(ApiError::limit());
        }
        if !s.stress.executor.available()
            || entries
                .iter()
                .any(|e| !terminal(&e.run.state()) || e.writing.load(Ordering::Acquire))
        {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "stress_busy",
                "a run is active or finalizing",
            ));
        }
        let used: u64 = entries
            .iter()
            .map(|e| e.run.snapshot().evidence.bytes + 4 * 1024 * 1024)
            .sum();
        if used + p.plan.evidence.max_bytes > 4 * 1024 * 1024 * 1024 {
            return Err(storage("service evidence reservation exceeds 4 GiB"));
        }
    }
    let id = uuid::Uuid::new_v4().to_string();
    let simulated = p
        .plan
        .channels()
        .iter()
        .all(|c| s.runtime.capabilities(c).is_ok_and(|v| v.simulated));
    let mut initial = Report::initial(&p.plan, simulated, id.clone()).map_err(invalid)?;
    initial.capture_sources(&s.runtime);
    let identity = DiskIdentity {
        owner: owner.clone(),
        request: p.client_request_id.clone(),
        report: initial,
    };
    let root = s.stress.root.clone();
    let disk = identity.clone();
    let path = tokio::task::spawn_blocking(move || stress_store::prepare(&root, &disk))
        .await
        .map_err(storage)?
        .map_err(storage)?;
    let (sink, rx) = EvidenceSink::channel(256);
    let writer_sink = sink.monitor();
    let prepared_deadline = if coordinated {
        s.stress
            .coordinated
            .lock()
            .unwrap()
            .as_ref()
            .filter(|c| c.committing && c.group == p.plan.coordination.as_ref().unwrap().group_id)
            .map(|c| c.expires)
    } else {
        None
    };
    let starting = if coordinated {
        s.stress.executor.start_coordinated(
            p.plan.clone(),
            true,
            id,
            Some(sink.clone()),
            Some((i.id.clone(), i.revoked.clone())),
            prepared_deadline.unwrap_or_else(Instant::now),
        )
    } else {
        s.stress.executor.start_recorded(
            p.plan.clone(),
            true,
            id,
            Some(sink.clone()),
            Some((i.id.clone(), i.revoked.clone())),
        )
    };
    let run = match starting {
        Ok(run) => run,
        Err(e) => {
            let _ = tokio::fs::remove_dir_all(path).await;
            return Err(invalid(e));
        }
    };
    let writer =
        tokio::task::spawn_blocking(move || stress_store::write(path, identity, writer_sink, rx));
    let writing = Arc::new(AtomicBool::new(true));
    let writing_done = writing.clone();
    let result = run.clone();
    tokio::spawn(async move {
        loop {
            let state = result.state();
            if let Some(state) = state.strip_prefix("saving_") {
                let mut report = result.snapshot();
                report.state = state.to_owned();
                let _ = sink.finish(report).await;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        match writer.await {
            Ok(Ok(report)) => {
                let mut r = result.report.lock().unwrap();
                *r = report;
                writing_done.store(false, Ordering::Release);
            }
            error => {
                let mut r = result.report.lock().unwrap();
                r.state = "failed".into();
                r.verdict = "inconclusive".into();
                r.measurement_complete = false;
                r.evidence.complete = false;
                r.evidence.error = Some(format!("evidence finalization failed: {error:?}"));
            }
        }
        writing_done.store(false, Ordering::Release);
    });
    let report = run.snapshot();
    s.stress.entries.lock().unwrap().push(Entry {
        owner,
        request: p.client_request_id,
        plan: p.plan,
        run,
        writing,
        downloads: Default::default(),
    });
    Ok((StatusCode::ACCEPTED, Json(report)))
}
pub(super) async fn list(
    State(s): State<Arc<AppState>>,
    Extension(i): Extension<Identity>,
) -> Result<Json<Runs>, ApiError> {
    s.stress.ensure().await?;
    Ok(Json(Runs {
        recovery_issues: if capture_owner(&i) == "native" {
            s.stress.recovery_issues.lock().unwrap().clone()
        } else {
            vec![]
        },
        runs: s
            .stress
            .entries
            .lock()
            .unwrap()
            .iter()
            .filter(|e| capture_owner(&i) == "native" || e.owner == capture_owner(&i))
            .map(|e| e.run.summary())
            .collect(),
    }))
}
pub(super) async fn get_run(
    State(s): State<Arc<AppState>>,
    Extension(i): Extension<Identity>,
    Path(id): Path<String>,
) -> Result<Json<Report>, ApiError> {
    s.stress.ensure().await?;
    Ok(Json(s.stress.find(capture_owner(&i), &id)?.0.snapshot()))
}
pub(super) async fn stop(
    State(s): State<Arc<AppState>>,
    Extension(i): Extension<Identity>,
    Path(id): Path<String>,
) -> Result<Json<Report>, ApiError> {
    s.stress.ensure().await?;
    let (run, _) = s.stress.find(capture_owner(&i), &id)?;
    run.stop();
    Ok(Json(run.snapshot()))
}
pub(super) async fn stop_flow(
    State(s): State<Arc<AppState>>,
    Extension(i): Extension<Identity>,
    Path((id, name)): Path<(String, String)>,
) -> Result<Json<Report>, ApiError> {
    s.stress.ensure().await?;
    let (run, _) = s.stress.find(capture_owner(&i), &id)?;
    run.stop_flow(&name).map_err(|_| missing())?;
    Ok(Json(run.snapshot()))
}
pub(super) async fn delete(
    State(s): State<Arc<AppState>>,
    Extension(i): Extension<Identity>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    s.stress.ensure().await?;
    let _admission = s.stress.admission.lock().await;
    let (run, writing) = s.stress.find(capture_owner(&i), &id)?;
    let downloading = s
        .stress
        .entries
        .lock()
        .unwrap()
        .iter()
        .find(|e| e.run.id() == id)
        .is_some_and(|e| e.downloads.load(Ordering::Acquire) > 0);
    if !terminal(&run.snapshot().state)
        || writing.load(Ordering::Acquire)
        || run.pending_io() > 0
        || downloading
    {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "stress_active",
            "wait for run, native I/O, evidence writing, and downloads to finish",
        ));
    }
    let path = s.stress.root.join(&id);
    tokio::task::spawn_blocking(move || {
        stress_store::directory(&path)?;
        std::fs::remove_dir_all(path).map_err(|e| e.to_string())
    })
    .await
    .map_err(storage)?
    .map_err(storage)?;
    let mut entries = s.stress.entries.lock().unwrap();
    let n = entries
        .iter()
        .position(|e| {
            (capture_owner(&i) == "native" || e.owner == capture_owner(&i)) && e.run.id() == id
        })
        .ok_or_else(missing)?;
    if uncertain(&run.snapshot()) {
        s.stress.retired_unknown.fetch_add(1, Ordering::AcqRel);
    }
    entries.remove(n);
    Ok(StatusCode::NO_CONTENT)
}
pub(super) async fn artifacts(
    State(s): State<Arc<AppState>>,
    Extension(i): Extension<Identity>,
    Path(id): Path<String>,
) -> Result<Json<Artifacts>, ApiError> {
    s.stress.ensure().await?;
    let (run, writing) = s.stress.find(capture_owner(&i), &id)?;
    if !terminal(&run.snapshot().state) || writing.load(Ordering::Acquire) {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "stress_active",
            "evidence is still being written",
        ));
    }
    let path = s.stress.root.join(id);
    Ok(Json(Artifacts {
        files: tokio::task::spawn_blocking(move || stress_store::artifacts(&path))
            .await
            .map_err(storage)?
            .map_err(storage)?,
    }))
}
struct DownloadLease(Arc<std::sync::atomic::AtomicUsize>);
impl Drop for DownloadLease {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}
pub(super) async fn download(
    State(s): State<Arc<AppState>>,
    Extension(i): Extension<Identity>,
    Path((id, name)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    s.stress.ensure().await?;
    let admission = s.stress.admission.lock().await;
    let (run, writing) = s.stress.find(capture_owner(&i), &id)?;
    if !terminal(&run.snapshot().state) || writing.load(Ordering::Acquire) {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "stress_active",
            "evidence is still being written",
        ));
    }
    if name == "identity.json" || !stress_store::valid_name(&name) {
        return Err(missing());
    }
    let permit = s
        .stress
        .downloads
        .clone()
        .try_acquire_owned()
        .map_err(|_| ApiError::limit())?;
    let downloads = s
        .stress
        .entries
        .lock()
        .unwrap()
        .iter()
        .find(|e| e.run.id() == id)
        .map(|e| e.downloads.clone())
        .ok_or_else(missing)?;
    downloads.fetch_add(1, Ordering::AcqRel);
    let download = DownloadLease(downloads);
    drop(admission);
    let root = s.stress.root.join(id);
    let filename = name.clone();
    let file = tokio::task::spawn_blocking(move || stress_store::open(&root, &filename))
        .await
        .map_err(storage)?
        .map_err(storage)?;
    let bytes = file.metadata().map_err(storage)?.len();
    if bytes > 16 * 1024 * 1024 {
        return Err(storage("artifact exceeds bounded download size"));
    }
    let stream =
        tokio_util::io::ReaderStream::new(tokio::fs::File::from_std(file)).map(move |chunk| {
            let _keep = (&permit, &download);
            chunk
        });
    let mut response = axum::body::Body::from_stream(stream).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/octet-stream"),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    response.headers_mut().insert(
        header::CONTENT_LENGTH,
        header::HeaderValue::from_str(&bytes.to_string()).unwrap(),
    );
    response.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        header::HeaderValue::from_str(&format!("attachment; filename=\"{name}\""))
            .map_err(storage)?,
    );
    Ok(response)
}
pub(super) async fn by_request(
    State(s): State<Arc<AppState>>,
    Extension(i): Extension<Identity>,
    Path(request): Path<String>,
) -> Result<Json<Report>, ApiError> {
    s.stress.ensure().await?;
    let entries = s.stress.entries.lock().unwrap();
    let mut matches = entries.iter().filter(|e| {
        (capture_owner(&i) == "native" || e.owner == capture_owner(&i)) && e.request == request
    });
    let found = matches.next().ok_or_else(missing)?;
    if matches.next().is_some() {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "stress_request_ambiguous",
            "request ID belongs to more than one owner; use a run ID",
        ));
    }
    Ok(Json(found.run.snapshot()))
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct EventQuery {
    #[serde(default)]
    after_revision: u64,
}
pub(super) async fn events(
    State(s): State<Arc<AppState>>,
    Extension(i): Extension<Identity>,
    Path(id): Path<String>,
    query: Result<axum::extract::Query<EventQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<Json<canharbor_stress::Events>, ApiError> {
    let after = query
        .map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, "invalid_cursor", e.to_string()))?
        .0
        .after_revision;
    s.stress.ensure().await?;
    let r = s.stress.find(capture_owner(&i), &id)?.0.snapshot();
    if after > r.lifecycle.revision {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "revision_conflict",
            "cursor is ahead of this run",
        ));
    }
    let first = r
        .lifecycle_history
        .first()
        .map_or(r.lifecycle.revision.saturating_add(1), |e| e.revision);
    Ok(Json(canharbor_stress::Events {
        run_id: r.id,
        latest_revision: r.lifecycle.revision,
        gap: after.saturating_add(1) < first,
        events: r
            .lifecycle_history
            .into_iter()
            .filter(|e| e.revision > after)
            .collect(),
    }))
}

#[cfg(test)]
mod audit_event_tests {
    use super::*;
    #[tokio::test]
    async fn event_cursor_reports_eviction_without_restarting_or_transmitting() {
        let runtime = Runtime::new(Arc::new(canharbor_virtual::VirtualBus::new(8192)));
        let s = AppState::new(runtime.clone(), "http://127.0.0.1:32180".into());
        let plan:Plan=serde_json::from_value(serde_json::json!({"sender":"virtual:0","receivers":["virtual:1"],"can_id":291,"extended":false,"run_tag":17,"count":1000,"fps":10,"bitrate":500000,"receivers_active":true,"drain_ms":20})).unwrap();
        let run = s.stress.executor.start(plan.clone(), true).unwrap();
        for n in 0..80 {
            run.transition(if n % 2 == 0 { "warming" } else { "running" });
        }
        run.stop();
        s.stress.entries.lock().unwrap().push(Entry {
            owner: "native".into(),
            request: "cursor".into(),
            plan,
            run: run.clone(),
            writing: Arc::new(AtomicBool::new(false)),
            downloads: Default::default(),
        });
        let i = Identity {
            revoked: Arc::new(AtomicBool::new(false)),
            id: "native:audit".into(),
            cookie: None,
            csrf: None,
        };
        let result = events(
            State(s.clone()),
            Extension(i),
            Path(run.id()),
            Ok(axum::extract::Query(EventQuery { after_revision: 0 })),
        )
        .await
        .unwrap()
        .0;
        assert!(result.gap);
        assert!(result.events.len() <= 64);
        assert!(
            result
                .events
                .windows(2)
                .all(|p| p[0].revision + 1 == p[1].revision)
        );
        tokio::time::timeout(Duration::from_secs(2), async {
            while !terminal(&run.state()) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(run.snapshot().submitted, 0);
        assert!(runtime.list().await.is_empty());
        let parent = s.stress.root.parent().unwrap();
        std::fs::remove_dir_all(parent).unwrap();
    }
}
