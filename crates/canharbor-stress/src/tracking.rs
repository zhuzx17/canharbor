// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "format", rename_all = "snake_case", deny_unknown_fields)]
#[schemars(rename = "StressTracking")]
pub enum Tracking {
    #[default]
    Exact,
    Sliding {
        capacity: u32,
    },
}
impl Tracking {
    pub fn is_exact(&self) -> bool {
        matches!(self, Self::Exact)
    }
    pub fn capacity(&self) -> Option<u32> {
        if let Self::Sliding { capacity } = self {
            Some(*capacity)
        } else {
            None
        }
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[schemars(rename = "StressTrackingReport")]
pub struct TrackingReport {
    pub capacity: u32,
    pub base: u32,
    pub finalized_through: u32,
    pub unique_retired: u64,
    pub finalized_missing_expected: u64,
    pub finalized_missing_submitted: u64,
    pub late_after_window: u64,
    pub future_outside_window: u64,
    pub finalized: bool,
}
pub(crate) struct Window {
    pub capacity: u32,
    pub base: u32,
    pub unique_retired: u64,
    pub missing_submitted: u64,
    pub late: u64,
    pub future: u64,
    pub ended: bool,
}
impl Window {
    pub fn new(capacity: u32) -> Self {
        Self {
            capacity,
            base: 0,
            unique_retired: 0,
            missing_submitted: 0,
            late: 0,
            future: 0,
            ended: false,
        }
    }
    pub fn target(&self, n: u32) -> Option<u32> {
        if u64::from(n) >= u64::from(self.base) + u64::from(self.capacity) {
            Some((n - (self.capacity - 1)).div_ceil(self.capacity / 2) * (self.capacity / 2))
        } else {
            None
        }
    }
}
impl Verifier {
    pub(crate) fn for_plan(channel: String, p: &Plan, c: &Compiled) -> Self {
        let mut v = Self::new(
            channel,
            if let Some(w) = p.tracking.capacity() {
                w
            } else {
                c.count
            },
        );
        v.planned = c.count;
        v.window = p.tracking.capacity().map(Window::new);
        // Exact mode derives stage totals from its retained bitmap at the end.
        if v.window.is_some() {
            v.layout = c.stages.iter().map(|s| (s.first, s.count)).collect();
            v.stage_counts = vec![0; c.stages.len()];
        }
        if p.queues.is_some() {
            v.report.queues = Some(QueueReport::default());
        }
        v.refresh_tracking(&[], false);
        v
    }
    pub(crate) fn needs_advance(&self, n: u32) -> bool {
        self.window.as_ref().is_some_and(|w| w.target(n).is_some())
    }
    pub(crate) fn advance(&mut self, n: u32, submitted: &[u64]) -> Result<(), String> {
        let w = self.window.as_mut().ok_or("advance for exact tracker")?;
        let next = w.target(n).ok_or("redundant tracking advance")?;
        let end = next.min(w.base.saturating_add(w.capacity));
        let mut at = w.base;
        while at < end {
            let word = at / 64;
            let stop = end.min((u64::from(word + 1) * 64).min(u64::from(u32::MAX)) as u32);
            let width = stop - at;
            let mask = if width == 64 {
                u64::MAX
            } else {
                ((1u64 << width) - 1) << (at % 64)
            };
            let index = word as usize % self.seen.len();
            let seen = self.seen[index] & mask;
            w.unique_retired += u64::from(seen.count_ones());
            w.missing_submitted +=
                u64::from((submitted[word as usize] & mask & !seen).count_ones());
            self.seen[index] &= !mask;
            at = stop;
        }
        w.base = next;
        self.refresh_tracking(submitted, false);
        Ok(())
    }
    pub(crate) fn count_seen(&self, first: u32, count: u32) -> u64 {
        if self.window.is_none() {
            bitmap_count(&self.seen, first, count)
        } else if first == 0 && count == self.planned {
            self.report.unique
        } else {
            self.layout
                .iter()
                .position(|s| *s == (first, count))
                .map_or(0, |i| self.stage_counts[i])
        }
    }
    pub(crate) fn missing_submitted(&self, submitted: &[u64]) -> u64 {
        if let Some(w) = &self.window {
            let mut missing = w.missing_submitted;
            let mut at = w.base;
            let end = self.planned.min(w.base.saturating_add(w.capacity));
            while at < end {
                let word = at / 64;
                let stop = end.min((u64::from(word + 1) * 64).min(u64::from(u32::MAX)) as u32);
                let width = stop - at;
                let mask = if width == 64 {
                    u64::MAX
                } else {
                    ((1u64 << width) - 1) << (at % 64)
                };
                missing += u64::from(
                    (submitted[word as usize] & !self.seen[word as usize % self.seen.len()] & mask)
                        .count_ones(),
                );
                at = stop;
            }
            missing
        } else {
            submitted
                .iter()
                .zip(&self.seen)
                .map(|(s, r)| (s & !r).count_ones() as u64)
                .sum()
        }
    }
    pub(crate) fn refresh_tracking(&mut self, submitted: &[u64], ended: bool) {
        if let Some(w) = &self.window {
            self.report.tracking = Some(TrackingReport {
                capacity: w.capacity,
                base: w.base,
                finalized_through: if ended { self.planned } else { w.base },
                unique_retired: if ended {
                    self.report.unique
                } else {
                    w.unique_retired
                },
                finalized_missing_expected: if ended {
                    u64::from(self.planned) - self.report.unique
                } else {
                    u64::from(w.base) - w.unique_retired
                },
                finalized_missing_submitted: if ended {
                    self.missing_submitted(submitted)
                } else {
                    w.missing_submitted
                },
                late_after_window: w.late,
                future_outside_window: w.future,
                finalized: ended,
            });
        }
    }
    pub(crate) fn tracking_snapshot(&self, submitted: &[u64]) -> Option<TrackingReport> {
        self.window.as_ref().map(|w| TrackingReport {
            capacity: w.capacity,
            base: w.base,
            finalized_through: self.planned,
            unique_retired: self.report.unique,
            finalized_missing_expected: u64::from(self.planned) - self.report.unique,
            finalized_missing_submitted: self.missing_submitted(submitted),
            late_after_window: w.late,
            future_outside_window: w.future,
            finalized: true,
        })
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "StressQueueOptions")]
pub struct QueueOptions {
    pub sample_every: u32,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[schemars(rename = "StressQueueReport")]
pub struct QueueReport {
    pub samples: u64,
    pub observed_high_water: u32,
    pub residence: LatencyHistogram,
    pub invalid_timestamps: u64,
}
impl QueueReport {
    pub(crate) fn sample(&mut self, received: u64, processed: u64, depth: u32) {
        self.samples += 1;
        self.observed_high_water = self.observed_high_water.max(depth);
        if processed < received {
            self.invalid_timestamps += 1;
        }
        self.residence.record(received, processed);
    }
}
impl Plan {
    pub(crate) fn resource_records(&self, fps: u32) -> u32 {
        self.queues.as_ref().map_or(0, |q| {
            fps.div_ceil(q.sample_every) * self.receivers.len() as u32
        }) + self.tracking.capacity().map_or(0, |w| {
            fps.div_ceil(w / 2) * (self.receivers.len() as u32 + 1)
        })
    }
    pub fn requires_resource_v11(&self) -> bool {
        !self.tracking.is_exact()
            || self.queues.is_some()
            || self
                .flows
                .iter()
                .any(|f| !f.tracking.is_exact() || f.queues.is_some())
    }
}

impl Report {
    pub(crate) fn resources_consistent(&self) -> bool {
        (!self.plan.requires_resource_v11() || self.version >= 11)
            && self.receivers.iter().all(|r| {
                r.tracking.as_ref().map(|w| w.capacity) == self.plan.tracking.capacity()
                    && r.queues.is_some() == self.plan.queues.is_some()
                    && r.queues.as_ref().is_none_or(|q| {
                        q.invalid_timestamps == 0
                            && q.residence.consistent()
                            && q.residence.negative == 0
                            && q.samples == q.residence.samples
                            && (q.samples == 0 || q.observed_high_water > 0)
                    })
                    && r.tracking.as_ref().is_none_or(|w| {
                        w.finalized
                            && w.finalized_through == self.planned
                            && w.base <= self.planned
                            && w.base % (w.capacity / 2) == 0
                            && w.unique_retired == r.unique
                            && w.finalized_missing_expected == r.missing_expected
                            && w.finalized_missing_submitted == r.missing_submitted
                            && w.late_after_window == 0
                            && w.future_outside_window == 0
                    })
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn plan() -> Plan {
        serde_json::from_value(serde_json::json!({"sender":"virtual:0","receivers":["virtual:1"],"can_id":291,"extended":false,"run_tag":17,"count":192,"fps":100,"bitrate":500000,"receivers_active":true,"drain_ms":20,"tracking":{"format":"sliding","capacity":64}})).unwrap()
    }
    #[test]
    fn eviction_commits_loss_and_late_frames_never_rewrite_it() {
        let p = plan();
        let c = p.compile().unwrap();
        let mut v = Verifier::for_plan("virtual:1".into(), &p, &c);
        let mut tx = vec![0; 3];
        bitmap_fill(&mut tx, 0, 64);
        for n in 1..64 {
            v.accept(&p, n, 1);
        }
        v.advance(64, &tx).unwrap();
        assert_eq!(
            v.report
                .tracking
                .as_ref()
                .unwrap()
                .finalized_missing_submitted,
            1
        );
        let before = v.stage_counts.clone();
        v.accept(&p, 0, 2);
        v.accept(&p, 1, 3);
        assert_eq!(v.report.unique, 63);
        assert_eq!(v.stage_counts, before);
        assert_eq!(v.report.duplicates, 0);
        assert_eq!(v.report.tracking.as_ref().unwrap().late_after_window, 2);
        for n in 64..96 {
            bitmap_set(&mut tx, n);
            v.accept(&p, n, 4);
        }
        v.advance(96, &tx).unwrap();
        assert_eq!(v.missing_submitted(&tx), 1);
        assert_eq!(v.report.unique, 95);
        v.advance(160, &tx).unwrap();
        v.accept(&p, 160, 5);
        assert_eq!(v.count_seen(0, c.count), 96);
        assert_eq!(v.missing_submitted(&tx), 1);
        let report = v.tracking_snapshot(&tx).unwrap();
        assert_eq!(report.finalized_missing_expected, 96);
        assert_eq!(report.finalized_missing_submitted, 1);
    }
    #[test]
    fn future_receive_cannot_move_window_and_invalid_options_are_rejected() {
        let mut p = plan();
        let c = p.compile().unwrap();
        let mut v = Verifier::for_plan("virtual:1".into(), &p, &c);
        v.accept(&p, 64, 1);
        assert_eq!(v.window.as_ref().unwrap().base, 0);
        assert_eq!(v.report.unique, 0);
        assert!(!v.report.evidence_complete);
        for capacity in [0, 1, 32, 65, 2_097_152, u32::MAX] {
            p.tracking = Tracking::Sliding { capacity };
            assert!(p.compile().is_err());
        }
        p.tracking = Tracking::Exact;
        p.queues = Some(QueueOptions { sample_every: 0 });
        assert!(p.compile().is_err());
    }
    #[test]
    fn window_memory_is_bounded_but_transmit_bitmap_and_total_limit_remain() {
        let mut p = plan();
        p.count = 200_000_000;
        p.fps = 1000;
        p.receivers = (1..8).map(|n| format!("virtual:{n}")).collect();
        p.evidence.max_bytes = 2 * 1024 * 1024 * 1024;
        // Reduce outcome/metrics estimate with a higher rate while staying inside 72h.
        p.fps = 20_000;
        p.tracking = Tracking::Exact;
        let exact = p.compile().unwrap().tracking_bytes;
        p.tracking = Tracking::Sliding { capacity: 4096 };
        let sliding = p.compile().unwrap().tracking_bytes;
        assert!(sliding < exact / 4);
        assert!(sliding >= u64::from(p.count).div_ceil(64) * 8);
        p.count = 260_000_000;
        p.tracking = Tracking::Exact;
        assert!(p.compile().unwrap_err().contains("256 MiB"));
        p.tracking = Tracking::Sliding { capacity: 4096 };
        assert!(p.compile().is_ok());
        p.count = u32::MAX;
        p.receivers = vec!["virtual:1".into()];
        assert!(p.compile().is_err());
    }
    #[test]
    fn many_stage_counts_match_exact_oracle_with_reordering_and_window_reuse() {
        let mut p = plan();
        p.count = 0;
        p.fps = 0;
        p.tracking = Tracking::Exact;
        p.stages = (0..32)
            .map(|i| Stage {
                name: format!("s{i}"),
                fps: 4,
                duration_seconds: 1,
                warmup_seconds: 1,
            })
            .collect();
        let c = p.compile().unwrap();
        assert_eq!(c.stages.len(), 64);
        p.count = c.count;
        let mut exact = Verifier::for_plan("virtual:1".into(), &p, &c);
        p.tracking = Tracking::Sliding { capacity: 64 };
        let mut sliding = Verifier::for_plan("virtual:1".into(), &p, &c);
        let tx = vec![u64::MAX; (c.count as usize).div_ceil(64)];
        for first in (0..c.count).step_by(32) {
            if sliding.needs_advance(first) {
                sliding.advance(first, &tx).unwrap();
            }
            for n in (first..first + 32).rev().filter(|n| n % 17 != 0) {
                exact.accept(&p, n, 0);
                sliding.accept(&p, n, 0);
                if n % 7 == 0 {
                    exact.accept(&p, n, 0);
                    sliding.accept(&p, n, 0);
                }
            }
        }
        assert_eq!(exact.report.unique, sliding.report.unique);
        assert_eq!(exact.report.duplicates, sliding.report.duplicates);
        assert_eq!(exact.report.reordered, sliding.report.reordered);
        assert_eq!(exact.missing_submitted(&tx), sliding.missing_submitted(&tx));
        for stage in &c.stages {
            let oracle = (stage.first..stage.first + stage.count)
                .filter(|n| n % 17 != 0)
                .count() as u64;
            assert_eq!(exact.count_seen(stage.first, stage.count), oracle);
            assert_eq!(sliding.count_seen(stage.first, stage.count), oracle);
        }
    }
    #[test]
    fn ordered_metric_cannot_change_capacity_or_rewrite_queue_history() {
        let mut p = plan();
        p.queues = Some(QueueOptions { sample_every: 1 });
        let c = p.compile().unwrap();
        let mut v = Verifier::for_plan("virtual:1".into(), &p, &c);
        v.report.queues.as_mut().unwrap().sample(0, 10000, 1);
        let mut report = Report::initial(&p, true, "test".into()).unwrap();
        report.receivers = vec![v.report.clone()];
        let wrapped = |m| EvidenceEvent::TrackingRecord {
            event: Box::new(EvidenceEvent::Metric(m)),
        };
        let mut replay = Replay::new(&p).unwrap();
        replay.event(&wrapped(report.metric())).unwrap();
        report.receivers[0].tracking.as_mut().unwrap().capacity = 128;
        assert!(replay.event(&wrapped(report.metric())).is_err());
        report.receivers[0].tracking.as_mut().unwrap().capacity = 64;
        let mut rewritten = QueueReport::default();
        rewritten.sample(0, 1000, 1);
        report.receivers[0].queues = Some(rewritten);
        assert!(replay.event(&wrapped(report.metric())).is_err());
    }
    #[test]
    fn persisted_host_seconds_roundtrip_without_evidence_mismatch() {
        for seconds in [
            1.237893208_f64,
            2.473740542,
            3.709316208,
            0.000000123456789,
            123456.987654321,
        ] {
            let encoded = serde_json::to_vec(&seconds).unwrap();
            let decoded: f64 = serde_json::from_slice(&encoded).unwrap();
            assert_eq!(seconds.to_bits(), decoded.to_bits());
        }
    }
    #[test]
    fn combined_telemetry_budget_and_queue_histograms_are_checked() {
        let mut p = plan();
        p.fps = 2000;
        p.evidence.sample_every = 1;
        p.queues = Some(QueueOptions { sample_every: 1 });
        assert!(p.compile().unwrap_err().contains("2000"));
        let mut q = QueueReport::default();
        q.sample(10, 20, 3);
        q.sample(20, 10, 8);
        assert_eq!(q.samples, 2);
        assert_eq!(q.invalid_timestamps, 1);
        assert_eq!(q.residence.negative, 1);
        assert_eq!(q.observed_high_water, 8);
    }
}

#[cfg(test)]
mod performance {
    use super::*;
    #[test]
    #[ignore = "paired verifier CPU diagnostic; no CAN backend or hardware; run explicitly"]
    fn verifier_throughput_matrix() {
        for stages in [1, 64] {
            for sliding in [false, true] {
                let mut p:Plan=serde_json::from_value(serde_json::json!({"sender":"virtual:0","receivers":["virtual:1"],"can_id":291,"extended":false,"run_tag":17,"count":0,"fps":0,"bitrate":500000,"receivers_active":true,"drain_ms":20})).unwrap();
                p.stages = (0..if stages == 64 { 32 } else { 1 })
                    .map(|i| Stage {
                        name: format!("s{i}"),
                        fps: 10000,
                        duration_seconds: if stages == 64 { 1 } else { 64 },
                        warmup_seconds: if stages == 64 { 1 } else { 0 },
                    })
                    .collect();
                if sliding {
                    p.tracking = Tracking::Sliding { capacity: 4096 };
                }
                let c = p.compile().unwrap();
                p.count = c.count;
                let mut v = Verifier::for_plan("virtual:1".into(), &p, &c);
                let tx = vec![u64::MAX; (c.count as usize).div_ceil(64)];
                let cpu = cpu_seconds();
                let at = std::time::Instant::now();
                for n in 0..c.count {
                    if v.needs_advance(n) {
                        v.advance(n, &tx).unwrap();
                    }
                    v.event(
                        &p,
                        Notice::Frame(canharbor_core::runtime::Event {
                            sequence: 0,
                            elapsed_ns: u64::from(n),
                            frame: p.frame(n),
                            direction: "rx",
                            metadata: Default::default(),
                        }),
                    );
                }
                let wall = at.elapsed().as_secs_f64();
                let cpu = cpu_seconds() - cpu;
                assert_eq!(v.report.unique, u64::from(c.count));
                assert_eq!(v.missing_submitted(&tx), 0);
                for stage in &c.stages {
                    assert_eq!(
                        v.count_seen(stage.first, stage.count),
                        u64::from(stage.count)
                    );
                }
                println!(
                    "VERIFIER {}",
                    serde_json::json!({"stages":stages,"sliding":sliding,"frames":c.count,"wall_seconds":wall,"cpu_seconds":cpu,"fps":f64::from(c.count)/wall})
                );
            }
        }
    }
    fn cpu_seconds() -> f64 {
        canharbor_platform::process::process_usage()
            .expect("process accounting available")
            .0
    }
}
