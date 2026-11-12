// SPDX-License-Identifier: AGPL-3.0-or-later
//! One blocking worker owns a physical device and its shared receive endpoint.
use crate::{
    Error,
    device::{CloseReport, Device, Event, State},
    usb::ReceiveTransport,
};
use canharbor_core::Frame;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, SyncSender, TrySendError},
    },
    thread,
    time::Duration,
};
use tokio::sync::{broadcast, oneshot};
const COMMANDS: usize = 32;
const EVENTS: usize = 256;
const POLL: Duration = Duration::from_millis(10);
const CLOSE_BUDGET: Duration = Duration::from_secs(1);
pub use crate::device::Start;
#[derive(Debug, Clone)]
pub enum Notice {
    Event(Event, std::time::Instant),
    Rejected(Arc<Error>),
    Fault(Arc<Error>),
}
type Reply<T> = oneshot::Sender<Result<T, Error>>;
enum Command {
    Start(Start, Option<canharbor_core::control::Permit>, Reply<()>),
    Stop(u8, Reply<()>),
    Tx(
        u8,
        Frame,
        Option<canharbor_core::control::Permit>,
        Option<canharbor_core::measurement::SubmissionTrace>,
        Reply<u32>,
    ),
    State(u8, Reply<State>),
    Info(Reply<Vec<crate::control::Capabilities>>),
    Subscribe(u8, Reply<broadcast::Receiver<Notice>>),
}
#[derive(Clone)]
pub struct Handle {
    sender: SyncSender<Command>,
    closing: Arc<AtomicBool>,
}
impl Handle {
    async fn request<T>(&self, make: impl FnOnce(Reply<T>) -> Command) -> Result<T, Error> {
        if self.closing.load(Ordering::Acquire) {
            return Err(Error::Closed);
        }
        let (tx, rx) = oneshot::channel();
        self.sender.try_send(make(tx)).map_err(|e| match e {
            TrySendError::Full(_) => Error::QueueFull,
            TrySendError::Disconnected(_) => Error::Closed,
        })?;
        rx.await.map_err(|_| Error::Closed)?
    }
    pub async fn start(&self, config: Start) -> Result<(), Error> {
        self.request(|r| Command::Start(config, None, r)).await
    }
    pub async fn stop(&self, channel: u8) -> Result<(), Error> {
        self.request(|r| Command::Stop(channel, r)).await
    }
    pub async fn transmit(&self, channel: u8, frame: Frame) -> Result<u32, Error> {
        self.request(|r| Command::Tx(channel, frame, None, None, r))
            .await
    }
    pub async fn start_authorized(
        &self,
        config: Start,
        permit: canharbor_core::control::Permit,
    ) -> Result<(), Error> {
        self.request(|r| Command::Start(config, Some(permit), r))
            .await
    }
    pub async fn transmit_authorized(
        &self,
        channel: u8,
        frame: Frame,
        permit: canharbor_core::control::Permit,
    ) -> Result<u32, Error> {
        self.request(|r| Command::Tx(channel, frame, Some(permit), None, r))
            .await
    }
    pub async fn transmit_observed(
        &self,
        channel: u8,
        frame: Frame,
        permit: canharbor_core::control::Permit,
        trace: canharbor_core::measurement::SubmissionTrace,
    ) -> Result<u32, Error> {
        self.request(|r| {
            trace.enqueued();
            Command::Tx(channel, frame, Some(permit), Some(trace), r)
        })
        .await
    }
    pub async fn capabilities(&self) -> Result<Vec<crate::control::Capabilities>, Error> {
        self.request(Command::Info).await
    }
    pub async fn state(&self, channel: u8) -> Result<State, Error> {
        self.request(|r| Command::State(channel, r)).await
    }
    pub async fn subscribe(&self, channel: u8) -> Result<broadcast::Receiver<Notice>, Error> {
        self.request(|r| Command::Subscribe(channel, r)).await
    }
}
pub struct Actor {
    handle: Handle,
    join: Option<thread::JoinHandle<Option<CloseReport>>>,
}
impl Actor {
    /// Initialization and all transport I/O run on the worker, not a Tokio thread.
    pub async fn spawn<T: ReceiveTransport + Send + 'static>(
        transport: T,
        interface: u16,
    ) -> Result<Self, Error> {
        let (sender, commands) = mpsc::sync_channel(COMMANDS);
        let (ready_tx, ready_rx) = oneshot::channel();
        let closing = Arc::new(AtomicBool::new(false));
        let worker_closing = closing.clone();
        let join = thread::Builder::new()
            .name("canharbor-gsusb".into())
            .spawn(move || {
                let mut device = match Device::open(transport, interface) {
                    Ok(d) => d,
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                        return None;
                    }
                };
                let streams: Vec<_> = (0..device.config().channels)
                    .map(|_| broadcast::channel(EVENTS).0)
                    .collect();
                if ready_tx.send(Ok(())).is_err() {
                    return Some(device.close(CLOSE_BUDGET));
                }
                let mut fault = None;
                loop {
                    if worker_closing.load(Ordering::Acquire) {
                        break;
                    }
                    if let Err(e) = device.check_echo_deadlines(std::time::Instant::now()) {
                        fault = Some(e);
                        break;
                    }
                    let command = commands.try_recv();
                    // Close does not consume a command slot and takes precedence
                    // over queued work. I/O already in progress cannot be retracted.
                    if worker_closing.load(Ordering::Acquire) {
                        break;
                    }
                    match command {
                        Ok(Command::Start(c, permit, r)) => {
                            if !r.is_closed() {
                                let _ = r.send(device.start_config(c, permit.as_ref()));
                            }
                        }
                        Ok(Command::Stop(ch, r)) => {
                            if !r.is_closed() {
                                let _ = r.send(device.stop(ch));
                            }
                        }
                        Ok(Command::Tx(ch, f, permit, trace, r)) => {
                            if let Some(t) = &trace {
                                t.worker_started();
                            }
                            if !r.is_closed() {
                                if let Some(t) = &trace {
                                    t.call_started("device_worker_submit");
                                }
                                let result = device.transmit_checked(ch, f, permit.as_ref());
                                if let Some(t) = &trace {
                                    t.call_completed();
                                }
                                if matches!(result, Err(Error::EchoTimeout)) {
                                    fault = Some(Error::EchoTimeout);
                                }
                                let _ = r.send(result);
                            }
                        }
                        Ok(Command::Info(r)) => {
                            let _ = r.send(
                                (0..device.config().channels)
                                    .map(|ch| device.capabilities(ch as u8).cloned())
                                    .collect(),
                            );
                        }
                        Ok(Command::State(ch, r)) => {
                            let _ = r.send(device.state(ch));
                        }
                        Ok(Command::Subscribe(ch, r)) => {
                            let _ = r.send(
                                streams
                                    .get(usize::from(ch))
                                    .map(broadcast::Sender::subscribe)
                                    .ok_or(Error::Protocol("channel")),
                            );
                        }
                        Err(mpsc::TryRecvError::Disconnected) => break,
                        Err(mpsc::TryRecvError::Empty) => {}
                    }
                    if device.state(0) == Ok(State::Faulted) {
                        if fault.is_none() {
                            fault = Some(Error::Transport);
                        }
                        break;
                    }
                    if worker_closing.load(Ordering::Acquire) {
                        break;
                    }
                    match device.poll(POLL) {
                        Ok(Some(event)) => {
                            let channel = match &event {
                                Event::Received(p) | Event::Echo(p) | Event::UnmatchedEcho(p) => {
                                    p.channel
                                }
                            };
                            let _ = streams[usize::from(channel)]
                                .send(Notice::Event(event, std::time::Instant::now()));
                        }
                        Ok(None) => {}
                        Err(Error::State) => {}
                        Err(e @ (Error::Transport | Error::EchoTimeout)) => {
                            fault = Some(e);
                            break;
                        }
                        Err(e) => {
                            let e = Arc::new(e);
                            for stream in &streams {
                                let _ = stream.send(Notice::Rejected(e.clone()));
                            }
                        }
                    }
                }
                worker_closing.store(true, Ordering::Release);
                if let Some(reason) = fault.as_ref() {
                    for stream in &streams {
                        let reported = if matches!(reason, Error::EchoTimeout) {
                            Error::EchoTimeout
                        } else {
                            Error::Transport
                        };
                        let _ = stream.send(Notice::Fault(Arc::new(reported)));
                    }
                }
                drop(commands); // reject queued commands before reset/cancellation waits
                let mut report = device.close(CLOSE_BUDGET);
                report.fault = fault;
                Some(report)
            })
            .map_err(|_| Error::Closed)?;
        ready_rx.await.map_err(|_| Error::Closed)??;
        Ok(Self {
            handle: Handle { sender, closing },
            join: Some(join),
        })
    }
    pub fn handle(&self) -> Handle {
        self.handle.clone()
    }
    /// Priority close: reject queued work, reset, cancel/drain, drop USB resources,
    /// and join outside the async executor. Inspect every field of the report.
    pub async fn close(mut self) -> Result<CloseReport, Error> {
        self.handle.closing.store(true, Ordering::Release);
        let join = self.join.take().ok_or(Error::Closed)?;
        tokio::task::spawn_blocking(move || join.join())
            .await
            .map_err(|_| Error::Closed)?
            .map_err(|_| Error::Closed)?
            .ok_or(Error::Closed)
    }
}
impl Drop for Actor {
    fn drop(&mut self) {
        // Drop cannot await. Signal cleanup even if cloned handles survive.
        // Explicit close joins and returns failures; drop only requests it.
        self.handle.closing.store(true, Ordering::Release);
    }
}
