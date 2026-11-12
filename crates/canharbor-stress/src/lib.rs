// SPDX-License-Identifier: AGPL-3.0-or-later
//! Service-owned Classic CAN sequence measurements; never infer physical ACK.
mod classic_pattern;
pub use classic_pattern::{ClassicPatternReceiver, ClassicPatternTransmit};
mod load;
pub use load::*;
mod adaptive;
pub use adaptive::{
    AdaptiveDecision, AdaptiveOptions, AdaptiveReport, AdaptiveStep, restore_adaptive_totals,
};
mod tracking;
pub use tracking::{QueueOptions, QueueReport, Tracking, TrackingReport};
mod group;
pub use group::{ChannelBudget, DeviceBudget, Flow, FlowReport, GroupBudget, GroupReport};
mod timing;
pub use timing::*;
mod provenance;
pub use provenance::*;
mod criteria;
pub use criteria::*;
mod coordination;
pub use coordination::*;
mod traffic;
pub use traffic::*;
mod rtt;
pub use rtt::*;
mod submissions;
pub use submissions::{SubmissionOutcome, SubmissionRange, SubmissionSummary};
mod evidence;
mod lifecycle;
mod stability;
pub use stability::*;
mod replay;
pub use replay::Replay;
mod plan;
use canharbor_core::{
    CanId, Config, Frame,
    runtime::{Notice, Runtime},
};
pub use evidence::*;
use futures_util::FutureExt;
pub use lifecycle::*;
pub use plan::*;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{sync::watch, time::Instant};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "StressPlan")]
pub struct Plan {
    #[serde(default, skip_serializing_if = "Tracking::is_exact")]
    pub tracking: Tracking,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queues: Option<QueueOptions>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adaptive: Option<AdaptiveOptions>,
    #[serde(default, skip_serializing_if = "LoadShape::is_uniform")]
    pub shape: LoadShape,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub flows: Vec<Flow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timing: Option<TimingOptions>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topology: Option<TopologyDeclaration>,
    #[serde(default, skip_serializing_if = "Criteria::is_default")]
    pub criteria: Criteria,
    #[serde(default, skip_serializing_if = "Traffic::is_classic")]
    pub traffic: Traffic,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rtt: Option<RttOptions>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coordination: Option<Coordination>,
    pub sender: String,
    pub receivers: Vec<String>,
    pub can_id: u32,
    pub extended: bool,
    pub run_tag: u32,
    #[serde(default)]
    pub stages: Vec<Stage>,
    #[serde(default)]
    pub evidence: EvidenceOptions,
    #[serde(default)]
    pub count: u32,
    #[serde(default)]
    pub fps: u32,
    pub bitrate: u32,
    /// Normal controller participation (ACK), not generation of data frames.
    pub receivers_active: bool,
    pub drain_ms: u32,
    #[serde(default, skip_serializing_if = "StabilityLimits::is_default")]
    pub stability: StabilityLimits,
}
impl Plan {
    pub fn fingerprint(&self) -> String {
        use sha2::{Digest, Sha256};
        Sha256::digest(serde_json::to_vec(self).expect("plan serialization"))
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }
    pub fn validate(&self, runtime: &Runtime) -> Result<(), String> {
        if self.adaptive.is_some() {
            return self.validate_adaptive(runtime);
        }
        if !self.flows.is_empty() {
            return self.validate_group(runtime);
        }
        CanId::new(self.can_id, self.extended).map_err(|e| e.to_string())?;
        self.compile()?;
        if self.stability.max_rss_growth_bytes.is_some() && current_rss().is_none() {
            return Err("required current RSS monitor unavailable".into());
        }
        if !(10..=10000).contains(&self.drain_ms)
            || self.receivers.is_empty()
            || self.receivers.len() > 7
        {
            return Err("receiver/drain bounds exceeded".into());
        }
        let mut channels = HashSet::new();
        for channel in std::iter::once(&self.sender).chain(self.receivers.iter()) {
            if channel.len() > 256 {
                return Err("channel ID exceeds 256 bytes".into());
            }
            if !channels.insert(channel) {
                return Err("roles require distinct channels".into());
            }
            let caps = runtime.capabilities(channel).map_err(|e| e.to_string())?;
            if !caps.fixed_nominal_bitrates.is_empty()
                && !caps.fixed_nominal_bitrates.contains(&self.bitrate)
            {
                return Err("bitrate not supported by channel".into());
            }
            if let Some(rate) = self.traffic.data_bitrate()
                && (!caps.fd
                    || (!caps.fixed_data_bitrates.is_empty()
                        && !caps.fixed_data_bitrates.contains(&rate)))
            {
                return Err("FD/data bitrate not supported by channel".into());
            }
            if !caps.simulated && !self.receivers_active {
                return Err("physical P1 tests require explicit ACK-capable receivers".into());
            }
        }
        let mut source_report = Report::initial(self, true, "preflight".into())?;
        source_report.capture_sources(runtime);
        let source = source_report.provenance.unwrap();
        if !source.valid(self)
            || serde_json::to_vec(&EvidenceEvent::Provenance(source))
                .map_err(|e| e.to_string())?
                .len() as u64
                + 1
                > self.evidence.segment_bytes
        {
            return Err("source evidence exceeds segment bounds; increase segment_bytes".into());
        }
        if self.bitrate == 0 {
            return Err("bitrate must be positive".into());
        }
        Ok(())
    }
    pub fn frame(&self, sequence: u32) -> Frame {
        self.wire_frame(sequence, false)
    }
}
#[derive(Serialize, Deserialize, schemars::JsonSchema)]
#[schemars(rename = "StressValidation")]
pub struct Validation {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub load_budget: Option<LoadBudget>,
    pub valid: bool,
    pub version: u32,
    #[serde(default)]
    pub group_budget: Option<GroupBudget>,
    pub plan_hash: String,
    pub plan: Plan,
    pub max_active_runs: u32,
    pub max_fps: u32,
    pub memory_limit_bytes: u64,
    pub scheduling_quantum_ns: u64,
    /// Estimated retained tracking state; includes optional telemetry reserves. Execution queue reserve is separate.
    pub tracking_bytes: u64,
    pub measurement: String,
}

