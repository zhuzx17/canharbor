// SPDX-License-Identifier: AGPL-3.0-or-later
use crate::{Backend, BackendEvent, BackendFault, Frame};
use futures_util::FutureExt;
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use tokio::{
    sync::{Mutex, broadcast},
    time::Instant,
};

/// A single-use submission bound to the current acquisition's execution permit.
/// Preparing does no transmit I/O; stop, restart, expiry and shutdown revoke it.
pub struct PreparedTransmission {
    trace: Option<crate::measurement::SubmissionTrace>,
    session: String,
    channel: String,
    generation: u64,
    frame: Frame,
    permit: crate::control::Permit,
    runtime_fence: Arc<std::sync::atomic::AtomicBool>,
    execution: crate::control::Permit,
}
/// Revokes only one prepared execution; it cannot revoke or renew its parent lease.
#[derive(Clone)]
pub struct TransmissionCancellation(crate::control::Permit);
impl TransmissionCancellation {
    pub fn cancel(&self) {
        self.0.revoke();
    }
}
impl PreparedTransmission {
    pub fn observe(
        &mut self,
        origin: Instant,
        preparation_started: Instant,
    ) -> crate::measurement::SubmissionTrace {
        let trace = crate::measurement::SubmissionTrace::new(origin, preparation_started);
        self.trace = Some(trace.clone());
        trace
    }
    pub fn bind_gate(&self, gate: &crate::control::ExecutionGate) -> Result<(), RuntimeError> {
        self.execution.bind_gate(gate).map_err(Into::into)
    }
    /// Restrict a queued execution to an additional local deadline; never extends authority.
    pub fn restrict_until(&self, deadline: std::time::Instant) {
        self.execution.restrict_until(deadline);
    }
    pub fn cancellation(&self) -> TransmissionCancellation {
        TransmissionCancellation(self.execution.clone())
    }
}
/// Reusable Classic-only authorization for one acquisition. Each frame still
/// gets a fresh short-lived child checked by the existing backend submission.
pub struct ClassicLane(PreparedTransmission);
/// Data-only lane retaining the configuration boundary established at acquisition.
pub struct DataLane {
    template: PreparedTransmission,
    fd: bool,
}
impl DataLane {
    pub fn prepare(&self, frame: Frame) -> Result<PreparedTransmission, RuntimeError> {
        if matches!(frame, Frame::ClassicRemote { .. })
            || (matches!(frame, Frame::FdData { .. }) && !self.fd)
        {
            return Err(crate::Error::Unsupported.into());
        }
        frame.validate()?;
        self.template.permit.check()?;
        Ok(PreparedTransmission {
            trace: None,
            session: self.template.session.clone(),
            channel: self.template.channel.clone(),
            generation: self.template.generation,
            frame,
            permit: self.template.permit.clone(),
            execution: self.template.permit.expiring_child(Duration::from_secs(1)),
            runtime_fence: self.template.runtime_fence.clone(),
        })
    }
}
impl ClassicLane {
    pub fn prepare(&self, frame: Frame) -> Result<PreparedTransmission, RuntimeError> {
        frame.validate()?;
        if !matches!(frame, Frame::ClassicData { .. }) {
            return Err(crate::Error::Unsupported.into());
        }
        self.0.permit.check()?;
        Ok(PreparedTransmission {
            trace: None,
            session: self.0.session.clone(),
            channel: self.0.channel.clone(),
            generation: self.0.generation,
            frame,
            permit: self.0.permit.clone(),
            execution: self.0.permit.expiring_child(Duration::from_secs(1)),
            runtime_fence: self.0.runtime_fence.clone(),
        })
    }
}
/// Independent low-overhead backend event receiver. A lag is incomplete evidence,
/// never an assertion about where the physical frame was lost.
pub struct MeasurementInput {
    pub events: broadcast::Receiver<Arc<BackendEvent>>,
    pub observation: Observation,
    pub backend_generation: u64,
    pub started: Instant,
}
const LEASE_TTL: Duration = Duration::from_secs(15);
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    ListenOnly,
    Active,
}
#[derive(Debug, Clone)]
pub struct SessionView {
    pub id: String,
    pub channel: String,
    pub mode: Mode,
    pub running: bool,
    pub revision: u64,
    pub generation: u64,
    pub controller: Option<String>,
    pub fault: Option<BackendFault>,
    pub config: crate::Config,
}
#[derive(Debug, Clone)]
pub struct Event {
    pub sequence: u64,
    pub elapsed_ns: u64,
    pub frame: Frame,
    pub direction: &'static str,
    pub metadata: crate::FrameMetadata,
}
#[derive(Debug, Clone)]
pub enum Notice {
    Frame(Event),
    Diagnostic {
        sequence: u64,
        elapsed_ns: u64,
        diagnostic: crate::Diagnostic,
    },
}
#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error("preparation task did not complete normally")]
    PreparationFailed,
    #[error("submission outcome is unknown after a backend task panic")]
    SubmissionUnknown,
    #[error("service is closing")]
    Closed,
    #[error("resource not found")]
    NotFound,
    #[error("channel already has a session")]
    Busy,
    #[error("another client holds control")]
    LeaseConflict,
    #[error("control lease is missing, expired, or stale")]
    LeaseExpired,
    #[error("resource revision changed")]
    RevisionConflict,
    #[error("operation is invalid in the current state")]
    InvalidState,
    #[error("listen-only sessions cannot transmit")]
    ListenOnly,
    #[error("session limit reached")]
    Limit,
    #[error(transparent)]
    Backend(#[from] crate::Error),
}
struct Lease {
    owner: String,
    until: Instant,
    permit: crate::control::Permit,
}
impl Drop for Lease {
    fn drop(&mut self) {
        self.permit.revoke();
    }
}
struct Acquisition {
    id: u64,
    end: std::sync::Mutex<Option<(u64, bool)>>,
    ended: tokio::sync::Notify,
}
#[derive(Clone)]
pub struct Observation {
    pub channel: String,
    inner: Arc<Acquisition>,
    pub start_sequence: u64,
    pub config: crate::Config,
}
impl Observation {
    pub fn epoch(&self) -> u64 {
        self.inner.id
    }
    /// Final published sequence and whether acquisition was interrupted.
    pub fn finished(&self) -> Option<(u64, bool)> {
        *self.inner.end.lock().unwrap()
    }
    /// Wake once the original acquisition ends. Late subscribers also observe it.
    pub async fn ended(&self) {
        loop {
            let notified = self.inner.ended.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.finished().is_some() {
                return;
            }
            notified.await;
        }
    }
}
struct Session {
    view: SessionView,
    lease: Option<Lease>,
    idle_since: Instant,
    events: Option<broadcast::Sender<Event>>,
    notices: Option<broadcast::Sender<Notice>>,
    seq: u64,
    backend_generation: u64,
    acquisition_started: Instant,
    acquisition: Option<Arc<Acquisition>>,
}
struct Inner {
    closing: bool,
    close_summary: Option<crate::CloseSummary>,
    sessions: BTreeMap<String, Session>,
}
struct CatalogSession {
    channel: String,
    device: String,
    control: Arc<Mutex<Inner>>,
}
struct CreationClaim {
    id: String,
    catalog: Arc<std::sync::Mutex<BTreeMap<String, CatalogSession>>>,
    committed: bool,
}
impl Drop for CreationClaim {
    fn drop(&mut self) {
        if !self.committed {
            self.catalog.lock().unwrap().remove(&self.id);
        }
    }
}
/// Atomic reservation of backend-reported device identities across all owners.
pub struct DeviceReservation {
    token: u64,
    ids: Vec<String>,
    registry: Arc<std::sync::Mutex<BTreeMap<String, u64>>>,
}
impl Drop for DeviceReservation {
    fn drop(&mut self) {
        let mut map = self.registry.lock().unwrap();
        for id in &self.ids {
            if map.get(id) == Some(&self.token) {
                map.remove(id);
            }
        }
    }
}
pub struct Runtime {
    stress_admission: Arc<tokio::sync::Semaphore>,
    backend: Arc<dyn Backend>,
    reservations: Arc<std::sync::Mutex<BTreeMap<String, u64>>>,
    controls: std::sync::Mutex<BTreeMap<String, std::sync::Weak<Mutex<Inner>>>>,
    catalog: Arc<std::sync::Mutex<BTreeMap<String, CatalogSession>>>,
    next_session: std::sync::atomic::AtomicU64,
    next_generation: std::sync::atomic::AtomicU64,
    myself: std::sync::Weak<Runtime>,
    started: Instant,
    next_acquisition: std::sync::atomic::AtomicU64,
    shutdown_fence: Arc<std::sync::atomic::AtomicBool>,
    shutdown_completion:
        std::sync::Mutex<Option<tokio::sync::watch::Receiver<Option<crate::CloseSummary>>>>,
}
impl Runtime {
    pub fn new(backend: Arc<dyn Backend>) -> Arc<Self> {
        Arc::new_cyclic(|myself| Self {
            stress_admission: Arc::new(tokio::sync::Semaphore::new(1)),
            backend,
            reservations: Arc::new(std::sync::Mutex::new(BTreeMap::new())),
            myself: myself.clone(),
            controls: Default::default(),
            catalog: Default::default(),
            next_session: std::sync::atomic::AtomicU64::new(0),
            next_generation: std::sync::atomic::AtomicU64::new(0),
            started: Instant::now(),
            next_acquisition: std::sync::atomic::AtomicU64::new(0),
            shutdown_fence: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            shutdown_completion: std::sync::Mutex::new(None),
        })
    }
    fn control_for_channel(
        &self,
        channel: &str,
    ) -> Result<(String, Arc<Mutex<Inner>>), RuntimeError> {
        if self
            .shutdown_fence
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(RuntimeError::Closed);
        }
        let device = self.backend.capabilities(channel)?.device_id;
        let mut controls = self.controls.lock().unwrap();
        controls.retain(|_, v| v.strong_count() > 0);
        let control = controls
            .get(&device)
            .and_then(std::sync::Weak::upgrade)
            .unwrap_or_else(|| {
                let c = Arc::new(Mutex::new(Inner {
                    closing: false,
                    close_summary: None,
                    sessions: BTreeMap::new(),
                }));
                controls.insert(device.clone(), Arc::downgrade(&c));
                c
            });
        Ok((device, control))
    }
    fn control_for_id(&self, id: &str) -> Result<Arc<Mutex<Inner>>, RuntimeError> {
        self.catalog
            .lock()
            .unwrap()
            .get(id)
            .map(|s| s.control.clone())
            .ok_or_else(|| {
                if self
                    .shutdown_fence
                    .load(std::sync::atomic::Ordering::Acquire)
                {
                    RuntimeError::Closed
                } else {
                    RuntimeError::NotFound
                }
            })
    }
    fn active_controls(&self) -> Vec<Arc<Mutex<Inner>>> {
        self.controls
            .lock()
            .unwrap()
            .values()
            .filter_map(std::sync::Weak::upgrade)
            .collect()
    }
    pub fn stress_admission(&self) -> Arc<tokio::sync::Semaphore> {
        self.stress_admission.clone()
    }
    pub fn capabilities(&self, channel: &str) -> Result<crate::Capabilities, crate::Error> {
        self.backend.capabilities(channel)
    }
    /// Fence queued mutations synchronously before asynchronous teardown begins.
    pub fn fence_shutdown(&self) {
        self.shutdown_fence
            .store(true, std::sync::atomic::Ordering::Release);
    }
    pub fn channels(&self) -> Vec<crate::Channel> {
        self.backend.channels()
    }
    fn finish_acquisition(session: &Session, interrupted: bool) {
        if let Some(acquisition) = &session.acquisition {
            let mut end = acquisition.end.lock().unwrap();
            if end.is_none() {
                *end = Some((session.seq, interrupted));
                acquisition.ended.notify_waiters();
            }
        }
    }
    pub async fn sequence(&self, id: &str) -> Result<u64, RuntimeError> {
        let control = self.control_for_id(id)?;
        let inner = control.lock().await;
        inner
            .sessions
            .get(id)
            .map(|session| session.seq)
            .ok_or(RuntimeError::NotFound)
    }
    /// Invoke a nonblocking callback at a publication boundary while sequence
    /// advancement is excluded. The callback must not call back into the runtime.
    pub async fn at_sequence(
        &self,
        id: &str,
        action: impl FnOnce(u64),
    ) -> Result<(), RuntimeError> {
        let control = self.control_for_id(id)?;
        let inner = control.lock().await;
        let session = inner.sessions.get(id).ok_or(RuntimeError::NotFound)?;
        action(session.seq);
        Ok(())
    }
    pub async fn observe(&self, id: &str) -> Result<Observation, RuntimeError> {
        let control = self.control_for_id(id)?;
        let inner = control.lock().await;
        if inner.closing
            || self
                .shutdown_fence
                .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(RuntimeError::Closed);
        }
        let session = inner.sessions.get(id).ok_or(RuntimeError::NotFound)?;
        if !session.view.running {
            return Err(RuntimeError::InvalidState);
        }
        Ok(Observation {
            channel: session.view.channel.clone(),
            inner: session
                .acquisition
                .as_ref()
                .ok_or(RuntimeError::InvalidState)?
                .clone(),
            start_sequence: session.seq,
            config: session.view.config,
        })
    }
    async fn expire(&self, s: &mut Session) {
        if s.lease
            .as_ref()
            .is_some_and(|l| l.until <= Instant::now() || l.permit.check().is_err())
        {
            if let Some(lease) = s.lease.take() {
                lease.permit.revoke();
            }
            s.view.controller = None;
            s.view.running = false;
            s.view.revision += 1;
            s.idle_since = Instant::now();
            if self.backend.stop(&s.view.channel).await.is_err() {
                self.fault(s, BackendFault::StopFailed).await;
            }
            Self::finish_acquisition(s, true);
        }
    }
    async fn fault(&self, s: &mut Session, fault: BackendFault) {
        Self::finish_acquisition(s, true);
        s.view.fault = Some(fault);
        s.view.running = false;
        s.view.controller = None;
        s.view.revision += 1;
        if let Some(lease) = s.lease.take() {
            lease.permit.revoke();
        }
        s.idle_since = Instant::now();
        // Closing the old event channel wakes subscribers. Recovery creates a
        // fresh channel; an old subscription can never resume automatically.
        s.events = None;
        s.notices = None;
        let _ = self.backend.stop(&s.view.channel).await;
    }
    pub async fn maintain(&self) {
        futures_util::future::join_all(self.active_controls().into_iter().map(
            |control| async move {
                let mut i = control.lock().await;
                for s in i.sessions.values_mut() {
                    self.expire(s).await;
                }
                let idle: Vec<_> = i
                    .sessions
                    .iter()
                    .filter(|(_, s)| {
                        s.lease.is_none() && s.idle_since.elapsed() >= Duration::from_secs(60)
                    })
                    .map(|(id, _)| id.clone())
                    .collect();
                for id in idle {
                    let channel = i.sessions[&id].view.channel.clone();
                    if self.backend.close(&channel).await.is_ok() {
                        i.sessions.remove(&id);
                        self.catalog.lock().unwrap().remove(&id);
                    } else if let Some(session) = i.sessions.get_mut(&id) {
                        self.fault(session, BackendFault::StopFailed).await;
                    }
                }
            },
        ))
        .await;
    }
    pub async fn create(
        self: &Arc<Self>,
        channel: &str,
        mode: Mode,
    ) -> Result<SessionView, RuntimeError> {
        self.create_configured(
            channel,
            crate::Config {
                listen_only: mode == Mode::ListenOnly,
                ..crate::Config::default()
            },
        )
        .await
    }
    pub fn cleanup_observation(&self, channel: &str) -> Option<crate::CloseSummary> {
        self.backend.cleanup_observation(channel)
    }
    pub async fn reserve_devices(
        &self,
        channels: &[String],
    ) -> Result<DeviceReservation, RuntimeError> {
        let catalog = self.catalog.lock().unwrap();
        if self
            .shutdown_fence
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(RuntimeError::Closed);
        }
        let mut ids = channels
            .iter()
            .map(|c| self.backend.capabilities(c).map(|v| v.device_id))
            .collect::<Result<Vec<_>, _>>()?;
        ids.sort();
        ids.dedup();
        if catalog.values().any(|s| ids.contains(&s.device)) {
            return Err(RuntimeError::Busy);
        }
        let mut map = self.reservations.lock().unwrap();
        if ids.iter().any(|id| map.contains_key(id)) {
            return Err(RuntimeError::Busy);
        }
        let token = self
            .next_acquisition
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        for id in &ids {
            map.insert(id.clone(), token);
        }
        Ok(DeviceReservation {
            token,
            ids,
            registry: self.reservations.clone(),
        })
    }
    pub async fn create_configured(
        self: &Arc<Self>,
        channel: &str,
        config: crate::Config,
    ) -> Result<SessionView, RuntimeError> {
        self.create_reserved(channel, config, None).await
    }
    pub async fn create_reserved(
        self: &Arc<Self>,
        channel: &str,
        config: crate::Config,
        reservation: Option<&DeviceReservation>,
    ) -> Result<SessionView, RuntimeError> {
        let (device, control) = self.control_for_channel(channel)?;
        let inner = control.clone().lock_owned().await;
        let runtime = self.clone();
        let channel = channel.to_string();
        let token = reservation.map(|r| {
            (
                r.token,
                r.ids.clone(),
                Arc::ptr_eq(&r.registry, &self.reservations),
            )
        });
        tokio::spawn(async move {
            runtime
                .create_locked(channel, config, token, device, control, inner)
                .await
        })
        .await
        .map_err(|_| RuntimeError::PreparationFailed)?
    }
    async fn create_locked(
        self: Arc<Self>,
        channel: String,
        config: crate::Config,
        reservation: Option<(u64, Vec<String>, bool)>,
        device: String,
        control: Arc<Mutex<Inner>>,
        mut i: tokio::sync::OwnedMutexGuard<Inner>,
    ) -> Result<SessionView, RuntimeError> {
        let channel = channel.as_str();
        let mode = if config.listen_only {
            Mode::ListenOnly
        } else {
            Mode::Active
        };
        if i.closing
            || self
                .shutdown_fence
                .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(RuntimeError::Closed);
        }
        let id = {
            let mut catalog = self.catalog.lock().unwrap();
            if self
                .shutdown_fence
                .load(std::sync::atomic::Ordering::Acquire)
            {
                return Err(RuntimeError::Closed);
            }
            let reservations = self.reservations.lock().unwrap();
            if match &reservation {
                Some((t, ids, same)) => {
                    !*same || !ids.contains(&device) || reservations.get(&device) != Some(t)
                }
                None => reservations.contains_key(&device),
            } {
                return Err(RuntimeError::Busy);
            }
            if catalog.values().any(|s| s.channel == channel) {
                return Err(RuntimeError::Busy);
            }
            if catalog.len() >= 8 {
                return Err(RuntimeError::Limit);
            }
            let id = format!(
                "s{}",
                self.next_session
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                    + 1
            );
            catalog.insert(
                id.clone(),
                CatalogSession {
                    channel: channel.into(),
                    device,
                    control: control.clone(),
                },
            );
            id
        };
        let mut claim = CreationClaim {
            id: id.clone(),
            catalog: self.catalog.clone(),
            committed: false,
        };
        self.backend.configure(channel, config).await?;
        let mut input = self.backend.subscribe(channel)?;
        let backend_generation = self.backend.check_channel(channel)?;
        if self
            .shutdown_fence
            .load(std::sync::atomic::Ordering::Acquire)
        {
            let _ = self.backend.close(channel).await;
            return Err(RuntimeError::Closed);
        }
        let view = SessionView {
            id: id.clone(),
            channel: channel.into(),
            mode,
            running: false,
            revision: 1,
            generation: 0,
            controller: None,
            fault: None,
            config,
        };
        i.sessions.insert(
            id.clone(),
            Session {
                view: view.clone(),
                lease: None,
                idle_since: Instant::now(),
                events: Some(broadcast::channel(256).0),
                notices: Some(broadcast::channel(256).0),
                seq: 0,
                backend_generation,
                acquisition_started: Instant::now(),
                acquisition: None,
            },
        );
        claim.committed = true;
        let weak = Arc::downgrade(&self);
        let weak_control = Arc::downgrade(&control);
        tokio::spawn(async move {
            loop {
                // A short poll timeout lets removed sessions release their bus receiver.
                let event = match tokio::time::timeout(Duration::from_secs(1), input.recv()).await {
                    Ok(Ok(e)) => Some(e),
                    Ok(Err(broadcast::error::RecvError::Lagged(_))) => {
                        if let Some(r) = weak.upgrade() {
                            let Some(control) = weak_control.upgrade() else {
                                break;
                            };
                            let mut i = control.lock().await;
                            if let Some(s) = i.sessions.get_mut(&id) {
                                r.fault(s, BackendFault::ReceiveOverflow).await;
                            }
                        }
                        continue;
                    }
                    Ok(Err(broadcast::error::RecvError::Closed)) => {
                        if let Some(r) = weak.upgrade() {
                            let Some(control) = weak_control.upgrade() else {
                                break;
                            };
                            let mut i = control.lock().await;
                            if let Some(s) = i.sessions.get_mut(&id) {
                                r.fault(s, BackendFault::Disconnected).await;
                            }
                        }
                        break;
                    }
                    Err(_) => None,
                };
                let Some(r) = weak.upgrade() else { break };
                let Some(control) = weak_control.upgrade() else {
                    break;
                };
                let mut i = control.lock().await;
                let Some(s) = i.sessions.get_mut(&id) else {
                    break;
                };
                r.expire(s).await;
                match event.as_deref() {
                    Some(BackendEvent::Fault { fault, generation })
                        if *generation > s.backend_generation =>
                    {
                        r.fault(s, *fault).await
                    }
                    Some(BackendEvent::Frame(e))
                        if s.view.running
                            && e.generation >= s.backend_generation
                            && e.received_at >= s.acquisition_started
                            && (!matches!(e.frame, Frame::FdData { .. })
                                || s.view.config.data_bitrate.is_some()) =>
                    {
                        s.seq += 1;
                        if let Some(events) = &s.events {
                            let event = Event {
                                sequence: s.seq,
                                elapsed_ns: e
                                    .received_at
                                    .saturating_duration_since(r.started)
                                    .as_nanos()
                                    .min(u64::MAX as u128)
                                    as u64,
                                metadata: e.metadata.clone(),
                                frame: e.frame.clone(),
                                direction: if e.source_channel == s.view.channel {
                                    "tx"
                                } else {
                                    "rx"
                                },
                            };
                            let _ = events.send(event.clone());
                            if let Some(notices) = &s.notices {
                                let _ = notices.send(Notice::Frame(event));
                            }
                        }
                    }
                    Some(BackendEvent::Diagnostic(diagnostic))
                        if s.view.running
                            && diagnostic.generation >= s.backend_generation
                            && diagnostic.received_at >= s.acquisition_started =>
                    {
                        s.seq += 1;
                        if let Some(notices) = &s.notices {
                            let _ = notices.send(Notice::Diagnostic {
                                sequence: s.seq,
                                elapsed_ns: diagnostic
                                    .received_at
                                    .saturating_duration_since(r.started)
                                    .as_nanos()
                                    .min(u64::MAX as u128)
                                    as u64,
                                diagnostic: diagnostic.clone(),
                            });
                        }
                    }
                    _ => {}
                }
            }
        });
        Ok(view)
    }
    /// Observe cached session state without lease maintenance or backend I/O.
    /// Expired leases may still appear controlled/running until maintenance runs.
    pub async fn session_snapshot(&self) -> Vec<SessionView> {
        let mut views = vec![];
        for control in self.active_controls() {
            views.extend(
                control
                    .lock()
                    .await
                    .sessions
                    .values()
                    .map(|s| s.view.clone()),
            );
        }
        views.sort_by(|a, b| a.id.cmp(&b.id));
        views
    }
    pub async fn list(&self) -> Vec<SessionView> {
        self.maintain().await;
        self.session_snapshot().await
    }
    pub async fn get(&self, id: &str) -> Result<SessionView, RuntimeError> {
        let control = self.control_for_id(id)?;
        let mut i = control.lock().await;
        let s = i.sessions.get_mut(id).ok_or(RuntimeError::NotFound)?;
        self.expire(s).await;
        Ok(s.view.clone())
    }
    pub async fn acquire(&self, id: &str, owner: &str) -> Result<SessionView, RuntimeError> {
        let control = self.control_for_id(id)?;
        let mut i = control.lock().await;
        if i.closing
            || self
                .shutdown_fence
                .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(RuntimeError::Closed);
        }

        let s = i.sessions.get_mut(id).ok_or(RuntimeError::NotFound)?;
        self.expire(s).await;
        if s.lease.is_some() {
            return Err(RuntimeError::LeaseConflict);
        }
        let generation = self
            .next_generation
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        let s = i.sessions.get_mut(id).unwrap();
        s.view.generation = generation;
        s.view.controller = Some(owner.into());
        s.view.revision += 1;
        s.lease = Some(Lease {
            owner: owner.into(),
            until: Instant::now() + LEASE_TTL,
            permit: crate::control::Permit::new(LEASE_TTL, self.shutdown_fence.clone()),
        });
        Ok(s.view.clone())
    }
    async fn authorize(
        &self,
        s: &mut Session,
        owner: &str,
        generation: u64,
    ) -> Result<(), RuntimeError> {
        self.expire(s).await;
        if s.view.generation != generation || !s.lease.as_ref().is_some_and(|l| l.owner == owner) {
            return Err(RuntimeError::LeaseExpired);
        }
        Ok(())
    }
    pub async fn renew(&self, id: &str, owner: &str, g: u64) -> Result<SessionView, RuntimeError> {
        let control = self.control_for_id(id)?;
        let mut i = control.lock().await;
        if i.closing
            || self
                .shutdown_fence
                .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(RuntimeError::Closed);
        }

        let s = i.sessions.get_mut(id).ok_or(RuntimeError::NotFound)?;
        self.authorize(s, owner, g).await?;
        let lease = s.lease.as_mut().unwrap();
        lease.until = Instant::now() + LEASE_TTL;
        lease.permit.renew(LEASE_TTL);
        Ok(s.view.clone())
    }
    pub async fn running(
        &self,
        id: &str,
        owner: &str,
        g: u64,
        running: bool,
    ) -> Result<SessionView, RuntimeError> {
        let control = self.control_for_id(id)?;
        let mut i = control.lock().await;
        if i.closing
            || self
                .shutdown_fence
                .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(RuntimeError::Closed);
        }

        let s = i.sessions.get_mut(id).ok_or(RuntimeError::NotFound)?;
        self.authorize(s, owner, g).await?;
        if s.view.running == running && s.view.fault.is_none() {
            return Ok(s.view.clone());
        }
        if running {
            s.backend_generation = match self.backend.check_channel(&s.view.channel) {
                Ok(generation) => generation,
                Err(error) => {
                    if let crate::Error::Fault(fault) = &error {
                        self.fault(s, *fault).await;
                    }
                    return Err(error.into());
                }
            };
            if !s.view.running {
                s.acquisition_started = Instant::now();
            }
            if let Err(error) = self
                .backend
                .start(&s.view.channel, s.lease.as_ref().unwrap().permit.clone())
                .await
            {
                if let crate::Error::Fault(fault) = &error {
                    self.fault(s, *fault).await;
                }
                return Err(error.into());
            }
            s.acquisition = Some(Arc::new(Acquisition {
                id: self
                    .next_acquisition
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                    + 1,
                end: std::sync::Mutex::new(None),
                ended: tokio::sync::Notify::new(),
            }));
            s.view.fault = None;
            if s.events.is_none() {
                s.events = Some(broadcast::channel(256).0);
                s.notices = Some(broadcast::channel(256).0);
            }
        }
        if !running {
            let lease = s.lease.as_mut().unwrap();
            lease.permit.revoke();
            lease.permit = lease
                .permit
                .replacement(lease.until.saturating_duration_since(Instant::now()));
            if let Err(error) = self.backend.stop(&s.view.channel).await {
                self.fault(s, BackendFault::StopFailed).await;
                return Err(error.into());
            }
        }
        if !running {
            Self::finish_acquisition(s, false);
        }
        s.view.running = running;
        s.view.revision += 1;
        Ok(s.view.clone())
    }
    pub async fn configure(
        &self,
        id: &str,
        owner: &str,
        g: u64,
        revision: u64,
        config: crate::Config,
    ) -> Result<SessionView, RuntimeError> {
        let control = self.control_for_id(id)?;
        let mut inner = control.lock().await;
        if inner.closing
            || self
                .shutdown_fence
                .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(RuntimeError::Closed);
        }
        let session = inner.sessions.get_mut(id).ok_or(RuntimeError::NotFound)?;
        self.authorize(session, owner, g).await?;
        if session.view.running {
            return Err(RuntimeError::InvalidState);
        }
        if session.view.revision != revision {
            return Err(RuntimeError::RevisionConflict);
        }
        if let Err(error) = self.backend.configure(&session.view.channel, config).await {
            if let crate::Error::Fault(fault) = &error {
                self.fault(session, *fault).await;
            }
            return Err(error.into());
        }
        let lease = session.lease.as_mut().unwrap();
        lease.permit.revoke();
        lease.permit = lease
            .permit
            .replacement(lease.until.saturating_duration_since(Instant::now()));
        session.view.config = config;
        session.view.mode = if config.listen_only {
            Mode::ListenOnly
        } else {
            Mode::Active
        };
        session.view.revision += 1;
        Ok(session.view.clone())
    }
    pub async fn release(&self, id: &str, owner: &str, g: u64) -> Result<(), RuntimeError> {
        let control = self.control_for_id(id)?;
        let mut i = control.lock().await;
        if i.closing
            || self
                .shutdown_fence
                .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(RuntimeError::Closed);
        }

        let s = i.sessions.get_mut(id).ok_or(RuntimeError::NotFound)?;
        self.authorize(s, owner, g).await?;
        if let Some(lease) = s.lease.take() {
            lease.permit.revoke();
        }
        if self.backend.stop(&s.view.channel).await.is_err() {
            self.fault(s, BackendFault::StopFailed).await;
            return Err(crate::Error::Fault(BackendFault::StopFailed).into());
        }
        Self::finish_acquisition(s, true);
        s.view.controller = None;
        s.view.running = false;
        s.idle_since = Instant::now();
        s.view.revision += 1;
        Ok(())
    }
    pub async fn close(&self, id: &str, owner: &str, g: u64) -> Result<(), RuntimeError> {
        self.close_checked(id, owner, g, None).await
    }
    /// Cleanup may only affect the acquisition it originally created. Epoch zero
    /// denotes a session reserved but not started; a later start is protected too.
    pub async fn close_at_epoch(
        &self,
        id: &str,
        owner: &str,
        g: u64,
        epoch: u64,
    ) -> Result<(), RuntimeError> {
        self.close_checked(id, owner, g, Some(epoch)).await
    }
    async fn close_checked(
        &self,
        id: &str,
        owner: &str,
        g: u64,
        epoch: Option<u64>,
    ) -> Result<(), RuntimeError> {
        let control = self.control_for_id(id)?;
        let mut i = control.lock().await;
        if i.closing
            || self
                .shutdown_fence
                .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(RuntimeError::Closed);
        }

        let s = i.sessions.get_mut(id).ok_or(RuntimeError::NotFound)?;
        if epoch.is_some_and(|expected| {
            s.view.generation != g || s.acquisition.as_ref().map_or(0, |a| a.id) != expected
        }) {
            return Err(RuntimeError::RevisionConflict);
        }
        self.authorize(s, owner, g).await?;
        if let Some(lease) = s.lease.as_ref() {
            lease.permit.revoke();
        }
        if let Err(error) = self.backend.close(&s.view.channel).await {
            self.fault(s, BackendFault::StopFailed).await;
            return Err(error.into());
        }
        Self::finish_acquisition(s, false);
        i.sessions.remove(id);
        self.catalog.lock().unwrap().remove(id);
        Ok(())
    }
    pub async fn prepare_send(
        &self,
        id: &str,
        owner: &str,
        g: u64,
        frame: Frame,
    ) -> Result<PreparedTransmission, RuntimeError> {
        frame.validate()?;
        // No work is detached while authorization is queued. Once the lock is
        // acquired, expiry/fault cleanup must outlive a cancelled waiter.
        let inner = self.control_for_id(id)?.lock_owned().await;
        let runtime = self.myself.upgrade().ok_or(RuntimeError::Closed)?;
        let id = id.to_owned();
        let owner = owner.to_owned();
        tokio::spawn(async move { runtime.prepare_locked(inner, &id, &owner, g, frame).await })
            .await
            .map_err(|_| RuntimeError::PreparationFailed)?
    }
    async fn prepare_locked(
        &self,
        mut i: tokio::sync::OwnedMutexGuard<Inner>,
        id: &str,
        owner: &str,
        g: u64,
        frame: Frame,
    ) -> Result<PreparedTransmission, RuntimeError> {
        if i.closing
            || self
                .shutdown_fence
                .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(RuntimeError::Closed);
        }

        let s = i.sessions.get_mut(id).ok_or(RuntimeError::NotFound)?;
        self.authorize(s, owner, g).await?;
        if !s.view.running {
            return Err(RuntimeError::InvalidState);
        }
        if s.view.mode == Mode::ListenOnly {
            return Err(RuntimeError::ListenOnly);
        }
        if matches!(frame, Frame::FdData { .. }) && s.view.config.data_bitrate.is_none() {
            return Err(crate::Error::Unsupported.into());
        }
        if let Err(error) = self.backend.check_channel(&s.view.channel) {
            if let crate::Error::Fault(fault) = &error {
                self.fault(s, *fault).await;
            }
            return Err(error.into());
        }
        let channel = s.view.channel.clone();
        let permit = s.lease.as_ref().unwrap().permit.clone();
        let execution = permit.expiring_child(Duration::from_secs(1));
        Ok(PreparedTransmission {
            trace: None,
            session: id.into(),
            channel,
            generation: g,
            frame,
            permit,
            execution,
            runtime_fence: self.shutdown_fence.clone(),
        })
    }
    pub async fn data_lane(
        &self,
        id: &str,
        owner: &str,
        generation: u64,
        frame: Frame,
    ) -> Result<DataLane, RuntimeError> {
        if matches!(frame, Frame::ClassicRemote { .. }) {
            return Err(crate::Error::Unsupported.into());
        }
        let template = self.prepare_send(id, owner, generation, frame).await?;
        let control = self.control_for_id(id)?;
        let inner = control.lock().await;
        let session = inner.sessions.get(id).ok_or(RuntimeError::NotFound)?;
        template.permit.check()?;
        Ok(DataLane {
            template,
            fd: session.view.config.data_bitrate.is_some(),
        })
    }
    pub async fn classic_lane(
        &self,
        id: &str,
        owner: &str,
        generation: u64,
        frame: Frame,
    ) -> Result<ClassicLane, RuntimeError> {
        if !matches!(frame, Frame::ClassicData { .. }) {
            return Err(crate::Error::Unsupported.into());
        }
        Ok(ClassicLane(
            self.prepare_send(id, owner, generation, frame).await?,
        ))
    }
    pub async fn measurement_input(&self, id: &str) -> Result<MeasurementInput, RuntimeError> {
        let control = self.control_for_id(id)?;
        let inner = control.lock().await;
        let s = inner.sessions.get(id).ok_or(RuntimeError::NotFound)?;
        if !s.view.running {
            return Err(RuntimeError::InvalidState);
        }
        Ok(MeasurementInput {
            events: self.backend.subscribe(&s.view.channel)?,
            observation: Observation {
                channel: s.view.channel.clone(),
                inner: s
                    .acquisition
                    .as_ref()
                    .ok_or(RuntimeError::InvalidState)?
                    .clone(),
                start_sequence: s.seq,
                config: s.view.config,
            },
            backend_generation: s.backend_generation,
            started: s.acquisition_started,
        })
    }
    pub async fn send(
        &self,
        id: &str,
        owner: &str,
        g: u64,
        frame: Frame,
    ) -> Result<(), RuntimeError> {
        let prepared = self.prepare_send(id, owner, g, frame).await?;
        self.submit_prepared(prepared).await
    }
    pub fn source_evidence(&self, channel: &str) -> Vec<crate::measurement::SourceEvidence> {
        self.backend.source_evidence(channel)
    }
    pub async fn submit_prepared(
        &self,
        prepared: PreparedTransmission,
    ) -> Result<(), RuntimeError> {
        let PreparedTransmission {
            trace,
            session: id,
            channel,
            generation: g,
            frame,
            permit,
            runtime_fence,
            execution,
        } = prepared;
        if !Arc::ptr_eq(&runtime_fence, &self.shutdown_fence) {
            return Err(RuntimeError::InvalidState);
        }
        execution.check()?;
        let outcome = std::panic::AssertUnwindSafe(async {
            if let Some(t) = &trace {
                t.dispatched();
            }
            if let Some(t) = &trace {
                self.backend
                    .submit_observed(&channel, frame, execution.clone(), t.clone())
                    .await
            } else {
                self.backend
                    .submit(&channel, frame, execution.clone())
                    .await
            }
        })
        .catch_unwind()
        .await;
        let (result, panicked) = match outcome {
            Ok(result) => (result, false),
            Err(_) => (Err(crate::Error::Fault(BackendFault::TransmitFailed)), true),
        };
        if let Err(error) = result {
            let control = self.control_for_id(&id).ok();
            if let Some(control) = control {
                let mut inner = control.lock().await;
                if let Some(session) = inner.sessions.get_mut(&id)
                    && session.view.generation == g
                    && session
                        .lease
                        .as_ref()
                        .is_some_and(|lease| lease.permit.same_as(&permit))
                    && let crate::Error::Fault(fault) = &error
                {
                    self.fault(session, *fault).await;
                }
            }
            if let Some(t) = &trace {
                t.completed();
            }
            return Err(if panicked {
                RuntimeError::SubmissionUnknown
            } else {
                error.into()
            });
        }
        if let Some(t) = &trace {
            t.completed();
        }
        Ok(())
    }
    pub async fn subscribe_all(
        &self,
        id: &str,
    ) -> Result<broadcast::Receiver<Notice>, RuntimeError> {
        let control = self.control_for_id(id)?;
        let inner = control.lock().await;
        let session = inner.sessions.get(id).ok_or(RuntimeError::NotFound)?;
        session
            .notices
            .as_ref()
            .map(|events| events.subscribe())
            .ok_or_else(|| {
                RuntimeError::Backend(crate::Error::Fault(
                    session.view.fault.unwrap_or(BackendFault::Disconnected),
                ))
            })
    }
    pub async fn subscribe(&self, id: &str) -> Result<broadcast::Receiver<Event>, RuntimeError> {
        let control = self.control_for_id(id)?;
        let i = control.lock().await;
        let s = i.sessions.get(id).ok_or(RuntimeError::NotFound)?;
        s.events.as_ref().map(|e| e.subscribe()).ok_or_else(|| {
            RuntimeError::Backend(crate::Error::Fault(
                s.view.fault.unwrap_or(BackendFault::Disconnected),
            ))
        })
    }
    pub async fn revoke(&self, owner: &str) {
        futures_util::future::join_all(self.active_controls().into_iter().map(
            |control| async move {
                let mut i = control.lock().await;
                for s in i.sessions.values_mut() {
                    if s.lease.as_ref().is_some_and(|l| l.owner == owner) {
                        if let Some(lease) = s.lease.take() {
                            lease.permit.revoke();
                        }
                        if self.backend.stop(&s.view.channel).await.is_err() {
                            self.fault(s, BackendFault::StopFailed).await;
                            continue;
                        }
                        Self::finish_acquisition(s, true);
                        s.view.controller = None;
                        s.view.running = false;
                        s.idle_since = Instant::now();
                        s.view.revision += 1;
                    }
                }
            },
        ))
        .await;
    }
    pub async fn shutdown(self: &Arc<Self>) -> crate::CloseSummary {
        self.shutdown_fence
            .store(true, std::sync::atomic::Ordering::Release);
        let mut completion = {
            let mut current = self.shutdown_completion.lock().unwrap();
            if current.is_none() {
                let (completed, receive) = tokio::sync::watch::channel(None);
                *current = Some(receive);
                let runtime = self.clone();
                tokio::spawn(async move {
                    let report = runtime.shutdown_owned().await;
                    completed.send_replace(Some(report));
                });
            }
            current.as_ref().unwrap().clone()
        };
        loop {
            let report = { completion.borrow().clone() };
            if let Some(report) = report {
                return report;
            }
            if completion.changed().await.is_err() {
                return crate::CloseSummary {
                    unknown_transmissions: 0,
                    failures: vec![
                        "runtime shutdown task failed; cleanup outcome unavailable".into(),
                    ],
                };
            }
        }
    }
    async fn shutdown_control(&self, control: Arc<Mutex<Inner>>) -> crate::CloseSummary {
        let mut inner = control.lock().await;
        if let Some(summary) = &inner.close_summary {
            return summary.clone();
        }
        inner.closing = true;
        let mut summary = crate::CloseSummary::default();
        for session in inner.sessions.values_mut() {
            Self::finish_acquisition(session, true);
            if let Some(lease) = session.lease.take() {
                lease.permit.revoke();
            }
            if let Err(error) = self.backend.close(&session.view.channel).await {
                summary.failures.push(format!("channel close: {error}"));
                self.fault(session, BackendFault::StopFailed).await;
            }
        }
        for id in inner.sessions.keys() {
            self.catalog.lock().unwrap().remove(id);
        }
        inner.sessions.clear();
        inner.close_summary = Some(summary.clone());
        summary
    }
    async fn shutdown_owned(&self) -> crate::CloseSummary {
        let mut summary = crate::CloseSummary::default();
        for report in futures_util::future::join_all(
            self.active_controls()
                .into_iter()
                .map(|c| self.shutdown_control(c)),
        )
        .await
        {
            summary.failures.extend(report.failures);
        }
        match self.backend.shutdown_report().await {
            Ok(report) => {
                summary.unknown_transmissions = report.unknown_transmissions;
                summary.failures.extend(report.failures);
            }
            Err(error) => summary.failures.push(format!("backend close: {error}")),
        }

        summary
    }
}
