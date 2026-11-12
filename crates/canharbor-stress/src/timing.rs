// SPDX-License-Identifier: AGPL-3.0-or-later
//! Bounded sampled host timing. The event reducer is also used by offline replay.
use super::*;
use std::collections::BTreeMap;
const MAX_NS: u64 = 73 * 3600 * 1_000_000_000;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default, deny_unknown_fields)]
#[schemars(rename = "StressTimingOptions")]
pub struct TimingOptions {
    pub sample_every: u32,
    pub max_pending: u16,
}
impl Default for TimingOptions {
    fn default() -> Self {
        Self {
            sample_every: 100,
            max_pending: 4096,
        }
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[schemars(rename = "StressSubmissionPoints")]
pub struct Points {
    pub preparation_started_ns: u64,
    pub prepared_ns: u64,
    pub dispatched_ns: Option<u64>,
    pub enqueued_ns: Option<u64>,
    pub worker_started_ns: Option<u64>,
    pub call_started_ns: Option<u64>,
    pub call_completed_ns: Option<u64>,
    pub completed_ns: Option<u64>,
    pub scope: String,
}
impl From<canharbor_core::measurement::SubmissionPoints> for Points {
    fn from(p: canharbor_core::measurement::SubmissionPoints) -> Self {
        Self {
            preparation_started_ns: p.preparation_started_ns,
            prepared_ns: p.prepared_ns,
            dispatched_ns: p.dispatched_ns,
            enqueued_ns: p.enqueued_ns,
            worker_started_ns: p.worker_started_ns,
            call_started_ns: p.call_started_ns,
            call_completed_ns: p.call_completed_ns,
            completed_ns: p.completed_ns,
            scope: p.scope,
        }
    }
}
impl Points {
    fn valid(&self) -> bool {
        let mut last = self.preparation_started_ns;
        for n in [
            Some(self.prepared_ns),
            self.dispatched_ns,
            self.enqueued_ns,
            self.worker_started_ns,
            self.call_started_ns,
            self.call_completed_ns,
            self.completed_ns,
        ]
        .into_iter()
        .flatten()
        {
            if n < last || n > MAX_NS {
                return false;
            }
            last = n;
        }
        (self.worker_started_ns.is_none() || self.enqueued_ns.is_some())
            && (self.call_completed_ns.is_none() || self.call_started_ns.is_some())
            && (self.call_started_ns.is_none() || self.dispatched_ns.is_some())
            && matches!(
                self.scope.as_str(),
                "" | "backend_boundary; worker_queue_unknown"
                    | "native_worker_driver_call"
                    | "device_worker_submit"
            )
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[schemars(rename = "StressSignedLatency")]
pub struct SignedLatency {
    pub negative_magnitude: LatencyHistogram,
    pub nonnegative: LatencyHistogram,
    pub min_ns: Option<i64>,
    pub max_ns: Option<i64>,
    pub p50_upper_ns: Option<i64>,
    pub p95_upper_ns: Option<i64>,
    pub p99_upper_ns: Option<i64>,
}
impl SignedLatency {
    fn record(&mut self, completion: u64, rx: u64) {
        let ns = rx as i64 - completion as i64;
        if ns < 0 {
            self.negative_magnitude.record(0, ns.unsigned_abs());
        } else {
            self.nonnegative.record(0, ns as u64);
        }
        self.min_ns = Some(self.min_ns.map_or(ns, |v| v.min(ns)));
        self.max_ns = Some(self.max_ns.map_or(ns, |v| v.max(ns)));
        self.p50_upper_ns = self.quantile(50);
        self.p95_upper_ns = self.quantile(95);
        self.p99_upper_ns = self.quantile(99);
    }
    fn quantile(&self, p: u64) -> Option<i64> {
        let samples = self.negative_magnitude.samples + self.nonnegative.samples;
        if samples == 0 {
            return None;
        }
        let rank = samples / 100 * p + ((samples % 100) * p).div_ceil(100);
        let mut n = self.negative_magnitude.overflow;
        if n >= rank {
            return Some(-((1000u64 << 24) as i64));
        }
        for (i, b) in self.negative_magnitude.bins.iter().enumerate().rev() {
            n += b;
            if n >= rank {
                return Some(if i == 0 {
                    0
                } else {
                    -((1000u64 << (i - 1)) as i64)
                });
            }
        }
        for (i, b) in self.nonnegative.bins.iter().enumerate() {
            n += b;
            if n >= rank {
                return Some((1000u64 << i) as i64);
            }
        }
        None
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[schemars(rename = "StressCallTiming")]
pub struct CallTiming {
    pub samples: u64,
    pub submitted: u64,
    pub failed: u64,
    pub unknown: u64,
    pub preparation: LatencyHistogram,
    pub dispatch_wait: LatencyHistogram,
    pub worker_queue: LatencyHistogram,
    pub submit_call: LatencyHistogram,
    pub completion_wait: LatencyHistogram,
    pub total: LatencyHistogram,
    pub worker_queue_unknown: u64,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[schemars(rename = "StressReceiverTiming")]
pub struct ReceiverTiming {
    pub channel: String,
    pub sampled_rx: u64,
    pub matched: u64,
    pub unmatched_rx: u64,
    pub missing_sampled_submitted: u64,
    pub latency: SignedLatency,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[schemars(rename = "StressTimingReport")]
pub struct TimingReport {
    pub pending: u32,
    pub error: Option<String>,
    pub request: CallTiming,
    pub response: CallTiming,
    pub receivers: Vec<ReceiverTiming>,
    pub window_high_water: u32,
    pub evicted: u64,
    pub abandoned: u64,
    pub finalized: bool,
    pub scope: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case", deny_unknown_fields)]
pub enum TimingEvent {
    Begin {
        sequence: u32,
        response: bool,
    },
    Settled {
        sequence: u32,
        response: bool,
        outcome: String,
        points: Points,
    },
    Received {
        sequence: u32,
        channel: String,
        received_ns: u64,
    },
    Finish,
}
struct Pending {
    order: u64,
    rx: Vec<Option<u64>>,
    settled: Option<(String, Points)>,
}
pub struct TimingState {
    allow_response: bool,
    allow_request: bool,
    options: TimingOptions,
    count: u32,
    entries: BTreeMap<(bool, u32), Pending>,
    ordinal: u64,
    flags: [Vec<u8>; 2],
    expected: [Vec<u8>; 2],
    rx_seen: Vec<Vec<u64>>,
    pub report: TimingReport,
}
impl TimingState {
    pub fn new(p: &Plan) -> Option<Self> {
        let options = p.timing.clone()?;
        let compiled = p.compile().ok()?;
        let samples = compiled.count.div_ceil(options.sample_every) as usize;
        let channels = if p.rtt.as_ref().is_some_and(|r| r.role == RttRole::Responder) {
            vec![]
        } else {
            p.receivers.clone()
        };
        Some(Self {
            allow_response: p.rtt.as_ref().is_some_and(|r|r.role!=RttRole::Probe),
            allow_request: !p.rtt.as_ref().is_some_and(|r| r.role == RttRole::Responder),
            count: compiled.count,
            options,
            entries: Default::default(),
            ordinal: 0,
            flags: [vec![0;samples],vec![0;samples]],
            expected: [vec![0;samples],vec![0;samples]],
            rx_seen: vec![vec![0;samples.div_ceil(64)];channels.len()],
            report: TimingReport {
                receivers: channels.into_iter().map(|channel| ReceiverTiming { channel, ..Default::default() }).collect(),
                scope: "sampled host RX minus backend call completion; negative values retained; request RX only, response submission timing separate; no wire or cross-host latency".into(),
                ..Default::default()
            },
        })
    }
    fn index(&self, n: u32) -> Result<usize, String> {
        if n >= self.count || !n.is_multiple_of(self.options.sample_every) {
            Err("timing sequence is not in frozen sample set".into())
        } else {
            Ok((n / self.options.sample_every) as usize)
        }
    }
    pub fn selected(&self, n: u32) -> bool {
        self.index(n).is_ok()
    }
    fn close(&mut self, key: (bool, u32)) {
        let Some(entry) = self.entries.remove(&key) else {
            return;
        };
        if entry.settled.is_none() {
            self.report.abandoned += 1;
        }
        if !key.0 {
            for (i, rx) in entry.rx.into_iter().enumerate() {
                let r = &mut self.report.receivers[i];
                match (&entry.settled, rx) {
                    (Some((outcome, p)), Some(rx))
                        if outcome == "submitted" && p.call_completed_ns.is_some() =>
                    {
                        r.matched += 1;
                        r.latency.record(p.call_completed_ns.unwrap(), rx);
                    }
                    (_, Some(_)) => r.unmatched_rx += 1,
                    _ => {}
                }
            }
        }
    }
    pub fn apply(&mut self, event: &TimingEvent) -> Result<(), String> {
        if self.report.finalized {
            return Err("timing event after finish".into());
        }
        match event {
            TimingEvent::Begin { sequence, response } => {
                if (*response && !self.allow_response) || (!*response && !self.allow_request) {
                    return Err("timing lane not authorized by plan".into());
                }
                let i = self.index(*sequence)?;
                let role = usize::from(*response);
                if self.flags[role][i] != 0 {
                    return Err("duplicate timing begin".into());
                }
                if self.entries.len() >= usize::from(self.options.max_pending) {
                    let key = *self.entries.iter().min_by_key(|(_, p)| p.order).unwrap().0;
                    self.close(key);
                    self.report.evicted += 1;
                }
                self.flags[role][i] = 1;
                self.ordinal += 1;
                self.entries.insert(
                    (*response, *sequence),
                    Pending {
                        order: self.ordinal,
                        rx: vec![None; self.report.receivers.len()],
                        settled: None,
                    },
                );
                self.report.window_high_water =
                    self.report.window_high_water.max(self.entries.len() as u32);
            }
            TimingEvent::Settled {
                sequence,
                response,
                outcome,
                points,
            } => {
                let i = self.index(*sequence)?;
                let role = usize::from(*response);
                let flag = match outcome.as_str() {
                    "submitted" => 2,
                    "failed" => 3,
                    "unknown" => 4,
                    _ => return Err("invalid timing outcome".into()),
                };
                if self.flags[role][i] != 1
                    || !points.valid()
                    || (outcome == "submitted"
                        && (points.completed_ns.is_none() || points.call_completed_ns.is_none()))
                {
                    return Err("invalid timing settlement or timestamp order".into());
                }
                self.flags[role][i] = flag;
                let r = if *response {
                    &mut self.report.response
                } else {
                    &mut self.report.request
                };
                r.samples += 1;
                match flag {
                    2 => r.submitted += 1,
                    3 => r.failed += 1,
                    _ => r.unknown += 1,
                };
                r.preparation
                    .record(points.preparation_started_ns, points.prepared_ns);
                if let Some(n) = points.dispatched_ns {
                    r.dispatch_wait.record(points.prepared_ns, n);
                }
                if let (Some(a), Some(b)) = (points.enqueued_ns, points.worker_started_ns) {
                    r.worker_queue.record(a, b);
                } else {
                    r.worker_queue_unknown += 1;
                }
                if let (Some(a), Some(b)) = (points.call_started_ns, points.call_completed_ns) {
                    r.submit_call.record(a, b);
                }
                if let (Some(a), Some(b)) = (points.call_completed_ns, points.completed_ns) {
                    r.completion_wait.record(a, b);
                }
                if let Some(n) = points.completed_ns {
                    r.total.record(points.preparation_started_ns, n);
                }
                if let Some(e) = self.entries.get_mut(&(*response, *sequence)) {
                    e.settled = Some((outcome.clone(), points.clone()));
                }
                if *response
                    || self
                        .entries
                        .get(&(*response, *sequence))
                        .is_some_and(|e| e.rx.iter().all(Option::is_some))
                {
                    self.close((*response, *sequence));
                }
            }
            TimingEvent::Received {
                sequence,
                channel,
                received_ns,
            } => {
                let i = self.index(*sequence)?;
                let r = self
                    .report
                    .receivers
                    .iter()
                    .position(|r| r.channel == *channel)
                    .ok_or("unexpected timing receiver")?;
                if *received_ns > MAX_NS || self.rx_seen[r][i / 64] & (1 << (i % 64)) != 0 {
                    return Err("invalid or duplicate timing RX".into());
                }
                self.rx_seen[r][i / 64] |= 1 << (i % 64);
                self.report.receivers[r].sampled_rx += 1;
                if let Some(e) = self.entries.get_mut(&(false, *sequence)) {
                    e.rx[r] = Some(*received_ns);
                } else {
                    self.report.receivers[r].unmatched_rx += 1;
                }
                if self
                    .entries
                    .get(&(false, *sequence))
                    .is_some_and(|e| e.settled.is_some() && e.rx.iter().all(Option::is_some))
                {
                    self.close((false, *sequence));
                }
            }
            TimingEvent::Finish => {
                let keys = self.entries.keys().copied().collect::<Vec<_>>();
                for key in keys {
                    self.close(key);
                }
                for r in &mut self.report.receivers {
                    r.missing_sampled_submitted =
                        self.report.request.submitted.saturating_sub(r.matched);
                }
                self.report.finalized = true;
            }
        }
        Ok(())
    }
    pub fn expect(&mut self, first: u32, count: u32, response: bool, outcome: &str) {
        let every = self.options.sample_every;
        let end = u64::from(first) + u64::from(count);
        let mut n = u64::from(first).div_ceil(u64::from(every)) * u64::from(every);
        while n < end {
            let flag = match outcome {
                "submitted" => 2,
                "failed" => 3,
                "unknown" => 4,
                _ => 0,
            };
            self.expected[usize::from(response)][(n / u64::from(every)) as usize] = flag;
            n += u64::from(every);
        }
    }
    pub fn verify_outcomes(&self) -> Result<(), String> {
        for (role, expected) in self.expected.iter().enumerate() {
            for (i, e) in expected.iter().enumerate() {
                let actual = self.flags[role][i];
                if (actual >= 2 && actual != *e) || (*e >= 2 && actual != *e) {
                    return Err(
                        "sampled timing outcomes differ from independent submissions/replies"
                            .into(),
                    );
                }
            }
        }
        Ok(())
    }
    fn release_tracking(&mut self) {
        self.entries = BTreeMap::new();
        self.flags = [vec![], vec![]];
        self.expected = [vec![], vec![]];
        self.rx_seen = vec![];
    }
    pub fn snapshot(&self) -> TimingReport {
        let mut r = self.report.clone();
        r.pending = self.entries.len() as u32;
        for peer in &mut r.receivers {
            peer.missing_sampled_submitted = r.request.submitted.saturating_sub(peer.matched);
        }
        r
    }
}
pub(crate) struct TimingRecorder {
    pub state: TimingState,
    sink: Option<EvidenceSink>,
}
impl TimingRecorder {
    pub fn new(p: &Plan, sink: Option<EvidenceSink>) -> Option<Self> {
        Some(Self {
            state: TimingState::new(p)?,
            sink,
        })
    }
    fn emit(&mut self, event: TimingEvent) {
        if let Err(e) = self.state.apply(&event) {
            self.state.report.error = Some(e.clone());
            if let Some(s) = &self.sink {
                s.fail(e);
            }
            return;
        }
        if let Some(s) = &self.sink {
            s.emit(EvidenceEvent::Timing(event));
        }
    }
}
impl Run {
    pub(crate) fn observe_submission(
        &self,
        p: &mut canharbor_core::runtime::PreparedTransmission,
        n: u32,
        response: bool,
        start: Instant,
    ) -> Option<canharbor_core::measurement::SubmissionTrace> {
        let mut t = self.timing.as_ref()?.lock().unwrap();
        if !t.state.selected(n) {
            return None;
        }
        t.emit(TimingEvent::Begin {
            sequence: n,
            response,
        });
        Some(p.observe(self.started, start))
    }
    pub(crate) fn settle_timing(
        &self,
        n: u32,
        response: bool,
        trace: Option<canharbor_core::measurement::SubmissionTrace>,
        outcome: &str,
    ) {
        if let (Some(t), Some(trace)) = (&self.timing, trace) {
            t.lock().unwrap().emit(TimingEvent::Settled {
                sequence: n,
                response,
                outcome: outcome.into(),
                points: trace.snapshot().into(),
            });
        }
    }
    pub(crate) fn timing_rx(&self, n: u32, channel: &str, at: Instant) {
        if let Some(t) = &self.timing {
            let mut t = t.lock().unwrap();
            if t.state.allow_request && t.state.selected(n) {
                t.emit(TimingEvent::Received {
                    sequence: n,
                    channel: channel.into(),
                    received_ns: at
                        .saturating_duration_since(self.started)
                        .as_nanos()
                        .min(u128::from(u64::MAX)) as u64,
                });
            }
        }
    }
    pub(crate) fn finish_timing(&self) {
        if let Some(t) = &self.timing {
            let mut t = t.lock().unwrap();
            t.emit(TimingEvent::Finish);
            self.report.lock().unwrap().timing = Some(t.state.snapshot());
            t.state.release_tracking();
        }
    }
}

impl TimingReport {
    pub fn consistent(&self, plan: &Plan) -> bool {
        let Some(options) = &plan.timing else {
            return false;
        };
        let Ok(compiled) = plan.compile() else {
            return false;
        };
        let selected = u64::from(compiled.count).div_ceil(u64::from(options.sample_every));
        let calls = |r: &CallTiming| {
            r.submitted
                .checked_add(r.failed)
                .and_then(|n| n.checked_add(r.unknown))
                == Some(r.samples)
                && r.preparation.samples == r.samples
                && r.worker_queue.samples.checked_add(r.worker_queue_unknown) == Some(r.samples)
                && [
                    &r.preparation,
                    &r.dispatch_wait,
                    &r.worker_queue,
                    &r.submit_call,
                    &r.completion_wait,
                    &r.total,
                ]
                .iter()
                .all(|h| {
                    h.consistent()
                        && h.negative == 0
                        && h.samples <= r.samples
                        && h.max_ns <= MAX_NS
                })
        };
        let expected = if plan
            .rtt
            .as_ref()
            .is_some_and(|r| r.role == RttRole::Responder)
        {
            vec![]
        } else {
            plan.receivers.clone()
        };
        self.finalized && self.error.is_none() && self.pending==0 && self.window_high_water<=u32::from(options.max_pending)
            && self.request.samples<=selected && self.response.samples<=selected
            && (plan.rtt.as_ref().is_none_or(|r|r.role!=RttRole::Responder)||self.request.samples==0)
            && calls(&self.request)&&calls(&self.response)
            && (plan.rtt.is_some()||self.response.samples==0)
            && self.receivers.len()==expected.len()
            && self.receivers.iter().map(|r|&r.channel).collect::<std::collections::BTreeSet<_>>()==expected.iter().collect::<std::collections::BTreeSet<_>>()
            && self.receivers.iter().all(|r| {
                let h=&r.latency;
                r.matched<=self.request.submitted && r.sampled_rx.checked_sub(r.matched)==Some(r.unmatched_rx)
                && r.missing_sampled_submitted==self.request.submitted-r.matched
                && h.negative_magnitude.samples.checked_add(h.nonnegative.samples)==Some(r.matched)
                && h.negative_magnitude.consistent()&&h.nonnegative.consistent()
                && h.negative_magnitude.negative==0&&h.nonnegative.negative==0
                && h.p50_upper_ns==h.quantile(50)&&h.p95_upper_ns==h.quantile(95)&&h.p99_upper_ns==h.quantile(99)
                && if r.matched==0 {h.min_ns.is_none()&&h.max_ns.is_none()}else{matches!((h.min_ns,h.max_ns),(Some(a),Some(b)) if a<=b && a>=-(MAX_NS as i64) && b<=MAX_NS as i64)}
            })
    }
}

#[cfg(test)]
mod lifetime_tests {
    use super::*;
    #[tokio::test(start_paused = true)]
    async fn completed_run_releases_matching_and_dedup_storage_but_retains_report() {
        let p:Plan=serde_json::from_value(serde_json::json!({"sender":"virtual:0","receivers":["virtual:1"],"can_id":291,"extended":false,"run_tag":17,"count":5,"fps":100,"bitrate":500000,"receivers_active":true,"drain_ms":20,"timing":{"sample_every":1,"max_pending":2}})).unwrap();
        let run = Executor::new(Runtime::new(Arc::new(
            canharbor_virtual::VirtualBus::default(),
        )))
        .start(p, true)
        .unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            while run.state() != "completed" {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        let t = run.timing.as_ref().unwrap().lock().unwrap();
        assert!(t.state.flags.iter().all(Vec::is_empty));
        assert!(t.state.expected.iter().all(Vec::is_empty));
        assert!(t.state.rx_seen.is_empty());
        assert!(t.state.entries.is_empty());
        assert_eq!(run.snapshot().timing.unwrap().receivers[0].matched, 5);
        assert_eq!(t.state.snapshot().request.submitted, 5);
    }
}
