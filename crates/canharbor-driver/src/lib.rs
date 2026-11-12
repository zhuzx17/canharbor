// SPDX-License-Identifier: AGPL-3.0-or-later
//! Owned worker boundary for optional native driver adapters.
use canharbor_core::{
    Backend, BackendEvent, BackendFault, BackendFuture, BusFrame, Capabilities, Channel,
    CloseSummary, Config, Diagnostic, Error, Frame, FrameMetadata, control::Permit,
};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, SyncSender, TrySendError},
    },
    thread,
    time::Duration,
};
use tokio::sync::{broadcast, oneshot, watch};
#[derive(Clone)]
pub struct Descriptor {
    pub key: u32,
    pub label: String,
    pub capabilities: Capabilities,
}
pub enum Event {
    Frame {
        frame: Frame,
        metadata: FrameMetadata,
        received_at: tokio::time::Instant,
        echo: bool,
    },
    Diagnostic(Diagnostic),
}
/// Implementations own their foreign library and all handles. Construction and
/// descriptors must not start controllers. Receive is nonblocking. Mutating
/// calls check `guard` before each adapter-controlled API or I/O call.
/// Opaque foreign calls may perform several I/O operations internally; the
/// worker cannot preempt them. Stop never retries
/// a transmission and reports outcomes it cannot establish. Stop is idempotent,
/// retires each unknown count once, and only releases handles owned by this driver.
pub trait Driver: Send + 'static {
    fn source_evidence(&self) -> Vec<canharbor_core::measurement::SourceEvidence> {
        vec![]
    }
    fn descriptors(&self) -> Vec<Descriptor>;
    fn start(
        &mut self,
        key: u32,
        config: Config,
        guard: &dyn Fn() -> Result<(), Error>,
    ) -> Result<(), Error>;
    fn stop(&mut self, key: u32) -> Result<CloseSummary, Error>;
    fn submit(
        &mut self,
        key: u32,
        frame: Frame,
        guard: &dyn Fn() -> Result<(), Error>,
    ) -> Result<(), Error>;
    fn receive(&mut self, key: u32) -> Result<Option<Event>, Error>;
}
struct Shared {
    fault: Mutex<Option<BackendFault>>,
    cleanup: Mutex<CloseSummary>,
    generation: AtomicU64,
    events: broadcast::Sender<Arc<BackendEvent>>,
}
#[derive(Clone)]
struct Info {
    descriptor: Descriptor,
    id: String,
    shared: Arc<Shared>,
}
struct Active {
    info: Info,
    config: Option<Config>,
    permit: Option<Permit>,
}
enum Operation {
    Configure(Config),
    Start(Permit),
    Stop,
    Submit(Frame, Permit),
}
struct Request {
    trace: Option<canharbor_core::measurement::SubmissionTrace>,
    key: u32,
    operation: Operation,
    reply: oneshot::Sender<Result<(), Error>>,
}
pub struct DriverBackend {
    sources: Vec<canharbor_core::measurement::SourceEvidence>,
    channels: BTreeMap<String, Info>,
    closing: Arc<AtomicBool>,
    commands: SyncSender<Request>,
    worker: Mutex<Option<thread::JoinHandle<CloseSummary>>>,
    completion: watch::Sender<Option<Result<CloseSummary, BackendFault>>>,
}
struct Opening(Option<Arc<AtomicBool>>);
impl Drop for Opening {
    fn drop(&mut self) {
        if let Some(closing) = &self.0 {
            closing.store(true, Ordering::Release);
        }
    }
}
fn collect(total: &mut CloseSummary, summary: CloseSummary) {
    total.unknown_transmissions = total
        .unknown_transmissions
        .saturating_add(summary.unknown_transmissions);
    let remaining = 32usize.saturating_sub(total.failures.len());
    total
        .failures
        .extend(summary.failures.into_iter().take(remaining));
}
fn stop(
    driver: &mut dyn Driver,
    key: u32,
    active: &mut Active,
    total: &mut CloseSummary,
) -> Result<(), Error> {
    active.permit = None;
    match driver.stop(key) {
        Ok(summary) => {
            collect(
                &mut active.info.shared.cleanup.lock().unwrap(),
                summary.clone(),
            );
            let success = summary.failures.is_empty();
            collect(total, summary);
            if success {
                Ok(())
            } else {
                Err(Error::Fault(BackendFault::StopFailed))
            }
        }
        Err(error) => {
            let detail = format!("native channel {key} cleanup failed: {error}");
            if total.failures.len() < 32 {
                total.failures.push(detail.clone());
            }
            let mut observation = active.info.shared.cleanup.lock().unwrap();
            if observation.failures.len() < 32 {
                observation.failures.push(detail);
            }
            Err(error)
        }
    }
}
fn fault(active: &mut Active, reason: BackendFault) {
    *active.info.shared.fault.lock().unwrap() = Some(reason);
    let generation = active.info.shared.generation.fetch_add(1, Ordering::AcqRel) + 1;
    let _ = active
        .info
        .shared
        .events
        .send(Arc::new(BackendEvent::Fault {
            fault: reason,
            generation,
        }));
}
fn publish(active: &Active, event: Event) -> Result<bool, Error> {
    let generation = active.info.shared.generation.load(Ordering::Acquire);
    let (event, overflow) = match event {
        Event::Frame {
            frame,
            metadata,
            received_at,
            echo,
        } => {
            frame.validate()?;
            (
                BackendEvent::Frame(BusFrame {
                    frame,
                    metadata,
                    received_at,
                    generation,
                    source_channel: if echo {
                        active.info.id.clone()
                    } else {
                        String::new()
                    },
                }),
                false,
            )
        }
        Event::Diagnostic(mut diagnostic) => {
            diagnostic.generation = generation;
            let overflow = diagnostic.overflow;
            (BackendEvent::Diagnostic(diagnostic), overflow)
        }
    };
    let _ = active.info.shared.events.send(Arc::new(event));
    Ok(overflow)
}
impl DriverBackend {
    /// `epoch` is unique to this open and must not be reused after reconnect.
    pub async fn open(
        factory: impl FnOnce() -> Result<Box<dyn Driver>, Error> + Send + 'static,
        epoch: String,
    ) -> Result<Self, Error> {
        if epoch.is_empty()
            || epoch.len() > 64
            || !epoch
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(Error::InvalidState);
        }
        let closing = Arc::new(AtomicBool::new(false));
        let mut opening = Opening(Some(closing.clone()));
        let state = closing.clone();
        let (commands, input) = mpsc::sync_channel::<Request>(32);
        let (ready_tx, ready_rx) = oneshot::channel();
        let worker = thread::Builder::new()
            .name("canharbor-native-driver".into())
            .spawn(move || {
                let mut driver = match factory() {
                    Ok(driver) => driver,
                    Err(error) => {
                        let _ = ready_tx.send(Err(error));
                        return CloseSummary::default();
                    }
                };
                let sources = driver.source_evidence();
                let descriptors = driver.descriptors();
                if descriptors.is_empty() || descriptors.len() > 32 {
                    let _ = ready_tx.send(Err(Error::InvalidState));
                    return CloseSummary::default();
                }
                let mut channels = BTreeMap::new();
                for mut descriptor in descriptors {
                    if channels.contains_key(&descriptor.key)
                        || descriptor.capabilities.backend.is_empty()
                        || !descriptor
                            .capabilities
                            .backend
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
                    {
                        let _ = ready_tx.send(Err(Error::InvalidState));
                        return CloseSummary::default();
                    }
                    let id = format!(
                        "{}:{epoch}:{}",
                        descriptor.capabilities.backend, descriptor.key
                    );
                    descriptor.capabilities.device_id =
                        format!("{}:{epoch}", descriptor.capabilities.device_id);
                    let shared = Arc::new(Shared {
                        fault: Mutex::new(None),
                        cleanup: Mutex::new(CloseSummary::default()),
                        generation: AtomicU64::new(0),
                        events: broadcast::channel(256).0,
                    });
                    let key = descriptor.key;
                    channels.insert(
                        key,
                        Active {
                            info: Info {
                                descriptor,
                                id,
                                shared,
                            },
                            config: None,
                            permit: None,
                        },
                    );
                }
                let _ = ready_tx.send(Ok((
                    sources,
                    channels
                        .values()
                        .map(|channel| channel.info.clone())
                        .collect::<Vec<_>>(),
                )));
                let mut total = CloseSummary::default();
                while !state.load(Ordering::Acquire) {
                    match input.recv_timeout(Duration::from_millis(2)) {
                        Ok(Request {
                            trace,
                            key,
                            operation,
                            reply,
                        }) => {
                            if let Some(t) = &trace {
                                t.worker_started();
                            }
                            let Some(active) = channels.get_mut(&key) else {
                                let _ = reply.send(Err(Error::UnknownChannel));
                                continue;
                            };
                            if reply.is_closed() && !matches!(operation, Operation::Stop) {
                                continue;
                            }
                            if let Some(reason) = *active.info.shared.fault.lock().unwrap() {
                                let _ = reply.send(Err(Error::Fault(reason)));
                                continue;
                            }
                            let starting = matches!(operation, Operation::Start(_));
                            let stopping = matches!(operation, Operation::Stop);
                            let result = match operation {
                                Operation::Configure(config) => {
                                    let caps = &active.info.descriptor.capabilities;
                                    if active.permit.is_some() {
                                        Err(Error::InvalidState)
                                    } else if config.nominal_bitrate == 0
                                        || config.data_bitrate == Some(0)
                                        || (config.data_bitrate.is_some() && !caps.fd)
                                        || (config.listen_only && !caps.listen_only)
                                    {
                                        Err(Error::Unsupported)
                                    } else {
                                        active.config = Some(config);
                                        Ok(())
                                    }
                                }
                                Operation::Start(permit) => match active.config {
                                    Some(config) if active.permit.is_none() => {
                                        let guard = || {
                                            if state.load(Ordering::Acquire) || reply.is_closed() {
                                                return Err(Error::ControlExpired);
                                            }
                                            permit.check()
                                        };
                                        driver.start(key, config, &guard).map(|()| {
                                            active.permit = Some(permit);
                                        })
                                    }
                                    _ => Err(Error::InvalidState),
                                },
                                Operation::Stop => stop(driver.as_mut(), key, active, &mut total),
                                Operation::Submit(frame, permit) => {
                                    if active.permit.is_none()
                                        || active.config.is_none_or(|config| config.listen_only)
                                    {
                                        Err(Error::InvalidState)
                                    } else if let Err(error) = frame.validate() {
                                        Err(error)
                                    } else if matches!(frame, Frame::FdData { .. })
                                        && active
                                            .config
                                            .is_none_or(|config| config.data_bitrate.is_none())
                                    {
                                        Err(Error::Unsupported)
                                    } else {
                                        if let Some(t) = &trace {
                                            t.call_started("native_worker_driver_call");
                                        }
                                        let result = driver.submit(key, frame, &|| {
                                            if state.load(Ordering::Acquire) || reply.is_closed() {
                                                return Err(Error::ControlExpired);
                                            }
                                            permit.check()
                                        });
                                        if let Some(t) = &trace {
                                            t.call_completed();
                                        }
                                        result
                                    }
                                }
                            };
                            let terminal = if let Err(Error::Fault(reason)) = &result {
                                Some(*reason)
                            } else {
                                None
                            };
                            let failed_start = starting && result.is_err();
                            let cancelled = reply.send(result).is_err();
                            if let Some(reason) = terminal {
                                fault(active, reason);
                                if !stopping {
                                    let _ = stop(driver.as_mut(), key, active, &mut total);
                                }
                            } else if (cancelled || failed_start)
                                && starting
                                && stop(driver.as_mut(), key, active, &mut total).is_err()
                            {
                                fault(active, BackendFault::StopFailed);
                            }
                        }
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                    }
                    for (&key, active) in &mut channels {
                        if active.permit.is_none() {
                            continue;
                        }
                        if active
                            .permit
                            .as_ref()
                            .is_some_and(|permit| permit.check().is_err())
                        {
                            if stop(driver.as_mut(), key, active, &mut total).is_err() {
                                fault(active, BackendFault::StopFailed);
                            }
                            continue;
                        }
                        for _ in 0..16 {
                            let result = driver.receive(key).and_then(|event| {
                                event.map(|event| publish(active, event)).transpose()
                            });
                            match result {
                                Ok(None) => break,
                                Ok(Some(false)) => {}
                                Ok(Some(true)) => {
                                    fault(active, BackendFault::ReceiveOverflow);
                                    let _ = stop(driver.as_mut(), key, active, &mut total);
                                    break;
                                }
                                Err(error) => {
                                    fault(
                                        active,
                                        match error {
                                            Error::Fault(reason) => reason,
                                            _ => BackendFault::Disconnected,
                                        },
                                    );
                                    let _ = stop(driver.as_mut(), key, active, &mut total);
                                    break;
                                }
                            }
                        }
                    }
                }
                // Stop every selected channel, including a partially failed startup.
                for (&key, active) in &mut channels {
                    let _ = stop(driver.as_mut(), key, active, &mut total);
                }
                total
            })
            .map_err(|_| Error::Fault(BackendFault::Disconnected))?;
        let (sources, channels) = ready_rx
            .await
            .map_err(|_| Error::Fault(BackendFault::Disconnected))??;
        opening.0 = None;
        Ok(Self {
            sources,
            channels: channels
                .into_iter()
                .map(|info| (info.id.clone(), info))
                .collect(),
            closing,
            commands,
            worker: Mutex::new(Some(worker)),
            completion: watch::channel(None).0,
        })
    }
    fn info(&self, channel: &str) -> Result<&Info, Error> {
        self.channels.get(channel).ok_or(Error::UnknownChannel)
    }
    async fn request(&self, channel: &str, operation: Operation) -> Result<(), Error> {
        self.request_observed(channel, operation, None).await
    }
    async fn request_observed(
        &self,
        channel: &str,
        operation: Operation,
        trace: Option<canharbor_core::measurement::SubmissionTrace>,
    ) -> Result<(), Error> {
        self.check_channel(channel)?;
        let key = self.info(channel)?.descriptor.key;
        let (reply, receive) = oneshot::channel();
        if let Some(t) = &trace {
            t.enqueued();
        }
        self.commands
            .try_send(Request {
                trace,
                key,
                operation,
                reply,
            })
            .map_err(|error| match error {
                TrySendError::Full(_) => Error::QueueFull,
                TrySendError::Disconnected(_) => Error::Fault(BackendFault::Disconnected),
            })?;
        receive
            .await
            .map_err(|_| Error::Fault(BackendFault::Disconnected))?
    }
}
impl Drop for DriverBackend {
    fn drop(&mut self) {
        self.closing.store(true, Ordering::Release);
    }
}
impl Backend for DriverBackend {
    fn source_evidence(&self, channel: &str) -> Vec<canharbor_core::measurement::SourceEvidence> {
        if self.channels.contains_key(channel) {
            self.sources.clone()
        } else {
            vec![]
        }
    }
    fn submit_observed<'a>(
        &'a self,
        c: &'a str,
        f: Frame,
        p: Permit,
        t: canharbor_core::measurement::SubmissionTrace,
    ) -> BackendFuture<'a, ()> {
        Box::pin(self.request_observed(c, Operation::Submit(f, p), Some(t)))
    }

    fn cleanup_observation(&self, channel: &str) -> Option<CloseSummary> {
        self.channels
            .get(channel)
            .map(|info| info.shared.cleanup.lock().unwrap().clone())
    }
    fn channels(&self) -> Vec<Channel> {
        self.channels
            .values()
            .map(|info| Channel {
                id: info.id.clone(),
                label: info.descriptor.label.clone(),
            })
            .collect()
    }
    fn capabilities(&self, channel: &str) -> Result<Capabilities, Error> {
        Ok(self.info(channel)?.descriptor.capabilities.clone())
    }
    fn check_channel(&self, channel: &str) -> Result<u64, Error> {
        let info = self.info(channel)?;
        if self.closing.load(Ordering::Acquire) {
            return Err(Error::Fault(BackendFault::Disconnected));
        }
        if let Some(reason) = *info.shared.fault.lock().unwrap() {
            return Err(Error::Fault(reason));
        }
        Ok(info.shared.generation.load(Ordering::Acquire))
    }
    fn subscribe(&self, channel: &str) -> Result<broadcast::Receiver<Arc<BackendEvent>>, Error> {
        Ok(self.info(channel)?.shared.events.subscribe())
    }
    fn transmit(&self, _channel: &str, _frame: Frame) -> Result<(), Error> {
        Err(Error::Unsupported)
    }
    fn configure<'a>(&'a self, channel: &'a str, config: Config) -> BackendFuture<'a, ()> {
        Box::pin(self.request(channel, Operation::Configure(config)))
    }
    fn start<'a>(&'a self, channel: &'a str, permit: Permit) -> BackendFuture<'a, ()> {
        Box::pin(self.request(channel, Operation::Start(permit)))
    }
    fn stop<'a>(&'a self, channel: &'a str) -> BackendFuture<'a, ()> {
        Box::pin(self.request(channel, Operation::Stop))
    }
    fn close<'a>(&'a self, channel: &'a str) -> BackendFuture<'a, ()> {
        Box::pin(self.request(channel, Operation::Stop))
    }
    fn submit<'a>(
        &'a self,
        channel: &'a str,
        frame: Frame,
        permit: Permit,
    ) -> BackendFuture<'a, ()> {
        Box::pin(self.request(channel, Operation::Submit(frame, permit)))
    }
    fn shutdown_report(&self) -> BackendFuture<'_, CloseSummary> {
        Box::pin(async {
            self.closing.store(true, Ordering::Release);
            let mut completion = self.completion.subscribe();
            {
                let mut worker = self.worker.lock().unwrap();
                if let Some(worker) = worker.take() {
                    let completed = self.completion.clone();
                    // The result belongs to the backend, independently of the
                    // caller awaiting it. Cancelling that caller cannot lose it.
                    tokio::task::spawn_blocking(move || {
                        let result = worker.join().map_err(|_| BackendFault::StopFailed);
                        completed.send_replace(Some(result));
                    });
                }
            }
            loop {
                let result = { completion.borrow().clone() };
                if let Some(result) = result {
                    return result.map_err(Error::Fault);
                }
                completion
                    .changed()
                    .await
                    .map_err(|_| Error::Fault(BackendFault::StopFailed))?;
            }
        })
    }
    fn shutdown(&self) -> BackendFuture<'_, ()> {
        Box::pin(async {
            let summary = self.shutdown_report().await?;
            if summary.failures.is_empty() {
                Ok(())
            } else {
                Err(Error::Fault(BackendFault::StopFailed))
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use canharbor_core::{CanId, runtime::Runtime};
    use std::{
        collections::{BTreeSet, VecDeque},
        time::Instant,
    };
    #[derive(Default)]
    struct Gate {
        entered: AtomicBool,
        released: AtomicBool,
    }
    #[derive(Default)]
    struct State {
        owner: Option<thread::ThreadId>,
        opened: BTreeSet<u32>,
        starts: Vec<u32>,
        writes: Vec<u32>,
        stops: Vec<u32>,
        events: BTreeMap<u32, VecDeque<Event>>,
        unknown: BTreeMap<u32, u64>,
        gate: Option<Arc<Gate>>,
        submit_gate: Option<Arc<Gate>>,
        stop_fails: bool,
        stop_gate: Option<Arc<Gate>>,
        dropped: bool,
    }
    struct Fake(Arc<Mutex<State>>);
    impl Drop for Fake {
        fn drop(&mut self) {
            let mut state = self.0.lock().unwrap();
            assert_eq!(state.owner, Some(thread::current().id()));
            state.dropped = true;
        }
    }
    impl Fake {
        fn state(&self) -> std::sync::MutexGuard<'_, State> {
            let state = self.0.lock().unwrap();
            assert_eq!(state.owner, Some(thread::current().id()));
            state
        }
    }
    impl Driver for Fake {
        fn descriptors(&self) -> Vec<Descriptor> {
            let _guard = self.state();
            (0..2)
                .map(|key| Descriptor {
                    key,
                    label: format!("Fake native {key}"),
                    capabilities: Capabilities {
                        device_id: "fake-device".into(),
                        backend: "fake_native".into(),
                        simulated: true,
                        fd: key == 1,
                        listen_only: true,
                        timestamps: false,
                        nominal_timing: None,
                        data_timing: None,
                        fixed_nominal_bitrates: vec![500_000],
                        fixed_data_bitrates: if key == 1 { vec![2_000_000] } else { vec![] },
                    },
                })
                .collect()
        }
        fn start(
            &mut self,
            key: u32,
            _config: Config,
            guard: &dyn Fn() -> Result<(), Error>,
        ) -> Result<(), Error> {
            guard()?;
            let gate = {
                let mut state = self.state();
                state.opened.insert(key);
                state.gate.clone()
            };
            if let Some(gate) = gate {
                gate.entered.store(true, Ordering::Release);
                let deadline = Instant::now() + Duration::from_secs(2);
                while !gate.released.load(Ordering::Acquire) {
                    if Instant::now() > deadline {
                        return Err(Error::Fault(BackendFault::Disconnected));
                    }
                    thread::sleep(Duration::from_millis(1));
                }
            }
            guard()?;
            self.state().starts.push(key);
            Ok(())
        }
        fn stop(&mut self, key: u32) -> Result<CloseSummary, Error> {
            let gate = self.state().stop_gate.clone();
            if let Some(gate) = gate {
                gate.entered.store(true, Ordering::Release);
                let deadline = Instant::now() + Duration::from_secs(2);
                while !gate.released.load(Ordering::Acquire) {
                    if Instant::now() > deadline {
                        return Err(Error::Fault(BackendFault::StopFailed));
                    }
                    thread::sleep(Duration::from_millis(1));
                }
            }
            let mut state = self.state();
            if !state.opened.remove(&key) {
                return Ok(CloseSummary::default());
            }
            state.stops.push(key);
            let unknown = state.unknown.remove(&key).unwrap_or(0);
            Ok(CloseSummary {
                unknown_transmissions: unknown,
                failures: if state.stop_fails {
                    vec!["injected close failure".into()]
                } else {
                    vec![]
                },
            })
        }
        fn submit(
            &mut self,
            key: u32,
            _frame: Frame,
            guard: &dyn Fn() -> Result<(), Error>,
        ) -> Result<(), Error> {
            guard()?;
            let gate = self.state().submit_gate.clone();
            if let Some(gate) = gate {
                gate.entered.store(true, Ordering::Release);
                let deadline = Instant::now() + Duration::from_secs(2);
                while !gate.released.load(Ordering::Acquire) {
                    if Instant::now() > deadline {
                        return Err(Error::Fault(BackendFault::Disconnected));
                    }
                    thread::sleep(Duration::from_millis(1));
                }
            }
            guard()?;
            let mut state = self.state();
            assert!(state.opened.contains(&key));
            state.writes.push(key);
            *state.unknown.entry(key).or_default() += 1;
            Ok(())
        }
        fn receive(&mut self, key: u32) -> Result<Option<Event>, Error> {
            Ok(self.state().events.entry(key).or_default().pop_front())
        }
    }
    async fn backend(state: Arc<Mutex<State>>) -> Arc<DriverBackend> {
        Arc::new(
            DriverBackend::open(
                move || {
                    state.lock().unwrap().owner = Some(thread::current().id());
                    Ok(Box::new(Fake(state)))
                },
                "epoch".into(),
            )
            .await
            .unwrap(),
        )
    }
    fn config(fd: bool) -> Config {
        Config {
            nominal_bitrate: 500_000,
            data_bitrate: fd.then_some(2_000_000),
            listen_only: false,
        }
    }
    fn frame() -> Frame {
        Frame::classic(CanId::new(0x548, false).unwrap(), vec![7]).unwrap()
    }
    async fn session(runtime: &Arc<Runtime>, key: u32) -> (String, u64) {
        let session = runtime
            .create_configured(&format!("fake_native:epoch:{key}"), config(key == 1))
            .await
            .unwrap();
        let lease = runtime.acquire(&session.id, "owner").await.unwrap();
        runtime
            .running(&session.id, "owner", lease.generation, true)
            .await
            .unwrap();
        (session.id, lease.generation)
    }
    async fn wait_until(predicate: impl Fn() -> bool) {
        tokio::time::timeout(Duration::from_secs(2), async {
            while !predicate() {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn observed_native_queue_is_measured_and_cancelled_queued_io_stays_unauthorized() {
        let state = Arc::new(Mutex::new(State::default()));
        let backend = backend(state.clone()).await;
        let runtime = Runtime::new(backend.clone());
        let (a, g) = session(&runtime, 0).await;
        let gate = Arc::new(Gate::default());
        state.lock().unwrap().submit_gate = Some(gate.clone());
        let origin = tokio::time::Instant::now();
        let mut one = runtime.prepare_send(&a, "owner", g, frame()).await.unwrap();
        let t1 = one.observe(origin, origin);
        let rt = runtime.clone();
        let first = tokio::spawn(async move { rt.submit_prepared(one).await });
        wait_until(|| gate.entered.load(Ordering::Acquire)).await;
        let mut two = runtime.prepare_send(&a, "owner", g, frame()).await.unwrap();
        let t2 = two.observe(origin, tokio::time::Instant::now());
        let cancel = two.cancellation();
        let rt = runtime.clone();
        let second = tokio::spawn(async move { rt.submit_prepared(two).await });
        wait_until(|| t2.snapshot().enqueued_ns.is_some()).await;
        assert!(t2.snapshot().worker_started_ns.is_none());
        cancel.cancel();
        gate.released.store(true, Ordering::Release);
        first.await.unwrap().unwrap();
        assert!(second.await.unwrap().is_err());
        let p = t1.snapshot();
        assert_eq!(p.scope, "native_worker_driver_call");
        assert!(p.worker_started_ns.unwrap() >= p.enqueued_ns.unwrap());
        assert!(p.call_completed_ns.unwrap() >= p.call_started_ns.unwrap());
        assert!(t2.snapshot().worker_started_ns.unwrap() >= t2.snapshot().enqueued_ns.unwrap());
        assert_eq!(state.lock().unwrap().writes.len(), 1);
        runtime.shutdown().await;
    }
    #[test]
    fn native_cleanup_failure_details_remain_bounded() {
        let mut total = CloseSummary::default();
        for _ in 0..128 {
            collect(
                &mut total,
                CloseSummary {
                    unknown_transmissions: 1,
                    failures: vec!["failure".into(); 20],
                },
            );
        }
        assert_eq!(total.unknown_transmissions, 128);
        assert_eq!(total.failures.len(), 32);
    }
    #[tokio::test]
    async fn one_worker_routes_multiple_channels_and_aggregates_unknown_close_results_once() {
        let state = Arc::new(Mutex::new(State::default()));
        let backend = backend(state.clone()).await;
        let runtime = Runtime::new(backend.clone());
        let (a, ga) = session(&runtime, 0).await;
        let (b, gb) = session(&runtime, 1).await;
        let mut receive = runtime.subscribe(&b).await.unwrap();
        state
            .lock()
            .unwrap()
            .events
            .entry(1)
            .or_default()
            .push_back(Event::Frame {
                frame: frame(),
                metadata: FrameMetadata::default(),
                received_at: tokio::time::Instant::now(),
                echo: false,
            });
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), receive.recv())
                .await
                .unwrap()
                .unwrap()
                .direction,
            "rx"
        );
        runtime.send(&a, "owner", ga, frame()).await.unwrap();
        runtime.send(&b, "owner", gb, frame()).await.unwrap();
        runtime.running(&a, "owner", ga, false).await.unwrap();
        assert_eq!(
            runtime
                .cleanup_observation("fake_native:epoch:0")
                .unwrap()
                .unknown_transmissions,
            1
        );
        assert_eq!(
            runtime
                .cleanup_observation("fake_native:epoch:1")
                .unwrap()
                .unknown_transmissions,
            0
        );
        assert!(!runtime.get(&a).await.unwrap().running);
        assert!(runtime.get(&b).await.unwrap().running);
        let summary = runtime.shutdown().await;
        assert_eq!(summary.unknown_transmissions, 2);
        assert_eq!(
            runtime
                .cleanup_observation("fake_native:epoch:1")
                .unwrap()
                .unknown_transmissions,
            1
        );
        assert!(summary.failures.is_empty());
        assert_eq!(
            backend
                .shutdown_report()
                .await
                .unwrap()
                .unknown_transmissions,
            2
        );
        assert_eq!(state.lock().unwrap().writes, vec![0, 1]);
        assert_eq!(state.lock().unwrap().stops, vec![0, 1]);
        assert!(state.lock().unwrap().dropped);
    }
    #[tokio::test]
    async fn cancellation_between_startup_calls_closes_partial_resources() {
        let state = Arc::new(Mutex::new(State::default()));
        let gate = Arc::new(Gate::default());
        state.lock().unwrap().gate = Some(gate.clone());
        let backend = backend(state.clone()).await;
        let runtime = Runtime::new(backend);
        let session = runtime
            .create_configured("fake_native:epoch:0", config(false))
            .await
            .unwrap();
        let lease = runtime.acquire(&session.id, "owner").await.unwrap();
        let copy = runtime.clone();
        let id = session.id.clone();
        let start =
            tokio::spawn(async move { copy.running(&id, "owner", lease.generation, true).await });
        wait_until(|| gate.entered.load(Ordering::Acquire)).await;
        start.abort();
        assert!(start.await.unwrap_err().is_cancelled());
        gate.released.store(true, Ordering::Release);
        wait_until(|| !state.lock().unwrap().opened.contains(&0)).await;
        assert!(state.lock().unwrap().starts.is_empty());
        assert_eq!(state.lock().unwrap().stops, vec![0]);
        runtime.shutdown().await;
    }
    #[tokio::test]
    async fn priority_shutdown_fences_startup_and_rejects_a_full_queue() {
        let state = Arc::new(Mutex::new(State::default()));
        let gate = Arc::new(Gate::default());
        state.lock().unwrap().gate = Some(gate.clone());
        let backend = backend(state.clone()).await;
        let runtime = Runtime::new(backend.clone());
        let session = runtime
            .create_configured("fake_native:epoch:0", config(false))
            .await
            .unwrap();
        let lease = runtime.acquire(&session.id, "owner").await.unwrap();
        let copy = runtime.clone();
        let id = session.id.clone();
        let start =
            tokio::spawn(async move { copy.running(&id, "owner", lease.generation, true).await });
        wait_until(|| gate.entered.load(Ordering::Acquire)).await;
        let mut replies = Vec::new();
        for _ in 0..32 {
            let (reply, receive) = oneshot::channel();
            backend
                .commands
                .try_send(Request {
                    trace: None,
                    key: 0,
                    operation: Operation::Configure(config(false)),
                    reply,
                })
                .unwrap_or_else(|_| panic!("queue should have exactly 32 slots"));
            replies.push(receive);
        }
        assert!(matches!(
            backend
                .configure("fake_native:epoch:0", config(false))
                .await,
            Err(Error::QueueFull)
        ));
        let copy = backend.clone();
        let closing = tokio::spawn(async move { copy.shutdown_report().await });
        wait_until(|| backend.closing.load(Ordering::Acquire)).await;
        gate.released.store(true, Ordering::Release);
        assert!(closing.await.unwrap().unwrap().failures.is_empty());
        assert!(start.await.unwrap().is_err());
        for reply in replies {
            assert!(reply.await.is_err());
        }
        assert!(state.lock().unwrap().starts.is_empty());
        assert!(state.lock().unwrap().writes.is_empty());
        runtime.shutdown().await;
    }
    #[tokio::test]
    async fn overflow_diagnostic_faults_only_its_channel_and_preserves_close_failures() {
        let state = Arc::new(Mutex::new(State::default()));
        let backend = backend(state.clone()).await;
        let runtime = Runtime::new(backend);
        let (a, _) = session(&runtime, 0).await;
        let (b, _) = session(&runtime, 1).await;
        let mut notices = runtime.subscribe_all(&a).await.unwrap();
        state
            .lock()
            .unwrap()
            .events
            .entry(0)
            .or_default()
            .push_back(Event::Diagnostic(Diagnostic {
                received_at: tokio::time::Instant::now(),
                generation: 999,
                classes: 1,
                data: [0; 8],
                overflow: true,
                stamp: Default::default(),
            }));
        assert!(
            matches!(tokio::time::timeout(Duration::from_secs(2),notices.recv()).await.unwrap().unwrap(),canharbor_core::runtime::Notice::Diagnostic{diagnostic,..} if diagnostic.overflow)
        );
        tokio::time::timeout(Duration::from_secs(2), async {
            while runtime.get(&a).await.unwrap().fault.is_none() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(runtime.get(&b).await.unwrap().running);
        state.lock().unwrap().stop_fails = true;
        assert!(!runtime.shutdown().await.failures.is_empty());
    }
    #[tokio::test]
    async fn cancelled_shutdown_waiter_does_not_lose_cleanup_or_unknown_results() {
        let state = Arc::new(Mutex::new(State::default()));
        let backend = backend(state.clone()).await;
        let runtime = Runtime::new(backend.clone());
        let (id, g) = session(&runtime, 0).await;
        runtime.send(&id, "owner", g, frame()).await.unwrap();
        let gate = Arc::new(Gate::default());
        {
            let mut state = state.lock().unwrap();
            state.stop_gate = Some(gate.clone());
            state.stop_fails = true;
        }
        let copy = backend.clone();
        let first = tokio::spawn(async move { copy.shutdown_report().await });
        wait_until(|| gate.entered.load(Ordering::Acquire)).await;
        first.abort();
        assert!(first.await.unwrap_err().is_cancelled());
        let copy = backend.clone();
        let second = tokio::spawn(async move { copy.shutdown_report().await });
        tokio::task::yield_now().await;
        assert!(!second.is_finished());
        gate.released.store(true, Ordering::Release);
        let report = second.await.unwrap().unwrap();
        assert_eq!(report.unknown_transmissions, 1);
        assert!(!report.failures.is_empty());
        let repeated = backend.shutdown_report().await.unwrap();
        assert_eq!(repeated.unknown_transmissions, 1);
        assert_eq!(repeated.failures, report.failures);
        runtime.shutdown().await;
    }
    #[tokio::test]
    async fn dropping_owner_releases_the_driver_on_its_worker() {
        let state = Arc::new(Mutex::new(State::default()));
        let backend = backend(state.clone()).await;
        drop(backend);
        wait_until(|| state.lock().unwrap().dropped).await;
    }
}

#[cfg(feature = "dynamic")]
pub mod library;
