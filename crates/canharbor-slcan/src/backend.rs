// SPDX-License-Identifier: AGPL-3.0-or-later
//! One owned serial worker, bounded requests, and execution-time authorization.
use crate::{
    Profile,
    engine::{Engine, Error as EngineError},
    serial::Transport,
};
use canharbor_core::{
    Backend, BackendEvent, BackendFault, BackendFuture, BusFrame, Capabilities, Channel,
    CloseSummary, Config, Diagnostic, Error, Frame, FrameMetadata,
    control::Permit,
    timestamp::{Quality, Stamp},
};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, SyncSender, TrySendError},
    },
    thread,
    time::{Duration, Instant},
};
use tokio::sync::{broadcast, oneshot, watch};
enum Operation {
    Configure(Config),
    Start(Permit),
    Stop,
    Close,
    Transmit(Frame, Permit),
}
struct Request {
    operation: Operation,
    reply: oneshot::Sender<Result<(), Error>>,
}
struct Shared {
    closing: AtomicBool,
    fault: Mutex<Option<BackendFault>>,
    generation: AtomicU64,
    events: broadcast::Sender<Arc<BackendEvent>>,
}
pub struct SlcanBackend {
    profile: Profile,
    id: String,
    device: String,
    shared: Arc<Shared>,
    commands: SyncSender<Request>,
    worker: Mutex<Option<thread::JoinHandle<CloseSummary>>>,
    completion: watch::Sender<Option<Result<CloseSummary, BackendFault>>>,
}
fn map(error: EngineError) -> Error {
    match error {
        EngineError::Control(error) => error,
        EngineError::Protocol(crate::Error::Frame(error)) => error,
        EngineError::Protocol(crate::Error::Unsupported | crate::Error::Rejected) => {
            Error::Unsupported
        }
        EngineError::Overflow => Error::Fault(BackendFault::ReceiveOverflow),
        EngineError::Io(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::BrokenPipe
                    | std::io::ErrorKind::UnexpectedEof
                    | std::io::ErrorKind::NotConnected
            ) =>
        {
            Error::Fault(BackendFault::Disconnected)
        }
        _ => Error::Fault(BackendFault::TransmitFailed),
    }
}
fn fault(shared: &Shared, reason: BackendFault) {
    *shared.fault.lock().unwrap() = Some(reason);
    let generation = shared.generation.fetch_add(1, Ordering::AcqRel) + 1;
    let _ = shared.events.send(Arc::new(BackendEvent::Fault {
        fault: reason,
        generation,
    }));
}
fn publish<T: Transport>(engine: &mut Engine<T>, shared: &Shared) -> bool {
    let generation = shared.generation.load(Ordering::Acquire);
    for received in engine.frames() {
        let metadata = FrameMetadata {
            stamp: match received.timestamp_ms {
                Some(ms) => Stamp {
                    raw_us: Some(u32::from(ms) * 1000),
                    extended_us: None,
                    epoch: generation,
                    quality: Quality::HardwareUnmapped,
                },
                None => Stamp::default(),
            },
            ..Default::default()
        };
        let _ = shared.events.send(Arc::new(BackendEvent::Frame(BusFrame {
            received_at: received.received_at,
            frame: received.frame,
            source_channel: String::new(),
            generation,
            metadata,
        })));
    }
    let mut overflow = false;
    for flags in engine.statuses() {
        if flags == 0 {
            continue;
        }
        let lost = flags & 0x09 != 0;
        overflow |= lost;
        let mut data = [0; 8];
        data[0] = flags;
        let _ = shared
            .events
            .send(Arc::new(BackendEvent::Diagnostic(Diagnostic {
                received_at: tokio::time::Instant::now(),
                generation,
                classes: 0,
                data,
                overflow: lost,
                stamp: Stamp::default(),
            })));
    }
    overflow
}
impl SlcanBackend {
    /// The caller explicitly selects a transport and firmware polling profile.
    /// Device identity denotes this open epoch, not verified physical identity.
    pub async fn open<T: Transport>(
        transport: T,
        device: String,
        polling: bool,
    ) -> Result<Self, Error> {
        Self::open_with_profile(transport, device, polling, Profile::Lawicel).await
    }
    pub async fn open_with_profile<T: Transport>(
        transport: T,
        device: String,
        polling: bool,
        profile: Profile,
    ) -> Result<Self, Error> {
        if polling && profile == Profile::CanableFd {
            return Err(Error::Unsupported);
        }
        if device.is_empty()
            || device.len() > 64
            || !device
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(Error::InvalidState);
        }
        let shared = Arc::new(Shared {
            closing: AtomicBool::new(false),
            fault: Mutex::new(None),
            generation: AtomicU64::new(0),
            events: broadcast::channel(256).0,
        });
        let (commands, input) = mpsc::sync_channel::<Request>(32);
        let (ready_tx, ready_rx) = oneshot::channel();
        let state = shared.clone();
        let worker = thread::Builder::new()
            .name("canharbor-slcan".into())
            .spawn(move || {
                let mut engine = Engine::with_profile(transport, profile);
                if let Err(error) = engine.initialize() {
                    let _ = ready_tx.send(Err(map(error)));
                    return CloseSummary {
                        unknown_transmissions: 0,
                        failures: vec!["SLCAN initialization failed".into()],
                    };
                }
                let _ = ready_tx.send(Ok(()));
                let mut config: Option<Config> = None;
                let mut permit: Option<Permit> = None;
                let mut status_at = Instant::now();
                while !state.closing.load(Ordering::Acquire) {
                    let mut terminal = None;
                    match input.recv_timeout(if permit.is_some() {
                        Duration::ZERO
                    } else {
                        Duration::from_millis(20)
                    }) {
                        Ok(request) => {
                            let Request { operation, reply } = request;
                            if reply.is_closed()
                                && !matches!(operation, Operation::Stop | Operation::Close)
                            {
                                continue;
                            }
                            let starting = matches!(operation, Operation::Start(_));
                            let result = match operation {
                                Operation::Configure(value) => {
                                    if permit.is_some() {
                                        Err(Error::InvalidState)
                                    } else if profile.validate_config(value).is_err() {
                                        Err(Error::Unsupported)
                                    } else {
                                        config = Some(value);
                                        Ok(())
                                    }
                                }
                                Operation::Start(control) => {
                                    let guard = || {
                                        if state.closing.load(Ordering::Acquire)
                                            || reply.is_closed()
                                        {
                                            return Err(Error::ControlExpired);
                                        }
                                        control.check()
                                    };
                                    match config {
                                        Some(config) if permit.is_none() => engine
                                            .start(config, guard)
                                            .map(|()| {
                                                permit = Some(control);
                                                status_at = Instant::now();
                                            })
                                            .map_err(map),
                                        _ => Err(Error::InvalidState),
                                    }
                                }
                                Operation::Stop | Operation::Close => {
                                    permit = None;
                                    engine.stop().map_err(map)
                                }
                                Operation::Transmit(frame, control) => {
                                    if permit.is_none() {
                                        Err(Error::InvalidState)
                                    } else {
                                        engine
                                            .transmit(&frame, || {
                                                if state.closing.load(Ordering::Acquire)
                                                    || reply.is_closed()
                                                {
                                                    return Err(Error::ControlExpired);
                                                }
                                                control.check()
                                            })
                                            .map_err(map)
                                    }
                                }
                            };
                            if let Err(Error::Fault(reason)) = &result {
                                terminal = Some(*reason);
                            }
                            if reply.send(result).is_err() && starting {
                                permit = None;
                                if engine.stop().is_err() {
                                    terminal = Some(BackendFault::StopFailed);
                                }
                            }
                        }
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                    }
                    if publish(&mut engine, &state) {
                        terminal = Some(BackendFault::ReceiveOverflow);
                    }
                    if let Some(reason) = terminal {
                        fault(&state, reason);
                        break;
                    }
                    if let Some(control) = &permit {
                        if control.check().is_err() {
                            permit = None;
                            if engine.stop().is_err() {
                                fault(&state, BackendFault::StopFailed);
                                break;
                            }
                            continue;
                        }
                        let guard = || {
                            if state.closing.load(Ordering::Acquire) {
                                return Err(Error::ControlExpired);
                            }
                            control.check()
                        };
                        let result = engine.poll(polling, guard).and_then(|()| {
                            if profile == Profile::Lawicel
                                && status_at.elapsed() >= Duration::from_millis(100)
                            {
                                status_at = Instant::now();
                                engine.status(guard)
                            } else {
                                Ok(())
                            }
                        });
                        let overflow = publish(&mut engine, &state);
                        if overflow {
                            fault(&state, BackendFault::ReceiveOverflow);
                            break;
                        }
                        if let Err(error) = result {
                            match map(error) {
                                Error::ControlExpired => {}
                                Error::Fault(reason) => {
                                    fault(&state, reason);
                                    break;
                                }
                                _ => {
                                    fault(&state, BackendFault::Disconnected);
                                    break;
                                }
                            }
                        }
                    }
                }
                let mut failures = Vec::new();
                if engine.stop().is_err() {
                    failures.push(
                        "SLCAN controller cleanup failed; state requires explicit reopen".into(),
                    );
                }
                CloseSummary {
                    unknown_transmissions: engine.unknown_transmissions(),
                    failures,
                }
            })
            .map_err(|_| Error::Fault(BackendFault::Disconnected))?;
        let backend = Self {
            profile,
            id: format!("slcan:{device}:0"),
            device,
            shared,
            commands,
            worker: Mutex::new(Some(worker)),
            completion: watch::channel(None).0,
        };
        ready_rx
            .await
            .map_err(|_| Error::Fault(BackendFault::Disconnected))??;
        Ok(backend)
    }
    fn channel(&self, id: &str) -> Result<(), Error> {
        if id == self.id {
            Ok(())
        } else {
            Err(Error::UnknownChannel)
        }
    }
    async fn request(&self, channel: &str, operation: Operation) -> Result<(), Error> {
        self.channel(channel)?;
        if self.shared.closing.load(Ordering::Acquire) {
            return Err(Error::Fault(BackendFault::Disconnected));
        }
        if let Some(fault) = *self.shared.fault.lock().unwrap() {
            return Err(Error::Fault(fault));
        }
        let (reply, receive) = oneshot::channel();
        self.commands
            .try_send(Request { operation, reply })
            .map_err(|error| match error {
                TrySendError::Full(_) => Error::QueueFull,
                TrySendError::Disconnected(_) => Error::Fault(BackendFault::Disconnected),
            })?;
        receive
            .await
            .map_err(|_| Error::Fault(BackendFault::Disconnected))?
    }
}
impl Drop for SlcanBackend {
    fn drop(&mut self) {
        self.shared.closing.store(true, Ordering::Release);
    }
}
impl Backend for SlcanBackend {
    fn channels(&self) -> Vec<Channel> {
        vec![Channel {
            id: self.id.clone(),
            label: match self.profile {
                Profile::Lawicel => "SLCAN Classic CAN (hardware unverified)",
                Profile::CanableFd => "SLCAN CANable FD (unconfirmed commands)",
            }
            .into(),
        }]
    }
    fn capabilities(&self, channel: &str) -> Result<Capabilities, Error> {
        self.channel(channel)?;
        Ok(Capabilities {
            device_id: self.device.clone(),
            backend: "slcan".into(),
            simulated: false,
            fd: self.profile == Profile::CanableFd,
            listen_only: true,
            timestamps: self.profile == Profile::Lawicel,
            nominal_timing: None,
            data_timing: None,
            fixed_nominal_bitrates: self.profile.nominal_bitrates().to_vec(),
            fixed_data_bitrates: self.profile.data_bitrates().to_vec(),
        })
    }
    fn check_channel(&self, channel: &str) -> Result<u64, Error> {
        self.channel(channel)?;
        if self.shared.closing.load(Ordering::Acquire) {
            return Err(Error::Fault(BackendFault::Disconnected));
        }
        if let Some(fault) = *self.shared.fault.lock().unwrap() {
            return Err(Error::Fault(fault));
        }
        Ok(self.shared.generation.load(Ordering::Acquire))
    }
    fn subscribe(&self, channel: &str) -> Result<broadcast::Receiver<Arc<BackendEvent>>, Error> {
        self.channel(channel)?;
        Ok(self.shared.events.subscribe())
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
        Box::pin(self.request(channel, Operation::Close))
    }
    fn submit<'a>(
        &'a self,
        channel: &'a str,
        frame: Frame,
        permit: Permit,
    ) -> BackendFuture<'a, ()> {
        Box::pin(self.request(channel, Operation::Transmit(frame, permit)))
    }
    fn shutdown_report(&self) -> BackendFuture<'_, CloseSummary> {
        Box::pin(async {
            self.shared.closing.store(true, Ordering::Release);
            let mut completion = self.completion.subscribe();
            {
                let mut worker = self.worker.lock().unwrap();
                if let Some(worker) = worker.take() {
                    let completed = self.completion.clone();
                    tokio::task::spawn_blocking(move || {
                        completed.send_replace(Some(
                            worker.join().map_err(|_| BackendFault::StopFailed),
                        ));
                    });
                }
            }
            loop {
                let report = { completion.borrow().clone() };
                if let Some(report) = report {
                    return report.map_err(Error::Fault);
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