#[derive(Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "StressRequest")]
#[derive(Clone)]
pub struct Request {
    pub client_request_id: String,
    pub plan_hash: String,
    pub active: bool,
    pub plan: Plan,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[schemars(rename = "StressReceiverReport")]
pub struct ReceiverReport {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tracking: Option<TrackingReport>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queues: Option<QueueReport>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub classic_pattern: Option<ClassicPatternReceiver>,
    pub channel: String,
    #[serde(default)]
    pub crc_errors: u64,
    #[serde(default)]
    pub pattern_errors: u64,
    #[serde(default)]
    pub short_sequence_only: u64,
    pub unique: u64,
    pub missing_expected: u64,
    pub missing_submitted: u64,
    pub duplicates: u64,
    pub reordered: u64,
    pub unrelated: u64,
    pub out_of_range: u64,
    pub diagnostics: u64,
    pub evidence_complete: bool,
    pub queue_high_water: u32,
}
fn bitmap_get(bits: &[u64], n: u32) -> bool {
    bits[n as usize / 64] & (1 << (n % 64)) != 0
}
fn bitmap_set(bits: &mut [u64], n: u32) {
    bits[n as usize / 64] |= 1 << (n % 64);
}
fn bitmap_fill(bits: &mut [u64], first: u32, count: u32) {
    let end = u64::from(first) + u64::from(count);
    let mut at = u64::from(first);
    while at < end {
        let offset = at % 64;
        let width = (64 - offset).min(end - at);
        let mask = if width == 64 {
            u64::MAX
        } else {
            ((1u64 << width) - 1) << offset
        };
        bits[at as usize / 64] |= mask;
        at += width;
    }
}
fn bitmap_count(bits: &[u64], first: u32, count: u32) -> u64 {
    let end = u64::from(first) + u64::from(count);
    let mut total = 0;
    let mut cursor = u64::from(first);
    while cursor < end {
        let word = cursor / 64;
        let offset = cursor % 64;
        let n = (64 - offset).min(end - cursor);
        let mask = if n == 64 {
            u64::MAX
        } else {
            ((1u64 << n) - 1) << offset
        };
        total += (bits[word as usize] & mask).count_ones() as u64;
        cursor += n;
    }
    total
}
struct Verifier {
    window: Option<tracking::Window>,
    planned: u32,
    layout: Vec<(u32, u32)>,
    stage_counts: Vec<u64>,
    pattern: Option<classic_pattern::PatternTracker>,
    accepted: Vec<(u32, u64)>,
    seen: Vec<u64>,
    highest: Option<u32>,
    report: ReceiverReport,
}
impl Verifier {
    fn new(channel: String, count: u32) -> Self {
        Self {
            window: None,
            planned: count,
            layout: vec![],
            stage_counts: vec![],
            pattern: None,
            accepted: vec![],
            seen: vec![0; (count as usize).div_ceil(64)],
            highest: None,
            report: ReceiverReport {
                channel,
                evidence_complete: true,
                ..Default::default()
            },
        }
    }
    fn seen(&self, n: u32) -> bool {
        if let Some(w) = &self.window {
            if n < w.base || u64::from(n) >= u64::from(w.base) + u64::from(w.capacity) {
                return true;
            }
            bitmap_get(&self.seen, n % w.capacity)
        } else {
            bitmap_get(&self.seen, n)
        }
    }
    fn finalized_receiver(&self, p: &Plan, submitted: &[u64]) -> ReceiverReport {
        let mut report = self.report.clone();
        report.tracking = self.tracking_snapshot(submitted);
        if let Traffic::ClassicPatternV1 { max_pending, .. } = p.traffic {
            let mut tracker = self
                .pattern
                .clone()
                .unwrap_or_else(|| classic_pattern::PatternTracker::new(max_pending as usize));
            tracker.finish(p.count, report.classic_pattern.get_or_insert_default());
        }
        report
    }
    fn finish_pattern(&mut self, p: &Plan) {
        if let Traffic::ClassicPatternV1 { max_pending, .. } = p.traffic {
            let tracker = self
                .pattern
                .get_or_insert_with(|| classic_pattern::PatternTracker::new(max_pending as usize));
            tracker.finish(p.count, self.report.classic_pattern.get_or_insert_default());
        }
    }
    fn accept(&mut self, p: &Plan, n: u32, at: u64) {
        if n >= p.count {
            self.report.out_of_range += 1;
            return;
        }
        if let Some(w) = &mut self.window {
            if w.ended || n < w.base {
                w.late += 1;
                self.refresh_tracking(&[], false);
                return;
            }
            if u64::from(n) >= u64::from(w.base) + u64::from(w.capacity) {
                w.future += 1;
                self.report.evidence_complete = false;
                self.refresh_tracking(&[], false);
                return;
            }
        }
        if self.seen(n) {
            self.report.duplicates += 1;
            return;
        }
        let index = self.window.as_ref().map_or(n, |w| n % w.capacity);
        bitmap_set(&mut self.seen, index);
        // Exact tracking has no stage counters. Most window plans have one stage.
        match self.stage_counts.len() {
            0 => {}
            1 => self.stage_counts[0] += 1,
            _ => {
                if let Some(i) = self
                    .layout
                    .partition_point(|(first, _)| *first <= n)
                    .checked_sub(1)
                {
                    self.stage_counts[i] += 1;
                }
            }
        }
        self.report.unique += 1;
        self.accepted.push((n, at));
        if let Some(r) = &mut self.report.classic_pattern {
            if n.is_multiple_of(2) {
                r.checkpoints_unique += 1;
            } else {
                r.data_verified_unique += 1;
            }
        }
        if self.highest.is_some_and(|h| n < h) {
            self.report.reordered += 1;
        }
        self.highest = Some(self.highest.map_or(n, |h| h.max(n)));
    }
    fn event(&mut self, p: &Plan, event: Notice) {
        self.accepted.clear();
        let e = match event {
            Notice::Diagnostic { diagnostic, .. } => {
                self.report.diagnostics += 1;
                if diagnostic.overflow {
                    self.report.evidence_complete = false;
                }
                return;
            }
            Notice::Frame(e) => e,
        };
        if e.direction != "rx" {
            return;
        }
        if let Traffic::ClassicPatternV1 { max_pending, .. } = p.traffic {
            if !p.pattern_id_matches(&e.frame) {
                self.report.unrelated += 1;
                return;
            }
            if e.frame.id().value() == p.traffic.checkpoint_id().unwrap()
                && matches!(e.frame, Frame::ClassicData { .. })
                && e.frame.data().len() == 8
                && e.frame.data()[..4] == p.run_tag.to_be_bytes()
                && u32::from_be_bytes(e.frame.data()[4..].try_into().unwrap()) >= p.count / 2
            {
                self.report.out_of_range += 1;
                return;
            }
            let repeated = p
                .correlate(&e.frame, false)
                .is_some_and(|n| n < p.count && self.seen(n));
            let tracker = self
                .pattern
                .get_or_insert_with(|| classic_pattern::PatternTracker::new(max_pending as usize));
            let accepted = tracker.event(
                p,
                &e.frame,
                e.elapsed_ns,
                repeated,
                self.report.classic_pattern.get_or_insert_default(),
            );
            for (n, at) in accepted {
                self.accept(p, n, at);
            }
            return;
        }
        let n = match p.decode(&e.frame, false) {
            Ok(n) => n,
            Err(PayloadError::Unrelated) => {
                self.report.unrelated += 1;
                return;
            }
            Err(PayloadError::Crc) => {
                self.report.crc_errors += 1;
                return;
            }
            Err(_) => {
                self.report.pattern_errors += 1;
                return;
            }
        };
        if !p.traffic.is_classic() && e.frame.data().len() < 32 {
            self.report.short_sequence_only += 1;
        }
        self.accept(p, n, e.elapsed_ns);
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[schemars(rename = "StressReport")]
pub struct Report {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adaptive: Option<AdaptiveReport>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub load: Option<LoadObservation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub classic_pattern_transmit: Option<ClassicPatternTransmit>,
    #[serde(default)]
    pub group: Option<GroupReport>,
    #[serde(default)]
    pub timing: Option<TimingReport>,
    #[serde(default)]
    pub provenance: Option<Provenance>,
    #[serde(default)]
    pub assessment: Option<Assessment>,
    pub plan_hash: String,
    pub version: u32,
    pub id: String,
    pub plan: Plan,
    pub state: String,
    pub verdict: String,
    pub workload_achieved: bool,
    pub measurement_complete: bool,
    pub counters_complete: bool,
    pub simulated: bool,
    pub attempted: u64,
    pub submitted: u64,
    pub skipped: u64,
    pub failed: u64,
    pub unknown: u64,
    pub unknown_sequence: Option<u32>,
    pub elapsed_seconds: f64,
    #[serde(default)]
    pub execution_timeout_ns: u64,
    pub max_dispatch_lateness_ns: u64,
    pub receivers: Vec<ReceiverReport>,
    pub errors: Vec<String>,
    pub hardware_verified: bool,
    #[serde(default)]
    pub backend_cleanup: Vec<CleanupObservation>,
    pub planned: u32,
    pub active_stage: u32,
    pub stages: Vec<StageReport>,
    pub evidence: EvidenceStatus,
    pub history: Vec<Metric>,
    pub scheduling_quantum_ns: u64,
    pub rx_lag_events: u64,
    #[serde(default)]
    pub lifecycle: Lifecycle,
    #[serde(default)]
    pub lifecycle_history: Vec<LifecycleRecord>,
    #[serde(default)]
    pub submission_evidence: SubmissionSummary,
    #[serde(default)]
    pub rtt_report: Option<RttReport>,
    #[serde(default)]
    pub integrity: String,
    #[serde(default)]
    pub recovery: Recovery,
    #[serde(default)]
    pub stability: StabilityReport,
}
pub struct Run {
    parent: Option<std::sync::Weak<Run>>,
    children: Mutex<std::collections::BTreeMap<String, Arc<Run>>>,
    flow_stops: Mutex<std::collections::BTreeMap<String, u64>>,
    timing: Option<Mutex<timing::TimingRecorder>>,
    pub report: Arc<Mutex<Report>>,
    cancel: watch::Sender<bool>,
    started: Instant,
    sink: Option<EvidenceSink>,
    inflight: Mutex<Option<canharbor_core::runtime::TransmissionCancellation>>,
    responder_inflight: Mutex<Option<canharbor_core::runtime::TransmissionCancellation>>,
    coord_deadline: Mutex<Option<Instant>>,
    gate: canharbor_core::control::ExecutionGate,
    execution_deadline: Instant,
    pending_io: Arc<std::sync::atomic::AtomicUsize>,
}
pub(crate) struct PendingIo(Arc<std::sync::atomic::AtomicUsize>);
impl Drop for PendingIo {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
    }
}
impl Run {
    pub fn id(&self) -> String {
        self.report.lock().unwrap().id.clone()
    }
    pub fn state(&self) -> String {
        self.report.lock().unwrap().state.clone()
    }
    pub fn summary(&self) -> Summary {
        let r = self.report.lock().unwrap();
        Summary {
            id: r.id.clone(),
            state: r.state.clone(),
            verdict: r.verdict.clone(),
            submitted: r.submitted,
            planned: r.planned,
            simulated: r.simulated,
        }
    }
    pub fn stop(&self) {
        {
            let mut r = self.report.lock().unwrap();
            if matches!(
                r.state.as_str(),
                "completed" | "failed" | "aborted" | "interrupted"
            ) || r.state.starts_with("saving_")
                || r.lifecycle.stop_requested_ns.is_some()
            {
                return;
            }
            self.close_gate();
            r.lifecycle.stop_requested_ns = Some(self.elapsed_ns());
            if r.lifecycle.termination.is_none() {
                r.lifecycle.termination = Some(Termination {
                    code: "operator_stop".into(),
                    detail: "stop requested; already issued I/O cannot be recalled".into(),
                    ..Default::default()
                });
            }
        }
        self.cancel.send_replace(true);
        for child in self.children.lock().unwrap().values() {
            child.stop();
        }
        if let Some(child) = self.inflight.lock().unwrap().as_ref() {
            child.cancel();
        }
        if let Some(child) = self.responder_inflight.lock().unwrap().as_ref() {
            child.cancel();
        }
        if self.state() != "stopping_blocked" {
            self.transition("cancelling");
        }
        let report = self.report.clone();
        let sink = self.sink.clone();
        let started = self.started;
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                tokio::time::sleep(Duration::from_secs(2)).await;
                let mut r = report.lock().unwrap();
                if !matches!(
                    r.state.as_str(),
                    "completed" | "failed" | "aborted" | "interrupted" | "stopping_blocked"
                ) && !r.state.starts_with("saving_")
                    && r.lifecycle.execution_end_ns.is_none()
                {
                    r.state = "stopping_blocked".into();
                    Self::journal(
                        &mut r,
                        &sink,
                        started.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64,
                    );
                }
            });
        }
    }
    pub fn pending_io(&self) -> usize {
        self.pending_io.load(std::sync::atomic::Ordering::Acquire)
            + self
                .children
                .lock()
                .unwrap()
                .values()
                .map(|r| r.pending_io())
                .sum::<usize>()
    }
    pub(crate) fn keep_pending_io(&self) -> PendingIo {
        self.pending_io
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        PendingIo(self.pending_io.clone())
    }
    pub(crate) fn execution_expired(&self) -> bool {
        Instant::now() >= self.execution_deadline
    }
    pub(crate) fn bind_execution(
        &self,
        p: &canharbor_core::runtime::PreparedTransmission,
    ) -> Result<(), canharbor_core::runtime::RuntimeError> {
        p.bind_gate(&self.gate)?;
        p.restrict_until(self.execution_deadline.into_std());
        if let Some(t) = self.coordination_deadline() {
            p.restrict_until(t.into_std());
        }
        Ok(())
    }
    fn close_gate(&self) {
        self.gate.revoke();
    }
    pub fn snapshot(&self) -> Report {
        self.report.lock().unwrap().clone()
    }
}
/// One executor per service: active and completed runs share an explicit bounded registry.
struct ExecutionContext {
    shared: Option<Arc<group::SharedExecution>>,
    authority: Option<(String, Arc<std::sync::atomic::AtomicBool>)>,
    slot: Arc<tokio::sync::OwnedSemaphorePermit>,
}
pub struct Executor {
    runtime: Arc<Runtime>,
    active: Arc<tokio::sync::Semaphore>,
}
impl Executor {
    pub fn new(runtime: Arc<Runtime>) -> Self {
        Self {
            active: runtime.stress_admission(),
            runtime,
        }
    }
    pub fn available(&self) -> bool {
        self.active.available_permits() > 0
    }
    pub fn start(&self, plan: Plan, active: bool) -> Result<Arc<Run>, String> {
        self.start_recorded(plan, active, uuid::Uuid::new_v4().to_string(), None, None)
    }
    pub fn start_recorded(
        &self,
        plan: Plan,
        active: bool,
        id: String,
        sink: Option<EvidenceSink>,
        authority: Option<(String, Arc<std::sync::atomic::AtomicBool>)>,
    ) -> Result<Arc<Run>, String> {
        self.start_scoped(plan, active, id, sink, authority, None)
    }
    pub fn start_coordinated(
        &self,
        plan: Plan,
        active: bool,
        id: String,
        sink: Option<EvidenceSink>,
        authority: Option<(String, Arc<std::sync::atomic::AtomicBool>)>,
        deadline: Instant,
    ) -> Result<Arc<Run>, String> {
        if plan.coordination.is_none() {
            return Err("missing coordinated plan".into());
        }
        self.start_scoped(plan, active, id, sink, authority, Some(deadline))
    }
    fn start_scoped(
        &self,
        plan: Plan,
        active: bool,
        id: String,
        sink: Option<EvidenceSink>,
        authority: Option<(String, Arc<std::sync::atomic::AtomicBool>)>,
        deadline: Option<Instant>,
    ) -> Result<Arc<Run>, String> {
        if deadline.is_some_and(|t| Instant::now() >= t) {
            return Err("prepared coordination lease expired before admission".into());
        }
        let sink = sink.map(|s| {
            s.ordered(plan.flows.is_empty() && plan.adaptive.is_none() && !plan.tracking.is_exact())
        });
        plan.validate(&self.runtime)?;
        let compiled = plan.compile()?;
        if !active {
            return Err("explicit active authorization required".into());
        }
        let permit = self
            .active
            .clone()
            .try_acquire_owned()
            .map_err(|_| "stress run already active")?;
        let permit = Arc::new(permit);
        let simulated = plan
            .channels()
            .iter()
            .all(|c| self.runtime.capabilities(c).is_ok_and(|v| v.simulated));
        let (cancel, rx) = watch::channel(false);
        let mut initial = Report::initial(&plan, simulated, id)?;
        initial.capture_sources(&self.runtime);
        let started = Instant::now();
        let execution_deadline = started + Duration::from_nanos(initial.execution_timeout_ns);
        let run = Arc::new(Run {
            parent: None,
            children: Default::default(),
            flow_stops: Default::default(),
            timing: if plan.flows.is_empty() && plan.adaptive.is_none() {
                timing::TimingRecorder::new(&plan, sink.clone()).map(Mutex::new)
            } else {
                None
            },
            report: Arc::new(Mutex::new(initial)),
            cancel,
            started,
            execution_deadline,
            pending_io: Default::default(),
            sink: sink.clone(),
            inflight: Mutex::new(None),
            responder_inflight: Mutex::new(None),
            coord_deadline: Mutex::new(None),
            gate: Default::default(),
        });
        if let Some(coord) = &plan.coordination {
            *run.coord_deadline.lock().unwrap() = Some(
                deadline
                    .unwrap_or_else(|| {
                        Instant::now() + Duration::from_millis(u64::from(coord.lease_ms))
                    })
                    .min(Instant::now() + Duration::from_millis(u64::from(coord.lease_ms))),
            );
            let watched = Arc::downgrade(&run);
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    let Some(run) = watched.upgrade() else {
                        break;
                    };
                    if matches!(
                        run.state().as_str(),
                        "completed" | "failed" | "aborted" | "interrupted"
                    ) {
                        break;
                    }
                    if run
                        .coord_deadline
                        .lock()
                        .unwrap()
                        .is_some_and(|t| Instant::now() >= t)
                    {
                        run.terminate(
                            "coordinator_lost",
                            "coordination heartbeat lease expired; no automatic resume",
                            None,
                            None,
                        );
                        run.stop();
                        break;
                    }
                }
            });
        }
        let watched = Arc::downgrade(&run);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(200)).await;
                let Some(run) = watched.upgrade() else {
                    break;
                };
                if matches!(
                    run.state().as_str(),
                    "completed" | "failed" | "aborted" | "interrupted"
                ) {
                    break;
                }
                if run.execution_expired() {
                    run.terminate(
                        "execution_deadline",
                        "arming plus schedule/drain authorization expired; no resume",
                        None,
                        None,
                    );
                    run.stop();
                    break;
                }
            }
        });
        let runtime = self.runtime.clone();
        let result = run.clone();
        tokio::spawn(async move {
            let _permit = permit.clone();
            let recorded = sink.is_some();
            let owner = format!("stress:{}", result.id());
            if std::panic::AssertUnwindSafe(execute(
                runtime.clone(),
                plan,
                compiled,
                result.clone(),
                rx,
                sink,
                ExecutionContext {
                    shared: None,
                    authority,
                    slot: permit.clone(),
                },
            ))
            .catch_unwind()
            .await
            .is_err()
            {
                result.close_gate();
                result.cancel.send_replace(true);
                result.report.clear_poison();
                {
                    let mut r = result.report.lock().unwrap();
                    r.state = "stopping_blocked".into();
                    r.measurement_complete = false;
                    r.verdict = "inconclusive".into();
                    r.unknown = r
                        .unknown
                        .max(r.attempted.saturating_sub(r.submitted + r.failed));
                    if r.unknown > 0 {
                        r.unknown_sequence = r
                            .attempted
                            .checked_add(r.skipped)
                            .and_then(|n| n.checked_sub(1))
                            .and_then(|n| u32::try_from(n).ok());
                    }
                    r.errors
                        .push("stress executor panicked; owner authority revoked; no retry".into());
                }
                runtime.revoke(&owner).await;
                result.report.lock().unwrap().state =
                    if recorded { "saving_failed" } else { "failed" }.into();
            }
        });
        Ok(run)
    }
}
impl Report {
    pub fn known_violation(&self) -> bool {
        if let Some(a) = &self.adaptive {
            return self.failed > 0
                || self.unknown > 0
                || self.stability.limit_exceeded
                || a.steps.last().is_some_and(|s| {
                    s.name == "measurement" && s.decision.as_ref().is_some_and(|d| !d.qualified)
                });
        }
        if let Some(g) = &self.group {
            return self.failed > 0
                || self.stability.limit_exceeded
                || g.flows.iter().any(|f| f.report.known_violation());
        }
        self.receivers.iter().any(|r| {
            r.tracking
                .as_ref()
                .is_some_and(|w| w.late_after_window > 0 || w.future_outside_window > 0)
                || r.queues.as_ref().is_some_and(|q| q.invalid_timestamps > 0)
        }) || self.failed > 0
            || self.skipped > u64::from(self.plan.criteria.max_scheduler_skipped)
            || self.stability.limit_exceeded
            || self.receivers.iter().any(|r| {
                r.classic_pattern.as_ref().is_some_and(|v| {
                    v.unverified_data > 0 || v.invalid_envelopes > 0 || v.finalized && !v.passed()
                })
            })
            || self
                .plan
                .criteria
                .max_dispatch_lateness_ns
                .is_some_and(|limit| self.max_dispatch_lateness_ns > limit)
            || self
                .receivers
                .iter()
                .any(|r| self.plan.criteria.receiver_violation(r))
            || self.rtt_report.as_ref().is_some_and(|r| {
                r.timed_out > 0
                    || r.duplicate_responses > 0
                    || r.late_or_unmatched > 0
                    || r.invalid_responses > 0
                    || r.responder_failed > 0
                    || r.latency.negative > 0
            })
            || self.lifecycle.termination.as_ref().is_some_and(|t| {
                matches!(
                    t.code.as_str(),
                    "bus_off" | "receive_overflow" | "responder_queue_full"
                )
            })
    }
    pub fn initial(plan: &Plan, simulated: bool, id: String) -> Result<Self, String> {
        let compiled = plan.compile()?;
        Ok(Report {
            adaptive: if plan.adaptive.is_some() {
                Some(AdaptiveReport::initial(plan)?)
            } else {
                None
            },
            load: None,
            classic_pattern_transmit: (plan.flows.is_empty()
                && plan.adaptive.is_none()
                && plan.traffic.is_pattern())
            .then(Default::default),
            version: if plan.requires_resource_v11() {
                11
            } else if plan.requires_load_v10() {
                10
            } else if plan.has_classic_pattern() {
                9
            } else {
                8
            },
            group: if plan.flows.is_empty() {
                None
            } else {
                Some(GroupReport::initial(plan)?)
            },
            timing: None,
            provenance: None,
            assessment: None,
            id,
            plan_hash: plan.fingerprint(),
            plan: plan.clone(),
            state: "arming".into(),
            verdict: "inconclusive".into(),
            workload_achieved: false,
            measurement_complete: false,
            counters_complete: true,
            simulated,
            attempted: 0,
            submitted: 0,
            skipped: 0,
            failed: 0,
            unknown: 0,
            unknown_sequence: None,
            elapsed_seconds: 0.0,
            execution_timeout_ns: compiled.duration_ns
                + 30_000_000_000
                + u64::from(plan.coordination.as_ref().map_or(0, |c| c.start_delay_ms)) * 1_000_000
                + u64::from(
                    plan.flow_plans()
                        .iter()
                        .map(|(_, p)| p.drain_ms.max(p.rtt.as_ref().map_or(0, |r| r.timeout_ms)))
                        .max()
                        .unwrap_or(plan.drain_ms),
                ) * 1_000_000,
            max_dispatch_lateness_ns: 0,
            receivers: vec![],
            errors: vec![],
            hardware_verified: false,
            backend_cleanup: vec![],
            planned: compiled.count,
            active_stage: 0,
            stages: compiled.stages.clone(),
            evidence: EvidenceStatus::default(),
            history: vec![],
            scheduling_quantum_ns: QUANTUM_NS,
            rx_lag_events: 0,
            lifecycle: Lifecycle::default(),
            lifecycle_history: vec![],
            submission_evidence: Default::default(),
            rtt_report: if plan.flows.is_empty() {
                plan.rtt.as_ref().map(|_| RttReport::default())
            } else {
                None
            },
            integrity: if plan.adaptive.is_some() {
                "adaptive: pass applies to selected fixed measurement; see exploration decisions"
                    .into()
            } else if plan.flows.is_empty() {
                plan.traffic.integrity().into()
            } else {
                "multi_flow; see each flow integrity".into()
            },
            recovery: Recovery::default(),
            stability: StabilityReport::default(),
        })
    }
}
impl Run {
    pub fn recovered(mut report: Report) -> Arc<Self> {
        if !matches!(
            report.state.as_str(),
            "completed" | "failed" | "aborted" | "interrupted"
        ) {
            report.state = "interrupted".into();
            report.verdict = "inconclusive".into();
            report.measurement_complete = false;
            report.counters_complete = false;
            report.evidence.complete = false;
            report
                .errors
                .push("service interrupted; no automatic resume".into());
        }
        let (cancel, _) = watch::channel(false);
        Arc::new(Self {
            parent: None,
            children: Default::default(),
            flow_stops: Default::default(),
            timing: None,
            report: Arc::new(Mutex::new(report)),
            cancel,
            started: Instant::now(),
            sink: None,
            execution_deadline: Instant::now(),
            pending_io: Default::default(),
            inflight: Mutex::new(None),
            responder_inflight: Mutex::new(None),
            coord_deadline: Mutex::new(None),
            gate: Default::default(),
        })
    }
}

