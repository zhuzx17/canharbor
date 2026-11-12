// SPDX-License-Identifier: AGPL-3.0-or-later
//! A single reserved Runtime, sequential exploration and a frozen selected measurement.
use super::*;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "StressAdaptiveOptions")]
pub struct AdaptiveOptions {
    pub rates: Vec<u32>,
    pub exploration_seconds: u32,
    pub measurement_seconds: u32,
    pub min_observed_ppm: u32,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[schemars(rename = "StressAdaptiveDecision")]
pub struct AdaptiveDecision {
    pub name: String,
    pub input_fps: u32,
    pub qualified: bool,
    pub criteria: Assessment,
    pub actual_load: Option<LoadObservation>,
    pub selected_fps: Option<u32>,
    pub next: Option<String>,
    pub reason: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[schemars(rename = "StressAdaptiveStep")]
pub struct AdaptiveStep {
    pub name: String,
    pub report: Box<Report>,
    pub decision: Option<AdaptiveDecision>,
}
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[schemars(rename = "StressAdaptiveReport")]
pub struct AdaptiveReport {
    pub reserved_planned: u32,
    pub reserved_peak_fps: u32,
    pub phase: String,
    pub selected_fps: Option<u32>,
    pub steps: Vec<AdaptiveStep>,
    pub scope: String,
}
impl AdaptiveReport {
    pub fn initial(p: &Plan) -> Result<Self, String> {
        let c = p.compile_adaptive()?;
        Ok(Self{reserved_planned:c.count,reserved_peak_fps:c.max_fps,phase:"exploration".into(),selected_fps:None,steps:vec![],scope:"host load exploration; pass refers only to selected fixed measurement; not measured bus occupancy".into()})
    }
}
impl Plan {
    pub fn adaptive_step_plan(&self, name: &str, selected: Option<u32>) -> Result<Plan, String> {
        let a = self.adaptive.as_ref().ok_or("adaptive policy missing")?;
        let (index, fps, seconds) = if name == "measurement" {
            (
                a.rates.len(),
                selected.ok_or("measurement has no qualified selected rate")?,
                a.measurement_seconds,
            )
        } else {
            let index = name
                .strip_prefix("explore-")
                .and_then(|s| s.parse::<usize>().ok())
                .ok_or("invalid adaptive step")?;
            (
                index,
                *a.rates.get(index).ok_or("adaptive index out of range")?,
                a.exploration_seconds,
            )
        };
        let mut p = self.clone();
        p.adaptive = None;
        p.flows.clear();
        p.stages.clear();
        p.topology = None;
        p.coordination = None;
        p.stability = Default::default();
        p.run_tag = self.run_tag.wrapping_add(index as u32 + 1);
        if let Traffic::ClassicPatternV1 { seed, .. } = &mut p.traffic {
            *seed ^= index as u32 + 1;
        }
        p.fps = fps;
        p.count = fps.checked_mul(seconds).ok_or("adaptive count overflow")?;
        Ok(p)
    }
    pub fn identity_plans(&self) -> Vec<Plan> {
        if let Some(a) = &self.adaptive {
            (0..=a.rates.len())
                .filter_map(|i| {
                    self.adaptive_step_plan(
                        &if i == a.rates.len() {
                            "measurement".into()
                        } else {
                            format!("explore-{i}")
                        },
                        a.rates.last().copied(),
                    )
                    .ok()
                })
                .collect()
        } else {
            self.flow_plans().into_iter().map(|(_, p)| p).collect()
        }
    }
    pub fn compile_adaptive(&self) -> Result<Compiled, String> {
        let a = self.adaptive.as_ref().ok_or("adaptive policy missing")?;
        if !self.flows.is_empty()
            || self.count != 0
            || self.fps != 0
            || !self.stages.is_empty()
            || self.rtt.is_some()
            || a.rates.is_empty()
            || a.rates.len() > 8
            || a.rates.windows(2).any(|w| w[0] >= w[1])
            || !(1..=1_000_000).contains(&a.min_observed_ppm)
            || a.exploration_seconds == 0
            || a.measurement_seconds == 0
        {
            return Err("adaptive needs 1..8 increasing rates, nonzero windows, observed ppm 1..1000000, count=fps=0, no stages/flows/RTT".into());
        }
        if self.evidence.segment_bytes < 256 * 1024 {
            return Err("adaptive segments must be at least 256 KiB".into());
        }
        if let Some(c) = &self.coordination {
            c.validate()?;
        }
        if let Some(t) = &self.topology {
            t.validate(self)?;
        }
        let mut count = 0u32;
        let mut duration = 0u64;
        let mut tracking = 0;
        let mut peak = 0;
        let mut bytes = 4 * 1024 * 1024;
        for i in 0..=a.rates.len() {
            let name = if i == a.rates.len() {
                "measurement".into()
            } else {
                format!("explore-{i}")
            };
            let c = self
                .adaptive_step_plan(&name, a.rates.last().copied())?
                .compile()?;
            count = count
                .checked_add(c.count)
                .ok_or("adaptive sequence budget overflow")?;
            duration = duration
                .checked_add(c.duration_ns + u64::from(self.drain_ms) * 1_000_000 + 20_000_000)
                .ok_or("adaptive duration overflow")?;
            tracking = tracking.max(c.tracking_bytes);
            peak = peak.max(c.max_fps);
            bytes += c.evidence_bytes - 4 * 1024 * 1024 + 256 * 1024;
        }
        if duration > 72 * 3600 * 1_000_000_000
            || bytes > self.evidence.max_bytes
            || (bytes - 4 * 1024 * 1024).div_ceil(self.evidence.segment_bytes) + 5 > 1000
        {
            return Err("adaptive aggregate duration/evidence budget exceeded".into());
        }
        if self.stability.max_rss_growth_bytes.is_some_and(|n| n == 0)
            || self.stability.max_rss_growth_bytes.is_some()
                && u64::from(self.stability.warmup_seconds) * 1_000_000_000
                    >= (u64::from(a.exploration_seconds) + u64::from(a.measurement_seconds))
                        * 1_000_000_000
        {
            return Err("invalid adaptive RSS limit/warmup".into());
        }
        Ok(Compiled {
            shape: LoadShape::default(),
            evidence_bytes: bytes,
            count,
            duration_ns: duration,
            tracking_bytes: tracking,
            stages: vec![],
            max_fps: peak,
        })
    }
    pub fn validate_adaptive(&self, runtime: &Runtime) -> Result<(), String> {
        self.compile_adaptive()?;
        let a = self.adaptive.as_ref().unwrap();
        let mut bytes = 64 * 1024;
        for i in 0..=a.rates.len() {
            let name = if i == a.rates.len() {
                "measurement".into()
            } else {
                format!("explore-{i}")
            };
            let p = self.adaptive_step_plan(&name, a.rates.last().copied())?;
            p.validate(runtime)?;
            let mut r = Report::initial(&p, true, "budget".into())?;
            r.capture_sources(runtime);
            bytes += serde_json::to_vec(&r.provenance)
                .map_err(|e| e.to_string())?
                .len()
                + 64 * 1024;
        }
        if bytes > 768 * 1024 {
            return Err("adaptive report/source metadata budget exceeded".into());
        }
        if self.stability.max_rss_growth_bytes.is_some() && current_rss().is_none() {
            return Err("required RSS monitor unavailable".into());
        }
        Ok(())
    }
}
pub(crate) fn decision(p: &Plan, name: &str, r: &Report, prior: Option<u32>) -> AdaptiveDecision {
    let a = p.adaptive.as_ref().unwrap();
    let measurement = name == "measurement";
    let assessment = r.assess();
    let healthy = r.state == "completed"
        && r.measurement_complete
        && r.counters_complete
        && r.failed == 0
        && r.unknown == 0
        && r.errors.is_empty()
        && r.lifecycle.stop_requested_ns.is_none()
        && r.load.as_ref().is_some_and(|l| l.completed && l.valid(r));
    let rate = r.load.as_ref().is_some_and(|l| {
        r.submitted <= u64::from(r.planned)
            && l.end_ns > l.start_ns
            && u128::from(r.submitted) * u128::from(l.scheduled_duration_ns) * 1_000_000
                >= u128::from(r.planned)
                    * u128::from(l.end_ns - l.start_ns)
                    * u128::from(a.min_observed_ppm)
    });
    let qualified = healthy && r.verdict == "pass" && r.passing_invariants() && rate;
    let selected = if measurement {
        prior
    } else if qualified {
        Some(r.plan.fps)
    } else {
        prior
    };
    let index = name
        .strip_prefix("explore-")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(a.rates.len());
    let next = if !healthy || measurement {
        None
    } else if qualified && index + 1 < a.rates.len() {
        Some(format!("explore-{}", index + 1))
    } else {
        selected.map(|_| "measurement".into())
    };
    let reason = if !healthy {
        "execution_or_evidence_fault"
    } else if !r.passing_invariants() {
        "frozen_quality_or_workload_not_met"
    } else if !rate {
        "observed_host_rate_below_declared_floor"
    } else if measurement {
        "fixed_measurement_passed"
    } else {
        "candidate_accepted"
    };
    AdaptiveDecision {
        name: name.into(),
        input_fps: r.plan.fps,
        qualified,
        criteria: assessment,
        actual_load: r.load.clone(),
        selected_fps: selected,
        next,
        reason: reason.into(),
    }
}
fn totals(r: &mut Report) {
    let Some(a) = &r.adaptive else {
        return;
    };
    let c = group::totals(a.steps.iter().map(|s| s.report.as_ref()));
    r.planned = a.steps.iter().map(|s| s.report.planned).sum();
    r.attempted = c.attempted;
    r.submitted = c.submitted;
    r.skipped = c.skipped;
    r.failed = c.failed;
    r.unknown = c.unknown;
    r.rx_lag_events = a.steps.iter().map(|s| s.report.rx_lag_events).sum();
    r.max_dispatch_lateness_ns = a
        .steps
        .iter()
        .map(|s| s.report.max_dispatch_lateness_ns)
        .max()
        .unwrap_or(0);
}
fn snapshot_step(parent: &Run, name: &str, child: &Run) {
    let mut r = parent.report.lock().unwrap();
    if let Some(s) = r
        .adaptive
        .as_mut()
        .unwrap()
        .steps
        .iter_mut()
        .find(|s| s.name == name)
    {
        *s.report = group::compact(child.snapshot());
    }
    totals(&mut r);
}
pub(crate) async fn execute_adaptive(
    runtime: Arc<Runtime>,
    p: Plan,
    run: Arc<Run>,
    mut cancel: watch::Receiver<bool>,
    sink: Option<EvidenceSink>,
    context: ExecutionContext,
) {
    let owner = format!("stress:{}", run.id());
    let channels = p.channels();
    let reservation = runtime.reserve_devices(&channels).await.map(Arc::new);
    let mut sessions = BTreeMap::new();
    let before = channels
        .iter()
        .map(|c| (c.clone(), runtime.cleanup_observation(c)))
        .collect::<BTreeMap<_, _>>();
    if let (Some(s), Some(source)) = (&sink, run.snapshot().provenance) {
        s.emit(EvidenceEvent::Provenance(source));
    }
    run.transition("arming");
    let result: Result<(), String> = async {
        let reservation = reservation.as_ref().map_err(|e| e.to_string())?;
        run.transition("reserved");

        for channel in &channels {
            if *cancel.borrow() || run.gate.is_revoked() {
                return Err("adaptive cancelled before configuration".into());
            }
            let v = runtime
                .create_reserved(
                    channel,
                    Config {
                        nominal_bitrate: p.bitrate,
                        data_bitrate: p.traffic.data_bitrate(),
                        listen_only: channel != &p.sender && !p.receivers_active,
                    },
                    Some(reservation),
                )
                .await
                .map_err(|e| e.to_string())?;
            sessions.insert(channel.clone(), (v.id.clone(), 0, 0));
            let a = runtime
                .acquire(&v.id, &owner)
                .await
                .map_err(|e| e.to_string())?;
            sessions.get_mut(channel).unwrap().1 = a.generation;
        }

        for (id, g, epoch) in sessions.values_mut() {
            if *cancel.borrow() || run.gate.is_revoked() {
                return Err("adaptive cancelled before arming".into());
            }
            runtime
                .running(id, &owner, *g, true)
                .await
                .map_err(|e| e.to_string())?;
            *epoch = runtime
                .observe(id)
                .await
                .map_err(|e| e.to_string())?
                .epoch();
        }

        let mut name = "explore-0".to_string();
        let mut selected = None;
        let mut first = true;

        loop {
            let plan = p.adaptive_step_plan(&name, selected)?;
            let compiled = plan.compile()?;
            let mut child = group::make_child(
                &run,
                &plan,
                &runtime,
                sink.as_ref().map(|s| s.for_flow(&name)),
            )?;

            let timeout = child.report.lock().unwrap().execution_timeout_ns;
            Arc::get_mut(&mut child).unwrap().execution_deadline = run
                .execution_deadline
                .min(Instant::now() + Duration::from_nanos(timeout));

            run.children
                .lock()
                .unwrap()
                .insert(name.clone(), child.clone());

            {
                let mut r = run.report.lock().unwrap();
                let a = r.adaptive.as_mut().unwrap();
                a.phase = if name == "measurement" {
                    "measurement"
                } else {
                    "exploration"
                }
                .into();
                a.steps.push(AdaptiveStep {
                    name: name.clone(),
                    report: Box::new(group::compact(child.snapshot())),
                    decision: None,
                });
                totals(&mut r);
            }

            let shared = Arc::new(group::SharedExecution {
                owner: owner.clone(),
                reservation: reservation.clone(),
                sessions: sessions.clone(),
                ready: AtomicUsize::new(0),
                release: watch::channel(None).0,
            });

            let rt = runtime.clone();
            let c = child.clone();
            let rx = c.cancel.subscribe();
            let evidence = sink.as_ref().map(|s| s.for_flow(&name));
            let ctx = ExecutionContext {
                authority: context.authority.clone(),
                slot: context.slot.clone(),
                shared: Some(shared.clone()),
            };

            let watched = child.clone();
            let mut task = tokio::spawn(async move {
                if std::panic::AssertUnwindSafe(execute_flow(
                    rt, plan, compiled, c, rx, evidence, ctx,
                ))
                .catch_unwind()
                .await
                .is_err()
                {
                    watched.close_gate();
                    watched.report.clear_poison();
                    let mut report = watched.report.lock().unwrap();
                    report.state = "failed".into();
                    report.measurement_complete = false;
                    report.counters_complete = false;
                    report
                        .errors
                        .push("adaptive step executor panicked; no retry".into());
                }
            });
            let mut released = false;
            let mut renew = Instant::now();
            let mut metric = Instant::now();
            let mut aborted = None;

            loop {
                snapshot_step(&run, &name, &child);

                if *cancel.borrow()
                    || run.gate.is_revoked()
                    || run.execution_expired()
                    || run.coordination_expired()
                    || context
                        .authority
                        .as_ref()
                        .is_some_and(|a| a.1.load(Ordering::Acquire))
                    || sink.as_ref().is_some_and(EvidenceSink::failed)
                {
                    if !*cancel.borrow() {
                        run.terminate_if_unset(
                            "adaptive_execution_fault",
                            "authority, deadline or evidence failure",
                            None,
                            None,
                        );
                    }
                    run.stop();
                    aborted =
                        Some("adaptive authority, stop, deadline or evidence failure".to_string());
                }

                if aborted.is_none() && !released && shared.ready.load(Ordering::Acquire) == 1 {
                    let delay = if first {
                        p.coordination
                            .as_ref()
                            .map_or(20, |c| u64::from(c.start_delay_ms))
                    } else {
                        20
                    };
                    shared
                        .release
                        .send_replace(Some(Instant::now() + Duration::from_millis(delay)));
                    released = true;
                    run.transition("running");
                }

                if Instant::now() >= renew && aborted.is_none() {
                    for (id, g, _) in sessions.values() {
                        if let Err(e) = runtime.renew(id, &owner, *g).await {
                            run.terminate_if_unset("lease_lost", &e.to_string(), None, None);
                            run.stop();
                            aborted = Some(e.to_string());
                            break;
                        }
                    }
                    renew = Instant::now() + Duration::from_secs(1);
                }

                if Instant::now() >= metric {
                    publish(&run, &[], &sink, run.started.elapsed());
                    let stability = run.snapshot().stability;
                    if stability.limit_exceeded || stability.status == "unsupported" {
                        run.terminate_if_unset(
                            "memory_stability",
                            "adaptive RSS criterion failed",
                            None,
                            None,
                        );
                        run.stop();
                        aborted = Some("adaptive RSS criterion failed".into());
                    }
                    metric = Instant::now() + Duration::from_secs(1);
                }

                tokio::select! {
                biased;
                result=&mut task=>{
                if result.is_err(){
                run.stop();
                return Err("adaptive step task failed; no retry".into());
                }
                break;
                }
                ,_=cancel.changed()=>{
                }
                ,_=tokio::time::sleep(Duration::from_millis(10))=>{
                }
                }
            }

            let mut report = group::compact(child.snapshot());
            if let Some(state) = report.state.strip_prefix("saving_") {
                report.state = state.into();
            }
            *child.report.lock().unwrap() = report.clone();

            let d = decision(&p, &name, &report, selected);

            if let Some(s) = &sink {
                s.emit(EvidenceEvent::FlowEnd {
                    name: name.clone(),
                    report: Box::new(report.clone()),
                });
                s.emit(EvidenceEvent::AdaptiveDecision(d.clone()));
            }

            {
                let mut r = run.report.lock().unwrap();
                let a = r.adaptive.as_mut().unwrap();
                let step = a.steps.last_mut().unwrap();
                *step.report = report;
                step.decision = Some(d.clone());
                a.selected_fps = d.selected_fps;
                totals(&mut r);
            }

            if let Some(error) = aborted {
                return Err(error);
            }
            selected = d.selected_fps;

            if let Some(next) = d.next {
                name = next;
                first = false;
            } else {
                if name != "measurement" {
                    return Err(format!("adaptive exploration ended: {}", d.reason));
                }
                break;
            }
        }
        Ok(())
    }
    .await;

    run.close_gate();
    if result.is_err() {
        for c in run.children.lock().unwrap().values() {
            c.stop();
        }
    }
    run.transition("finalizing");
    for (channel, (id, g, epoch)) in sessions.into_iter().rev() {
        let closing = runtime.close_at_epoch(&id, &owner, g, epoch);
        tokio::pin!(closing);
        let closed = match tokio::time::timeout(Duration::from_secs(2), &mut closing).await {
            Ok(r) => r,
            Err(_) => {
                run.transition("stopping_blocked");
                closing.await
            }
        };
        let observation = match (
            before.get(&channel).and_then(Option::as_ref),
            runtime.cleanup_observation(&channel),
        ) {
            (Some(a), Some(b)) if closed.is_ok() => CleanupObservation {
                channel,
                backend_unconfirmed: Some(
                    b.unknown_transmissions
                        .saturating_sub(a.unknown_transmissions),
                ),
                failures: b
                    .failures
                    .into_iter()
                    .skip(a.failures.len())
                    .take(32)
                    .collect(),
                scope: "closed_original_adaptive_epoch".into(),
            },
            _ => CleanupObservation {
                channel,
                scope: "unsupported_or_cleanup_incomplete".into(),
                ..Default::default()
            },
        };
        run.report.lock().unwrap().backend_cleanup.push(observation);
        if let Err(e) = closed {
            run.report
                .lock()
                .unwrap()
                .errors
                .push(format!("adaptive cleanup: {e}"));
        }
    }
    let ending = {
        let mut r = run.report.lock().unwrap();
        if let Err(e) = result {
            r.errors.push(e);
        }
        totals(&mut r);
        let a = r.adaptive.as_ref().unwrap();
        let measured = a.steps.last().filter(|s| s.name == "measurement");
        let passing = measured.is_some_and(|s| s.decision.as_ref().is_some_and(|d| d.qualified));
        let workload = measured.is_some_and(|s| s.report.workload_achieved);
        let measurement_complete = measured.is_some_and(|s| s.report.measurement_complete);
        r.workload_achieved = workload;
        r.measurement_complete = measurement_complete && r.errors.is_empty();
        r.counters_complete = a_steps_complete(&r);
        r.elapsed_seconds = run.started.elapsed().as_secs_f64();
        r.lifecycle.execution_end_ns = Some(run.elapsed_ns());
        let ending = if *cancel.borrow() {
            "aborted"
        } else if !r.errors.is_empty() {
            "failed"
        } else {
            "completed"
        };
        r.verdict = if passing && r.measurement_complete && ending == "completed" {
            "pass"
        } else if ending == "aborted" {
            "inconclusive"
        } else {
            "fail"
        }
        .into();
        r.adaptive.as_mut().unwrap().phase = if ending == "completed" {
            "complete"
        } else {
            "stopped"
        }
        .into();
        ending.to_string()
    };
    run.transition(&if sink.is_some() {
        format!("saving_{ending}")
    } else {
        ending
    });
    publish(&run, &[], &sink, run.started.elapsed());
}
fn a_steps_complete(r: &Report) -> bool {
    r.adaptive.as_ref().is_some_and(|a| {
        !a.steps.is_empty()
            && a.steps
                .iter()
                .all(|s| s.decision.is_some() && s.report.counters_complete)
    })
}
impl Report {
    pub fn adaptive_passing_invariants(&self) -> bool {
        let Some(a) = &self.adaptive else {
            return false;
        };
        let Ok(c) = self.plan.compile_adaptive() else {
            return false;
        };
        let mut selected = None;
        let mut next = Some("explore-0".to_string());
        for s in &a.steps {
            if next.as_deref() != Some(s.name.as_str())
                || self
                    .plan
                    .adaptive_step_plan(&s.name, selected)
                    .ok()
                    .as_ref()
                    != Some(&s.report.plan)
                || s.report.id != self.id
                || s.report.adaptive.is_some()
                || s.report.group.is_some()
            {
                return false;
            }
            let Ok(compiled) = s.report.plan.compile() else {
                return false;
            };
            if s.report.plan_hash != s.report.plan.fingerprint()
                || s.report.planned != compiled.count
                || s.report.submitted > u64::from(compiled.count)
                || s.report
                    .attempted
                    .checked_add(s.report.skipped)
                    .is_none_or(|n| n > u64::from(compiled.count))
                || s.report
                    .submitted
                    .checked_add(s.report.failed)
                    .and_then(|n| n.checked_add(s.report.unknown))
                    != Some(s.report.attempted)
            {
                return false;
            }
            let d = decision(&self.plan, &s.name, &s.report, selected);
            if s.decision.as_ref() != Some(&d) {
                return false;
            }
            selected = d.selected_fps;
            next = d.next;
        }
        let sum = group::totals(a.steps.iter().map(|s| s.report.as_ref()));
        self.version
            >= if self.plan.requires_resource_v11() {
                11
            } else {
                10
            }
            && self.group.is_none()
            && self.load.is_none()
            && self.receivers.is_empty()
            && self.stages.is_empty()
            && self.timing.is_none()
            && self.rtt_report.is_none()
            && self.classic_pattern_transmit.is_none()
            && a.reserved_planned == c.count
            && a.reserved_peak_fps == c.max_fps
            && a.phase == "complete"
            && a.selected_fps == selected
            && next.is_none()
            && a.steps.last().is_some_and(|s| {
                s.name == "measurement" && s.decision.as_ref().is_some_and(|d| d.qualified)
            })
            && self.lifecycle.execution_end_ns.is_some_and(|end| {
                a.steps.iter().all(|s| {
                    s.report
                        .lifecycle
                        .execution_end_ns
                        .is_some_and(|n| n <= end)
                })
            })
            && self.state == "completed"
            && self.errors.is_empty()
            && self.measurement_complete
            && self.counters_complete
            && self.workload_achieved
            && self.lifecycle.stop_requested_ns.is_none()
            && !self.stability.limit_exceeded
            && (self.plan.stability.max_rss_growth_bytes.is_none()
                || self.stability.status == "monitoring")
            && self
                .provenance
                .as_ref()
                .is_some_and(|p| p.valid(&self.plan))
            && self.planned == a.steps.iter().map(|s| s.report.planned).sum::<u32>()
            && self.attempted == sum.attempted
            && self.submitted == sum.submitted
            && self.skipped == sum.skipped
            && self.failed == sum.failed
            && self.unknown == sum.unknown
            && self.failed == 0
            && self.unknown == 0
    }
}

pub(crate) struct AdaptiveReplay {
    plan: Plan,
    pub steps: Vec<AdaptiveStep>,
    selected: Option<u32>,
    next: Option<String>,
    active: Option<(String, Box<Replay>)>,
}
impl AdaptiveReplay {
    pub fn new(p: &Plan) -> Self {
        Self {
            plan: p.clone(),
            steps: vec![],
            selected: None,
            next: Some("explore-0".into()),
            active: None,
        }
    }
    pub fn event(&mut self, event: &EvidenceEvent) -> Result<(), String> {
        match event {
            EvidenceEvent::FlowRecord { name, event } => {
                if self.steps.last().is_some_and(|s| s.decision.is_none())
                    || self.next.as_deref() != Some(name)
                {
                    return Err("adaptive step started without a verified selection".into());
                }
                if self.active.is_none() {
                    self.active = Some((
                        name.clone(),
                        Box::new(Replay::new(
                            &self.plan.adaptive_step_plan(name, self.selected)?,
                        )?),
                    ));
                }
                let (active, replay) = self.active.as_mut().unwrap();
                if active != name {
                    return Err("adaptive active step changed".into());
                }
                replay.event(event)?;
            }
            EvidenceEvent::FlowEnd { name, report } => {
                let (active, replay) = self
                    .active
                    .as_ref()
                    .ok_or("adaptive end without step records")?;
                if active != name
                    || report.plan != self.plan.adaptive_step_plan(name, self.selected)?
                    || report.adaptive.is_some()
                    || report.group.is_some()
                {
                    return Err("adaptive frozen step plan mismatch".into());
                }
                replay.verify_report(report)?;
                self.steps.push(AdaptiveStep {
                    name: name.clone(),
                    report: report.clone(),
                    decision: None,
                });
                self.active = None;
            }
            EvidenceEvent::AdaptiveDecision(d) => {
                let step = self.steps.last_mut().ok_or("decision without a step")?;
                if step.decision.is_some() || step.name != d.name {
                    return Err("duplicate or unordered adaptive decision".into());
                }
                let expected = decision(&self.plan, &step.name, &step.report, self.selected);
                if *d != expected {
                    return Err(
                        "adaptive decision differs from frozen rules and verified observations"
                            .into(),
                    );
                }
                step.decision = Some(d.clone());
                self.selected = d.selected_fps;
                self.next = d.next.clone();
            }
            _ => return Err("invalid adaptive child record".into()),
        }
        Ok(())
    }
    pub fn verify(&self, r: &Report) -> Result<(), String> {
        let a = r.adaptive.as_ref().ok_or("adaptive report missing")?;
        let c = self.plan.compile_adaptive()?;
        if self.active.is_some()
            || r.group.is_some()
            || r.load.is_some()
            || !r.receivers.is_empty()
            || !r.stages.is_empty()
            || r.timing.is_some()
            || r.rtt_report.is_some()
            || r.classic_pattern_transmit.is_some()
            || a.scope != AdaptiveReport::initial(&self.plan)?.scope
            || a.phase
                != if r.state == "completed" {
                    "complete"
                } else {
                    "stopped"
                }
            || a.reserved_planned != c.count
            || a.reserved_peak_fps != c.max_fps
            || a.selected_fps != self.selected
            || serde_json::to_value(&a.steps).ok() != serde_json::to_value(&self.steps).ok()
            || a.steps.iter().any(|s| s.report.id != r.id)
        {
            return Err("adaptive report differs from independently verified steps".into());
        }
        let sum = group::totals(a.steps.iter().map(|s| s.report.as_ref()));
        if r.planned != a.steps.iter().map(|s| s.report.planned).sum::<u32>()
            || (r.attempted, r.submitted, r.skipped, r.failed, r.unknown)
                != (
                    sum.attempted,
                    sum.submitted,
                    sum.skipped,
                    sum.failed,
                    sum.unknown,
                )
        {
            return Err("adaptive physical totals mismatch".into());
        }
        if r.verdict == "pass" && !r.adaptive_passing_invariants() {
            return Err("adaptive pass violates fixed measurement invariants".into());
        }
        Ok(())
    }
    pub fn recovered(&self, id: &str) -> AdaptiveReport {
        let mut a = AdaptiveReport::initial(&self.plan).unwrap();
        a.phase = "interrupted".into();
        a.selected_fps = self.selected;
        a.steps = self.steps.clone();
        if let Some((name, replay)) = &self.active {
            let p = self.plan.adaptive_step_plan(name, self.selected).unwrap();
            let mut r = Report::initial(&p, false, id.into()).unwrap();
            let q = replay.submission_summary();
            r.provenance = replay.recovered_provenance();
            r.attempted = q.attempted();
            r.submitted = q.submitted;
            r.skipped = q.skipped;
            r.failed = q.failed;
            r.unknown = q.unknown;
            r.unknown_sequence = q.unknown_sequence;
            r.submission_evidence = q;
            r.classic_pattern_transmit = replay.pattern_transmit();
            r.stages = replay.recovered_stages();
            r.timing = replay.recovered_timing();
            r.load = replay.recovered_load();
            r.receivers = replay.sample_receivers();
            r.state = "interrupted".into();
            r.counters_complete = false;
            r.measurement_complete = false;
            r.workload_achieved = false;
            r.assessment = None;
            r.recovery.counters_are_lower_bounds = true;
            r.recovery.status = "interrupted".into();
            a.steps.push(AdaptiveStep {
                name: name.clone(),
                report: Box::new(group::compact(r)),
                decision: None,
            });
        }
        for s in &mut a.steps {
            s.report.id = id.into();
            s.report.state = "interrupted".into();
            s.report.verdict = "inconclusive".into();
            s.report.measurement_complete = false;
            s.report.workload_achieved = false;
            s.report.assessment = None;
            s.report.recovery.counters_are_lower_bounds = true;
        }
        a
    }
}
pub fn restore_adaptive_totals(r: &mut Report) {
    totals(r);
}
