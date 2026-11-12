// SPDX-License-Identifier: AGPL-3.0-or-later
//! One admission, one device reservation and one acquisition per channel for all flows.
use super::*;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicUsize, Ordering};
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "StressFlow")]
pub struct Flow {
    #[serde(default, skip_serializing_if = "Tracking::is_exact")]
    pub tracking: Tracking,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queues: Option<QueueOptions>,
    #[serde(default, skip_serializing_if = "LoadShape::is_uniform")]
    pub shape: LoadShape,
    pub name: String,
    pub sender: String,
    pub receivers: Vec<String>,
    pub can_id: u32,
    pub extended: bool,
    pub run_tag: u32,
    #[serde(default)]
    pub count: u32,
    #[serde(default)]
    pub fps: u32,
    #[serde(default)]
    pub stages: Vec<Stage>,
    #[serde(default, skip_serializing_if = "Traffic::is_classic")]
    pub traffic: Traffic,
    #[serde(default, skip_serializing_if = "Criteria::is_default")]
    pub criteria: Criteria,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timing: Option<TimingOptions>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rtt: Option<RttOptions>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[schemars(rename = "StressChannelBudget")]
pub struct ChannelBudget {
    pub channel: String,
    pub peak_transmit_fps: u32,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[schemars(rename = "StressDeviceBudget")]
pub struct DeviceBudget {
    pub device_id: String,
    pub channels: Vec<String>,
    pub peak_transmit_fps: u32,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[schemars(rename = "StressGroupBudget")]
pub struct GroupBudget {
    pub memory_reserve_bytes: u64,
    pub flow_count: u32,
    pub peak_transmit_fps: u32,
    pub maximum_inflight: u32,
    pub tracking_bytes: u64,
    pub estimated_evidence_bytes: u64,
    pub evidence_records_per_second: u32,
    pub channels: Vec<ChannelBudget>,
    pub devices: Vec<DeviceBudget>,
    pub data_bitrate: Option<u32>,
}
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[schemars(rename = "StressFlowReport")]
pub struct FlowReport {
    pub name: String,
    pub report: Box<Report>,
}
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[schemars(rename = "StressGroupReport")]
pub struct GroupReport {
    pub stop_requests: BTreeMap<String, u64>,
    pub start_ns: Option<u64>,
    pub budget: GroupBudget,
    pub flows: Vec<FlowReport>,
    pub evidence_scope: String,
}
impl GroupReport {
    pub fn initial(p: &Plan) -> Result<Self, String> {
        Ok(Self{stop_requests:Default::default(),start_ns:None,budget:p.group_budget()?,flows:vec![],evidence_scope:"one shared run evidence set; per-flow records namespaced; parent verification required".into()})
    }
}
impl Plan {
    pub fn flow_plans(&self) -> Vec<(String, Plan)> {
        let mut primary = self.clone();
        primary.flows.clear();
        primary.coordination = None;
        primary.topology = None;
        primary.stability = Default::default();
        let mut plans = vec![("primary".into(), primary.clone())];
        for f in &self.flows {
            let mut p = primary.clone();
            p.tracking = f.tracking.clone();
            p.queues = f.queues.clone();
            p.shape = f.shape.clone();
            p.sender = f.sender.clone();
            p.receivers = f.receivers.clone();
            p.can_id = f.can_id;
            p.extended = f.extended;
            p.run_tag = f.run_tag;
            p.count = f.count;
            p.fps = f.fps;
            p.stages = f.stages.clone();
            p.traffic = f.traffic.clone();
            p.criteria = f.criteria.clone();
            p.timing = f.timing.clone();
            p.rtt = f.rtt.clone();
            plans.push((f.name.clone(), p));
        }
        plans
    }
    pub fn channels(&self) -> Vec<String> {
        let mut channels = BTreeSet::new();
        channels.insert(self.sender.clone());
        channels.extend(self.receivers.clone());
        for f in &self.flows {
            channels.insert(f.sender.clone());
            channels.extend(f.receivers.clone());
        }
        channels.into_iter().collect()
    }
    pub fn producers(&self) -> Vec<(String, u32, bool, u32)> {
        let mut result = vec![];
        for (_, p) in self.flow_plans() {
            let fps = p.compile().map_or(0, |c| c.max_fps);
            result.extend(p.producers_single(fps));
        }
        result
    }
    pub fn group_budget(&self) -> Result<GroupBudget, String> {
        if self.flows.is_empty() || self.flows.len() > 7 {
            return Err("multi-flow needs 1..7 additional flows plus primary".into());
        }
        let channels = self.channels();
        if channels.len() > 8 {
            return Err("group exceeds eight unique channels".into());
        }
        if self.evidence.segment_bytes < 256 * 1024 {
            return Err("multi-flow evidence segment_bytes must be at least 256 KiB".into());
        }
        let mut names = BTreeSet::new();
        names.insert("primary");
        for f in &self.flows {
            if f.name.is_empty()
                || f.name.len() > 32
                || !f
                    .name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                || !names.insert(&f.name)
            {
                return Err(
                    "flow names must be unique 1..32 ASCII identifiers; primary is reserved".into(),
                );
            }
        }
        let mut tags = BTreeSet::new();
        let mut ids = BTreeSet::new();
        let mut data = None;
        let mut tracking = 0u64;
        let mut bytes = 4 * 1024 * 1024u64;
        let mut records = 0u32;
        let mut inflight = 0;
        let mut per_channel: BTreeMap<String, u32> =
            channels.iter().map(|c| (c.clone(), 0)).collect();
        let mut quantum = 0;
        for (_, p) in self.flow_plans() {
            let c = p.compile()?;
            if !tags.insert(p.run_tag) {
                return Err("each local flow needs a distinct run_tag".into());
            }
            if let Some(rate) = p.traffic.data_bitrate() {
                if data.is_some_and(|r| r != rate) {
                    return Err("group FD data bitrate mismatch".into());
                }
                data = Some(rate);
            }
            tracking = tracking
                .checked_add(c.tracking_bytes)
                .ok_or("group memory overflow")?;
            bytes = bytes
                .checked_add(c.evidence_bytes - 4 * 1024 * 1024 + 256 * 1024)
                .ok_or("group evidence overflow")?;
            let lanes = if p.rtt.is_some() { 2 } else { 1 };
            records += p.timing.as_ref().map_or(0, |t| {
                c.max_fps.div_ceil(t.sample_every) * (2 * lanes + p.receivers.len() as u32)
            });
            if p.evidence.sample_every > 0 {
                records += c.max_fps.div_ceil(p.evidence.sample_every) * p.receivers.len() as u32;
            }
            records += p.resource_records(c.max_fps);
            inflight += if p.rtt.as_ref().is_some_and(|r| r.role == RttRole::Loopback) {
                2
            } else {
                1
            };
            for (ch, id, ext, fps) in p.producers_single(c.max_fps) {
                if !ids.insert((id, ext)) {
                    return Err(
                        "multiple group producers cannot share a CAN ID/width on the declared bus"
                            .into(),
                    );
                }
                *per_channel.get_mut(&ch).unwrap() += fps;
                quantum += fps.div_ceil(100);
            }
        }
        let peak = per_channel.values().sum::<u32>();
        if peak > MAX_FPS || quantum > MAX_BATCH {
            return Err(
                "aggregate group transmit/quantum budget exceeded (including RTT responses)".into(),
            );
        }
        if records > 2000 {
            return Err("group evidence sampling exceeds 2000 records/s".into());
        }
        if tracking + 32 * 1024 * 1024 > MEMORY_LIMIT {
            return Err("group tracking and queue reserve exceeds 256 MiB".into());
        }
        if bytes > self.evidence.max_bytes
            || (bytes - 4 * 1024 * 1024).div_ceil(self.evidence.segment_bytes) + 5 > 1000
        {
            return Err("group evidence byte or segment budget exceeded".into());
        }
        Ok(GroupBudget {
            memory_reserve_bytes: 32 * 1024 * 1024,
            flow_count: self.flows.len() as u32 + 1,
            peak_transmit_fps: peak,
            maximum_inflight: inflight,
            tracking_bytes: tracking,
            estimated_evidence_bytes: bytes,
            evidence_records_per_second: records,
            channels: per_channel
                .into_iter()
                .map(|(channel, peak_transmit_fps)| ChannelBudget {
                    channel,
                    peak_transmit_fps,
                })
                .collect(),
            devices: vec![],
            data_bitrate: data,
        })
    }
    fn producers_single(&self, fps: u32) -> Vec<(String, u32, bool, u32)> {
        let mut v = vec![];
        if !self
            .rtt
            .as_ref()
            .is_some_and(|r| r.role == RttRole::Responder)
        {
            if let Some(id) = self.traffic.checkpoint_id() {
                v.push((self.sender.clone(), id, self.extended, fps / 2));
                v.push((self.sender.clone(), self.can_id, self.extended, fps / 2));
            } else {
                v.push((self.sender.clone(), self.can_id, self.extended, fps));
            }
        }
        if let Some(r) = &self.rtt
            && r.role != RttRole::Probe
        {
            v.push((
                self.receivers[0].clone(),
                r.response_can_id,
                self.extended,
                fps,
            ));
        }
        v
    }
    pub fn compile_group(&self) -> Result<Compiled, String> {
        let budget = self.group_budget()?;
        if let Some(t) = &self.topology {
            t.validate(self)?;
        }
        let mut count = 0u32;
        let mut duration = 0;
        for (_, p) in self.flow_plans() {
            let c = p.compile()?;
            count = count
                .checked_add(
                    c.count
                        .checked_mul(
                            if p.rtt.as_ref().is_some_and(|r| r.role == RttRole::Loopback) {
                                2
                            } else {
                                1
                            },
                        )
                        .ok_or("group response count overflow")?,
                )
                .ok_or("group planned count exceeds u32")?;
            duration = duration.max(c.duration_ns);
        }
        if let Some(c) = &self.coordination {
            c.validate()?;
        }
        if self.stability.max_rss_growth_bytes.is_some_and(|n| n == 0)
            || self.stability.max_rss_growth_bytes.is_some()
                && u64::from(self.stability.warmup_seconds) * 1_000_000_000 >= duration
        {
            return Err("invalid group stability warmup/limit".into());
        }
        Ok(Compiled {
            shape: LoadShape::default(),
            count,
            duration_ns: duration,
            tracking_bytes: budget.tracking_bytes,
            stages: vec![],
            max_fps: budget.peak_transmit_fps,
            evidence_bytes: budget.estimated_evidence_bytes,
        })
    }
    pub fn validate_group(&self, runtime: &Runtime) -> Result<(), String> {
        let budget = self.group_budget()?;
        self.compile_group()?;
        for (_, p) in self.flow_plans() {
            p.validate(runtime)?;
        }
        for channel in self.channels() {
            let caps = runtime.capabilities(&channel).map_err(|e| e.to_string())?;
            if let Some(rate) = budget.data_bitrate
                && (!caps.fd
                    || (!caps.fixed_data_bitrates.is_empty()
                        && !caps.fixed_data_bitrates.contains(&rate)))
            {
                return Err("every group channel must support the shared FD configuration".into());
            }
        }
        if self.stability.max_rss_growth_bytes.is_some() && current_rss().is_none() {
            return Err("required current RSS monitor unavailable".into());
        }
        let c = self.compile_group()?;
        if self.stability.max_rss_growth_bytes.is_some_and(|n| n == 0)
            || self.stability.max_rss_growth_bytes.is_some()
                && u64::from(self.stability.warmup_seconds) * 1_000_000_000 >= c.duration_ns
        {
            return Err("invalid group stability warmup/limit".into());
        }
        let mut bytes = 64 * 1024;
        for (_, p) in self.flow_plans() {
            let mut r = Report::initial(&p, true, "source-check".into())?;
            r.capture_sources(runtime);
            bytes += serde_json::to_vec(&r.provenance)
                .map_err(|e| e.to_string())?
                .len()
                + 64 * 1024;
        }
        if bytes > 768 * 1024 {
            return Err(
                "group report exceeds API metadata budget; reduce flows or source metadata".into(),
            );
        }
        Ok(())
    }
}
impl GroupBudget {
    pub fn bind_sources(&mut self, source: &Provenance) {
        let mut devices: BTreeMap<String, DeviceBudget> = BTreeMap::new();
        for c in &self.channels {
            if let Some(s) = source.channels.iter().find(|s| s.channel == c.channel) {
                let d = devices.entry(s.device_id.clone()).or_insert(DeviceBudget {
                    device_id: s.device_id.clone(),
                    channels: vec![],
                    peak_transmit_fps: 0,
                });
                d.channels.push(c.channel.clone());
                d.peak_transmit_fps += c.peak_transmit_fps;
            }
        }
        self.devices = devices.into_values().collect();
    }
}
pub(crate) struct SharedExecution {
    pub owner: String,
    pub reservation: Arc<canharbor_core::runtime::DeviceReservation>,
    pub sessions: BTreeMap<String, (String, u64, u64)>,
    pub ready: AtomicUsize,
    pub release: watch::Sender<Option<Instant>>,
}

fn terminal(state: &str) -> bool {
    matches!(state, "completed" | "failed" | "aborted" | "interrupted")
        || state.starts_with("saving_")
}
pub(crate) fn compact(mut r: Report) -> Report {
    r.history.clear();
    r.lifecycle_history.clear();
    r.recovery.segments.clear();
    r.recovery.sample_receivers.clear();
    r
}
impl Run {
    pub fn stop_flow(&self, name: &str) -> Result<(), String> {
        let valid = {
            let r = self.report.lock().unwrap();
            !r.plan.flows.is_empty() && r.plan.flow_plans().iter().any(|(n, _)| n == name)
        };
        if !valid {
            return Err("unknown group flow".into());
        }
        if terminal(&self.state())
            || self
                .children
                .lock()
                .unwrap()
                .get(name)
                .is_some_and(|r| terminal(&r.state()))
        {
            return Ok(());
        }
        let mut stopped = self.flow_stops.lock().unwrap();
        if stopped.contains_key(name) {
            return Ok(());
        }
        let at = self.elapsed_ns();
        stopped.insert(name.into(), at);
        drop(stopped);
        if let Some(g) = &mut self.report.lock().unwrap().group {
            g.stop_requests.insert(name.into(), at);
        }
        if let Some(s) = &self.sink {
            s.emit(EvidenceEvent::FlowStop {
                name: name.into(),
                requested_ns: at,
            });
        }
        if let Some(child) = self.children.lock().unwrap().get(name) {
            child.stop();
        }
        Ok(())
    }
}
pub(crate) fn make_child(
    parent: &Arc<Run>,
    plan: &Plan,
    runtime: &Runtime,
    sink: Option<EvidenceSink>,
) -> Result<Arc<Run>, String> {
    let sink = sink.map(|s| s.ordered(!plan.tracking.is_exact()));
    let simulated = plan
        .channels()
        .iter()
        .all(|c| runtime.capabilities(c).is_ok_and(|v| v.simulated));
    let mut initial = Report::initial(plan, simulated, parent.id())?;
    initial.capture_sources(runtime);
    initial.execution_timeout_ns += u64::from(
        parent
            .report
            .lock()
            .unwrap()
            .plan
            .coordination
            .as_ref()
            .map_or(0, |c| c.start_delay_ms),
    ) * 1_000_000;
    let deadline = parent
        .execution_deadline
        .min(parent.started + Duration::from_nanos(initial.execution_timeout_ns));
    let (cancel, _) = watch::channel(false);
    Ok(Arc::new(Run {
        parent: Some(Arc::downgrade(parent)),
        children: Default::default(),
        flow_stops: Default::default(),
        timing: timing::TimingRecorder::new(plan, sink.clone()).map(Mutex::new),
        report: Arc::new(Mutex::new(initial)),
        cancel,
        started: parent.started,
        sink,
        execution_deadline: deadline,
        pending_io: Default::default(),
        inflight: Mutex::new(None),
        responder_inflight: Mutex::new(None),
        coord_deadline: Mutex::new(parent.coordination_deadline()),
        gate: parent.gate.child(),
    }))
}
fn refresh(parent: &Run) {
    let snapshots = parent
        .children
        .lock()
        .unwrap()
        .iter()
        .map(|(name, r)| FlowReport {
            name: name.clone(),
            report: Box::new(compact(r.snapshot())),
        })
        .collect::<Vec<_>>();
    let mut r = parent.report.lock().unwrap();
    let counts = totals(snapshots.iter().map(|f| f.report.as_ref()));
    r.attempted = counts.attempted;
    r.submitted = counts.submitted;
    r.skipped = counts.skipped;
    r.failed = counts.failed;
    r.unknown = counts.unknown;
    r.rx_lag_events = snapshots.iter().map(|f| f.report.rx_lag_events).sum();
    r.max_dispatch_lateness_ns = snapshots
        .iter()
        .map(|f| f.report.max_dispatch_lateness_ns)
        .max()
        .unwrap_or(0);
    if let Some(g) = &mut r.group {
        g.flows = snapshots;
    }
}
pub(crate) async fn execute_group(
    runtime: Arc<Runtime>,
    p: Plan,
    run: Arc<Run>,
    mut cancel: watch::Receiver<bool>,
    sink: Option<EvidenceSink>,
    context: ExecutionContext,
) {
    let owner = format!("stress:{}", run.id());
    if let (Some(s), Some(source)) = (&sink, run.snapshot().provenance) {
        s.emit(EvidenceEvent::Provenance(source));
    }
    run.transition("arming");
    let channels = p.channels();
    let plans = p.flow_plans();
    let mut sessions = BTreeMap::new();
    let mut jobs = vec![];
    let cleanup_before = channels
        .iter()
        .map(|c| (c.clone(), runtime.cleanup_observation(c)))
        .collect::<BTreeMap<_, _>>();
    let reservation = runtime.reserve_devices(&channels).await.map(Arc::new);
    let result: Result<(), String> = async {
        let reservation = reservation.as_ref().map_err(|e| e.to_string())?;

        run.transition("reserved");
        let budget = p.group_budget()?;
        let producers = p
            .producers()
            .into_iter()
            .map(|(c, _, _, _)| c)
            .collect::<BTreeSet<_>>();

        for channel in &channels {
            if *cancel.borrow() || run.gate.is_revoked() {
                return Err("group cancelled before configuration".into());
            }

            let view = runtime
                .create_reserved(
                    channel,
                    Config {
                        nominal_bitrate: p.bitrate,
                        data_bitrate: budget.data_bitrate,
                        listen_only: !producers.contains(channel) && !p.receivers_active,
                    },
                    Some(reservation),
                )
                .await
                .map_err(|e| e.to_string())?;

            sessions.insert(channel.clone(), (view.id.clone(), 0, 0));

            let acquired = runtime
                .acquire(&view.id, &owner)
                .await
                .map_err(|e| e.to_string())?;
            sessions.get_mut(channel).unwrap().1 = acquired.generation;
        }

        for channel in &channels {
            if *cancel.borrow() || run.gate.is_revoked() {
                return Err("group cancelled before arming".into());
            }

            let (id, g, epoch) = sessions.get_mut(channel).unwrap();
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

        let shared = Arc::new(SharedExecution {
            owner: owner.clone(),
            reservation: reservation.clone(),
            sessions: sessions.clone(),
            ready: AtomicUsize::new(0),
            release: watch::channel(None).0,
        });

        for (name, plan) in &plans {
            let evidence = sink.as_ref().map(|s| s.for_flow(name));
            let child = make_child(&run, plan, &runtime, evidence.clone())?;

            if run.flow_stops.lock().unwrap().contains_key(name) {
                child.stop();
            }

            run.children
                .lock()
                .unwrap()
                .insert(name.clone(), child.clone());
            let rt = runtime.clone();
            let plan = plan.clone();
            let compiled = plan.compile()?;
            let ctx = ExecutionContext {
                authority: context.authority.clone(),
                slot: context.slot.clone(),
                shared: Some(shared.clone()),
            };
            let rx = child.cancel.subscribe();

            let watched = child.clone();
            let task = tokio::spawn(async move {
                if std::panic::AssertUnwindSafe(execute_flow(
                    rt, plan, compiled, child, rx, evidence, ctx,
                ))
                .catch_unwind()
                .await
                .is_err()
                {
                    watched.close_gate();
                    watched.report.clear_poison();
                    watched.report.lock().unwrap().state = "failed".into();
                    watched
                        .report
                        .lock()
                        .unwrap()
                        .errors
                        .push("flow executor panicked; no retry".into());
                }
            });
            jobs.push((name.clone(), task));
        }

        let mut released = false;
        let mut metric_at = Instant::now();
        let mut renew_at = Instant::now();

        loop {
            refresh(&run);

            if *cancel.borrow()
                || run.gate.is_revoked()
                || context
                    .authority
                    .as_ref()
                    .is_some_and(|a| a.1.load(std::sync::atomic::Ordering::Acquire))
            {
                run.stop();
                return Err("group cancelled or authority revoked".into());
            }

            if sink.as_ref().is_some_and(EvidenceSink::failed) {
                run.terminate(
                    "evidence_failure",
                    "shared group evidence failed",
                    None,
                    None,
                );
                run.stop();
                return Err("group evidence failed".into());
            }

            let snapshot = run.snapshot();
            let group = snapshot.group.as_ref().unwrap();

            let bad = {
                let stopped = run.flow_stops.lock().unwrap();
                group
                    .flows
                    .iter()
                    .find(|f| {
                        let unknown = f.report.unknown > 0
                            || f.report
                                .rtt_report
                                .as_ref()
                                .is_some_and(|r| r.responder_unknown > 0);

                        let soft = stopped.contains_key(&f.name)
                            && f.report
                                .lifecycle
                                .termination
                                .as_ref()
                                .is_some_and(|t| t.code == "operator_stop")
                            && !unknown;

                        unknown
                            || (terminal(&f.report.state) && !soft && f.report.verdict != "pass")
                    })
                    .map(|f| f.name.clone())
            };

            if let Some(name) = bad {
                run.terminate(
                    "flow_failed",
                    format!("flow {name} failed; group gate revoked"),
                    None,
                    None,
                );
                run.stop();
                return Err(format!("flow {name} failed"));
            }

            if !released && group.flows.iter().any(|f| terminal(&f.report.state)) {
                run.stop();
                return Err("flow stopped during arming; no partial group release".into());
            }
            // Serialize release against stop requests, including a ready child whose
            // cancellation has not yet reached its terminal report snapshot.
            let stopped_before_release = if !released {
                let stopped = run.flow_stops.lock().unwrap();
                if !stopped.is_empty() {
                    true
                } else {
                    if shared.ready.load(Ordering::Acquire) == plans.len() {
                        let t0 = Instant::now()
                            + Duration::from_millis(
                                p.coordination
                                    .as_ref()
                                    .map_or(20, |c| u64::from(c.start_delay_ms)),
                            );
                        let start_ns = t0.duration_since(run.started).as_nanos() as u64;
                        if let Some(s) = &sink {
                            s.emit(EvidenceEvent::GroupRelease {
                                start_ns,
                                ready_flows: plans.len() as u32,
                            });
                        }
                        run.report.lock().unwrap().group.as_mut().unwrap().start_ns =
                            Some(start_ns);
                        shared.release.send_replace(Some(t0));
                        released = true;
                        run.transition("running");
                    }
                    false
                }
            } else {
                false
            };
            if stopped_before_release {
                run.stop();
                return Err("flow stopped before group release".into());
            }

            if released && group.flows.iter().all(|f| terminal(&f.report.state)) {
                break;
            }

            if Instant::now() >= renew_at {
                for (id, g, _) in sessions.values() {
                    runtime
                        .renew(id, &owner, *g)
                        .await
                        .map_err(|e| e.to_string())?;
                }
                renew_at = Instant::now() + Duration::from_secs(1);
            }

            if Instant::now() >= metric_at {
                publish(&run, &[], &sink, run.started.elapsed());
                let s = run.snapshot().stability;
                if s.limit_exceeded || s.status == "unsupported" {
                    run.terminate("memory_stability", "group RSS criterion failed", None, None);
                    run.stop();
                    return Err("group stability criterion failed".into());
                }
                metric_at = Instant::now() + Duration::from_secs(1);
            }

            tokio::select! {
                biased;
                _ = cancel.changed() => {},
                _ = tokio::time::sleep(Duration::from_millis(10)) => {},
            }
        }

        Ok(())
    }
    .await;

    run.close_gate();
    if result.is_err() {
        for child in run.children.lock().unwrap().values() {
            child.stop();
        }
    }
    for (name, task) in jobs {
        if task.await.is_err() {
            run.report
                .lock()
                .unwrap()
                .errors
                .push(format!("flow {name} join failed"));
        }
    }
    // Emit the final canonical snapshot once per flow, after all of its taps have drained.
    for (name, child) in run.children.lock().unwrap().iter() {
        let mut r = compact(child.snapshot());
        if let Some(state) = r.state.strip_prefix("saving_") {
            r.state = state.into();
        }
        *child.report.lock().unwrap() = r.clone();
        if let Some(s) = &sink {
            s.emit(EvidenceEvent::FlowEnd {
                name: name.clone(),
                report: Box::new(r),
            });
        }
    }
    refresh(&run);
    run.transition("finalizing");
    for (channel, (id, g, epoch)) in sessions.into_iter().rev() {
        let closing = runtime.close_at_epoch(&id, &owner, g, epoch);
        tokio::pin!(closing);
        let result = match tokio::time::timeout(Duration::from_secs(2), &mut closing).await {
            Ok(r) => r,
            Err(_) => {
                run.transition("stopping_blocked");
                closing.await
            }
        };
        let observation = match (
            cleanup_before.get(&channel).and_then(Option::as_ref),
            runtime.cleanup_observation(&channel),
        ) {
            (Some(a), Some(b)) if result.is_ok() => CleanupObservation {
                channel: channel.clone(),
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
                scope: "closed_original_group_epoch".into(),
            },
            _ => CleanupObservation {
                channel: channel.clone(),
                scope: "unsupported_or_cleanup_incomplete".into(),
                ..Default::default()
            },
        };
        run.report.lock().unwrap().backend_cleanup.push(observation);
        if let Err(e) = result {
            run.report
                .lock()
                .unwrap()
                .errors
                .push(format!("group cleanup: {e}"));
        }
    }
    let any_flow_stop = !run.flow_stops.lock().unwrap().is_empty();
    let ending = {
        let mut r = run.report.lock().unwrap();
        if let Err(e) = result {
            r.errors.push(e);
        }
        let g = r.group.as_ref().unwrap();
        let all_completed =
            g.flows.len() == plans.len() && g.flows.iter().all(|f| f.report.state == "completed");
        let all_measurement =
            g.flows.len() == plans.len() && g.flows.iter().all(|f| f.report.measurement_complete);
        let all_workload =
            g.flows.len() == plans.len() && g.flows.iter().all(|f| f.report.workload_achieved);
        let all_pass = g.flows.iter().all(|f| f.report.verdict == "pass");
        r.measurement_complete =
            all_measurement && r.errors.is_empty() && sink.as_ref().is_none_or(|s| !s.failed());
        r.workload_achieved = all_workload;
        r.counters_complete = r.group.as_ref().unwrap().flows.len() == plans.len()
            && r.group
                .as_ref()
                .unwrap()
                .flows
                .iter()
                .all(|f| f.report.counters_complete);
        r.elapsed_seconds = run.started.elapsed().as_secs_f64();
        r.lifecycle.execution_end_ns = Some(run.elapsed_ns());
        let ending = if *cancel.borrow() || any_flow_stop {
            "aborted"
        } else if r.errors.is_empty() && all_completed {
            "completed"
        } else {
            "failed"
        };
        r.verdict = if r.known_violation() {
            "fail"
        } else if !r.measurement_complete || ending != "completed" {
            "inconclusive"
        } else if all_pass {
            "pass"
        } else {
            "fail"
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

impl GroupReport {
    pub fn assess(&self) -> Assessment {
        let mut failures = vec![];
        let mut workload = true;
        for f in &self.flows {
            let a = f.report.assess();
            workload &= a.workload_met;
            for mut e in a.failures {
                e.criterion = format!("flow:{}:{}", f.name, e.criterion);
                failures.push(e);
            }
        }
        Assessment {
            passed: !self.flows.is_empty() && failures.is_empty(),
            workload_met: !self.flows.is_empty() && workload,
            failures,
        }
    }
}
impl Report {
    pub fn group_passing_invariants(&self) -> bool {
        let Some(g) = &self.group else {
            return false;
        };
        let Ok(mut budget) = self.plan.group_budget() else {
            return false;
        };
        let Some(source) = &self.provenance else {
            return false;
        };
        budget.bind_sources(source);
        let expected = self
            .plan
            .flow_plans()
            .into_iter()
            .collect::<BTreeMap<_, _>>();
        self.version
            >= if self.plan.requires_resource_v11() {
                11
            } else if self.plan.requires_load_v10() {
                10
            } else if self.plan.has_classic_pattern() {
                9
            } else {
                8
            }
            && source.valid(&self.plan)
            && budget == g.budget
            && g.flows.len() == expected.len()
            && g.flows
                .iter()
                .map(|f| &f.name)
                .collect::<BTreeSet<_>>()
                .len()
                == expected.len()
            && g.flows.iter().all(|f| {
                f.report.group.is_none()
                    && f.report.id == self.id
                    && f.report.plan.flows.is_empty()
                    && expected.get(&f.name) == Some(&f.report.plan)
                    && f.report.passing_invariants()
            })
            && self.lifecycle.execution_end_ns.is_some_and(|end| {
                g.start_ns.is_some_and(|start| start <= end)
                    && g.flows.iter().all(|f| {
                        f.report
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
            && g.stop_requests.is_empty()
            && self.lifecycle.stop_requested_ns.is_none()
            && !self.stability.limit_exceeded
            && (self.plan.stability.max_rss_growth_bytes.is_none()
                || self.stability.status == "monitoring")
            && self.receivers.is_empty()
            && self.stages.is_empty()
            && self.timing.is_none()
            && self.rtt_report.is_none()
            && self.planned == self.plan.compile().map_or(0, |c| c.count)
            && {
                let c = totals(g.flows.iter().map(|f| f.report.as_ref()));
                self.attempted == c.attempted
                    && self.submitted == c.submitted
                    && self.skipped == c.skipped
                    && self.failed == c.failed
                    && self.unknown == c.unknown
            }
            && self.failed == 0
            && self.unknown == 0
    }
}

#[derive(Default)]
pub(crate) struct Totals {
    pub attempted: u64,
    pub submitted: u64,
    pub skipped: u64,
    pub failed: u64,
    pub unknown: u64,
}
pub(crate) fn totals<'a>(reports: impl Iterator<Item = &'a Report>) -> Totals {
    let mut c = Totals::default();
    for r in reports {
        c.attempted += r.attempted;
        c.submitted += r.submitted;
        c.skipped += r.skipped;
        c.failed += r.failed;
        c.unknown += r.unknown;
        if r.plan
            .rtt
            .as_ref()
            .is_some_and(|p| p.role == RttRole::Loopback)
        {
            if let Some(t) = &r.rtt_report {
                c.attempted += t.responder_submitted + t.responder_failed + t.responder_unknown;
                c.submitted += t.responder_submitted;
                c.failed += t.responder_failed;
                c.unknown += t.responder_unknown;
            }
            c.skipped += r.skipped;
        }
    }
    c
}
