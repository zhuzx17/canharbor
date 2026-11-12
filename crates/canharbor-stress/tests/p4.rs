// SPDX-License-Identifier: AGPL-3.0-or-later
use canharbor_core::{Frame, runtime::Runtime};
use canharbor_stress::*;
use canharbor_virtual::VirtualBus;
use std::{sync::Arc, time::Duration};
fn plan() -> Plan {
    serde_json::from_value(serde_json::json!({"sender":"virtual:0","receivers":["virtual:1"],"can_id":291,"extended":false,"run_tag":17,"count":24,"fps":100,"bitrate":500000,"receivers_active":true,"drain_ms":20,"traffic":{"format":"fd_v1","data_bitrate":2000000,"stream":2,"epoch":3,"seed":42,"variants":[{"length":32,"brs":false},{"length":48,"brs":true},{"length":64,"brs":true}]}})).unwrap()
}
#[test]
fn crc32c_standard_vector_and_fd_golden_wire_are_portable() {
    assert_eq!(crc32c(b"123456789"), 0xe3069283);
    assert_eq!(crc32c(b""), 0);
    let p = plan();
    let f = p.frame(0);
    assert_eq!(p.decode(&f, false), Ok(0));
    assert_eq!(
        f.data()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>(),
        "4348010000000011000200000000000300000000000000000000002a6cd00b0c"
    );
    assert_eq!(
        p.frame(2)
            .data()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>(),
        "4348010000000011000200000000000300000000000000020000002ae1a24867dc29955bdd63df70a468f991ce78d1ca4c1fdd00ccf861eb94be8a7ca165c3e5"
    );
    for n in 0..24 {
        let f = p.frame(n);
        assert_eq!(p.decode(&f, false), Ok(n));
        assert_eq!(f.dlc(), [13, 14, 15][n as usize % 3]);
    }
}
#[test]
fn fd_crc_covers_header_payload_id_width_and_brs() {
    let p = plan();
    let f = p.frame(1);
    for i in 0..f.data().len() {
        let Frame::FdData { id, mut data, brs } = f.clone() else {
            unreachable!()
        };
        data[i] ^= 1;
        assert!(
            p.decode(&Frame::fd(id, data, brs).unwrap(), false).is_err(),
            "byte {i}"
        );
    }
    let Frame::FdData { id, data, brs } = f else {
        unreachable!()
    };
    assert_eq!(
        p.decode(&Frame::fd(id, data, !brs).unwrap(), false),
        Err(PayloadError::Crc)
    );
}
#[test]
fn mixed_short_fd_is_explicitly_sequence_only_and_rtt_requires_crc() {
    let mut p = plan();
    if let Traffic::FdV1 { variants, .. } = &mut p.traffic {
        *variants = vec![
            FdVariant {
                length: 8,
                brs: false,
            },
            FdVariant {
                length: 12,
                brs: true,
            },
            FdVariant {
                length: 24,
                brs: false,
            },
            FdVariant {
                length: 64,
                brs: true,
            },
        ];
    }
    assert_eq!(
        p.traffic.integrity(),
        "mixed_crc32c_and_short_sequence_only"
    );
    for n in 0..24 {
        assert_eq!(p.decode(&p.frame(n), false), Ok(n));
    }
    p.rtt = Some(RttOptions {
        role: RttRole::Loopback,
        response_can_id: 292,
        timeout_ms: 100,
        max_inflight: 32,
    });
    assert!(p.compile().is_err());
}
#[tokio::test(start_paused = true)]
async fn mixed_fd_rtt_responder_reconciles_and_stops_without_echo_loop() {
    let mut p = plan();
    p.timing = Some(TimingOptions {
        sample_every: 1,
        max_pending: 4,
    });
    p.rtt = Some(RttOptions {
        role: RttRole::Loopback,
        response_can_id: 292,
        timeout_ms: 100,
        max_inflight: 32,
    });
    let rt = Runtime::new(Arc::new(VirtualBus::new(8192)));
    let run = Executor::new(rt.clone()).start(p, true).unwrap();
    let r = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let r = run.snapshot();
            if matches!(r.state.as_str(), "completed" | "failed" | "aborted") {
                break r;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(r.verdict, "pass", "{r:?}");
    assert_eq!(r.timing.as_ref().unwrap().response.submitted, 24);
    assert_eq!(r.timing.as_ref().unwrap().receivers[0].matched, 24);
    let t = r.rtt_report.unwrap();
    assert_eq!(t.responder_submitted, 24);
    assert_eq!(t.latency.samples, 24);
    assert_eq!(t.timed_out, 0);
    assert!(rt.list().await.is_empty());
}
#[test]
fn histogram_has_fixed_storage_and_does_not_clip_negative_observations() {
    let mut h = LatencyHistogram::default();
    h.record(10, 9);
    assert_eq!(h.negative, 1);
    assert_eq!(h.samples, 0);
    for n in 0..10000 {
        h.record(0, n * 10000);
    }
    assert_eq!(h.bins.len(), 25);
    assert_eq!(h.samples, 10000);
    assert!(h.p99_upper_ns >= h.p50_upper_ns);
    h.record(0, u64::MAX);
    assert_eq!(h.overflow, 1);
}

async fn final_report(run: &Run) -> Report {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let r = run.snapshot();
            if matches!(r.state.as_str(), "completed" | "failed" | "aborted") {
                break r;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap()
}
#[tokio::test(start_paused = true)]
async fn independent_probe_and_responder_use_only_initiators_clock() {
    let bus = Arc::new(VirtualBus::with_channel_count(8192, 4));
    let a = Runtime::new(bus.clone());
    let b = Runtime::new(bus);
    let mut probe = plan();
    probe.timing = Some(TimingOptions {
        sample_every: 1,
        max_pending: 4,
    });
    probe.rtt = Some(RttOptions {
        role: RttRole::Probe,
        response_can_id: 292,
        timeout_ms: 100,
        max_inflight: 32,
    });
    let mut peer = probe.clone();
    peer.sender = "virtual:2".into();
    peer.receivers = vec!["virtual:3".into()];
    peer.rtt.as_mut().unwrap().role = RttRole::Responder;
    let responder = Executor::new(b.clone()).start(peer, true).unwrap();
    while responder.state() != "running" {
        tokio::task::yield_now().await;
    }
    let initiator = Executor::new(a.clone()).start(probe, true).unwrap();
    let (x, y) = tokio::join!(final_report(&initiator), final_report(&responder));
    assert_eq!(x.verdict, "pass", "{x:?}");
    assert_eq!(y.verdict, "pass", "{y:?}");
    assert_eq!(x.timing.as_ref().unwrap().receivers[0].matched, 24);
    assert!(y.timing.as_ref().unwrap().receivers.is_empty());
    assert_eq!(y.timing.as_ref().unwrap().response.submitted, 24);
    assert!(y.timing.as_ref().unwrap().error.is_none());
    assert_eq!(x.rtt_report.unwrap().latency.samples, 24);
    assert_eq!(y.rtt_report.unwrap().latency.samples, 0);
    assert!(a.list().await.is_empty());
    assert!(b.list().await.is_empty());
}
#[tokio::test(start_paused = true)]
async fn missing_responder_times_out_and_never_fabricates_latency() {
    let mut p = plan();
    p.rtt = Some(RttOptions {
        role: RttRole::Probe,
        response_can_id: 292,
        timeout_ms: 100,
        max_inflight: 32,
    });
    let run = Executor::new(Runtime::new(Arc::new(VirtualBus::new(8192))))
        .start(p, true)
        .unwrap();
    let r = final_report(&run).await;
    assert_eq!(r.verdict, "fail");
    let t = r.rtt_report.unwrap();
    assert_eq!(t.latency.samples, 0);
    assert_eq!(t.timed_out, 24);
}
#[tokio::test(start_paused = true)]
async fn lost_coordinator_lease_stops_and_cannot_be_renewed_after_expiry() {
    let mut p = plan();
    p.count = 10000;
    p.coordination = Some(Coordination {
        group_id: uuid::Uuid::new_v4().to_string(),
        participant: "virtual-a".into(),
        lease_ms: 3000,
        start_delay_ms: 2000,
        clock_quality: "unsynchronized".into(),
    });
    let run = Executor::new(Runtime::new(Arc::new(VirtualBus::new(8192))))
        .start(p, true)
        .unwrap();
    let r = final_report(&run).await;
    assert_eq!(r.state, "aborted");
    assert!(r.submitted < 10000);
    assert_eq!(r.lifecycle.termination.unwrap().code, "coordinator_lost");
    assert!(run.renew_coordination().is_err());
    let sent = run.snapshot().submitted;
    tokio::time::sleep(Duration::from_secs(30)).await;
    assert_eq!(run.snapshot().submitted, sent);
}

struct Faulty {
    bus: VirtualBus,
    corrupt: bool,
    entered: tokio::sync::Notify,
    release: tokio::sync::Semaphore,
    responses: std::sync::atomic::AtomicUsize,
}
impl canharbor_core::Backend for Faulty {
    fn capabilities(&self, c: &str) -> Result<canharbor_core::Capabilities, canharbor_core::Error> {
        canharbor_core::Backend::capabilities(&self.bus, c)
    }
    fn channels(&self) -> Vec<canharbor_core::Channel> {
        canharbor_core::Backend::channels(&self.bus)
    }
    fn subscribe(
        &self,
        c: &str,
    ) -> Result<
        tokio::sync::broadcast::Receiver<Arc<canharbor_core::BackendEvent>>,
        canharbor_core::Error,
    > {
        canharbor_core::Backend::subscribe(&self.bus, c)
    }
    fn check_channel(&self, c: &str) -> Result<u64, canharbor_core::Error> {
        canharbor_core::Backend::check_channel(&self.bus, c)
    }
    fn transmit(&self, c: &str, mut f: Frame) -> Result<(), canharbor_core::Error> {
        if self.corrupt
            && f.id().value() == 291
            && let Frame::FdData { data, .. } = &mut f
        {
            data[28] ^= 1;
        }
        canharbor_core::Backend::transmit(&self.bus, c, f)
    }
    fn configure<'a>(
        &'a self,
        c: &'a str,
        v: canharbor_core::Config,
    ) -> canharbor_core::BackendFuture<'a, ()> {
        canharbor_core::Backend::configure(&self.bus, c, v)
    }
    fn start<'a>(
        &'a self,
        c: &'a str,
        p: canharbor_core::control::Permit,
    ) -> canharbor_core::BackendFuture<'a, ()> {
        canharbor_core::Backend::start(&self.bus, c, p)
    }
    fn stop<'a>(&'a self, c: &'a str) -> canharbor_core::BackendFuture<'a, ()> {
        canharbor_core::Backend::stop(&self.bus, c)
    }
    fn close<'a>(&'a self, c: &'a str) -> canharbor_core::BackendFuture<'a, ()> {
        canharbor_core::Backend::close(&self.bus, c)
    }
    fn submit<'a>(
        &'a self,
        c: &'a str,
        f: Frame,
        p: canharbor_core::control::Permit,
    ) -> canharbor_core::BackendFuture<'a, ()> {
        Box::pin(async move {
            p.check()?;
            let response = f.id().value() == 292;
            self.transmit(c, f)?;
            if response && !self.corrupt {
                self.responses
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                self.entered.notify_one();
                self.release.acquire().await.unwrap().forget();
            }
            Ok(())
        })
    }
}
fn faulty(corrupt: bool) -> Arc<Faulty> {
    Arc::new(Faulty {
        bus: VirtualBus::new(8192),
        corrupt,
        entered: Default::default(),
        release: tokio::sync::Semaphore::new(0),
        responses: Default::default(),
    })
}
#[tokio::test(start_paused = true)]
async fn crc_corruption_is_retained_and_independently_replayed_as_failure() {
    let mut p = plan();
    p.evidence.sample_every = 1;
    let compiled = p.compile().unwrap();
    let (sink, mut rx) = EvidenceSink::channel(256);
    let run = Executor::new(Runtime::new(faulty(true)))
        .start_recorded(
            p.clone(),
            true,
            uuid::Uuid::new_v4().to_string(),
            Some(sink),
            None,
        )
        .unwrap();
    let mut r = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let r = run.snapshot();
            if r.state.starts_with("saving_") {
                break r;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(r.verdict, "fail");
    assert_eq!(r.receivers[0].crc_errors, u64::from(compiled.count));
    assert_eq!(r.receivers[0].unique, 0);
    r.state = r.state.strip_prefix("saving_").unwrap().to_owned();
    let mut replay = Replay::new(&p).unwrap();
    while let Ok(event) = rx.try_recv() {
        replay.event(&event).unwrap();
    }
    replay.verify_report(&r).unwrap();
}
#[tokio::test(start_paused = true)]
async fn unknown_response_keeps_executor_reserved_and_is_never_retried_after_stop() {
    let backend = faulty(false);
    let mut p = plan();
    p.timing = Some(TimingOptions {
        sample_every: 1,
        max_pending: 4,
    });
    p.rtt = Some(RttOptions {
        role: RttRole::Loopback,
        response_can_id: 292,
        timeout_ms: 100,
        max_inflight: 32,
    });
    let executor = Executor::new(Runtime::new(backend.clone()));
    let run = executor.start(p.clone(), true).unwrap();
    backend.entered.notified().await;
    run.stop();
    let r = final_report(&run).await;
    assert_ne!(r.verdict, "pass");
    assert_eq!(r.rtt_report.unwrap().responder_unknown, 1);
    assert!(!executor.available());
    assert!(executor.start(p, true).is_err());
    assert_eq!(
        backend.responses.load(std::sync::atomic::Ordering::SeqCst),
        1
    );
    backend.release.add_permits(1);
    for _ in 0..50 {
        tokio::task::yield_now().await;
    }
    assert!(executor.available());
    assert_eq!(
        backend.responses.load(std::sync::atomic::Ordering::SeqCst),
        1
    );
}

#[tokio::test]
async fn data_lane_never_upgrades_classic_configuration_or_survives_restart() {
    let rt = Runtime::new(Arc::new(VirtualBus::new(8192)));
    let view = rt
        .create_configured(
            "virtual:0",
            canharbor_core::Config {
                nominal_bitrate: 500000,
                data_bitrate: None,
                listen_only: false,
            },
        )
        .await
        .unwrap();
    let lease = rt.acquire(&view.id, "p4").await.unwrap();
    rt.running(&view.id, "p4", lease.generation, true)
        .await
        .unwrap();
    let frame =
        Frame::classic(canharbor_core::CanId::new(291, false).unwrap(), vec![0; 8]).unwrap();
    let lane = rt
        .data_lane(&view.id, "p4", lease.generation, frame.clone())
        .await
        .unwrap();
    assert!(lane.prepare(plan().frame(0)).is_err());
    rt.running(&view.id, "p4", lease.generation, false)
        .await
        .unwrap();
    rt.running(&view.id, "p4", lease.generation, true)
        .await
        .unwrap();
    assert!(lane.prepare(frame).is_err());
    rt.close(&view.id, "p4", lease.generation).await.unwrap();
}
#[test]
fn invalid_fd_envelopes_and_resource_plans_are_rejected_before_io() {
    for length in [0, 7, 9, 31, 33, 65] {
        let mut p = plan();
        if let Traffic::FdV1 { variants, .. } = &mut p.traffic {
            variants[0].length = length;
        }
        assert!(p.compile().is_err());
    }
    let mut p = plan();
    p.can_id = 0x800;
    assert!(p.compile().is_err());
    p = plan();
    p.rtt = Some(RttOptions {
        role: RttRole::Loopback,
        response_can_id: 291,
        timeout_ms: 100,
        max_inflight: 32,
    });
    assert!(p.compile().is_err());
    p.rtt.as_mut().unwrap().response_can_id = 292;
    p.fps = 1001;
    assert!(p.compile().is_err());
}

#[tokio::test(start_paused = true)]
async fn fd_rtt_staircase_keeps_stage_and_response_budgets_consistent() {
    let mut p = plan();
    p.count = 0;
    p.fps = 0;
    p.stages = vec![
        Stage {
            name: "ramp".into(),
            fps: 10,
            duration_seconds: 1,
            warmup_seconds: 1,
        },
        Stage {
            name: "peak".into(),
            fps: 20,
            duration_seconds: 1,
            warmup_seconds: 0,
        },
    ];
    p.rtt = Some(RttOptions {
        role: RttRole::Loopback,
        response_can_id: 292,
        timeout_ms: 100,
        max_inflight: 32,
    });
    let run = Executor::new(Runtime::new(Arc::new(VirtualBus::new(8192))))
        .start(p, true)
        .unwrap();
    let r = final_report(&run).await;
    assert_eq!(r.verdict, "pass", "{r:?}");
    assert_eq!(r.submitted, 40);
    assert_eq!(
        r.stages.iter().map(|s| s.submitted).collect::<Vec<_>>(),
        vec![10, 10, 20]
    );
    assert_eq!(r.rtt_report.unwrap().responder_submitted, 40);
}

#[tokio::test(start_paused = true)]
async fn delayed_commit_cannot_extend_its_prepared_deadline_or_issue_queued_io_after_expiry() {
    let rt = Runtime::new(Arc::new(VirtualBus::new(8192)));
    let executor = Executor::new(rt.clone());
    let mut p = plan();
    p.coordination = Some(Coordination {
        group_id: uuid::Uuid::new_v4().to_string(),
        participant: "bounded".into(),
        lease_ms: 3000,
        start_delay_ms: 2000,
        clock_quality: "unsynchronized".into(),
    });
    assert!(
        executor
            .start_coordinated(
                p.clone(),
                true,
                uuid::Uuid::new_v4().to_string(),
                None,
                None,
                tokio::time::Instant::now() - Duration::from_millis(1)
            )
            .is_err()
    );
    assert!(rt.list().await.is_empty());
    let run = executor
        .start_coordinated(
            p,
            true,
            uuid::Uuid::new_v4().to_string(),
            None,
            None,
            tokio::time::Instant::now() + Duration::from_millis(500),
        )
        .unwrap();
    let r = final_report(&run).await;
    assert_eq!(r.state, "aborted");
    assert_eq!(r.submitted, 0);
    assert!(run.renew_coordination().is_err());
    let view = rt
        .create("virtual:0", canharbor_core::runtime::Mode::Active)
        .await
        .unwrap();
    let lease = rt.acquire(&view.id, "gate").await.unwrap();
    rt.running(&view.id, "gate", lease.generation, true)
        .await
        .unwrap();
    let prepared = rt
        .prepare_send(&view.id, "gate", lease.generation, plan().frame(0))
        .await
        .unwrap();
    prepared.restrict_until(std::time::Instant::now());
    prepared.restrict_until(std::time::Instant::now() + Duration::from_secs(15));
    assert!(rt.submit_prepared(prepared).await.is_err());
    rt.close(&view.id, "gate", lease.generation).await.unwrap();
}

#[tokio::test]
async fn job_gate_revocation_reaches_the_backend_and_cannot_be_rebound() {
    let bus = Arc::new(VirtualBus::new(8192));
    let rt = Runtime::new(bus.clone());
    let s = rt
        .create("virtual:0", canharbor_core::runtime::Mode::Active)
        .await
        .unwrap();
    let l = rt.acquire(&s.id, "audit").await.unwrap();
    rt.running(&s.id, "audit", l.generation, true)
        .await
        .unwrap();
    let prepared = rt
        .prepare_send(&s.id, "audit", l.generation, plan().frame(0))
        .await
        .unwrap();
    let gate = canharbor_core::control::ExecutionGate::default();
    prepared.bind_gate(&gate).unwrap();
    gate.revoke();
    assert!(prepared.bind_gate(&Default::default()).is_err());
    assert!(rt.submit_prepared(prepared).await.is_err());
    rt.close(&s.id, "audit", l.generation).await.unwrap();
}
#[tokio::test(start_paused = true)]
async fn terminal_state_cannot_regress_on_late_stop_or_transition() {
    let run = Executor::new(Runtime::new(Arc::new(VirtualBus::new(8192))))
        .start(plan(), true)
        .unwrap();
    let report = final_report(&run).await;
    for state in ["cancelling", "running", "finalizing", "stopping_blocked"] {
        run.transition(state);
        run.stop();
        assert_eq!(run.state(), report.state);
    }
}
#[tokio::test(start_paused = true)]
async fn responder_only_stops_on_failed_evidence_and_does_not_wait_for_requests() {
    let mut p = plan();
    p.count = 1000;
    p.rtt = Some(RttOptions {
        role: RttRole::Responder,
        response_can_id: 292,
        timeout_ms: 100,
        max_inflight: 32,
    });
    let (sink, _rx) = EvidenceSink::channel(256);
    let run = Executor::new(Runtime::new(Arc::new(VirtualBus::new(8192))))
        .start_recorded(
            p,
            true,
            uuid::Uuid::new_v4().to_string(),
            Some(sink.clone()),
            None,
        )
        .unwrap();
    while run.state() != "running" {
        tokio::task::yield_now().await;
    }
    sink.fail("injected disk failure");
    let r = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let r = run.snapshot();
            if r.state.starts_with("saving_") {
                break r;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(r.state, "saving_failed");
    assert_eq!(r.submitted, 0);
    assert_ne!(r.verdict, "pass");
}
