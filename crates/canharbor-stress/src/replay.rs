// SPDX-License-Identifier: AGPL-3.0-or-later
//! Independent persisted-event checks. Samples never imply physical bus coverage.
use super::*;
pub struct Replay {
    inside_tracking: bool,
    queue_eligible: std::collections::BTreeMap<String, u64>,
    queue_ordinals: std::collections::BTreeMap<String, u64>,
    adaptive: Option<Box<adaptive::AdaptiveReplay>>,
    load: Option<LoadObservation>,
    pattern_transmit: ClassicPatternTransmit,
    group_release: Option<u64>,
    flows: std::collections::BTreeMap<String, Replay>,
    flow_ends: std::collections::BTreeMap<String, Report>,
    flow_stops: std::collections::BTreeMap<String, u64>,
    timing: Option<TimingState>,
    provenance: Option<Provenance>,
    audit_tail: Vec<LifecycleRecord>,
    original_hash: String,
    submitted: Vec<u64>,
    submissions: SubmissionSummary,
    stage_skipped: Vec<u64>,
    rtt: LatencyHistogram,
    replies: RttReport,
    replied_seen: Vec<u64>,
    rtt_seen: Vec<u64>,
    plan: Plan,
    compiled: Compiled,
    verifiers: Vec<Verifier>,
    last: Option<Metric>,
    revision: u64,
    last_audit: Option<Metric>,
    pub audit_records: u64,
    pub metric_records: u64,
}
impl Replay {
    pub fn new(plan: &Plan) -> Result<Self, String> {
        let compiled = plan.compile()?;
        if plan.receivers.is_empty()
            || plan.receivers.len() > 7
            || plan.sender.len() > 256
            || plan.receivers.iter().any(|c| c.len() > 256)
        {
            return Err("invalid persisted role bounds".into());
        }
        let mut normalized = plan.clone();
        if plan.adaptive.is_none() {
            normalized.count = compiled.count;
        }
        let verifiers = if plan.adaptive.is_none()
            && plan.flows.is_empty()
            && (plan.evidence.sample_every == 1
                || !plan.tracking.is_exact()
                || plan.queues.is_some())
        {
            plan.receivers
                .iter()
                .map(|c| Verifier::for_plan(c.clone(), plan, &compiled))
                .collect()
        } else {
            vec![]
        };
        Ok(Self {
            inside_tracking: false,
            queue_ordinals: Default::default(),
            queue_eligible: Default::default(),
            adaptive: plan
                .adaptive
                .as_ref()
                .map(|_| Box::new(adaptive::AdaptiveReplay::new(plan))),
            load: None,
            pattern_transmit: Default::default(),
            group_release: None,
            flows: if plan.flows.is_empty() {
                Default::default()
            } else {
                plan.flow_plans()
                    .into_iter()
                    .map(|(n, p)| Ok((n, Replay::new(&p)?)))
                    .collect::<Result<_, String>>()?
            },
            flow_ends: Default::default(),
            flow_stops: Default::default(),
            timing: if plan.flows.is_empty() && plan.adaptive.is_none() {
                TimingState::new(plan)
            } else {
                None
            },
            provenance: None,
            audit_tail: vec![],
            original_hash: plan.fingerprint(),
            submitted: if plan.flows.is_empty() && plan.adaptive.is_none() {
                vec![0; (compiled.count as usize).div_ceil(64)]
            } else {
                vec![]
            },
            submissions: Default::default(),
            stage_skipped: vec![0; compiled.stages.len()],
            rtt: Default::default(),
            replies: Default::default(),
            replied_seen: if plan.rtt.is_some() {
                vec![0; (compiled.count as usize).div_ceil(64)]
            } else {
                vec![]
            },
            rtt_seen: if plan.rtt.is_some() {
                vec![0; (compiled.count as usize).div_ceil(64)]
            } else {
                vec![]
            },
            plan: normalized,
            compiled,
            verifiers,
            last: None,
            revision: 0,
            last_audit: None,
            audit_records: 0,
            metric_records: 0,
        })
    }
    pub fn counters(&self, m: &Metric) -> Result<(), String> {
        if !self.flows.is_empty() || self.adaptive.is_some() {
            if m.attempted
                .checked_add(m.skipped)
                .is_none_or(|n| n > u64::from(self.compiled.count))
                || m.submitted
                    .checked_add(m.failed)
                    .and_then(|n| n.checked_add(m.unknown))
                    .is_none_or(|n| {
                        n > m.attempted || m.attempted - n > self.flows.len().max(1) as u64
                    })
                || !m.receivers.is_empty()
            {
                return Err("group counter conservation violated".into());
            }
            return Ok(());
        }

        if m.attempted > u64::from(self.compiled.count)
            || m.attempted
                .checked_add(m.skipped)
                .is_none_or(|n| n > u64::from(self.compiled.count))
        {
            return Err("attempt/skip range violated".into());
        }
        let settled = m
            .submitted
            .checked_add(m.failed)
            .and_then(|n| n.checked_add(m.unknown))
            .ok_or("counter overflow")?;
        if settled > m.attempted
            || m.attempted - settled > 1
            || m.active_stage as usize >= self.compiled.stages.len()
        {
            return Err("submission counter conservation violated".into());
        }
        let mut roles = HashSet::new();
        for r in &m.receivers {
            if !self.plan.receivers.contains(&r.channel)
                || !roles.insert(&r.channel)
                || r.unique > u64::from(self.compiled.count)
            {
                return Err("receiver range violated".into());
            }
            if r.tracking.as_ref().map(|w| w.capacity) != self.plan.tracking.capacity()
                || r.queues.is_some() != self.plan.queues.is_some()
                || r.queues.as_ref().is_some_and(|q| {
                    !q.residence.consistent()
                        || q.residence.samples.checked_add(q.invalid_timestamps) != Some(q.samples)
                        || q.invalid_timestamps != q.residence.negative
                        || q.samples > 0 && q.observed_high_water == 0
                })
                || r.tracking.as_ref().is_some_and(|w| {
                    w.base > self.compiled.count
                        || w.base % (w.capacity / 2) != 0
                        || w.finalized_through
                            != if w.finalized {
                                self.compiled.count
                            } else {
                                w.base
                            }
                        || w.unique_retired > r.unique
                        || u64::from(w.finalized_through).checked_sub(w.unique_retired)
                            != Some(w.finalized_missing_expected)
                        || w.finalized_missing_submitted > w.finalized_missing_expected
                        || w.finalized_missing_submitted > m.submitted
                })
            {
                return Err("receiver tracking or queue metric shape invalid".into());
            }
        }
        Ok(())
    }
    pub fn event(&mut self, event: &EvidenceEvent) -> Result<(), String> {
        if let EvidenceEvent::TrackingRecord { event } = event {
            if self.plan.tracking.is_exact()
                || self.inside_tracking
                || !self.flows.is_empty()
                || self.adaptive.is_some()
                || matches!(
                    event.as_ref(),
                    EvidenceEvent::TrackingRecord { .. }
                        | EvidenceEvent::FlowRecord { .. }
                        | EvidenceEvent::FlowEnd { .. }
                        | EvidenceEvent::End(_)
                )
            {
                return Err("invalid ordered tracking record".into());
            }
            self.inside_tracking = true;
            let result = self.event(event);
            self.inside_tracking = false;
            return result;
        }
        if !self.plan.tracking.is_exact()
            && self.flows.is_empty()
            && self.adaptive.is_none()
            && !self.inside_tracking
        {
            return Err("sliding tracker requires one ordered evidence stream".into());
        }
        if self.inside_tracking
            && matches!(
                event,
                EvidenceEvent::Sample { .. }
                    | EvidenceEvent::PatternFrame { .. }
                    | EvidenceEvent::QueueSample { .. }
                    | EvidenceEvent::TrackingAdvance { .. }
            )
            && [self.last.as_ref(), self.last_audit.as_ref()]
                .into_iter()
                .flatten()
                .any(|m| m.lifecycle.execution_end_ns.is_some())
        {
            return Err("receive or advance after ordered execution finalization".into());
        }
        if let Some(a) = &mut self.adaptive
            && matches!(
                event,
                EvidenceEvent::FlowRecord { .. }
                    | EvidenceEvent::FlowEnd { .. }
                    | EvidenceEvent::AdaptiveDecision(_)
            )
        {
            return a.event(event);
        }

        if (!self.flows.is_empty() || self.adaptive.is_some())
            && !matches!(
                event,
                EvidenceEvent::GroupRelease { .. }
                    | EvidenceEvent::FlowRecord { .. }
                    | EvidenceEvent::FlowEnd { .. }
                    | EvidenceEvent::FlowStop { .. }
                    | EvidenceEvent::Provenance(_)
                    | EvidenceEvent::Metric(_)
                    | EvidenceEvent::Lifecycle { .. }
            )
        {
            return Err("group event missing flow namespace".into());
        }
        match event {
            EvidenceEvent::TrackingRecord { .. } => unreachable!(),
            EvidenceEvent::TrackingAdvance {
                channel,
                next_sequence,
            } => {
                if *next_sequence != self.submissions.covered
                    || *next_sequence >= self.compiled.count
                {
                    return Err("tracking advance lacks settled transmit prefix".into());
                }
                let v = self
                    .verifiers
                    .iter_mut()
                    .find(|v| v.report.channel == *channel)
                    .ok_or("tracking channel missing")?;
                v.advance(*next_sequence, &self.submitted)?;
            }
            EvidenceEvent::QueueSample {
                channel,
                ordinal,
                received_ns,
                processed_ns,
                depth,
            } => {
                let options = self
                    .plan
                    .queues
                    .as_ref()
                    .ok_or("queue sample without declaration")?;
                if *depth == 0
                    || *processed_ns > 73 * 3600 * 1_000_000_000u64
                    || *received_ns > 73 * 3600 * 1_000_000_000u64
                    || *ordinal != self.queue_ordinals.get(channel).copied().unwrap_or(0)
                {
                    return Err("queue sample bounds or ordinal gap".into());
                }
                let v = self
                    .verifiers
                    .iter_mut()
                    .find(|v| v.report.channel == *channel)
                    .ok_or("queue channel missing")?;
                v.report
                    .queues
                    .as_mut()
                    .ok_or("queue tracker missing")?
                    .sample(*received_ns, *processed_ns, *depth);
                self.queue_ordinals.insert(
                    channel.clone(),
                    ordinal
                        .checked_add(u64::from(options.sample_every))
                        .ok_or("queue ordinal overflow")?,
                );
            }
            EvidenceEvent::LoadWindow(l) => {
                if self.load.is_some() || self.adaptive.is_some() || !self.flows.is_empty() {
                    return Err("duplicate or parent load window".into());
                }
                self.load = Some(l.clone());
            }
            EvidenceEvent::AdaptiveDecision(_) => return Err("unexpected adaptive decision".into()),

            EvidenceEvent::Diagnostic {
                channel,
                elapsed_ns,
                overflow,
                ..
            } => {
                if !self.plan.receivers.contains(channel)
                    || *elapsed_ns > 73 * 3600 * 1_000_000_000u64
                {
                    return Err("diagnostic outside receiver or time bounds".into());
                }
                if *overflow
                    && let Some(v) = self
                        .verifiers
                        .iter_mut()
                        .find(|v| v.report.channel == *channel)
                {
                    v.report.evidence_complete = false;
                }
            }
            EvidenceEvent::SchedulerSkip {
                first,
                count,
                elapsed_ns,
                ..
            } => {
                if *count == 0
                    || first
                        .checked_add(*count)
                        .is_none_or(|end| end > self.compiled.count)
                    || *elapsed_ns > 73 * 3600 * 1_000_000_000u64
                {
                    return Err("scheduler skip outside plan bounds".into());
                }
            }
            EvidenceEvent::GroupRelease {
                start_ns,
                ready_flows,
            } => {
                if self.flows.is_empty()
                    || self.group_release.is_some()
                    || *ready_flows != self.flows.len() as u32
                    || *start_ns > 73 * 3600 * 1_000_000_000
                {
                    return Err("invalid group release/ready barrier".into());
                }
                self.group_release = Some(*start_ns);
            }
            EvidenceEvent::FlowRecord { name, event } => {
                let scoped = if let EvidenceEvent::TrackingRecord { event } = event.as_ref() {
                    event.as_ref()
                } else {
                    event.as_ref()
                };
                if matches!(
                    scoped,
                    EvidenceEvent::Submissions { .. }
                        | EvidenceEvent::Reply { .. }
                        | EvidenceEvent::SchedulerSkip { .. }
                ) && self.group_release.is_none()
                {
                    return Err("flow execution evidence before group ready/release".into());
                }
                if let EvidenceEvent::Timing(TimingEvent::Settled { points, .. }) = scoped
                    && points
                        .call_started_ns
                        .is_some_and(|n| self.group_release.is_none_or(|t| n < t))
                {
                    return Err("flow submit call preceded group T0".into());
                }

                if self.flow_ends.contains_key(name) {
                    return Err("record after flow end".into());
                }
                self.flows
                    .get_mut(name)
                    .ok_or("unknown flow record namespace")?
                    .event(event)?;
            }
            EvidenceEvent::FlowEnd { name, report } => {
                if self.flow_ends.contains_key(name)
                    || report.group.is_some()
                    || !report.plan.flows.is_empty()
                {
                    return Err("duplicate or nested flow end".into());
                }
                self.flows
                    .get(name)
                    .ok_or("unknown flow end")?
                    .verify_report(report)?;
                self.flow_ends.insert(name.clone(), *report.clone());
            }
            EvidenceEvent::FlowStop { name, requested_ns } => {
                if !self.flows.contains_key(name)
                    || self.flow_stops.contains_key(name)
                    || *requested_ns > 73 * 3600 * 1_000_000_000
                {
                    return Err("invalid flow stop command".into());
                }
                self.flow_stops.insert(name.clone(), *requested_ns);
            }
            EvidenceEvent::Timing(e) => {
                self.timing
                    .as_mut()
                    .ok_or("timing events without frozen options")?
                    .apply(e)?;
            }
            EvidenceEvent::Provenance(p) => {
                if self.provenance.is_some() || !p.valid(&self.plan) {
                    return Err("invalid or repeated source evidence".into());
                }
                self.provenance = Some(p.clone());
            }
            EvidenceEvent::Submissions { ranges } => {
                if ranges.is_empty()
                    || ranges.len() > 32
                    || self
                        .plan
                        .rtt
                        .as_ref()
                        .is_some_and(|r| r.role == RttRole::Responder)
                {
                    return Err("invalid outcome range batch".into());
                }
                // Validate the whole row before advancing its valid-prefix boundary.
                let mut next = self.submissions.covered;
                let mut unknown = self.submissions.unknown;
                for range in ranges {
                    if range.count == 0
                        || range.first != next
                        || u64::from(range.first) + u64::from(range.count)
                            > u64::from(self.compiled.count)
                    {
                        return Err("outcome range gap, overlap, or bounds".into());
                    }
                    if range.outcome == SubmissionOutcome::Unknown {
                        if range.count != 1 || unknown > 0 {
                            return Err("multiple unknown executions".into());
                        }
                        unknown += 1;
                    }
                    if range.outcome != SubmissionOutcome::Skipped
                        && self
                            .verifiers
                            .iter()
                            .any(|v| v.needs_advance(range.first + range.count - 1))
                    {
                        return Err("outcomes crossed window without a prior advance".into());
                    }
                    next += range.count;
                }
                for range in ranges {
                    if let Some(t) = &mut self.timing {
                        t.expect(
                            range.first,
                            range.count,
                            false,
                            match range.outcome {
                                SubmissionOutcome::Submitted => "submitted",
                                SubmissionOutcome::Failed => "failed",
                                SubmissionOutcome::Unknown => "unknown",
                                _ => "skipped",
                            },
                        );
                    }
                    if range.outcome == SubmissionOutcome::Submitted {
                        bitmap_fill(&mut self.submitted, range.first, range.count);
                    }
                    if range.outcome == SubmissionOutcome::Skipped {
                        for (i, s) in self.compiled.stages.iter().enumerate() {
                            let a = range.first.max(s.first);
                            let b = (range.first + range.count).min(s.first + s.count);
                            if b > a {
                                self.stage_skipped[i] += u64::from(b - a);
                            }
                        }
                    }
                    self.submissions
                        .add(range.first, range.count, range.outcome);
                    self.pattern_transmit
                        .add(range.first, range.count, range.outcome);
                }
            }

            EvidenceEvent::Reply { sequence, outcome } => {
                if self
                    .plan
                    .rtt
                    .as_ref()
                    .is_none_or(|r| r.role == RttRole::Probe)
                    || *sequence >= self.compiled.count
                    || bitmap_get(&self.replied_seen, *sequence)
                    || !matches!(outcome.as_str(), "submitted" | "failed" | "unknown")
                {
                    return Err("invalid/repeated response evidence".into());
                }
                if self.plan.rtt.as_ref().unwrap().role == RttRole::Responder
                    && outcome == "unknown"
                    && self.submissions.unknown > 0
                {
                    return Err("multiple unknown responses".into());
                }
                if let Some(t) = &mut self.timing {
                    t.expect(*sequence, 1, true, outcome);
                }
                bitmap_set(&mut self.replied_seen, *sequence);
                if self.plan.rtt.as_ref().unwrap().role == RttRole::Responder {
                    let outcome = match outcome.as_str() {
                        "submitted" => SubmissionOutcome::Submitted,
                        "failed" => SubmissionOutcome::Failed,
                        _ => SubmissionOutcome::Unknown,
                    };
                    if outcome == SubmissionOutcome::Unknown && self.submissions.unknown > 0 {
                        return Err("multiple unknown responses".into());
                    }
                    if outcome == SubmissionOutcome::Submitted {
                        bitmap_set(&mut self.submitted, *sequence);
                    }
                    self.submissions.add(*sequence, 1, outcome);
                }

                match outcome.as_str() {
                    "submitted" => self.replies.responder_submitted += 1,
                    "failed" => self.replies.responder_failed += 1,
                    _ => {
                        self.replies.responder_unknown += 1;
                        self.replies.responder_unknown_sequence = Some(*sequence);
                    }
                }
            }
            EvidenceEvent::Rtt {
                sequence,
                sent_ns,
                received_ns,
            } => {
                if self
                    .plan
                    .rtt
                    .as_ref()
                    .is_none_or(|r| r.role == RttRole::Responder)
                    || *sequence >= self.compiled.count
                    || bitmap_get(&self.rtt_seen, *sequence)
                    || received_ns < sent_ns
                    || received_ns - sent_ns
                        > u64::from(self.plan.rtt.as_ref().unwrap().timeout_ms) * 1_000_000
                {
                    return Err("invalid RTT observation".into());
                }
                bitmap_set(&mut self.rtt_seen, *sequence);
                self.rtt.record(*sent_ns, *received_ns);
            }
            EvidenceEvent::End(_) => return Err("terminal reports are not stream records".into()),
            EvidenceEvent::Metric(m) => {
                self.counters(m)?;
                if self.inside_tracking
                    && m.receivers.iter().any(|r| {
                        r.tracking.as_ref().is_some_and(|w| {
                            self.verifiers
                                .iter()
                                .find(|v| v.report.channel == r.channel)
                                .is_none_or(|v| v.window.as_ref().is_none_or(|v| v.base != w.base))
                        })
                    })
                {
                    return Err("metric window differs from ordered advances".into());
                }
                if let Some(old) = &self.last {
                    if m.elapsed_ms < old.elapsed_ms
                        || m.attempted < old.attempted
                        || m.submitted < old.submitted
                        || m.failed < old.failed
                        || m.unknown < old.unknown
                        || m.skipped < old.skipped
                        || m.max_dispatch_lateness_ns < old.max_dispatch_lateness_ns
                    {
                        return Err("metric counters or time regressed".into());
                    }
                    for r in &old.receivers {
                        if let Some(next) = m.receivers.iter().find(|n| n.channel == r.channel)
                            && (next.unique < r.unique
                                || next.duplicates < r.duplicates
                                || next.reordered < r.reordered
                                || next.diagnostics < r.diagnostics
                                || next.crc_errors < r.crc_errors
                                || next.pattern_errors < r.pattern_errors
                                || match (&r.tracking, &next.tracking) {
                                    (Some(a), Some(b)) => {
                                        b.base < a.base
                                            || b.unique_retired < a.unique_retired
                                            || b.finalized_missing_expected
                                                < a.finalized_missing_expected
                                            || b.finalized_missing_submitted
                                                < a.finalized_missing_submitted
                                            || b.late_after_window < a.late_after_window
                                            || b.future_outside_window < a.future_outside_window
                                            || a.finalized && !b.finalized
                                    }
                                    (Some(_), None) => true,
                                    _ => false,
                                }
                                || match (&r.queues, &next.queues) {
                                    (Some(a), Some(b)) => {
                                        b.samples < a.samples
                                            || b.observed_high_water < a.observed_high_water
                                            || b.invalid_timestamps < a.invalid_timestamps
                                            || b.residence.max_ns < a.residence.max_ns
                                            || a.residence
                                                .bins
                                                .iter()
                                                .zip(&b.residence.bins)
                                                .any(|(a, b)| b < a)
                                    }
                                    (Some(_), None) => true,
                                    _ => false,
                                }
                                || next.out_of_range < r.out_of_range
                                || next.short_sequence_only < r.short_sequence_only
                                || (next.evidence_complete && !r.evidence_complete))
                        {
                            return Err("receiver counters regressed".into());
                        }
                    }
                }
                self.last = Some(m.clone());
                self.metric_records += 1;
            }
            EvidenceEvent::Lifecycle {
                elapsed_ns,
                state,
                attempted,
                submitted,
                skipped,
                failed,
                unknown,
                unknown_sequence,
                lifecycle,
            } => {
                if lifecycle.revision != self.revision + 1 {
                    return Err("audit revision did not advance".into());
                }
                let m = Metric {
                    max_dispatch_lateness_ns: self
                        .last
                        .as_ref()
                        .map_or(0, |m| m.max_dispatch_lateness_ns),
                    elapsed_ms: elapsed_ns / 1_000_000,
                    state: state.clone(),
                    attempted: *attempted,
                    submitted: *submitted,
                    skipped: *skipped,
                    failed: *failed,
                    unknown: *unknown,
                    unknown_sequence: *unknown_sequence,
                    lifecycle: lifecycle.clone(),
                    active_stage: self.last.as_ref().map_or(0, |m| m.active_stage),
                    target_fps: 0,
                    receivers: self
                        .last
                        .as_ref()
                        .map_or_else(Vec::new, |m| m.receivers.clone()),
                    evidence_queue_high_water: 0,
                    process_cpu_seconds: None,
                    process_peak_rss_bytes: None,
                    process_current_rss_bytes: None,
                    stability: StabilityReport::default(),
                };
                self.counters(&m)?;
                if self.last_audit.as_ref().is_some_and(|old| {
                    m.elapsed_ms < old.elapsed_ms
                        || m.attempted < old.attempted
                        || m.submitted < old.submitted
                        || m.failed < old.failed
                        || m.unknown < old.unknown
                        || m.skipped < old.skipped
                }) {
                    return Err("audit counters regressed".into());
                }
                if self.audit_tail.len() == 64 {
                    self.audit_tail.remove(0);
                }
                self.audit_tail.push(LifecycleRecord {
                    revision: lifecycle.revision,
                    elapsed_ns: *elapsed_ns,
                    state: state.clone(),
                    attempted: *attempted,
                    submitted: *submitted,
                    skipped: *skipped,
                    failed: *failed,
                    unknown: *unknown,
                    lifecycle: lifecycle.clone(),
                });
                self.revision = lifecycle.revision;
                self.audit_records += 1;
                self.last_audit = Some(m);
            }
            EvidenceEvent::PatternFrame {
                channel,
                data_hex,
                wire,
                elapsed_ns,
            } => {
                if !self.plan.traffic.is_pattern()
                    || self.plan.evidence.sample_every != 1
                    || !self.plan.receivers.contains(channel)
                    || *elapsed_ns > 73 * 3600 * 1_000_000_000
                    || data_hex.len() > 128
                    || data_hex.len() % 2 != 0
                    || !data_hex.bytes().all(|b| b.is_ascii_hexdigit())
                {
                    return Err("invalid Classic pattern raw evidence".into());
                }
                let data = (0..data_hex.len() / 2)
                    .map(|n| u8::from_str_radix(&data_hex[n * 2..n * 2 + 2], 16).unwrap())
                    .collect();
                let frame = wire.frame(data)?;
                if !self.plan.pattern_id_matches(&frame) {
                    return Err("pattern evidence ID mismatch".into());
                }
                *self.queue_eligible.entry(channel.clone()).or_default() += 1;
                let v = self
                    .verifiers
                    .iter_mut()
                    .find(|v| v.report.channel == *channel)
                    .ok_or("pattern verifier missing")?;
                if v.report
                    .classic_pattern
                    .as_ref()
                    .is_some_and(|r| r.finalized)
                {
                    return Err("pattern evidence after finalization".into());
                }
                v.event(
                    &self.plan,
                    Notice::Frame(canharbor_core::runtime::Event {
                        sequence: 0,
                        elapsed_ns: *elapsed_ns,
                        frame,
                        direction: "rx",
                        metadata: Default::default(),
                    }),
                );
            }
            EvidenceEvent::Sample {
                channel,
                sequence,
                data_hex,
                elapsed_ns,
                wire,
            } => {
                if self.plan.traffic.is_pattern()
                    || !self.plan.receivers.contains(channel)
                    || self.plan.evidence.sample_every == 0
                    || !sequence.is_multiple_of(self.plan.evidence.sample_every)
                    || data_hex.len() > 128
                    || data_hex.len() % 2 != 0
                    || !data_hex.bytes().all(|b| b.is_ascii_hexdigit())
                {
                    return Err("sample metadata invalid".into());
                }
                let data = (0..data_hex.len() / 2)
                    .map(|n| u8::from_str_radix(&data_hex[n * 2..n * 2 + 2], 16).unwrap())
                    .collect::<Vec<_>>();
                let frame = if let Some(wire) = wire {
                    wire.frame(data)?
                } else {
                    Frame::classic(
                        CanId::new(self.plan.can_id, self.plan.extended)
                            .map_err(|e| e.to_string())?,
                        data,
                    )
                    .map_err(|e| e.to_string())?
                };
                if self.plan.correlate(&frame, false) != Some(*sequence) {
                    return Err("sample correlation mismatch".into());
                }
                *self.queue_eligible.entry(channel.clone()).or_default() += 1;
                if let Some(v) = self
                    .verifiers
                    .iter_mut()
                    .find(|v| v.report.channel == *channel)
                {
                    v.event(
                        &self.plan,
                        Notice::Frame(canharbor_core::runtime::Event {
                            sequence: 0,
                            elapsed_ns: *elapsed_ns,
                            frame,
                            direction: "rx",
                            metadata: Default::default(),
                        }),
                    );
                }
            }
        }
        Ok(())
    }
    pub fn lifecycle_tail(&self) -> Vec<LifecycleRecord> {
        self.audit_tail.clone()
    }
    pub fn recovered_stages(&self) -> Vec<StageReport> {
        let mut stages = self.compiled.stages.clone();
        for (i, s) in stages.iter_mut().enumerate() {
            s.submitted = bitmap_count(&self.submitted, s.first, s.count);
            s.skipped = self.stage_skipped[i];
            s.received_unique = self
                .verifiers
                .iter()
                .map(|v| v.count_seen(s.first, s.count))
                .collect();
        }
        stages
    }
    pub fn recovered_group(&self, plan: &Plan, id: &str) -> Option<GroupReport> {
        if self.flows.is_empty() {
            return None;
        }
        let mut g = GroupReport::initial(plan).ok()?;
        g.start_ns = self.group_release;
        g.stop_requests = self.flow_stops.clone();
        if let Some(s) = &self.provenance {
            g.budget.bind_sources(s);
        }
        for (name, replay) in &self.flows {
            let mut r = if let Some(end) = self.flow_ends.get(name) {
                end.clone()
            } else {
                let p = plan.flow_plans().into_iter().find(|(n, _)| n == name)?.1;
                let mut r = Report::initial(&p, false, id.into()).ok()?;
                r.provenance = replay.provenance.clone();
                if let Some(m) = replay.recovered_metric() {
                    r.attempted = m.attempted;
                    r.submitted = m.submitted;
                    r.failed = m.failed;
                    r.unknown = m.unknown;
                    r.skipped = m.skipped;
                    r.receivers = m.receivers;
                    r.lifecycle = m.lifecycle;
                    r.elapsed_seconds = m.elapsed_ms as f64 / 1000.0;
                }
                let q = replay.submission_summary();
                r.attempted = q.attempted();
                r.submitted = q.submitted;
                r.failed = q.failed;
                r.unknown = q.unknown;
                r.skipped = q.skipped;
                r.unknown_sequence = q.unknown_sequence;
                r.submission_evidence = q;
                r.classic_pattern_transmit = replay.pattern_transmit();
                r.stages = replay.recovered_stages();
                r.timing = replay.recovered_timing();
                if p.rtt.is_some() {
                    r.rtt_report = Some(replay.recovered_rtt());
                }
                r
            };
            r.state = "interrupted".into();
            r.verdict = if r.known_violation() {
                "fail"
            } else {
                "inconclusive"
            }
            .into();
            r.measurement_complete = false;
            r.counters_complete = false;
            r.workload_achieved = false;
            r.assessment = None;
            r.evidence.complete = false;
            let mut samples = replay.sample_receivers();
            for s in &mut samples {
                s.evidence_complete = false;
            }
            r = crate::group::compact(r);
            r.recovery.status = "recovered_prefix".into();
            r.recovery.counters_are_lower_bounds = true;
            r.recovery.sample_receivers = samples;
            g.flows.push(FlowReport {
                name: name.clone(),
                report: Box::new(r),
            });
        }
        Some(g)
    }
    pub fn recovered_timing(&self) -> Option<TimingReport> {
        self.timing.as_ref().map(TimingState::snapshot)
    }
    pub fn recovered_metric(&self) -> Option<Metric> {
        let mut newest = self.last()?.clone();
        if newest.receivers.is_empty()
            && let Some(m) = &self.last
        {
            newest.receivers = m.receivers.clone();
            newest.active_stage = m.active_stage;
            newest.stability = m.stability.clone();
            newest.max_dispatch_lateness_ns = m.max_dispatch_lateness_ns;
        }
        Some(newest)
    }
    pub fn submission_summary(&self) -> SubmissionSummary {
        self.submissions.clone()
    }
    pub fn recovered_rtt(&self) -> RttReport {
        let mut r = self.replies.clone();
        r.latency = self.rtt.clone();
        r
    }
    pub fn last(&self) -> Option<&Metric> {
        match (&self.last, &self.last_audit) {
            (Some(m), Some(a)) => Some(if a.elapsed_ms > m.elapsed_ms { a } else { m }),
            (Some(m), None) => Some(m),
            (None, Some(a)) => Some(a),
            _ => None,
        }
    }
    pub fn sample_receivers(&self) -> Vec<ReceiverReport> {
        if self.plan.evidence.sample_every != 1 {
            return vec![];
        }
        self.verifiers
            .iter()
            .map(|v| {
                let mut r = if self
                    .last
                    .as_ref()
                    .is_some_and(|m| m.lifecycle.execution_end_ns.is_some())
                {
                    v.finalized_receiver(&self.plan, &self.submitted)
                } else {
                    v.report.clone()
                };
                r.missing_expected = u64::from(self.compiled.count) - r.unique;
                r
            })
            .collect()
    }
    pub fn pattern_transmit(&self) -> Option<ClassicPatternTransmit> {
        (self.flows.is_empty() && self.adaptive.is_none() && self.plan.traffic.is_pattern())
            .then(|| self.pattern_transmit.clone())
    }
    pub fn recovered_load(&self) -> Option<LoadObservation> {
        self.load.clone()
    }
    pub fn recovered_provenance(&self) -> Option<Provenance> {
        self.provenance.clone()
    }
    pub fn recovered_adaptive(&self, id: &str) -> Option<AdaptiveReport> {
        self.adaptive.as_ref().map(|a| a.recovered(id))
    }
    pub fn verify_report(&self, r: &Report) -> Result<(), String> {
        if r.plan.requires_resource_v11() && r.version < 11 {
            return Err("resource features require report v11".into());
        }
        if let Some(a) = &self.adaptive {
            if (!r.lifecycle_history.is_empty() && r.lifecycle_history != self.audit_tail)
                || r.version < 10
                || r.plan != self.plan
                || r.plan_hash != self.original_hash
                || r.provenance != self.provenance
                || self.revision != r.lifecycle.revision
                || self.last_audit.as_ref().is_none_or(|m| {
                    m.lifecycle != r.lifecycle
                        || m.state.strip_prefix("saving_").unwrap_or(&m.state) != r.state
                })
                || self.last.as_ref().is_none_or(|m| {
                    (m.attempted, m.submitted, m.skipped, m.failed, m.unknown)
                        != (r.attempted, r.submitted, r.skipped, r.failed, r.unknown)
                })
            {
                return Err("adaptive root identity/audit/metric mismatch".into());
            }
            return a.verify(r);
        }
        if r.adaptive.is_some()
            || r.load != self.load
            || r.load.as_ref().is_some_and(|l| !l.valid(r))
            || (r.plan.flows.is_empty()
                && !r.plan.shape.is_uniform()
                && (r.load.is_none() || r.version < 10))
        {
            return Err("load window or schedule evidence mismatch".into());
        }

        if !r.classic_pattern_consistent()
            || r.classic_pattern_transmit != self.pattern_transmit()
            || r.plan.has_classic_pattern() && r.version < 9
        {
            return Err("pattern version or physical submission accounting mismatch".into());
        }
        if !self.flows.is_empty() {
            return self.verify_group(r);
        }
        if r.group.is_some() || !r.plan.flows.is_empty() {
            return Err("nested or unexpected group report".into());
        }

        if r.version < 7 && (r.plan.timing.is_some() || r.plan.topology.is_some()) {
            return Err("timing and topology need report v7".into());
        }
        if r.version >= 7 {
            if r.provenance.is_none() || r.provenance != self.provenance {
                return Err("source evidence differs from persisted provenance".into());
            }
            match (&self.timing, &r.timing) {
                (Some(t), Some(report)) => {
                    t.verify_outcomes()?;
                    if !report.finalized || report != &t.snapshot() {
                        return Err(
                            "timing report differs from independently reduced records".into()
                        );
                    }
                }
                (None, None) => {}
                _ => return Err("timing report/options mismatch".into()),
            }
        }

        if r.version < 6 && !r.plan.criteria.is_default() {
            return Err("non-default criteria require report version 6".into());
        }
        if !r.lifecycle_history.is_empty() && r.lifecycle_history != self.audit_tail {
            return Err("bounded lifecycle cache differs from audited transitions".into());
        }
        if r.plan.rtt.is_some() != r.rtt_report.is_some() {
            return Err("RTT report/profile mismatch".into());
        }
        if let Some(t) = &r.rtt_report
            && (t.latency != self.rtt
                || t.responder_submitted != self.replies.responder_submitted
                || t.responder_failed != self.replies.responder_failed
                || t.responder_unknown != self.replies.responder_unknown
                || t.responder_unknown_sequence != self.replies.responder_unknown_sequence)
        {
            return Err("RTT histogram differs from persisted observations".into());
        }
        if r.plan_hash != self.original_hash
            || r.plan_hash != r.plan.fingerprint()
            || r.planned != self.compiled.count
        {
            return Err("report plan identity mismatch".into());
        }
        self.counters(&r.metric())?;
        if r.version >= 3 && (self.audit_records == 0 || self.revision != r.lifecycle.revision) {
            return Err("audit boundary differs from report".into());
        }
        if r.version >= 3
            && self.last_audit.as_ref().is_none_or(|a| {
                a.lifecycle != r.lifecycle
                    || a.state.strip_prefix("saving_").unwrap_or(&a.state) != r.state
            })
        {
            return Err("final lifecycle differs from persisted audit".into());
        }
        if r.version >= 5
            && (r.submission_evidence != self.submissions
                || r.submitted != self.submissions.submitted
                || r.failed != self.submissions.failed
                || r.unknown != self.submissions.unknown
                || r.skipped != self.submissions.skipped
                || r.attempted != self.submissions.attempted()
                || r.unknown_sequence != self.submissions.unknown_sequence)
        {
            return Err("report differs from complete submission outcomes".into());
        }
        if r.stages.len() != self.compiled.stages.len() {
            return Err("stage count differs from compiled plan".into());
        }
        for (i, (stage, expected)) in r.stages.iter().zip(&self.compiled.stages).enumerate() {
            if stage.name != expected.name
                || stage.fps != expected.fps
                || stage.warmup != expected.warmup
                || stage.first != expected.first
                || stage.count != expected.count
                || stage.start_ns != expected.start_ns
                || stage.received_unique.len() != r.receivers.len()
                || stage
                    .submitted
                    .checked_add(stage.skipped)
                    .is_none_or(|n| n > u64::from(stage.count))
            {
                return Err("stage shape differs from immutable plan".into());
            }
            if r.version >= 5
                && (bitmap_count(&self.submitted, stage.first, stage.count) != stage.submitted
                    || self.stage_skipped[i] != stage.skipped)
            {
                return Err("stage submission set differs from outcomes".into());
            }
        }
        if r.stages.len() != self.compiled.stages.len()
            || r.stages
                .iter()
                .try_fold(0u64, |n, s| n.checked_add(s.submitted))
                != Some(r.submitted)
            || r.stages
                .iter()
                .try_fold(0u64, |n, s| n.checked_add(s.skipped))
                != Some(r.skipped)
        {
            return Err("stage conservation violated".into());
        }
        if self.plan.requires_resource_v11() && r.version < 11 {
            return Err("resource features require report v11".into());
        }
        let m = self.last.as_ref().ok_or("no aggregate stream evidence")?;
        if m.attempted != r.attempted
            || m.submitted != r.submitted
            || m.failed != r.failed
            || m.unknown != r.unknown
            || m.skipped != r.skipped
            || m.receivers.len() != r.receivers.len()
        {
            return Err("final report differs from persisted aggregate".into());
        }
        for receiver in &r.receivers {
            let observed = m
                .receivers
                .iter()
                .find(|v| v.channel == receiver.channel)
                .ok_or("receiver missing from aggregate")?;
            if receiver.tracking.as_ref().map(|w| w.capacity) != self.plan.tracking.capacity()
                || receiver.queues.is_some() != self.plan.queues.is_some()
                || receiver.tracking.as_ref().is_some_and(|w| {
                    !w.finalized
                        || w.finalized_through != self.compiled.count
                        || w.base > self.compiled.count
                        || w.base % (w.capacity / 2) != 0
                        || w.unique_retired != receiver.unique
                        || w.finalized_missing_expected != receiver.missing_expected
                        || w.finalized_missing_submitted != receiver.missing_submitted
                        || self
                            .verifiers
                            .iter()
                            .find(|v| v.report.channel == receiver.channel)
                            .is_none_or(|v| v.window.as_ref().is_none_or(|v| v.base != w.base))
                })
            {
                return Err("tracking or queue report shape inconsistent with plan".into());
            }
            if receiver != observed
                || receiver.missing_expected != u64::from(r.planned) - receiver.unique
            {
                return Err("receiver report differs from persisted aggregate".into());
            }
            if let Some(v) = self
                .verifiers
                .iter()
                .find(|v| v.report.channel == receiver.channel)
            {
                // Finalize the bounded window after all raw streams, without cloning bitmaps.
                let finalized = v.finalized_receiver(&self.plan, &self.submitted);
                if self.plan.evidence.sample_every == 1
                    && let Some(q) = &self.plan.queues
                    && receiver.queues.as_ref().is_none_or(|r| {
                        r.samples
                            != self
                                .queue_eligible
                                .get(&receiver.channel)
                                .copied()
                                .unwrap_or(0)
                                .div_ceil(u64::from(q.sample_every))
                    })
                {
                    return Err("queue coverage differs from full matching RX evidence".into());
                }
                if v.report.queues != receiver.queues {
                    return Err("queue distribution differs from samples".into());
                }
                if self.plan.evidence.sample_every == 1 && finalized.tracking != receiver.tracking {
                    return Err("window history differs from ordered evidence".into());
                }
                if self.plan.evidence.sample_every == 1
                    && (finalized.unique != receiver.unique
                        || finalized.duplicates != receiver.duplicates
                        || finalized.reordered != receiver.reordered
                        || finalized.out_of_range != receiver.out_of_range
                        || finalized.crc_errors != receiver.crc_errors
                        || finalized.pattern_errors != receiver.pattern_errors
                        || finalized.short_sequence_only != receiver.short_sequence_only
                        || finalized.classic_pattern != receiver.classic_pattern)
                {
                    return Err("full matching samples disagree with receiver counters".into());
                }
                if r.version >= 5
                    && self.plan.evidence.sample_every == 1
                    && v.missing_submitted(&self.submitted) != receiver.missing_submitted
                {
                    return Err("submitted-set RX loss differs from full samples".into());
                }

                for stage in r
                    .stages
                    .iter()
                    .filter(|_| self.plan.evidence.sample_every == 1)
                {
                    let index = r
                        .receivers
                        .iter()
                        .position(|n| n.channel == receiver.channel)
                        .unwrap();
                    if stage.received_unique.get(index)
                        != Some(&v.count_seen(stage.first, stage.count))
                    {
                        return Err("stage RX disagrees with samples".into());
                    }
                }
            }
        }
        if r.version >= 6
            && (r.assessment.as_ref() != Some(&r.assess())
                || r.workload_achieved != r.assess().workload_met
                || m.max_dispatch_lateness_ns != r.max_dispatch_lateness_ns)
        {
            return Err(
                "frozen criteria assessment or dispatch lateness differs from persisted evidence"
                    .into(),
            );
        }
        if r.verdict == "pass" && !r.passing_invariants() {
            return Err("pass verdict violates evidence invariants".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn raw_diagnostics_roundtrip_and_skip_evidence_is_bounded() {
        let mut replay = Replay::new(&report().plan).unwrap();
        let diagnostic = EvidenceEvent::Diagnostic {
            channel: "virtual:1".into(),
            elapsed_ns: 10,
            classes: 0x204,
            data: [1, 2, 3, 4, 5, 6, 7, 8],
            overflow: false,
        };
        let bytes = serde_json::to_vec(&diagnostic).unwrap();
        let restored: EvidenceEvent = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(serde_json::to_vec(&restored).unwrap(), bytes);
        replay.event(&restored).unwrap();
        replay
            .event(&EvidenceEvent::SchedulerSkip {
                first: 0,
                count: 2,
                elapsed_ns: 10,
                lateness_ns: 10,
            })
            .unwrap();
        assert!(
            replay
                .event(&EvidenceEvent::SchedulerSkip {
                    first: 1,
                    count: 2,
                    elapsed_ns: 10,
                    lateness_ns: 10
                })
                .is_err()
        );
        assert!(
            replay
                .event(&EvidenceEvent::Diagnostic {
                    channel: "foreign".into(),
                    elapsed_ns: 10,
                    classes: 0,
                    data: [0; 8],
                    overflow: false
                })
                .is_err()
        );
    }
    fn report() -> Report {
        let mut p:Plan=serde_json::from_value(serde_json::json!({"sender":"virtual:0","receivers":["virtual:1"],"can_id":291,"extended":false,"run_tag":17,"count":2,"fps":10,"bitrate":500000,"receivers_active":true,"drain_ms":20})).unwrap();
        p.evidence.sample_every = 1;
        let mut r = Report::initial(&p, true, "test".into()).unwrap();
        r.version = 2; // Legacy aggregates are accepted; independent samples are still checked.
        r.state = "completed".into();
        r.verdict = "pass".into();
        r.attempted = 2;
        r.submitted = 2;
        r.workload_achieved = true;
        r.measurement_complete = true;
        r.receivers = vec![ReceiverReport {
            channel: "virtual:1".into(),
            unique: 2,
            evidence_complete: true,
            ..Default::default()
        }];
        r.stages[0].submitted = 2;
        r.stages[0].received_unique = vec![2];
        r
    }
    #[test]
    fn full_sample_recomputation_rejects_missing_or_duplicate_rows() {
        let r = report();
        let mut replay = Replay::new(&r.plan).unwrap();
        replay.event(&EvidenceEvent::Metric(r.metric())).unwrap();
        let sample = |n| EvidenceEvent::Sample {
            channel: "virtual:1".into(),
            sequence: n,
            wire: None,
            data_hex: format!("00000011{n:08x}"),
            elapsed_ns: u64::from(n),
        };
        replay.event(&sample(0)).unwrap();
        assert!(replay.verify_report(&r).unwrap_err().contains("samples"));
        replay.event(&sample(1)).unwrap();
        replay.verify_report(&r).unwrap();
        replay.event(&sample(1)).unwrap();
        assert!(replay.verify_report(&r).is_err());
    }
    #[test]
    fn audit_gaps_or_regressions_are_rejected_without_advancing_boundary() {
        let r = report();
        let mut replay = Replay::new(&r.plan).unwrap();
        let event = |revision, attempted, elapsed_ns| EvidenceEvent::Lifecycle {
            elapsed_ns,
            state: "running".into(),
            attempted,
            submitted: attempted,
            skipped: 0,
            failed: 0,
            unknown: 0,
            unknown_sequence: None,
            lifecycle: Lifecycle {
                revision,
                ..Default::default()
            },
        };
        replay.event(&event(1, 1, 100)).unwrap();
        assert!(replay.event(&event(3, 1, 200)).is_err());
        assert!(replay.event(&event(2, 0, 200)).is_err());
        assert_eq!(replay.audit_records, 1);
        replay.event(&event(2, 2, 300)).unwrap();
        assert_eq!(replay.last().unwrap().submitted, 2);
    }
    #[test]
    fn default_stability_does_not_change_legacy_plan_serialization() {
        let r = report();
        let value = serde_json::to_value(&r.plan).unwrap();
        assert!(value.get("stability").is_none());
        let decoded: Plan = serde_json::from_value(value).unwrap();
        assert_eq!(decoded.fingerprint(), r.plan_hash);
    }
    #[test]
    fn published_virtual_soak_plan_is_admissible_without_hardware() {
        let plan: Plan = serde_json::from_str(include_str!(
            "../../../examples/stress/soak-24h-virtual.json"
        ))
        .unwrap();
        let compiled = plan.compile().unwrap();
        assert_eq!(compiled.count, 86400);
        assert_eq!(compiled.duration_ns, 86400 * 1_000_000_000);
        assert_eq!(plan.evidence.sample_every, 1);
        assert!(plan.sender.starts_with("virtual:"));
        assert!(plan.receivers.iter().all(|r| r.starts_with("virtual:")));
    }
}

#[cfg(test)]
mod audit_tests {
    use super::*;
    fn plan() -> Plan {
        serde_json::from_value(serde_json::json!({"sender":"virtual:0","receivers":["virtual:1"],"can_id":291,"extended":false,"run_tag":17,"count":4,"fps":10,"bitrate":500000,"receivers_active":true,"drain_ms":20})).unwrap()
    }
    #[test]
    fn rejected_range_batch_never_advances_valid_prefix_state() {
        let p = plan();
        let mut replay = Replay::new(&p).unwrap();
        let batch = |ranges| EvidenceEvent::Submissions { ranges };
        let range = |first, count| SubmissionRange {
            first,
            count,
            outcome: SubmissionOutcome::Submitted,
        };
        assert!(
            replay
                .event(&batch(vec![range(0, 1), range(2, 1)]))
                .is_err()
        );
        assert_eq!(replay.submission_summary().covered, 0);
        replay.event(&batch(vec![range(0, 2)])).unwrap();
        assert!(replay.event(&batch(vec![range(1, 2)])).is_err());
        assert_eq!(replay.submission_summary().submitted, 2);
    }
    #[tokio::test(start_paused = true)]
    async fn submitted_set_loss_is_recomputed_and_stage_metadata_cannot_escape_plan_bounds() {
        use canharbor_virtual::VirtualBus;
        let mut p = plan();
        p.evidence.sample_every = 1;
        let (sink, mut rx) = EvidenceSink::channel(256);
        let run = Executor::new(Runtime::new(Arc::new(VirtualBus::new(8192))))
            .start_recorded(
                p.clone(),
                true,
                uuid::Uuid::new_v4().to_string(),
                Some(sink),
                None,
            )
            .unwrap();
        let mut report = loop {
            let r = run.snapshot();
            if r.state.starts_with("saving_") {
                break r;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        };
        report.state = report.state.trim_start_matches("saving_").into();
        let mut replay = Replay::new(&p).unwrap();
        while let Ok(e) = rx.try_recv() {
            replay.event(&e).unwrap();
        }
        replay.verify_report(&report).unwrap();
        report.stages[0].first = u32::MAX;
        assert!(replay.verify_report(&report).unwrap_err().contains("stage"));
        report.stages[0].first = 0;
        report.receivers[0].missing_submitted = 1;
        assert!(replay.verify_report(&report).is_err());
    }
}

#[cfg(test)]
mod exact_set_tests {
    use super::*;
    #[test]
    fn equal_aggregate_counts_cannot_hide_different_submitted_and_received_sets() {
        let mut p:Plan=serde_json::from_value(serde_json::json!({"sender":"virtual:0","receivers":["virtual:1"],"can_id":291,"extended":false,"run_tag":17,"count":4,"fps":10,"bitrate":500000,"receivers_active":true,"drain_ms":20})).unwrap();
        p.evidence.sample_every = 1;
        let mut r = Report::initial(&p, true, uuid::Uuid::new_v4().to_string()).unwrap();
        r.version = 6;
        r.state = "completed".into();
        r.verdict = "fail".into();
        r.attempted = 2;
        r.submitted = 2;
        r.skipped = 2;
        r.measurement_complete = true;
        r.stages[0].submitted = 2;
        r.stages[0].skipped = 2;
        r.stages[0].received_unique = vec![2];
        r.lifecycle.revision = 1;
        r.submission_evidence = SubmissionSummary {
            covered: 4,
            submitted: 2,
            skipped: 2,
            ..Default::default()
        };
        r.receivers = vec![ReceiverReport {
            channel: "virtual:1".into(),
            unique: 2,
            missing_expected: 2,
            missing_submitted: 1,
            evidence_complete: true,
            ..Default::default()
        }];
        let mut events = vec![EvidenceEvent::Submissions {
            ranges: (0..4)
                .map(|n| SubmissionRange {
                    first: n,
                    count: 1,
                    outcome: if n % 2 == 0 {
                        SubmissionOutcome::Submitted
                    } else {
                        SubmissionOutcome::Skipped
                    },
                })
                .collect(),
        }];
        for n in [1, 2] {
            events.push(EvidenceEvent::Sample {
                channel: "virtual:1".into(),
                sequence: n,
                data_hex: format!("00000011{n:08x}"),
                wire: None,
                elapsed_ns: u64::from(n),
            });
        }
        events.push(EvidenceEvent::Lifecycle {
            elapsed_ns: 10,
            state: "saving_completed".into(),
            attempted: 2,
            submitted: 2,
            skipped: 2,
            failed: 0,
            unknown: 0,
            unknown_sequence: None,
            lifecycle: r.lifecycle.clone(),
        });
        for wrong in [false, true] {
            let mut candidate = r.clone();
            if wrong {
                candidate.receivers[0].missing_submitted = 0;
            }
            candidate.assessment = Some(candidate.assess());
            let mut replay = Replay::new(&p).unwrap();
            for e in &events {
                replay.event(e).unwrap();
            }
            replay
                .event(&EvidenceEvent::Metric(candidate.metric()))
                .unwrap();
            if wrong {
                assert!(
                    replay
                        .verify_report(&candidate)
                        .unwrap_err()
                        .contains("submitted-set")
                );
            } else {
                replay.verify_report(&candidate).unwrap();
            }
        }
    }
}

impl Replay {
    fn verify_group(&self, r: &Report) -> Result<(), String> {
        if !r.lifecycle_history.is_empty() && r.lifecycle_history != self.audit_tail {
            return Err("group lifecycle cache differs from audit".into());
        }
        if r.version
            < if r.plan.requires_resource_v11() {
                11
            } else if r.plan.requires_load_v10() {
                10
            } else {
                8
            }
            || r.plan_hash != self.original_hash
            || r.plan_hash != r.plan.fingerprint()
            || r.planned != self.compiled.count
            || r.provenance != self.provenance
        {
            return Err("group identity/source mismatch".into());
        }
        let g = r.group.as_ref().ok_or("group report missing")?;
        let mut budget = r.plan.group_budget()?;
        budget.bind_sources(r.provenance.as_ref().ok_or("group source missing")?);
        if g.stop_requests != self.flow_stops
            || g.start_ns != self.group_release
            || budget != g.budget
            || g.flows.len() != self.flows.len()
            || self.flow_ends.len() != self.flows.len()
        {
            return Err("group budget or terminal flow evidence incomplete".into());
        }
        let mut names = std::collections::BTreeSet::new();
        for f in &g.flows {
            if f.report.id != r.id
                || !names.insert(&f.name)
                || self.flow_ends.get(&f.name).is_none_or(|e| {
                    serde_json::to_value(e).ok() != serde_json::to_value(&f.report).ok()
                })
            {
                return Err("group snapshot differs from independently verified flow end".into());
            }
        }
        let c = crate::group::totals(g.flows.iter().map(|f| f.report.as_ref()));
        if (r.attempted, r.submitted, r.skipped, r.failed, r.unknown)
            != (c.attempted, c.submitted, c.skipped, c.failed, c.unknown)
        {
            return Err("group totals differ from per-flow evidence".into());
        }
        let m = self.last.as_ref().ok_or("group metric absent")?;
        self.counters(m)?;
        if (m.attempted, m.submitted, m.skipped, m.failed, m.unknown)
            != (r.attempted, r.submitted, r.skipped, r.failed, r.unknown)
            || self.last_audit.as_ref().is_none_or(|a| {
                a.lifecycle != r.lifecycle
                    || a.state.strip_prefix("saving_").unwrap_or(&a.state) != r.state
            })
            || self.revision != r.lifecycle.revision
        {
            return Err("group final metric/audit mismatch".into());
        }
        if r.verdict == "pass" && !r.group_passing_invariants() {
            return Err("group pass violates per-flow invariants".into());
        }
        Ok(())
    }
}