async fn execute(
    runtime: Arc<Runtime>,
    p: Plan,
    compiled: Compiled,
    run: Arc<Run>,
    cancel: watch::Receiver<bool>,
    sink: Option<EvidenceSink>,
    context: ExecutionContext,
) {
    if p.adaptive.is_some() {
        adaptive::execute_adaptive(runtime, p, run, cancel, sink, context).await;
    } else if p.flows.is_empty() {
        execute_flow(runtime, p, compiled, run, cancel, sink, context).await;
    } else {
        group::execute_group(runtime, p, run, cancel, sink, context).await;
    }
}
async fn execute_flow(
    runtime: Arc<Runtime>,
    mut p: Plan,
    compiled: Compiled,
    run: Arc<Run>,
    mut cancel: watch::Receiver<bool>,
    sink: Option<EvidenceSink>,
    context: ExecutionContext,
) {
    use canharbor_core::{BackendEvent, runtime::Event};
    p.count = compiled.count;
    let sink = sink.map(|s| s.ordered(!p.tracking.is_exact()));
    let shared = context.shared.as_ref();
    let owner = shared.map_or_else(|| format!("stress:{}", run.id()), |s| s.owner.clone());
    let authority_revoked = || {
        run.execution_expired()
            || run.coordination_expired()
            || context
                .authority
                .as_ref()
                .is_some_and(|a| a.1.load(std::sync::atomic::Ordering::Acquire))
    };
    let execution_denied = || authority_revoked() || run.gate.is_revoked();
    let mut session_channels = std::collections::BTreeMap::new();
    let mut sessions = vec![];
    let mut epochs = std::collections::BTreeMap::new();
    let mut readers = vec![];
    let mut verifiers: Vec<Arc<Mutex<Verifier>>> = vec![];
    let mut observations = vec![];
    let mut load_start: Option<Instant> = None;
    let mut load_end: Option<Instant> = None;
    let record_load = !p.shape.is_uniform()
        || run
            .parent
            .as_ref()
            .and_then(std::sync::Weak::upgrade)
            .is_some_and(|p| p.report.lock().unwrap().plan.adaptive.is_some());
    let mut outcomes = submissions::OutcomeJournal::new(sink.clone());
    let mut submitted = vec![0u64; (p.count as usize).div_ceil(64)];
    let start = run.started;
    let tracker = p
        .rtt
        .as_ref()
        .map(|_| Arc::new(Mutex::new(rtt::Tracker::new(p.count))));
    let send_release = shared.map_or_else(|| watch::channel(None).0, |s| s.release.clone());
    let (reply_tx, reply_rx) = tokio::sync::mpsc::channel(64);
    let mut reply_rx = Some(reply_rx);
    if let (Some(s), Some(p)) = (&sink, run.snapshot().provenance) {
        s.emit(EvidenceEvent::Provenance(p));
    }
    run.transition("arming");
    let channels = std::iter::once(p.sender.clone())
        .chain(p.receivers.clone())
        .collect::<Vec<_>>();
    let cleanup_before = channels
        .iter()
        .map(|c| (c.clone(), runtime.cleanup_observation(c)))
        .collect::<std::collections::BTreeMap<_, _>>();
    let reservation = if let Some(shared) = shared {
        Ok(shared.reservation.clone())
    } else {
        runtime.reserve_devices(&channels).await.map(Arc::new)
    };
    let result:Result<(),String>=async {
        let reservation=reservation.as_ref().map_err(|e|e.to_string())?;
        run.transition("reserved");
        // Reserve all roles before any controller starts; no data until all RX taps are ready.
        for channel in std::iter::once(&p.sender).chain(p.receivers.iter()){
            if *cancel.borrow() || execution_denied(){return Err("cancelled".into());}
            if let Some(shared)=shared {
                let (id,g,epoch)=shared.sessions.get(channel).ok_or("shared channel missing")?;
                session_channels.insert(id.clone(),channel.clone());sessions.push((id.clone(),*g));epochs.insert(id.clone(),*epoch);continue;
            }
            let view=runtime.create_reserved(channel,Config{nominal_bitrate:p.bitrate,data_bitrate:p.traffic.data_bitrate(),listen_only:channel!=&p.sender && !p.receivers_active},Some(reservation)).await.map_err(|e|e.to_string())?;
            session_channels.insert(view.id.clone(),channel.clone());
            sessions.push((view.id.clone(),0));epochs.insert(view.id.clone(),0);
            let acquired=runtime.acquire(&view.id,&owner).await.map_err(|e|e.to_string())?;
            sessions.last_mut().unwrap().1=acquired.generation;
        }
        for (index,channel) in p.receivers.iter().enumerate(){
            if *cancel.borrow() || execution_denied(){return Err("cancelled".into());}
            let (id,g)=&sessions[index+1];if shared.is_none(){runtime.running(id,&owner,*g,true).await.map_err(|e|e.to_string())?;}
            let input=runtime.measurement_input(id).await.map_err(|e|e.to_string())?;
            epochs.insert(id.clone(),input.observation.epoch());
            observations.push(input.observation.clone());
            let verifier=Arc::new(Mutex::new(Verifier::for_plan(channel.clone(),&p,&compiled)));verifiers.push(verifier.clone());
            if let Some(tracker)=&tracker && p.rtt.as_ref().unwrap().role!=RttRole::Probe {
                let lane=runtime.data_lane(id,&owner,*g,p.wire_frame(0,true)).await.map_err(|e|e.to_string())?;
                let (done,stopping)=watch::channel(false);
                let ctx=rtt::ReplyContext{release:send_release.subscribe(),runtime:runtime.clone(),lane,run:run.clone(),plan:p.clone(),tracker:tracker.clone(),reservation:Arc::clone(reservation),slot:context.slot.clone()};
                readers.push((done,tokio::spawn(rtt::respond(reply_rx.take().unwrap(),stopping,ctx))));
            }
            let plan=p.clone();let result_run=run.clone();let evidence=sink.clone();let origin=start;let replies=reply_tx.clone();
            let (done,mut stopping)=watch::channel(false);
            let task=tokio::spawn(async move {
                let mut stream=input.events;
                let mut queue_ordinal=0u64;
                let mut process=|event:Arc<BackendEvent>,depth:u32|{
                    let mut v=verifier.lock().unwrap();
                    v.report.queue_high_water=v.report.queue_high_water.max(depth);
                    match event.as_ref(){
                        BackendEvent::Frame(e) if e.generation>=input.backend_generation && e.received_at>=input.started=>{
                            let is_rx=e.source_channel!=input.observation.channel;
                            if let Some(q)=&plan.queues && is_rx && (plan.pattern_id_matches(&e.frame)||plan.correlate(&e.frame,false).is_some()) {
                                let ordinal=queue_ordinal;queue_ordinal+=1;if ordinal.is_multiple_of(u64::from(q.sample_every)){let received_ns=e.received_at.saturating_duration_since(origin).as_nanos()as u64;let processed_ns=origin.elapsed().as_nanos()as u64;v.report.queues.get_or_insert_default().sample(received_ns,processed_ns,depth);if let Some(s)=&evidence{s.emit(EvidenceEvent::QueueSample{channel:input.observation.channel.clone(),ordinal,received_ns,processed_ns,depth});}}
                            }

                            if result_run.timing.is_some() && is_rx && !plan.traffic.is_pattern() && let Ok(n)=plan.decode(&e.frame,false) && n<plan.count && !v.seen(n) {result_run.timing_rx(n,&input.observation.channel,e.received_at);}
                            if is_rx && plan.pattern_id_matches(&e.frame) && let Some(sink)=&evidence { sink.emit(EvidenceEvent::PatternFrame { channel:input.observation.channel.clone(), data_hex:e.frame.data().iter().map(|b|format!("{b:02X}")).collect(), wire:SampleWire::of(&e.frame), elapsed_ns:e.received_at.saturating_duration_since(origin).as_nanos().min(u128::from(u64::MAX)) as u64 }); }
                            if is_rx && !plan.traffic.is_pattern() && plan.evidence.sample_every>0 && let Some(sequence)=plan.correlate(&e.frame,false) && sequence.is_multiple_of(plan.evidence.sample_every) && let Some(sink)=&evidence {
                                    sink.emit(EvidenceEvent::Sample{channel:input.observation.channel.clone(),sequence,data_hex:e.frame.data().iter().map(|b|format!("{b:02X}")).collect(),wire:Some(SampleWire::of(&e.frame)),elapsed_ns:e.received_at.saturating_duration_since(origin).as_nanos().min(u128::from(u64::MAX)) as u64});
                            }
                            if is_rx && plan.rtt.as_ref().is_some_and(|r|r.role!=RttRole::Probe) && let Ok(n)=plan.decode(&e.frame,false) && n<plan.count && !v.seen(n) && replies.try_send(n).is_err(){v.report.evidence_complete=false;}
                            v.event(&plan,Notice::Frame(Event{sequence:0,elapsed_ns:e.received_at.saturating_duration_since(origin).as_nanos().min(u128::from(u64::MAX)) as u64,frame:e.frame.clone(),direction:if is_rx{"rx"}else{"tx"},metadata:e.metadata.clone()}));
                            if plan.traffic.is_pattern() { for (n,at) in v.accepted.drain(..) { result_run.timing_rx(n,&input.observation.channel,origin+Duration::from_nanos(at)); } }
                        },
                        BackendEvent::Diagnostic(d) if d.generation>=input.backend_generation && d.received_at>=input.started=>{
                            if let Some(sink)=&evidence {sink.emit(EvidenceEvent::Diagnostic{channel:input.observation.channel.clone(),elapsed_ns:d.received_at.saturating_duration_since(origin).as_nanos().min(u128::from(u64::MAX)) as u64,classes:d.classes,data:d.data,overflow:d.overflow});}
                            v.event(&plan,Notice::Diagnostic{sequence:0,elapsed_ns:0,diagnostic:d.clone()});
                        },
                        BackendEvent::Fault{generation,..} if *generation>input.backend_generation=>v.report.evidence_complete=false,
                        _=>{}
                    }
                    let receive_failed=!v.report.evidence_complete;
                    drop(v);
                    if receive_failed {result_run.close_gate();}
                    if let BackendEvent::Fault{fault,generation}=event.as_ref() && *generation>input.backend_generation {
                        result_run.terminate(match fault{canharbor_core::BackendFault::Disconnected=>"device_disconnected",canharbor_core::BackendFault::BusOff=>"bus_off",canharbor_core::BackendFault::ReceiveOverflow=>"receive_overflow",_=>"backend_fault"},format!("{fault:?}"),Some(input.observation.channel.clone()),None);
                    }
                    if let BackendEvent::Diagnostic(d)=event.as_ref() && d.overflow {result_run.terminate("receive_overflow","backend diagnostic overflow",Some(input.observation.channel.clone()),None);}
                };
                loop{tokio::select!{biased;
                    _=stopping.changed()=>{let tail=stream.len();for _ in 0..tail{match stream.try_recv(){Ok(e)=>process(e,stream.len().saturating_add(1).min(u32::MAX as usize) as u32),Err(tokio::sync::broadcast::error::TryRecvError::Empty)=>break,Err(_)=>{verifier.lock().unwrap().report.evidence_complete=false;result_run.close_gate();break;}}}break;},
                    e=stream.recv()=>match e{Ok(e)=>{let queued=stream.len().saturating_add(1).min(u32::MAX as usize) as u32;process(e,queued)},Err(error)=>{result_run.close_gate();verifier.lock().unwrap().report.evidence_complete=false;if matches!(error,tokio::sync::broadcast::error::RecvError::Lagged(_)){result_run.report.lock().unwrap().rx_lag_events+=1;}break;}}
                }}
            });
            readers.push((done,task));
        }
        if *cancel.borrow() || execution_denied(){return Err("cancelled".into());}
        let (tx,g)=&sessions[0];if shared.is_none(){runtime.running(tx,&owner,*g,true).await.map_err(|e|e.to_string())?;}
        let observation=runtime.observe(tx).await.map_err(|e|e.to_string())?;epochs.insert(tx.clone(),observation.epoch());
        let lane=runtime.data_lane(tx,&owner,*g,p.frame(0)).await.map_err(|e|e.to_string())?;
        let pattern_data_lane=if p.traffic.is_pattern(){Some(runtime.data_lane(tx,&owner,*g,p.frame(1)).await.map_err(|e|e.to_string())?)}else{None};
        if let Some(tracker)=&tracker && p.rtt.as_ref().unwrap().role!=RttRole::Responder {
            let input=runtime.measurement_input(tx).await.map_err(|e|e.to_string())?;
            let (done,stopping)=watch::channel(false);
            readers.push((done,tokio::spawn(rtt::observe(input,stopping,run.clone(),p.clone(),tracker.clone(),sink.clone()))));
        }
        let t0=if let Some(shared)=shared {
            let mut release=shared.release.subscribe();shared.ready.fetch_add(1,std::sync::atomic::Ordering::AcqRel);
            loop {if *cancel.borrow()||execution_denied(){return Err("cancelled before group release".into());}let value=*release.borrow();if let Some(t)=value{break t;}tokio::select!{biased;_=cancel.changed()=>return Err("cancelled before group release".into()),r=release.changed()=>{r.map_err(|_|"group release closed")?;}}}
        }else{Instant::now()+Duration::from_millis(p.coordination.as_ref().map_or(20,|c|u64::from(c.start_delay_ms)))};
        if record_load { load_start=Some(t0); }
        if shared.is_none(){send_release.send_replace(Some(t0));}
        run.transition("running");
        if p.rtt.as_ref().is_some_and(|r|r.role==RttRole::Responder){
            let deadline=t0+Duration::from_nanos(compiled.duration_ns)+Duration::from_millis(u64::from(p.rtt.as_ref().unwrap().timeout_ms));let mut renewed=Instant::now();
            loop {if *cancel.borrow()||execution_denied(){return Err("cancelled".into());}
                if sink.as_ref().is_some_and(EvidenceSink::failed)||observation.finished().is_some()||observations.iter().any(|o|o.finished().is_some())||verifiers.iter().any(|v|!v.lock().unwrap().report.evidence_complete){return Err("responder measurement or evidence ended".into());}
                let memory=run.report.lock().unwrap().stability.clone();if memory.limit_exceeded||memory.status=="unsupported"{return Err("responder memory criterion failed".into());}
                let t=tracker.as_ref().unwrap().lock().unwrap().report.clone();if t.responder_submitted==u64::from(p.count){break;}
                if t.responder_failed>0||t.responder_unknown>0||Instant::now()>=deadline{return Err("responder deadline or submission failure".into());}
                if Instant::now()>=renewed{for (id,g) in &sessions{runtime.renew(id,&owner,*g).await.map_err(|e|e.to_string())?;}let snapshot=tracker.as_ref().unwrap().lock().unwrap().report.clone();run.report.lock().unwrap().rtt_report=Some(snapshot);publish(&run,&verifiers,&sink,start.elapsed());renewed=Instant::now()+Duration::from_secs(1);}tokio::select!{biased;_=cancel.changed()=>return Err("cancelled".into()),_=tokio::time::sleep(Duration::from_millis(10))=>{}}}
            return Ok(());
        }
        let mut renew=Instant::now();let mut metric_at=Instant::now();let mut n=0u32;let mut quantum_batch=0u32;let mut last_quantum=u64::MAX;let mut phase=None;
        while n<p.count {
            let active=compiled.stage(n);if phase!=Some(active){run.report.lock().unwrap().active_stage=active as u32;run.transition(if compiled.stages[active].warmup{"warming"}else{"running"});phase=Some(active);}
            if *cancel.borrow() || execution_denied(){return Err("cancelled".into());}
            if sink.as_ref().is_some_and(EvidenceSink::failed){run.terminate("evidence_failure","evidence recorder failed",None,None);return Err(sink.as_ref().unwrap().error().unwrap_or("evidence failure".into()));}
            if observation.finished().is_some() || observations.iter().any(|o|o.finished().is_some()) || verifiers.iter().any(|v|!v.lock().unwrap().report.evidence_complete){run.terminate_if_unset("acquisition_ended","acquisition or RX source ended",None,Some(n));return Err("acquisition or receive evidence ended".into());}
            if Instant::now()>=renew {for (id,g) in &sessions{runtime.renew(id,&owner,*g).await.map_err(|e|{run.terminate("lease_lost",e.to_string(),None,Some(n));e.to_string()})?;}renew=Instant::now()+Duration::from_secs(5);}
            if Instant::now()>=metric_at {outcomes.flush();if p.traffic.is_pattern(){run.report.lock().unwrap().classic_pattern_transmit=Some(outcomes.pattern.clone());}
            if let Some(t)=&tracker {let snapshot=t.lock().unwrap().report.clone();run.report.lock().unwrap().rtt_report=Some(snapshot);}publish(&run,&verifiers,&sink,start.elapsed());metric_at=Instant::now()+Duration::from_secs(1);let status=run.report.lock().unwrap().stability.clone();if status.limit_exceeded||status.status=="unsupported"{run.terminate("memory_stability",format!("RSS monitor: {}",status.status),None,Some(n));return Err("memory stability criterion was not met".into());}}
            let relative=compiled.target_ns(n);let target=t0+Duration::from_nanos(relative);
            if relative!=last_quantum {quantum_batch=0;last_quantum=relative;}
            if Instant::now()<target {tokio::select!{biased;_=cancel.changed()=>return Err("cancelled".into()),_=tokio::time::sleep_until(target)=>{}}}
            let lateness=Instant::now().saturating_duration_since(target);
            {let mut report=run.report.lock().unwrap();report.max_dispatch_lateness_ns=report.max_dispatch_lateness_ns.max(lateness.as_nanos().min(u128::from(u64::MAX)) as u64);}
            if lateness.as_nanos()>=u128::from(QUANTUM_NS.max(1_000_000_000/u64::from(compiled.stages[compiled.stage(n)].fps))) || quantum_batch>=MAX_BATCH {
                let next=compiled.next_after(t0.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64,n).max(n+1);
                if let Some(sink)=&sink {sink.emit(EvidenceEvent::SchedulerSkip{first:n,count:next-n,elapsed_ns:run.elapsed_ns(),lateness_ns:lateness.as_nanos().min(u128::from(u64::MAX)) as u64});}
                outcomes.record(n,next-n,SubmissionOutcome::Skipped);let mut r=run.report.lock().unwrap();for s in &mut r.stages{let a=n.max(s.first);let b=next.min(s.first+s.count);if b>a{s.skipped+=u64::from(b-a);}}r.skipped+=u64::from(next-n);n=next;continue;
            }
            if let Some(tracker)=&tracker {let mut t=tracker.lock().unwrap();let opts=p.rtt.as_ref().unwrap();t.expire(run.elapsed_ns(),u64::from(opts.timeout_ms)*1_000_000);if t.pending.len()>=usize::from(opts.max_inflight)||t.report.responder_failed>0||t.report.responder_unknown>0||t.report.invalid_responses>0{return Err("RTT window/response evidence failed".into());}}
            if verifiers.iter().any(|v|v.lock().unwrap().needs_advance(n)){outcomes.flush();for v in &verifiers{let mut v=v.lock().unwrap();if v.needs_advance(n){v.advance(n,&submitted)?;if let Some(s)=&sink{s.emit(EvidenceEvent::TrackingAdvance{channel:v.report.channel.clone(),next_sequence:n});}}}}
            let preparation_started=Instant::now();let mut prepared=if p.traffic.is_pattern() && n%2==1 {pattern_data_lane.as_ref().unwrap()}else{&lane}.prepare(p.frame(n)).map_err(|e|{run.terminate("lease_lost",e.to_string(),Some(p.sender.clone()),Some(n));e.to_string()})?;run.bind_execution(&prepared).map_err(|e|e.to_string())?;let child=prepared.cancellation();
            *run.inflight.lock().unwrap()=Some(child.clone());
            if *cancel.borrow() || execution_denied(){child.cancel();run.inflight.lock().unwrap().take();return Err("cancelled".into());}
            let trace=run.observe_submission(&mut prepared,n,false,preparation_started);
            let stage=compiled.stage(n);
            {let mut r=run.report.lock().unwrap();r.active_stage=stage as u32;r.attempted+=1;r.lifecycle.last_attempt_ns=Some(run.elapsed_ns());r.lifecycle.last_attempt_sequence=Some(n);r.lifecycle.inflight_sequence=Some(n);r.max_dispatch_lateness_ns=r.max_dispatch_lateness_ns.max(lateness.as_nanos() as u64);}
            if let Some(tracker)=&tracker {tracker.lock().unwrap().pending.insert(n,run.elapsed_ns());}
            let rt=runtime.clone();let mut io=Box::pin(async move{rt.submit_prepared(prepared).await});
            let outcome=tokio::select!{biased;
                _=cancel.changed()=>{child.cancel();tokio::time::timeout(Duration::from_secs(1),&mut io).await.ok()},
                result=&mut io=>Some(result),
                _=tokio::time::sleep(Duration::from_secs(1))=>{child.cancel();None}
            };
            let timing_outcome=match &outcome{Some(Ok(()))=>"submitted",Some(Err(canharbor_core::runtime::RuntimeError::SubmissionUnknown))|None=>"unknown",Some(Err(canharbor_core::runtime::RuntimeError::Backend(canharbor_core::Error::Fault(_)))) if !run.report.lock().unwrap().simulated=>"unknown",_=>"failed"};
            run.settle_timing(n,false,trace,timing_outcome);
            if outcome.is_none(){let pending=run.keep_pending_io();let slot=context.slot.clone();let device=Arc::clone(reservation);tokio::spawn(async move{let _pending=pending;let _slot=slot;let _device=device;let _result=io.await;});}
            run.inflight.lock().unwrap().take();
            if outcome.is_some(){run.report.lock().unwrap().lifecycle.inflight_sequence=None;}
            match outcome{
                Some(Ok(()))=>{outcomes.record(n,1,SubmissionOutcome::Submitted);bitmap_set(&mut submitted,n);let mut r=run.report.lock().unwrap();r.submitted+=1;r.lifecycle.last_submitted_ns=Some(run.elapsed_ns());r.stages[stage].submitted+=1;},
                Some(Err(e))=>{let mut r=run.report.lock().unwrap();if matches!(e,canharbor_core::runtime::RuntimeError::SubmissionUnknown)||(!r.simulated && matches!(e,canharbor_core::runtime::RuntimeError::Backend(canharbor_core::Error::Fault(_)))){r.unknown+=1;r.unknown_sequence=Some(n);}else{r.failed+=1;}
                outcomes.record(n,1,if r.unknown>0{SubmissionOutcome::Unknown}else{SubmissionOutcome::Failed});
                if r.unknown>0 || r.lifecycle.termination.is_none(){r.lifecycle.termination=Some(Termination{code:if r.unknown>0{"submission_unknown"}else{"submission_failed"}.into(),detail:e.to_string(),channel:Some(p.sender.clone()),sequence:Some(n)});}return Err(e.to_string());},
                _=>{outcomes.record(n,1,SubmissionOutcome::Unknown);run.terminate("submission_unknown","submission deadline or interrupted response; no retry",Some(p.sender.clone()),Some(n));let mut r=run.report.lock().unwrap();r.unknown+=1;r.unknown_sequence=Some(n);return Err("submission outcome unknown; no retry".into());}
            }
            n+=1;quantum_batch+=1;
            if quantum_batch.is_multiple_of(16){tokio::task::yield_now().await;}
        }
        if record_load { tokio::select! { biased; _=cancel.changed()=>return Err("cancelled during fixed load window".into()), _=tokio::time::sleep_until(t0+Duration::from_nanos(compiled.duration_ns))=>{} } load_end=Some(Instant::now()); }
        run.transition("draining");
        tokio::select!{biased;_=cancel.changed()=>return Err("cancelled".into()),_=tokio::time::sleep(Duration::from_millis(u64::from(p.drain_ms.max(p.rtt.as_ref().map_or(0,|r|r.timeout_ms)))))=>{}}
        Ok(())
    }.await;
    outcomes.flush();
    run.close_gate();
    if observations.iter().any(|o| o.finished().is_some()) {
        for v in &verifiers {
            v.lock().unwrap().report.evidence_complete = false;
        }
    }
    run.transition("finalizing");
    for (done, task) in readers {
        done.send_replace(true);
        if task.await.is_err() {
            run.report
                .lock()
                .unwrap()
                .errors
                .push("receiver task failed".into());
        }
    }
    for v in &verifiers {
        v.lock().unwrap().finish_pattern(&p);
    }
    if let Some(tracker) = &tracker {
        let mut t = tracker.lock().unwrap();
        if *cancel.borrow() {
            t.report.cancelled += t.pending.len() as u64;
            t.pending.clear();
        } else {
            t.expire(
                run.elapsed_ns(),
                u64::from(p.rtt.as_ref().unwrap().timeout_ms) * 1_000_000,
            );
        }
        if p.rtt.as_ref().unwrap().role == RttRole::Responder {
            submitted = t.replied.clone();
        }
        if p.rtt.as_ref().unwrap().role == RttRole::Responder {
            outcomes.summary = SubmissionSummary {
                covered: (t.report.responder_submitted
                    + t.report.responder_failed
                    + t.report.responder_unknown) as u32,
                submitted: t.report.responder_submitted,
                failed: t.report.responder_failed,
                unknown: t.report.responder_unknown,
                skipped: 0,
                unknown_sequence: t.report.responder_unknown_sequence,
            };
        }
        run.report.lock().unwrap().rtt_report = Some(t.report.clone());
    }
    {
        let mut r = run.report.lock().unwrap();
        for v in &verifiers {
            let v = v.lock().unwrap();
            let mut report = v.report.clone();
            report.missing_expected = u64::from(p.count) - report.unique;
            report.missing_submitted = v.missing_submitted(&submitted);
            report.tracking = v.tracking_snapshot(&submitted);
            r.receivers.push(report);
        }
        // Replace live snapshots, not append to them.
        let receiver_count = verifiers.len();
        if r.receivers.len() > receiver_count {
            let skip = r.receivers.len() - receiver_count;
            r.receivers.drain(..skip);
        }
        for stage in &mut r.stages {
            stage.received_unique = verifiers
                .iter()
                .map(|v| v.lock().unwrap().count_seen(stage.first, stage.count))
                .collect();
        }
    }
    for (id, g) in if shared.is_none() { sessions } else { vec![] }
        .into_iter()
        .rev()
    {
        let closing = runtime.close_at_epoch(&id, &owner, g, epochs.get(&id).copied().unwrap_or(0));
        tokio::pin!(closing);
        let outcome = match tokio::time::timeout(Duration::from_secs(2), &mut closing).await {
            Ok(r) => r,
            Err(_) => {
                run.transition("stopping_blocked");
                run.report
                    .lock()
                    .unwrap()
                    .errors
                    .push("cleanup blocked; original device remains reserved".into());
                closing.await
            }
        };
        if let Some(channel) = session_channels.get(&id) {
            let before = cleanup_before.get(channel).and_then(Option::as_ref);
            let after = runtime.cleanup_observation(channel);
            let observation = if outcome.is_ok() {
                match (before, after) {
                    (Some(before), Some(after)) => CleanupObservation {
                        channel: channel.clone(),
                        backend_unconfirmed: Some(
                            after
                                .unknown_transmissions
                                .saturating_sub(before.unknown_transmissions),
                        ),
                        failures: after
                            .failures
                            .into_iter()
                            .skip(before.failures.len())
                            .take(32)
                            .collect(),
                        scope: "closed_original_epoch".into(),
                    },
                    _ => CleanupObservation {
                        channel: channel.clone(),
                        scope: "unsupported".into(),
                        ..Default::default()
                    },
                }
            } else {
                CleanupObservation {
                    channel: channel.clone(),
                    scope: "cleanup_incomplete_or_epoch_changed".into(),
                    ..Default::default()
                }
            };
            run.report.lock().unwrap().backend_cleanup.push(observation);
        }
        if let Err(e) = outcome {
            run.report
                .lock()
                .unwrap()
                .errors
                .push(format!("cleanup: {e}"));
        }
    }
    run.finish_timing();
    let ending = {
        let mut r = run.report.lock().unwrap();
        if let Err(e) = &result {
            r.errors.push(e.clone());
        }
        if let Some(sink) = &sink {
            r.evidence.queue_high_water = sink.high_water();
            if let Some(error) = sink.error() {
                r.evidence.error = Some(error.clone());
                r.errors.push(error);
            }
        }
        r.submission_evidence = outcomes.summary.clone();
        if let Some(t0) = load_start {
            let end = load_end.unwrap_or_else(Instant::now);
            let observation = LoadObservation::new(
                t0.saturating_duration_since(start).as_nanos() as u64,
                end.saturating_duration_since(start).as_nanos() as u64,
                compiled.duration_ns,
                load_end.is_some(),
                r.submitted,
                r.skipped,
            );
            if let Some(s) = &sink {
                s.emit(EvidenceEvent::LoadWindow(observation.clone()));
            }
            r.load = Some(observation);
        }
        if p.traffic.is_pattern() {
            r.classic_pattern_transmit = Some(outcomes.pattern.clone());
        }
        r.elapsed_seconds = start.elapsed().as_secs_f64();
        let assessment = r.assess();
        r.workload_achieved = assessment.workload_met;
        r.assessment = Some(assessment);
        r.measurement_complete = r.receivers.len() == p.receivers.len()
            && r.receivers.iter().all(|v| v.evidence_complete)
            && r.unknown == 0
            && r.errors.is_empty()
            && r.timing
                .as_ref()
                .is_none_or(|t| t.error.is_none() && t.finalized)
            && r.rtt_report.as_ref().is_none_or(|t| {
                t.responder_unknown == 0 && t.invalid_responses == 0 && t.latency.negative == 0
            });
        let ending = if *cancel.borrow() || authority_revoked() {
            "aborted"
        } else if result.is_ok() {
            "completed"
        } else {
            "failed"
        }
        .to_string();
        r.verdict = if r.known_violation() {
            "fail"
        } else if !r.measurement_complete || ending != "completed" {
            "inconclusive"
        } else if r
            .rtt_report
            .as_ref()
            .is_some_and(|t| !r.plan.rtt.as_ref().unwrap().passed(t, r.submitted))
        {
            "fail"
        } else if r.assessment.as_ref().is_some_and(|a| a.passed) {
            "pass"
        } else {
            "fail"
        }
        .into();
        r.lifecycle.execution_end_ns = Some(run.elapsed_ns());
        if r.lifecycle.termination.is_none() {
            r.lifecycle.termination = Some(Termination {
                code: if run.execution_expired() {
                    "execution_deadline"
                } else if run.coordination_expired() {
                    "coordinator_lost"
                } else if authority_revoked() {
                    "authority_revoked"
                } else if result.is_ok() {
                    "completed"
                } else {
                    "execution_failed"
                }
                .into(),
                detail: result
                    .as_ref()
                    .err()
                    .cloned()
                    .unwrap_or_else(|| "planned execution ended".into()),
                ..Default::default()
            });
        }
        ending
    };
    run.transition(&if sink.is_some() {
        format!("saving_{ending}")
    } else {
        ending
    });
    publish(&run, &[], &sink, start.elapsed());
}
fn publish(
    run: &Run,
    verifiers: &[Arc<Mutex<Verifier>>],
    sink: &Option<EvidenceSink>,
    elapsed: Duration,
) {
    let timing = run
        .timing
        .as_ref()
        .map(|t| t.lock().unwrap().state.snapshot());
    let mut r = run.report.lock().unwrap();
    if timing.is_some() {
        r.timing = timing;
    }
    r.elapsed_seconds = elapsed.as_secs_f64();
    if !verifiers.is_empty() {
        r.receivers = verifiers
            .iter()
            .map(|v| {
                let v = v.lock().unwrap();
                let mut report = v.report.clone();
                report.missing_expected = u64::from(r.planned) - report.unique;
                report
            })
            .collect();
    }
    if let Some(sink) = sink {
        r.evidence.queue_high_water = sink.high_water();
    }
    let previous_lifecycle = r.lifecycle.clone();
    let limits = r.plan.stability.clone();
    r.stability.observe(&limits, elapsed, current_rss());
    if r.plan.stability.max_rss_growth_bytes.is_some()
        && (r.stability.limit_exceeded || r.stability.status == "unsupported")
        && r.verdict == "pass"
    {
        r.measurement_complete = false;
        r.verdict = if r.stability.limit_exceeded {
            "fail"
        } else {
            "inconclusive"
        }
        .into();
        r.lifecycle.termination = Some(Termination {
            code: "memory_stability".into(),
            detail: format!("final RSS observation: {}", r.stability.status),
            ..Default::default()
        });
    }
    if r.lifecycle != previous_lifecycle {
        Run::journal(
            &mut r,
            sink,
            elapsed.as_nanos().min(u128::from(u64::MAX)) as u64,
        );
    }
    let metric = r.metric();
    if r.history.len() == 120 {
        r.history.remove(0);
    }
    r.history.push(metric.clone());
    if let Some(sink) = sink {
        sink.emit(EvidenceEvent::Metric(metric));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use canharbor_core::runtime::Event;
    use canharbor_virtual::{Fault, VirtualBus};
    fn plan() -> Plan {
        Plan {
            flows: vec![],
            timing: None,
            topology: None,
            criteria: Default::default(),
            tracking: Tracking::default(),
            queues: None,
            adaptive: None,
            shape: LoadShape::default(),
            traffic: Traffic::default(),
            rtt: None,
            coordination: None,
            sender: "virtual:0".into(),
            receivers: vec!["virtual:1".into()],
            can_id: 0x123,
            extended: false,
            run_tag: 0xabcdef01,
            count: 8,
            fps: 100,
            bitrate: 500000,
            receivers_active: true,
            stability: StabilityLimits::default(),
            stages: vec![],
            evidence: EvidenceOptions::default(),
            drain_ms: 20,
        }
    }
    fn event(p: &Plan, n: u32, direction: &'static str) -> Notice {
        Notice::Frame(Event {
            sequence: 0,
            elapsed_ns: 0,
            frame: p.frame(n),
            direction,
            metadata: Default::default(),
        })
    }
    async fn wait(run: &Run) -> Report {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let r = run.snapshot();
                if matches!(r.state.as_str(), "completed" | "failed" | "aborted") {
                    return r;
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .unwrap()
    }
    #[test]
    fn exact_sequence_sets_late_holes_duplicates_and_echo_exclusion() {
        let p = plan();
        let mut v = Verifier::new("b".into(), p.count);
        for n in [0, 2, 2, 1, 8] {
            v.event(&p, event(&p, n, "rx"));
        }
        v.event(&p, event(&p, 3, "tx"));
        assert_eq!(v.report.unique, 3);
        assert_eq!(v.report.duplicates, 1);
        assert_eq!(v.report.reordered, 1);
        assert_eq!(v.report.out_of_range, 1);
        assert!(!v.seen(3));
        assert_eq!(p.frame(1).data(), &[0xab, 0xcd, 0xef, 1, 0, 0, 0, 1]);
    }
    #[test]
    fn plan_limits_precede_all_io() {
        let rt = Runtime::new(Arc::new(VirtualBus::default()));
        let mut p = plan();
        p.receivers.push(p.sender.clone());
        assert!(p.validate(&rt).is_err());
        p = plan();
        p.count = 0;
        assert!(p.validate(&rt).is_err());
        p = plan();
        p.fps = MAX_FPS + 1;
        assert!(p.validate(&rt).is_err());
        p = plan();
        p.can_id = 0x800;
        assert!(p.validate(&rt).is_err());
        p = plan();
        assert!(Executor::new(rt).start(p, false).is_err());
    }
    #[test]
    fn million_frame_verifier_baseline_without_bus_io() {
        let mut p = plan();
        p.count = 1_000_000;
        let mut v = Verifier::new("baseline".into(), p.count);
        let started = std::time::Instant::now();
        // Reverse ordering exercises the entire finite range, including late holes.
        for n in (0..p.count).rev() {
            v.event(&p, event(&p, n, "rx"));
        }
        assert_eq!(v.report.unique, 1_000_000);
        assert_eq!(v.report.reordered, 999_999);
        assert_eq!(v.report.duplicates, 0);
        assert_eq!(v.seen.len() * 8, 125_000);
        println!(
            "P0 verifier-only baseline: frames={}, bitmap_bytes={}, elapsed_seconds={:.6}; no bus I/O, not a bus throughput measurement",
            v.report.unique,
            v.seen.len() * 8,
            started.elapsed().as_secs_f64()
        );
    }
    #[tokio::test(start_paused = true)]
    async fn virtual_end_to_end_and_resource_release() {
        let rt = Runtime::new(Arc::new(VirtualBus::default()));
        let executor = Executor::new(rt.clone());
        let run = executor.start(plan(), true).unwrap();
        assert!(executor.start(plan(), true).is_err());
        let r = wait(&run).await;
        assert_eq!(r.verdict, "pass", "{r:?}");
        assert_eq!(r.submitted, 8);
        assert_eq!(r.receivers[0].unique, 8);
        assert_eq!(r.receivers[0].missing_submitted, 0);
        assert!(rt.list().await.is_empty());
        assert_eq!(
            wait(&executor.start(plan(), true).unwrap()).await.verdict,
            "pass"
        );
    }
    #[tokio::test(start_paused = true)]
    async fn known_submission_failure_is_not_retried_or_passed() {
        let bus = Arc::new(VirtualBus::default());
        bus.inject(Fault::FailNextTransmit);
        let rt = Runtime::new(bus);
        let run = Executor::new(rt.clone()).start(plan(), true).unwrap();
        let r = wait(&run).await;
        assert_eq!(r.attempted, 1);
        assert_eq!(r.failed, 1);
        assert_eq!(r.unknown, 0);
        assert_ne!(r.verdict, "pass");
        assert!(rt.list().await.iter().all(|s| !s.running));
        assert!(!r.errors.is_empty()); // Runtime retains faulted sessions as evidence after revocation.
    }
    #[tokio::test(start_paused = true)]
    async fn cancel_during_schedule_never_resumes() {
        let rt = Runtime::new(Arc::new(VirtualBus::default()));
        let mut p = plan();
        p.count = 1000;
        let run = Executor::new(rt.clone()).start(p, true).unwrap();
        tokio::time::sleep(Duration::from_millis(60)).await;
        run.stop();
        let r = wait(&run).await;
        assert_eq!(r.state, "aborted");
        assert_ne!(r.verdict, "pass");
        assert!(r.attempted < 1000);
        assert!(rt.list().await.is_empty());
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert_eq!(run.snapshot().attempted, r.attempted);
    }
    #[tokio::test(start_paused = true)]
    async fn partial_reservation_rolls_back_without_sending() {
        let rt = Runtime::new(Arc::new(VirtualBus::default()));
        let occupied = rt
            .create("virtual:1", canharbor_core::runtime::Mode::Active)
            .await
            .unwrap();
        let run = Executor::new(rt.clone()).start(plan(), true).unwrap();
        let r = wait(&run).await;
        assert_eq!(r.attempted, 0);
        assert_ne!(r.verdict, "pass");
        let sessions = rt.list().await;
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, occupied.id);
    }
}

#[cfg(test)]
mod fault_tests {
    use super::*;
    use canharbor_core::{
        Backend, BackendEvent, BackendFuture, Capabilities, Channel, Error, control::Permit,
    };
    use canharbor_virtual::VirtualBus;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::broadcast;
    struct Inject {
        bus: VirtualBus,
        mode: u8,
        calls: AtomicUsize,
    }
    impl Backend for Inject {
        fn capabilities(&self, c: &str) -> Result<Capabilities, Error> {
            self.bus.capabilities(c)
        }
        fn channels(&self) -> Vec<Channel> {
            self.bus.channels()
        }
        fn subscribe(&self, c: &str) -> Result<broadcast::Receiver<Arc<BackendEvent>>, Error> {
            self.bus.subscribe(c)
        }
        fn check_channel(&self, c: &str) -> Result<u64, Error> {
            self.bus.check_channel(c)
        }
        fn transmit(&self, c: &str, f: Frame) -> Result<(), Error> {
            self.bus.transmit(c, f)
        }
        fn configure<'a>(&'a self, c: &'a str, v: Config) -> BackendFuture<'a, ()> {
            self.bus.configure(c, v)
        }
        fn start<'a>(&'a self, c: &'a str, p: Permit) -> BackendFuture<'a, ()> {
            self.bus.start(c, p)
        }
        fn stop<'a>(&'a self, c: &'a str) -> BackendFuture<'a, ()> {
            self.bus.stop(c)
        }
        fn close<'a>(&'a self, c: &'a str) -> BackendFuture<'a, ()> {
            self.bus.close(c)
        }
        fn submit<'a>(&'a self, c: &'a str, f: Frame, p: Permit) -> BackendFuture<'a, ()> {
            Box::pin(async move {
                let n = self.calls.fetch_add(1, Ordering::SeqCst);
                p.check()?;
                match self.mode {
                    1 if n == 0 => Ok(()), // Accepted but not observed at peer.
                    2 => {
                        self.bus.transmit(c, f)?;
                        panic!("injected lost submission outcome")
                    }
                    4 => {
                        tokio::time::sleep(Duration::from_secs(2)).await;
                        p.check()?;
                        self.bus.transmit(c, f)
                    }
                    3 => {
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        p.check()?;
                        self.bus.transmit(c, f)
                    }
                    _ => self.bus.transmit(c, f),
                }
            })
        }
    }
    fn plan() -> Plan {
        Plan {
            flows: vec![],
            timing: None,
            topology: None,
            criteria: Default::default(),
            tracking: Tracking::default(),
            queues: None,
            adaptive: None,
            shape: LoadShape::default(),
            traffic: Traffic::default(),
            rtt: None,
            coordination: None,
            sender: "virtual:0".into(),
            receivers: vec!["virtual:1".into()],
            can_id: 17,
            extended: false,
            run_tag: 42,
            count: 4,
            fps: 100,
            bitrate: 500000,
            receivers_active: true,
            stability: StabilityLimits::default(),
            stages: vec![],
            evidence: EvidenceOptions::default(),
            drain_ms: 30,
        }
    }
    async fn finish(run: &Run) -> Report {
        for _ in 0..1000 {
            let r = run.snapshot();
            if matches!(r.state.as_str(), "completed" | "failed" | "aborted") {
                return r;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        panic!("run did not finish")
    }
    #[tokio::test(start_paused = true)]
    async fn peer_loss_cannot_be_hidden_by_submission_success() {
        let rt = Runtime::new(Arc::new(Inject {
            bus: VirtualBus::default(),
            mode: 1,
            calls: AtomicUsize::new(0),
        }));
        let r = finish(&Executor::new(rt).start(plan(), true).unwrap()).await;
        assert_eq!(r.submitted, 4);
        assert_eq!(r.receivers[0].missing_submitted, 1);
        assert_eq!(r.verdict, "fail");
    }
    #[tokio::test(start_paused = true)]
    async fn panic_after_possible_write_is_unknown_and_never_retried() {
        let bus = Arc::new(Inject {
            bus: VirtualBus::default(),
            mode: 2,
            calls: AtomicUsize::new(0),
        });
        let r = finish(
            &Executor::new(Runtime::new(bus.clone()))
                .start(plan(), true)
                .unwrap(),
        )
        .await;
        assert_eq!(r.unknown, 1);
        assert_eq!(r.unknown_sequence, Some(0));
        assert_eq!(r.attempted, 1);
        assert_eq!(bus.calls.load(Ordering::SeqCst), 1);
        assert_ne!(r.verdict, "pass");
    }
    #[tokio::test(start_paused = true)]
    async fn slow_backend_skips_elapsed_slots_instead_of_catchup_burst() {
        let rt = Runtime::new(Arc::new(Inject {
            bus: VirtualBus::default(),
            mode: 3,
            calls: AtomicUsize::new(0),
        }));
        let r = finish(&Executor::new(rt).start(plan(), true).unwrap()).await;
        assert!(r.skipped > 0);
        assert_eq!(r.attempted + r.skipped, 4);
        assert_eq!(r.verdict, "fail");
    }
    #[tokio::test(start_paused = true)]
    async fn extended_virtual_run_renews_leases_and_preserves_exact_counts() {
        let rt = Runtime::new(Arc::new(VirtualBus::default()));
        let mut p = plan();
        p.count = 10000;
        p.fps = 100;
        let run = Executor::new(rt.clone()).start(p, true).unwrap();
        let r = tokio::time::timeout(Duration::from_secs(150), async {
            loop {
                let r = run.snapshot();
                if matches!(r.state.as_str(), "completed" | "failed" | "aborted") {
                    break r;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(r.verdict, "pass", "{r:?}");
        assert_eq!(r.submitted, 10000);
        assert_eq!(r.receivers[0].unique, 10000);
        assert!(rt.list().await.is_empty());
    }
    #[tokio::test(start_paused = true)]
    async fn unknown_inflight_io_keeps_device_and_executor_reserved_until_completion() {
        let rt = Runtime::new(Arc::new(Inject {
            bus: VirtualBus::default(),
            mode: 4,
            calls: AtomicUsize::new(0),
        }));
        let executor = Executor::new(rt.clone());
        let run = executor.start(plan(), true).unwrap();
        let r = finish(&run).await;
        assert_eq!(r.unknown, 1);
        assert!(!executor.available());
        assert!(executor.start(plan(), true).is_err());
        assert!(
            rt.create("virtual:0", canharbor_core::runtime::Mode::Active)
                .await
                .is_err()
        );
        tokio::time::advance(Duration::from_secs(3)).await;
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
        assert!(executor.available());
        assert!(
            rt.create("virtual:0", canharbor_core::runtime::Mode::Active)
                .await
                .is_ok()
        );
        rt.shutdown().await;
    }
}

#[cfg(test)]
mod p2_tests {
    use super::*;
    use canharbor_core::runtime::Mode;
    use canharbor_virtual::VirtualBus;
    fn plan() -> Plan {
        serde_json::from_value(serde_json::json!({"sender":"virtual:0","receivers":["virtual:1"],"can_id":291,"extended":false,"run_tag":42,"count":0,"fps":0,"bitrate":500000,"receivers_active":true,"drain_ms":20,"stages":[{"name":"low","fps":10,"duration_seconds":2,"warmup_seconds":1},{"name":"high","fps":100,"duration_seconds":2,"warmup_seconds":0}]})).unwrap()
    }
    #[test]
    fn stages_compile_exact_ranges_quantization_and_budgets() {
        let p = plan();
        let c = p.compile().unwrap();
        assert_eq!(c.count, 230);
        assert_eq!(c.stages.len(), 3);
        assert_eq!(c.duration_ns, 5_000_000_000);
        assert_eq!(c.stage(10), 1);
        assert_eq!(c.deadline_ns(30), 3_000_000_000);
        assert_eq!(c.target_ns(31), 3_010_000_000);
        let mut bad = p.clone();
        bad.stages[0].fps = MAX_FPS + 1;
        assert!(bad.compile().is_err());
        bad = p.clone();
        bad.stages[0].duration_seconds = 72 * 3600 + 1;
        assert!(bad.compile().is_err());
        bad = p.clone();
        bad.stages[0].fps = 20_000;
        bad.stages[0].duration_seconds = 72 * 3600;
        bad.evidence.max_bytes = 2 * 1024 * 1024 * 1024;
        assert!(bad.compile().is_err());
        bad = p.clone();
        bad.evidence.max_bytes = 1024;
        assert!(bad.compile().is_err());
        bad = p;
        bad.stages[0].fps = MAX_FPS;
        bad.evidence.sample_every = 1;
        assert!(bad.compile().is_err());
    }
    #[tokio::test(start_paused = true)]
    async fn staircase_and_warmup_accounting_are_independent() {
        let rt = Runtime::new(Arc::new(VirtualBus::new(8192)));
        let run = Executor::new(rt).start(plan(), true).unwrap();
        loop {
            let r = run.snapshot();
            if r.state == "completed" {
                assert_eq!(r.verdict, "pass", "{r:?}");
                assert_eq!(r.submitted, 230);
                assert!(r.stages[0].warmup);
                for s in r.stages {
                    assert_eq!(s.submitted, u64::from(s.count));
                    assert_eq!(s.received_unique, vec![u64::from(s.count)]);
                }
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
    #[tokio::test(start_paused = true)]
    async fn six_hour_soak_has_bounded_history_and_renewed_authority() {
        let rt = Runtime::new(Arc::new(VirtualBus::new(8192)));
        let mut p = plan();
        p.stages = vec![Stage {
            name: "soak".into(),
            fps: 1,
            duration_seconds: 6 * 3600,
            warmup_seconds: 0,
        }];
        p.evidence.max_bytes = 512 * 1024 * 1024;
        let run = Executor::new(rt.clone()).start(p, true).unwrap();
        loop {
            let r = run.snapshot();
            if matches!(r.state.as_str(), "completed" | "failed") {
                assert_eq!(r.verdict, "pass", "{r:?}");
                assert_eq!(r.submitted, 21600);
                assert!(r.history.len() <= 120);
                break;
            }
            tokio::time::sleep(Duration::from_secs(600)).await;
        }
        assert!(rt.list().await.is_empty());
    }
    #[tokio::test]
    async fn device_reservation_blocks_other_owners_and_lane_is_fenced_on_restart() {
        let rt = Runtime::new(Arc::new(VirtualBus::default()));
        let reservation = rt.reserve_devices(&["virtual:0".into()]).await.unwrap();
        assert!(rt.create("virtual:1", Mode::Active).await.is_err());
        let s = rt
            .create_reserved(
                "virtual:0",
                Config {
                    listen_only: false,
                    data_bitrate: None,
                    ..Default::default()
                },
                Some(&reservation),
            )
            .await
            .unwrap();
        let s = rt.acquire(&s.id, "owner").await.unwrap();
        rt.running(&s.id, "owner", s.generation, true)
            .await
            .unwrap();
        let p = plan();
        let lane = rt
            .classic_lane(&s.id, "owner", s.generation, p.frame(0))
            .await
            .unwrap();
        let queued = lane.prepare(p.frame(1)).unwrap();
        rt.running(&s.id, "owner", s.generation, false)
            .await
            .unwrap();
        rt.running(&s.id, "owner", s.generation, true)
            .await
            .unwrap();
        assert!(lane.prepare(p.frame(2)).is_err());
        assert!(rt.submit_prepared(queued).await.is_err());
        rt.close(&s.id, "owner", s.generation).await.unwrap();
        drop(reservation);
        assert!(rt.create("virtual:1", Mode::Active).await.is_ok());
    }
    #[test]
    fn evidence_backpressure_is_explicit_not_silent_loss() {
        let p = plan();
        let r = Report::initial(&p, true, "test".into()).unwrap();
        let (sink, _rx) = EvidenceSink::channel(1);
        sink.emit(EvidenceEvent::Metric(r.metric()));
        sink.emit(EvidenceEvent::Metric(r.metric()));
        assert!(sink.failed());
        assert!(sink.error().unwrap().contains("queue"));
    }
    #[tokio::test]
    #[ignore = "controlled virtual throughput benchmark; no hardware; run explicitly"]
    async fn controlled_virtual_lane_throughput() {
        let rt = Runtime::new(Arc::new(VirtualBus::new(65536)));
        let rx = rt.create("virtual:1", Mode::Active).await.unwrap();
        let rx = rt.acquire(&rx.id, "bench").await.unwrap();
        rt.running(&rx.id, "bench", rx.generation, true)
            .await
            .unwrap();
        let mut tap = rt.measurement_input(&rx.id).await.unwrap();
        let collector = tokio::spawn(async move {
            let mut count = 0u32;
            while count < 40_000 {
                let event = tap.events.recv().await.unwrap();
                if let canharbor_core::BackendEvent::Frame(f) = event.as_ref()
                    && f.source_channel == "virtual:0"
                {
                    count += 1;
                }
            }
            count
        });
        let tx = rt.create("virtual:0", Mode::Active).await.unwrap();
        let tx = rt.acquire(&tx.id, "bench").await.unwrap();
        rt.running(&tx.id, "bench", tx.generation, true)
            .await
            .unwrap();
        let p = plan();
        let lane = rt
            .classic_lane(&tx.id, "bench", tx.generation, p.frame(0))
            .await
            .unwrap();
        let started = std::time::Instant::now();
        for n in 0..40_000 {
            rt.submit_prepared(lane.prepare(p.frame(n)).unwrap())
                .await
                .unwrap();
            if n.is_multiple_of(16) {
                tokio::task::yield_now().await;
            }
        }
        assert_eq!(collector.await.unwrap(), 40_000);
        let seconds = started.elapsed().as_secs_f64();
        let fps = 40_000.0 / seconds;
        println!(
            "P2 controlled virtual path: submitted=40000 peer_rx=40000 elapsed_seconds={seconds:.6} fps={fps:.0} budget_max_fps={MAX_FPS}; excludes physical wire timing"
        );
        assert!(
            fps >= 2.0 * f64::from(MAX_FPS),
            "controlled host cannot sustain twice the configured stress budget"
        );
        rt.shutdown().await;
    }
}

#[cfg(test)]
mod authority_tests {
    use super::*;
    #[tokio::test(start_paused = true)]
    async fn revoked_start_authority_never_opens_or_transmits() {
        let rt = Runtime::new(Arc::new(canharbor_virtual::VirtualBus::default()));
        let p:Plan=serde_json::from_value(serde_json::json!({"sender":"virtual:0","receivers":["virtual:1"],"can_id":291,"extended":false,"run_tag":17,"count":5,"fps":10,"bitrate":500000,"receivers_active":true,"drain_ms":20})).unwrap();
        let revoked = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let run = Executor::new(rt.clone())
            .start_recorded(
                p,
                true,
                uuid::Uuid::new_v4().to_string(),
                None,
                Some(("browser:revoked".into(), revoked)),
            )
            .unwrap();
        loop {
            let r = run.snapshot();
            if r.state == "aborted" {
                assert_eq!(r.attempted, 0);
                assert_eq!(r.submitted, 0);
                assert_ne!(r.verdict, "pass");
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(rt.list().await.is_empty());
    }
    #[test]
    fn maximal_live_history_stays_below_api_response_limit() {
        let mut p:Plan=serde_json::from_value(serde_json::json!({"sender":"virtual:0","receivers":["virtual:1"],"can_id":291,"extended":false,"run_tag":17,"count":5,"fps":10,"bitrate":500000,"receivers_active":true,"drain_ms":20})).unwrap();
        p.receivers = (0..7).map(|n| format!("{n}{}", "x".repeat(255))).collect();
        let mut r = Report::initial(&p, true, uuid::Uuid::new_v4().to_string()).unwrap();
        r.receivers = p
            .receivers
            .iter()
            .map(|c| ReceiverReport {
                channel: c.clone(),
                unique: u32::MAX as u64,
                duplicates: u32::MAX as u64,
                ..Default::default()
            })
            .collect();
        p.timing = Some(TimingOptions {
            sample_every: 1,
            max_pending: 4,
        });
        r.timing = TimingState::new(&p).map(|t| t.snapshot());
        let mut source = Provenance::software();
        source.channels = std::iter::once(&p.sender)
            .chain(p.receivers.iter())
            .map(|c| ChannelSource {
                channel: c.clone(),
                device_id: "x".repeat(256),
                backend: "x".repeat(64),
                simulated: true,
                firmware: "x".repeat(128),
                libraries: vec![
                    SourceFile {
                        name: "x".repeat(128),
                        sha256: Some("f".repeat(64)),
                        bytes: Some(512 * 1024 * 1024),
                        status: "x".repeat(256)
                    };
                    16
                ],
            })
            .collect();
        r.provenance = Some(source);
        r.history = vec![r.metric(); 120];
        assert!(serde_json::to_vec(&r).unwrap().len() < 1024 * 1024);
    }
}
