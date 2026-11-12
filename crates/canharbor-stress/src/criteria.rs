// SPDX-License-Identifier: AGPL-3.0-or-later
//! Frozen integer criteria. These never relax authorization, unknown I/O or evidence requirements.
use super::*;
pub const RATIO_SCALE: u32 = 1_000_000;
fn full_ratio() -> u32 {
    RATIO_SCALE
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default, deny_unknown_fields)]
#[schemars(rename = "StressCriteria")]
pub struct Criteria {
    /// Checked for the entire run AND every compiled stage (including warmup).
    pub min_submitted_ppm: u32,
    pub max_scheduler_skipped: u32,
    pub max_missing_expected_ppm: u32,
    pub max_missing_submitted_ppm: u32,
    pub max_duplicates: u32,
    pub max_reordered: u32,
    /// Sum of CRC and pattern errors per receiver.
    pub max_payload_errors: u32,
    pub max_out_of_range: u32,
    pub max_dispatch_lateness_ns: Option<u64>,
}
impl Default for Criteria {
    fn default() -> Self {
        Self {
            min_submitted_ppm: full_ratio(),
            max_scheduler_skipped: 0,
            max_missing_expected_ppm: 0,
            max_missing_submitted_ppm: 0,
            max_duplicates: 0,
            max_reordered: 0,
            max_payload_errors: 0,
            max_out_of_range: 0,
            max_dispatch_lateness_ns: None,
        }
    }
}
impl Criteria {
    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }
    pub fn validate(&self) -> Result<(), String> {
        if self.min_submitted_ppm == 0
            || self.min_submitted_ppm > RATIO_SCALE
            || self.max_missing_expected_ppm >= RATIO_SCALE
            || self.max_missing_submitted_ppm >= RATIO_SCALE
        {
            return Err("criteria ratios use integer ppm: minimum submitted 1..1000000, maximum missing 0..999999".into());
        }
        if self
            .max_dispatch_lateness_ns
            .is_some_and(|n| n > 72 * 3600 * 1_000_000_000)
        {
            return Err(
                "dispatch lateness criterion exceeds the 72 hour authorization bound".into(),
            );
        }
        Ok(())
    }
    pub(crate) fn receiver_violation(&self, r: &ReceiverReport) -> bool {
        r.duplicates > u64::from(self.max_duplicates)
            || r.reordered > u64::from(self.max_reordered)
            || u128::from(r.crc_errors) + u128::from(r.pattern_errors)
                > u128::from(self.max_payload_errors)
            || r.out_of_range > u64::from(self.max_out_of_range)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[schemars(rename = "StressCriterionFailure")]
pub struct CriterionFailure {
    pub criterion: String,
    pub channel: Option<String>,
    pub stage: Option<u32>,
    pub actual: u64,
    pub limit: u64,
    /// Present for ratios; limit is in ppm. Integer cross-products decide the result.
    pub denominator: Option<u64>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[schemars(rename = "StressAssessment")]
pub struct Assessment {
    /// Workload/quality criteria only; pass ALSO requires completed execution and complete evidence.
    pub passed: bool,
    pub workload_met: bool,
    pub failures: Vec<CriterionFailure>,
}
fn ratio_at_least(actual: u64, denominator: u64, ppm: u32) -> bool {
    denominator > 0
        && u128::from(actual) * u128::from(RATIO_SCALE) >= u128::from(denominator) * u128::from(ppm)
}
fn ratio_at_most(actual: u64, denominator: u64, ppm: u32) -> bool {
    // No known submissions proves no post-submission loss, but cannot satisfy workload.
    u128::from(actual) * u128::from(RATIO_SCALE) <= u128::from(denominator) * u128::from(ppm)
}
impl Report {
    pub fn assess(&self) -> Assessment {
        if let Some(a) = &self.adaptive {
            return a.steps.last().filter(|s| s.name == "measurement").map_or(
                Assessment {
                    passed: false,
                    workload_met: false,
                    failures: vec![],
                },
                |s| s.report.assess(),
            );
        }
        if let Some(g) = &self.group {
            return g.assess();
        }
        let c = &self.plan.criteria;
        let mut a = Assessment {
            passed: true,
            workload_met: true,
            failures: vec![],
        };
        let mut check = |ok: bool,
                         name: &str,
                         actual: u64,
                         limit: u64,
                         denominator: Option<u64>,
                         channel: Option<&str>,
                         stage: Option<u32>| {
            if !ok {
                a.failures.push(CriterionFailure {
                    criterion: name.into(),
                    actual,
                    limit,
                    denominator,
                    channel: channel.map(str::to_owned),
                    stage,
                });
            }
        };
        let workload = ratio_at_least(self.submitted, u64::from(self.planned), c.min_submitted_ppm);
        check(
            workload,
            "min_submitted_ppm",
            self.submitted,
            u64::from(c.min_submitted_ppm),
            Some(u64::from(self.planned)),
            None,
            None,
        );
        let mut stages_met = true;
        for (i, s) in self.stages.iter().enumerate() {
            let ok = ratio_at_least(s.submitted, u64::from(s.count), c.min_submitted_ppm);
            stages_met &= ok;
            check(
                ok,
                "stage_min_submitted_ppm",
                s.submitted,
                u64::from(c.min_submitted_ppm),
                Some(u64::from(s.count)),
                None,
                Some(i as u32),
            );
        }
        check(
            self.skipped <= u64::from(c.max_scheduler_skipped),
            "max_scheduler_skipped",
            self.skipped,
            u64::from(c.max_scheduler_skipped),
            None,
            None,
            None,
        );
        if let Some(limit) = c.max_dispatch_lateness_ns {
            check(
                self.max_dispatch_lateness_ns <= limit,
                "max_dispatch_lateness_ns",
                self.max_dispatch_lateness_ns,
                limit,
                None,
                None,
                None,
            );
        }
        for r in &self.receivers {
            let channel = Some(r.channel.as_str());
            check(
                ratio_at_most(
                    r.missing_expected,
                    u64::from(self.planned),
                    c.max_missing_expected_ppm,
                ),
                "max_missing_expected_ppm",
                r.missing_expected,
                u64::from(c.max_missing_expected_ppm),
                Some(u64::from(self.planned)),
                channel,
                None,
            );
            check(
                ratio_at_most(
                    r.missing_submitted,
                    self.submitted,
                    c.max_missing_submitted_ppm,
                ),
                "max_missing_submitted_ppm",
                r.missing_submitted,
                u64::from(c.max_missing_submitted_ppm),
                Some(self.submitted),
                channel,
                None,
            );
            for (name, actual, limit) in [
                ("max_duplicates", r.duplicates, c.max_duplicates),
                ("max_reordered", r.reordered, c.max_reordered),
                (
                    "max_payload_errors",
                    r.crc_errors.saturating_add(r.pattern_errors),
                    c.max_payload_errors,
                ),
                ("max_out_of_range", r.out_of_range, c.max_out_of_range),
            ] {
                check(
                    actual <= u64::from(limit),
                    name,
                    actual,
                    u64::from(limit),
                    None,
                    channel,
                    None,
                );
            }
        }
        a.workload_met =
            workload && stages_met && self.skipped <= u64::from(c.max_scheduler_skipped);
        a.passed = a.failures.is_empty();
        a
    }
    /// A transport-independent check used by replay and multi-host coordination.
    /// Does not assert that a remote host's evidence is authentic or locally available.
    pub fn passing_invariants(&self) -> bool {
        if self.plan.adaptive.is_some() {
            return self.adaptive_passing_invariants();
        }
        if self.adaptive.is_some() {
            return false;
        }
        if !self.plan.flows.is_empty() {
            return self.group_passing_invariants();
        }
        if self.group.is_some() {
            return false;
        }
        let Ok(compiled) = self.plan.compile() else {
            return false;
        };
        if self.planned != compiled.count
            || self.stages.len() != compiled.stages.len()
            || self.stages.iter().zip(&compiled.stages).any(|(a, b)| {
                a.name != b.name
                    || a.fps != b.fps
                    || a.warmup != b.warmup
                    || a.first != b.first
                    || a.count != b.count
                    || a.start_ns != b.start_ns
                    || a.submitted.checked_add(a.skipped) != Some(u64::from(a.count))
                    || a.received_unique.len() != self.receivers.len()
                    || a.received_unique.iter().any(|n| *n > u64::from(a.count))
            })
            || self
                .stages
                .iter()
                .try_fold(0u64, |n, s| n.checked_add(s.submitted))
                != Some(self.submitted)
            || self
                .stages
                .iter()
                .try_fold(0u64, |n, s| n.checked_add(s.skipped))
                != Some(self.skipped)
            || self.receivers.iter().enumerate().any(|(i, r)| {
                r.unique > u64::from(self.planned)
                    || r.missing_expected != u64::from(self.planned).saturating_sub(r.unique)
                    || r.missing_submitted > self.submitted
                    || r.missing_submitted > r.missing_expected
                    || self.stages.iter().try_fold(0u64, |n, s| {
                        s.received_unique.get(i).and_then(|v| n.checked_add(*v))
                    }) != Some(r.unique)
            })
        {
            return false;
        }
        let a = self.assess();
        let channels: std::collections::BTreeSet<_> =
            self.receivers.iter().map(|r| &r.channel).collect();
        let expected: std::collections::BTreeSet<_> = self.plan.receivers.iter().collect();
        self.resources_consistent()
            && (self.plan.shape.is_uniform() || self.version >= 10)
            && self
                .load
                .as_ref()
                .is_none_or(|l| l.completed && l.valid(self))
            && (self.plan.shape.is_uniform() || self.load.is_some())
            && self.classic_pattern_consistent()
            && (!self.plan.traffic.is_pattern() || self.version >= 9)
            && self.receivers.iter().all(|r| {
                if self.plan.traffic.is_pattern() {
                    r.classic_pattern.as_ref().is_some_and(|v| {
                        v.passed() && v.checkpoints_unique + v.data_verified_unique == r.unique
                    })
                } else {
                    r.classic_pattern.is_none()
                }
            })
            && (self.version >= 7 || (self.plan.timing.is_none() && self.plan.topology.is_none()))
            && (self.version < 7
                || self
                    .provenance
                    .as_ref()
                    .is_some_and(|p| p.valid(&self.plan)))
            && self
                .timing
                .as_ref()
                .is_none_or(|t| t.consistent(&self.plan))
            && self.timing.as_ref().is_none_or(|t| {
                t.request.submitted <= self.submitted
                    && t.response.submitted
                        <= self
                            .rtt_report
                            .as_ref()
                            .map_or(0, |r| r.responder_submitted)
            })
            && (self.version < 7 || self.plan.timing.is_some() == self.timing.is_some())
            && (self.version >= 6 || self.plan.criteria.is_default())
            && a.passed
            && a.workload_met
            && self.workload_achieved
            && (self.version < 6 || self.assessment.as_ref() == Some(&a))
            && self.state == "completed"
            && self.errors.is_empty()
            && self.measurement_complete
            && self.counters_complete
            && !self.known_violation()
            && self.failed == 0
            && self.unknown == 0
            && self.unknown_sequence.is_none()
            && self.attempted == self.submitted
            && self.submitted.checked_add(self.skipped) == Some(u64::from(self.planned))
            && self.lifecycle.stop_requested_ns.is_none()
            && channels == expected
            && self.receivers.len() == expected.len()
            && self.receivers.iter().all(|r| r.evidence_complete)
            && self.rtt_report.is_some() == self.plan.rtt.is_some()
            && self
                .rtt_report
                .as_ref()
                .is_none_or(|t| self.plan.rtt.as_ref().unwrap().passed(t, self.submitted))
            && (self.plan.stability.max_rss_growth_bytes.is_none()
                || (self.stability.status == "monitoring" && !self.stability.limit_exceeded))
    }
}
