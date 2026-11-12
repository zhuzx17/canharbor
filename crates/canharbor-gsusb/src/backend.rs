// SPDX-License-Identifier: AGPL-3.0-or-later
//! Runtime adapter for a single explicitly opened gs_usb device.
use crate::{
    actor::{Actor, Handle, Notice, Start},
    control,
    device::Event,
    usb::ReceiveTransport,
    wire::Payload,
};
use canharbor_core::{
    Backend, BackendEvent, BackendFault, BackendFuture, BusFrame, Capabilities, Channel, Config,
    Error, Frame, control::Permit,
};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};
use tokio::sync::{broadcast, watch};
#[derive(Debug, Clone, Default)]
pub struct Diagnostics {
    pub rejected: u64,
    pub overflow: u64,
    pub raw_errors: VecDeque<(u8, u32, [u8; 8])>,
}
struct Inner {
    ids: Vec<String>,
    device_id: String,
    capabilities: Vec<control::Capabilities>,
    handle: Handle,
    actor: tokio::sync::Mutex<Option<Actor>>,
    completion: watch::Sender<Option<Result<canharbor_core::CloseSummary, Error>>>,
    configs: Mutex<BTreeMap<u8, Start>>,
    events: Vec<broadcast::Sender<Arc<BackendEvent>>>,
    faults: Mutex<Vec<Option<BackendFault>>>,
    generation: AtomicU64,
    diagnostics: Mutex<Diagnostics>,
}
pub struct GsUsbBackend {
    inner: Arc<Inner>,
}
fn timing_limits(clock_hz: u32, limits: control::Limits) -> canharbor_core::TimingLimits {
    canharbor_core::TimingLimits {
        clock_hz,
        tseg1_min: limits.tseg1_min,
        tseg1_max: limits.tseg1_max,
        tseg2_min: limits.tseg2_min,
        tseg2_max: limits.tseg2_max,
        sjw_max: limits.sjw_max,
        brp_min: limits.brp_min,
        brp_max: limits.brp_max,
        brp_inc: limits.brp_inc,
    }
}
fn map(error: crate::Error) -> Error {
    match error {
        crate::Error::Frame(e) => e,
        crate::Error::QueueFull => Error::QueueFull,
        crate::Error::State => Error::InvalidState,
        crate::Error::Closed => Error::Fault(BackendFault::Disconnected),
        crate::Error::Transport | crate::Error::EchoTimeout => {
            Error::Fault(BackendFault::TransmitFailed)
        }
        _ => Error::Unsupported,
    }
}
impl GsUsbBackend {
    /// device_id identifies this open epoch; callers must not reuse it after reopen.
    pub async fn open<T: ReceiveTransport + Send + 'static>(
        transport: T,
        interface: u16,
        device_id: String,
    ) -> Result<Self, Error> {
        let actor = Actor::spawn(transport, interface).await.map_err(map)?;
        let handle = actor.handle();
        let capabilities = handle.capabilities().await.map_err(map)?;
        let count = capabilities.len();
        let mut receivers = Vec::new();
        for ch in 0..count {
            receivers.push(handle.subscribe(ch as u8).await.map_err(map)?);
        }
        let inner = Arc::new(Inner {
            ids: (0..count)
                .map(|ch| format!("gs_usb:{device_id}:{ch}"))
                .collect(),
            device_id,
            capabilities,
            handle,
            actor: tokio::sync::Mutex::new(Some(actor)),
            completion: watch::channel(None).0,
            configs: Mutex::new(BTreeMap::new()),
            events: (0..count).map(|_| broadcast::channel(256).0).collect(),
            faults: Mutex::new(vec![None; count]),
            generation: AtomicU64::new(0),
            diagnostics: Mutex::new(Diagnostics::default()),
        });
        for (ch, mut rx) in receivers.into_iter().enumerate() {
            let weak = Arc::downgrade(&inner);
            tokio::spawn(async move {
                let mut clock = canharbor_core::timestamp::Clock32::default();
                loop {
                    let notice = rx.recv().await;
                    let Some(inner) = weak.upgrade() else {
                        break;
                    };
                    match notice {
                        Ok(Notice::Event(event, received)) => {
                            let (packet, echo, unmatched) = match event {
                                Event::Received(p) => (p, false, false),
                                Event::Echo(p) => (p, true, false),
                                Event::UnmatchedEcho(p) => (p, true, true),
                            };
                            if unmatched {
                                inner.diagnostics.lock().unwrap().rejected += 1;
                            }
                            let stamp = packet
                                .timestamp_us
                                .map(|raw| clock.observe(raw, received))
                                .unwrap_or_default();
                            let classes = match &packet.payload {
                                Payload::Error { classes, .. } => *classes,
                                _ => 0,
                            };
                            let diagnostic_data = match &packet.payload {
                                Payload::Error { data, .. } => *data,
                                _ => [0; 8],
                            };
                            if packet.overflow || matches!(packet.payload, Payload::Error { .. }) {
                                let _ = inner.events[ch].send(Arc::new(BackendEvent::Diagnostic(
                                    canharbor_core::Diagnostic {
                                        received_at: tokio::time::Instant::from_std(received),
                                        generation: inner.generation.load(Ordering::Acquire),
                                        classes,
                                        data: diagnostic_data,
                                        overflow: packet.overflow,
                                        stamp: stamp.clone(),
                                    },
                                )));
                            }
                            if packet.overflow {
                                inner.diagnostics.lock().unwrap().overflow += 1;
                                inner.fault(ch, BackendFault::ReceiveOverflow);
                            }
                            match packet.payload {
                                Payload::Error { classes, data } => {
                                    let mut diag = inner.diagnostics.lock().unwrap();
                                    if diag.raw_errors.len() == 64 {
                                        diag.raw_errors.pop_front();
                                    }
                                    diag.raw_errors.push_back((ch as u8, classes, data));
                                    drop(diag);
                                    if classes & 0x40 != 0 {
                                        inner.fault(ch, BackendFault::BusOff);
                                    }
                                }
                                Payload::Frame(frame) => {
                                    if unmatched {
                                        continue;
                                    }
                                    let _ = inner.events[ch].send(Arc::new(BackendEvent::Frame(
                                        BusFrame {
                                            received_at: tokio::time::Instant::from_std(received),
                                            metadata: canharbor_core::FrameMetadata {
                                                stamp,
                                                esi: packet.esi,
                                                raw_dlc: Some(packet.dlc),
                                            },
                                            frame,
                                            source_channel: if echo {
                                                inner.ids[ch].clone()
                                            } else {
                                                String::new()
                                            },
                                            generation: inner.generation.load(Ordering::Acquire),
                                        },
                                    )));
                                }
                            }
                        }
                        Ok(Notice::Rejected(_)) => inner.diagnostics.lock().unwrap().rejected += 1,
                        Ok(Notice::Fault(reason)) => {
                            inner.fault(
                                ch,
                                if matches!(reason.as_ref(), crate::Error::EchoTimeout) {
                                    BackendFault::TransmitFailed
                                } else {
                                    BackendFault::Disconnected
                                },
                            );
                            break;
                        }
                        Err(broadcast::error::RecvError::Lagged(_)) => {
                            inner.fault(ch, BackendFault::ReceiveOverflow)
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            inner.fault(ch, BackendFault::Disconnected);
                            break;
                        }
                    }
                }
            });
        }
        Ok(Self { inner })
    }
    fn index(&self, id: &str) -> Result<usize, Error> {
        self.inner
            .ids
            .iter()
            .position(|ch| ch == id)
            .ok_or(Error::UnknownChannel)
    }
    pub fn diagnostics(&self) -> Diagnostics {
        self.inner.diagnostics.lock().unwrap().clone()
    }
}
impl Inner {
    fn fault(&self, ch: usize, fault: BackendFault) {
        self.faults.lock().unwrap()[ch] = Some(fault);
        let generation = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
        let _ = self.events[ch].send(Arc::new(BackendEvent::Fault { fault, generation }));
    }
}
impl Backend for GsUsbBackend {
    fn channels(&self) -> Vec<Channel> {
        self.inner
            .ids
            .iter()
            .enumerate()
            .map(|(i, id)| Channel {
                id: id.clone(),
                label: format!("gs_usb CAN {i}"),
            })
            .collect()
    }
    fn capabilities(&self, id: &str) -> Result<Capabilities, Error> {
        let index = self.index(id)?;
        let features = self.inner.capabilities[index].features;
        Ok(Capabilities {
            device_id: self.inner.device_id.clone(),
            backend: "gs_usb".into(),
            simulated: false,
            fd: features & control::FD != 0,
            listen_only: features & control::LISTEN_ONLY != 0,
            timestamps: features & control::HW_TIMESTAMP != 0,
            nominal_timing: Some(timing_limits(
                self.inner.capabilities[index].clock_hz,
                self.inner.capabilities[index].nominal,
            )),
            data_timing: self.inner.capabilities[index]
                .data
                .map(|limits| timing_limits(self.inner.capabilities[index].clock_hz, limits)),
            fixed_nominal_bitrates: Vec::new(),
            fixed_data_bitrates: Vec::new(),
        })
    }
    fn subscribe(&self, id: &str) -> Result<broadcast::Receiver<Arc<BackendEvent>>, Error> {
        Ok(self.inner.events[self.index(id)?].subscribe())
    }
    fn check_channel(&self, id: &str) -> Result<u64, Error> {
        let index = self.index(id)?;
        if let Some(fault) = self.inner.faults.lock().unwrap()[index] {
            Err(Error::Fault(fault))
        } else {
            Ok(self.inner.generation.load(Ordering::Acquire))
        }
    }
    fn configure<'a>(&'a self, id: &'a str, config: Config) -> BackendFuture<'a, ()> {
        Box::pin(async move {
            let ch = self.index(id)?;
            self.check_channel(id)?;
            let caps = &self.inner.capabilities[ch];
            if config.listen_only && caps.features & control::LISTEN_ONLY == 0 {
                return Err(Error::Unsupported);
            }
            let nominal =
                crate::timing::solve(caps.clock_hz, config.nominal_bitrate, 875, caps.nominal)
                    .map_err(map)?
                    .timing;
            let data = if let Some(rate) = config.data_bitrate {
                if caps.features & control::FD == 0 {
                    return Err(Error::Unsupported);
                }
                Some(
                    crate::timing::solve(
                        caps.clock_hz,
                        rate,
                        800,
                        caps.data.ok_or(Error::Unsupported)?,
                    )
                    .map_err(map)?
                    .timing,
                )
            } else {
                None
            };
            if self.inner.handle.state(ch as u8).await.map_err(map)?
                != crate::device::State::Stopped
            {
                return Err(Error::Unsupported);
            }
            self.inner.configs.lock().unwrap().insert(
                ch as u8,
                Start {
                    channel: ch as u8,
                    nominal,
                    data,
                    listen_only: config.listen_only,
                    timestamps: caps.features & control::HW_TIMESTAMP != 0,
                },
            );
            Ok(())
        })
    }
    fn start<'a>(&'a self, id: &'a str, permit: Permit) -> BackendFuture<'a, ()> {
        Box::pin(async move {
            let ch = self.index(id)?;
            self.check_channel(id)?;
            let config = *self
                .inner
                .configs
                .lock()
                .unwrap()
                .get(&(ch as u8))
                .ok_or(Error::Unsupported)?;
            self.inner
                .handle
                .start_authorized(config, permit)
                .await
                .map_err(map)
        })
    }
    fn stop<'a>(&'a self, id: &'a str) -> BackendFuture<'a, ()> {
        Box::pin(async move {
            let ch = self.index(id)?;
            self.inner
                .handle
                .stop(ch as u8)
                .await
                .map_err(|_| Error::Fault(BackendFault::StopFailed))
        })
    }
    fn close<'a>(&'a self, id: &'a str) -> BackendFuture<'a, ()> {
        Box::pin(async move {
            self.stop(id).await?;
            self.inner
                .configs
                .lock()
                .unwrap()
                .remove(&(self.index(id)? as u8));
            Ok(())
        })
    }
    fn transmit(&self, _: &str, _: Frame) -> Result<(), Error> {
        Err(Error::Unsupported)
    }
    fn submit_observed<'a>(
        &'a self,
        id: &'a str,
        frame: Frame,
        permit: Permit,
        trace: canharbor_core::measurement::SubmissionTrace,
    ) -> BackendFuture<'a, ()> {
        Box::pin(async move {
            let ch = self.index(id)?;
            self.check_channel(id)?;
            self.inner
                .handle
                .transmit_observed(ch as u8, frame, permit, trace)
                .await
                .map_err(map)
                .map(|_| ())
        })
    }
    fn submit<'a>(&'a self, id: &'a str, frame: Frame, permit: Permit) -> BackendFuture<'a, ()> {
        Box::pin(async move {
            let ch = self.index(id)?;
            self.check_channel(id)?;
            self.inner
                .handle
                .transmit_authorized(ch as u8, frame, permit)
                .await
                .map_err(map)
                .map(|_| ())
        })
    }
    fn shutdown_report(&self) -> BackendFuture<'_, canharbor_core::CloseSummary> {
        Box::pin(async move {
            let mut completion = self.inner.completion.subscribe();
            {
                let mut actor = self.inner.actor.lock().await;
                if let Some(actor) = actor.take() {
                    let completed = self.inner.completion.clone();
                    tokio::spawn(async move {
                        let result = async {
                            let mut summary = canharbor_core::CloseSummary::default();
                            let report = actor.close().await.map_err(map)?;
                            summary.unknown_transmissions =
                                report.unknown_transmissions.len() as u64;
                            if report.reset.is_err() {
                                summary.failures.push("gs_usb channel reset failed".into());
                            }
                            if report.drain.is_err() {
                                summary
                                    .failures
                                    .push("gs_usb transfer draining failed".into());
                            }
                            if report.fault.is_some() {
                                summary
                                    .failures
                                    .push("gs_usb actor terminated with a transport fault".into());
                            }
                            Ok(summary)
                        }
                        .await;
                        completed.send_replace(Some(result));
                    });
                }
            }
            loop {
                let report = { completion.borrow().clone() };
                if let Some(report) = report {
                    return report;
                }
                completion
                    .changed()
                    .await
                    .map_err(|_| Error::Fault(BackendFault::StopFailed))?;
            }
        })
    }
    fn shutdown(&self) -> BackendFuture<'_, ()> {
        Box::pin(async move {
            let report = self.shutdown_report().await?;
            if report.failures.is_empty() {
                Ok(())
            } else {
                Err(Error::Fault(BackendFault::StopFailed))
            }
        })
    }
}
