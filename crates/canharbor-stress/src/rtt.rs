// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
#[schemars(rename = "StressRttRole")]
pub enum RttRole {
    #[default]
    Loopback,
    Probe,
    Responder,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "StressRttOptions")]
pub struct RttOptions {
    #[serde(default)]
    pub role: RttRole,
    pub response_can_id: u32,
    pub timeout_ms: u32,
    pub max_inflight: u16,
}
impl Plan {
    pub fn validate_rtt(&self) -> Result<(), String> {
        if let Some(r) = &self.rtt {
            CanId::new(r.response_can_id, self.extended).map_err(|e| e.to_string())?;
            if self.traffic.is_classic()
                || self.traffic.integrity() != "crc32c_pattern_v1"
                || self.receivers.len() != 1
                || !self.receivers_active
                || r.response_can_id == self.can_id
                || !(10..=10000).contains(&r.timeout_ms)
                || r.max_inflight == 0
                || r.max_inflight > 1024
            {
                return Err("RTT needs CRC FD variants, one active responder, distinct response ID, 10..10000 ms timeout and window 1..1024".into());
            }
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[schemars(rename = "StressLatencyHistogram")]
pub struct LatencyHistogram {
    pub samples: u64,
    pub bins: Vec<u64>,
    pub overflow: u64,
    pub negative: u64,
    pub max_ns: u64,
    pub p50_upper_ns: Option<u64>,
    pub p95_upper_ns: Option<u64>,
    pub p99_upper_ns: Option<u64>,
}
impl LatencyHistogram {
    pub fn record(&mut self, sent: u64, received: u64) {
        if self.bins.is_empty() {
            self.bins = vec![0; 25];
        }
        let Some(ns) = received.checked_sub(sent) else {
            self.negative += 1;
            return;
        };
        self.samples += 1;
        self.max_ns = self.max_ns.max(ns);
        if let Some(i) = (0..25).find(|i| ns <= 1000u64 << i) {
            self.bins[i] += 1;
        } else {
            self.overflow += 1;
        }
        self.p50_upper_ns = self.quantile(50);
        self.p95_upper_ns = self.quantile(95);
        self.p99_upper_ns = self.quantile(99);
    }
    pub fn consistent(&self) -> bool {
        if self.bins.len() != 25 && !(self.bins.is_empty() && self.samples == 0) {
            return false;
        }
        self.bins
            .iter()
            .try_fold(0u64, |a, b| a.checked_add(*b))
            .and_then(|n| n.checked_add(self.overflow))
            == Some(self.samples)
            && self.p50_upper_ns == self.quantile(50)
            && self.p95_upper_ns == self.quantile(95)
            && self.p99_upper_ns == self.quantile(99)
            && (self.samples > 0 || self.max_ns == 0)
    }
    fn quantile(&self, p: u64) -> Option<u64> {
        if self.samples == 0 {
            return None;
        }
        let rank = (self.samples / 100) * p + ((self.samples % 100) * p).div_ceil(100);
        let mut n = 0;
        for (i, b) in self.bins.iter().enumerate() {
            n += b;
            if n >= rank {
                return Some(1000u64 << i);
            }
        }
        None
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[schemars(rename = "StressRttReport")]
pub struct RttReport {
    pub latency: LatencyHistogram,
    pub timed_out: u64,
    pub cancelled: u64,
    pub duplicate_responses: u64,
    pub late_or_unmatched: u64,
    pub invalid_responses: u64,
    pub responder_submitted: u64,
    pub responder_failed: u64,
    pub responder_unknown: u64,
    #[serde(default)]
    pub responder_unknown_sequence: Option<u32>,
}
pub(crate) struct Tracker {
    pub pending: std::collections::BTreeMap<u32, u64>,
    pub seen: Vec<u64>,
    pub report: RttReport,
    pub replied: Vec<u64>,
}
impl Tracker {
    pub fn new(count: u32) -> Self {
        Self {
            pending: Default::default(),
            seen: vec![0; (count as usize).div_ceil(64)],
            report: Default::default(),
            replied: vec![0; (count as usize).div_ceil(64)],
        }
    }
    pub fn expire(&mut self, now: u64, timeout: u64) {
        let before = self.pending.len();
        self.pending.retain(|_, t| now.saturating_sub(*t) < timeout);
        self.report.timed_out += (before - self.pending.len()) as u64;
    }
    pub fn response(&mut self, n: u32, at: u64) -> Option<(u64, u64)> {
        if n as usize / 64 >= self.seen.len() {
            self.report.late_or_unmatched += 1;
            return None;
        }
        if bitmap_get(&self.seen, n) {
            self.report.duplicate_responses += 1;
            return None;
        }
        let Some(sent) = self.pending.remove(&n) else {
            self.report.late_or_unmatched += 1;
            return None;
        };
        bitmap_set(&mut self.seen, n);
        self.report.latency.record(sent, at);
        Some((sent, at))
    }
}

pub(crate) struct ReplyContext {
    pub release: watch::Receiver<Option<Instant>>,
    pub runtime: Arc<Runtime>,
    pub lane: canharbor_core::runtime::DataLane,
    pub run: Arc<Run>,
    pub plan: Plan,
    pub tracker: Arc<Mutex<Tracker>>,
    pub reservation: Arc<canharbor_core::runtime::DeviceReservation>,
    pub slot: Arc<tokio::sync::OwnedSemaphorePermit>,
}
pub(crate) async fn respond(
    mut rx: tokio::sync::mpsc::Receiver<u32>,
    mut done: watch::Receiver<bool>,
    ctx: ReplyContext,
) {
    let mut cancel = ctx.run.cancel.subscribe();
    let mut admission_plan = ctx.plan.clone();
    if !admission_plan.stages.is_empty() {
        admission_plan.count = 0;
    }
    let compiled = admission_plan.compile().expect("admitted response budget");

    let mut release = ctx.release.clone();
    let start = loop {
        if ctx.run.gate.is_revoked() || *cancel.borrow() || *done.borrow() {
            return;
        }
        let start = *release.borrow();
        if let Some(t) = start {
            break t;
        }
        tokio::select! {biased;_=cancel.changed()=>return,_=done.changed()=>return,r=release.changed()=>{if r.is_err(){return;}}}
    };
    tokio::select! {biased;_=cancel.changed()=>return,_=done.changed()=>return,_=tokio::time::sleep_until(start)=>{}}
    let mut next_reply = Instant::now();
    loop {
        if ctx.run.gate.is_revoked() || *cancel.borrow() || *done.borrow() {
            break;
        }
        let n = tokio::select! {biased;_=cancel.changed()=>break,_=done.changed()=>break,n=rx.recv()=>match n{Some(n)=>n,None=>break}};
        if *cancel.borrow()
            || *done.borrow()
            || ctx.run.execution_expired()
            || ctx.run.coordination_expired()
            || ctx.run.gate.is_revoked()
        {
            break;
        }
        tokio::select! {biased;_=cancel.changed()=>break,_=done.changed()=>break,_=tokio::time::sleep_until(next_reply)=>{}}
        let spacing =
            Duration::from_nanos(1_000_000_000 / u64::from(compiled.stages[compiled.stage(n)].fps));
        next_reply = Instant::now() + spacing;
        let responder_only = ctx.plan.rtt.as_ref().unwrap().role == RttRole::Responder;
        let preparation_started = Instant::now();
        let mut prepared = match ctx.lane.prepare(ctx.plan.wire_frame(n, true)) {
            Ok(p) => p,
            Err(e) => {
                ctx.tracker.lock().unwrap().report.responder_failed += 1;
                if let Some(sink) = &ctx.run.sink {
                    sink.emit(EvidenceEvent::Reply {
                        sequence: n,
                        outcome: "failed".into(),
                    });
                }
                ctx.run.terminate(
                    "responder_authority",
                    e.to_string(),
                    Some(ctx.plan.receivers[0].clone()),
                    Some(n),
                );
                break;
            }
        };
        if ctx.run.bind_execution(&prepared).is_err() {
            ctx.run.close_gate();
            break;
        }
        let child = prepared.cancellation();
        *ctx.run.responder_inflight.lock().unwrap() = Some(child.clone());
        if *cancel.borrow()
            || *done.borrow()
            || ctx.run.execution_expired()
            || ctx.run.coordination_expired()
            || ctx.run.gate.is_revoked()
        {
            child.cancel();
            break;
        }
        if responder_only {
            let mut r = ctx.run.report.lock().unwrap();
            r.attempted += 1;
            r.lifecycle.last_attempt_sequence = Some(n);
            r.lifecycle.last_attempt_ns = Some(ctx.run.elapsed_ns());
            r.lifecycle.inflight_sequence = Some(n);
        }
        let trace = ctx
            .run
            .observe_submission(&mut prepared, n, true, preparation_started);
        let rt = ctx.runtime.clone();
        let mut io = Box::pin(async move { rt.submit_prepared(prepared).await });
        let result = tokio::select! {biased;_=done.changed()=>{child.cancel();tokio::time::timeout(Duration::from_secs(1),&mut io).await.ok()},_=cancel.changed()=>{child.cancel();tokio::time::timeout(Duration::from_secs(1),&mut io).await.ok()},r=&mut io=>Some(r),_=tokio::time::sleep(Duration::from_secs(1))=>{child.cancel();None}};
        ctx.run.responder_inflight.lock().unwrap().take();
        let unknown = result.as_ref().is_none_or(|r| {
            matches!(
                r,
                Err(canharbor_core::runtime::RuntimeError::SubmissionUnknown)
            ) || (matches!(
                r,
                Err(canharbor_core::runtime::RuntimeError::Backend(
                    canharbor_core::Error::Fault(_)
                ))
            ) && !ctx.run.report.lock().unwrap().simulated)
        });
        let outcome = if unknown {
            "unknown"
        } else if matches!(&result, Some(Ok(()))) {
            "submitted"
        } else {
            "failed"
        };
        ctx.run.settle_timing(n, true, trace, outcome);
        if let Some(sink) = &ctx.run.sink {
            sink.emit(EvidenceEvent::Reply {
                sequence: n,
                outcome: outcome.into(),
            });
        }
        if responder_only {
            let mut r = ctx.run.report.lock().unwrap();
            match outcome {
                "submitted" => {
                    r.submitted += 1;
                    r.stages[compiled.stage(n)].submitted += 1;
                    r.lifecycle.last_submitted_ns = Some(ctx.run.elapsed_ns());
                    r.lifecycle.inflight_sequence = None;
                }
                "unknown" => {
                    r.unknown += 1;
                    r.unknown_sequence = Some(n);
                }
                _ => {
                    r.failed += 1;
                    r.lifecycle.inflight_sequence = None;
                }
            }
        }
        if unknown {
            {
                let mut t = ctx.tracker.lock().unwrap();
                t.report.responder_unknown += 1;
                t.report.responder_unknown_sequence = Some(n);
            }
            if result.is_none() {
                let pending = ctx.run.keep_pending_io();
                let reservation = ctx.reservation.clone();
                let slot = ctx.slot.clone();
                tokio::spawn(async move {
                    let _pending = pending;
                    let _keep = (reservation, slot);
                    let _ = io.await;
                });
            }
            ctx.run.terminate(
                "responder_unknown",
                "response outcome unknown; never retry",
                Some(ctx.plan.receivers[0].clone()),
                Some(n),
            );
            break;
        }
        match result.unwrap() {
            Ok(()) => {
                let mut t = ctx.tracker.lock().unwrap();
                t.report.responder_submitted += 1;
                bitmap_set(&mut t.replied, n);
            }
            Err(e) => {
                ctx.tracker.lock().unwrap().report.responder_failed += 1;
                ctx.run.terminate(
                    "responder_failed",
                    e.to_string(),
                    Some(ctx.plan.receivers[0].clone()),
                    Some(n),
                );
                break;
            }
        }
    }
}
pub(crate) async fn observe(
    mut input: canharbor_core::runtime::MeasurementInput,
    mut done: watch::Receiver<bool>,
    run: Arc<Run>,
    plan: Plan,
    tracker: Arc<Mutex<Tracker>>,
    sink: Option<EvidenceSink>,
) {
    loop {
        let event = tokio::select! {biased;_=done.changed()=>break,e=input.events.recv()=>e};
        let event = match event {
            Ok(e) => e,
            Err(_) => {
                tracker.lock().unwrap().report.invalid_responses += 1;
                run.close_gate();
                run.terminate(
                    "rtt_rx_gap",
                    "RTT response stream ended or lagged",
                    None,
                    None,
                );
                break;
            }
        };
        if let canharbor_core::BackendEvent::Frame(e) = event.as_ref() {
            if e.generation < input.backend_generation
                || e.received_at < input.started
                || e.source_channel == input.observation.channel
                || plan.correlate(&e.frame, true).is_none()
            {
                continue;
            }
            let n = match plan.decode(&e.frame, true) {
                Ok(n) => n,
                Err(_) => {
                    tracker.lock().unwrap().report.invalid_responses += 1;
                    run.close_gate();
                    continue;
                }
            };
            let received = e
                .received_at
                .saturating_duration_since(run.started)
                .as_nanos()
                .min(u128::from(u64::MAX)) as u64;
            let mut t = tracker.lock().unwrap();
            t.expire(
                received,
                u64::from(plan.rtt.as_ref().unwrap().timeout_ms) * 1_000_000,
            );
            if let Some((sent_ns, received_ns)) = t.response(n, received)
                && let Some(sink) = &sink
            {
                sink.emit(EvidenceEvent::Rtt {
                    sequence: n,
                    sent_ns,
                    received_ns,
                });
            }
        }
    }
}

impl RttOptions {
    pub fn passed(&self, t: &RttReport, submitted: u64) -> bool {
        t.latency.consistent()
            && (self.role != RttRole::Responder || t.latency.samples == 0)
            && (self.role != RttRole::Probe || t.responder_submitted == 0)
            && (self.role == RttRole::Responder || t.latency.samples == submitted)
            && (self.role == RttRole::Probe || t.responder_submitted == submitted)
            && t.timed_out == 0
            && t.cancelled == 0
            && t.responder_failed == 0
            && t.responder_unknown == 0
            && t.invalid_responses == 0
            && t.duplicate_responses == 0
            && t.late_or_unmatched == 0
            && t.latency.negative == 0
    }
}
