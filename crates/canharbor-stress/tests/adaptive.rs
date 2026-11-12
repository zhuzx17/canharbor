// SPDX-License-Identifier: AGPL-3.0-or-later
use canharbor_core::runtime::Runtime;
use canharbor_stress::*;
use canharbor_virtual::VirtualBus;
use std::{sync::Arc, time::Duration};
fn plan() -> Plan {
    serde_json::from_value(serde_json::json!({"sender":"virtual:0","receivers":["virtual:1"],"can_id":291,"extended":false,"run_tag":17,"count":0,"fps":0,"bitrate":500000,"receivers_active":true,"drain_ms":20,"adaptive":{"rates":[2,4,8],"exploration_seconds":1,"measurement_seconds":2,"min_observed_ppm":950000},"evidence":{"sample_every":1,"max_bytes":268435456,"segment_bytes":4194304}})).unwrap()
}
async fn end(run: &Run) -> Report {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let r = run.snapshot();
            if r.state.starts_with("saving_")
                || matches!(r.state.as_str(), "completed" | "failed" | "aborted")
            {
                return r;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap()
}
#[tokio::test(start_paused = true)]
async fn all_candidates_then_fixed_measurement_share_one_reservation_and_replay() {
    let p = plan();
    let rt = Runtime::new(Arc::new(VirtualBus::new(8192)));
    let executor = Executor::new(rt.clone());
    let (sink, mut rx) = EvidenceSink::channel(4096);
    let run = executor
        .start_recorded(
            p.clone(),
            true,
            uuid::Uuid::new_v4().to_string(),
            Some(sink),
            None,
        )
        .unwrap();
    let mut r = end(&run).await;
    r.state = r.state.strip_prefix("saving_").unwrap_or(&r.state).into();
    assert_eq!(r.verdict, "pass", "{r:?}");
    assert!(r.adaptive_passing_invariants(), "{r:?}");
    let a = r.adaptive.as_ref().unwrap();
    assert_eq!(a.selected_fps, Some(8));
    assert_eq!(a.steps.len(), 4);
    assert_eq!(a.steps[3].report.plan.count, 16);
    assert_eq!(r.submitted, 30);
    assert_eq!(r.backend_cleanup.len(), 2);
    assert!(rt.list().await.is_empty());
    assert!(executor.available());
    let mut replay = Replay::new(&p).unwrap();
    while let Ok(e) = rx.try_recv() {
        replay.event(&e).unwrap();
    }
    replay.verify_report(&r).unwrap();
    let mut bad = r.clone();
    bad.adaptive.as_mut().unwrap().selected_fps = Some(2);
    assert!(replay.verify_report(&bad).is_err());
    let mut impossible = r.adaptive.as_ref().unwrap().steps[0]
        .report
        .as_ref()
        .clone();
    let end = impossible.load.as_ref().unwrap().end_ns;
    impossible.lifecycle.last_submitted_ns = Some(end + 1);
    assert!(!impossible.load.as_ref().unwrap().valid(&impossible));
    let mut bad = r;
    bad.adaptive.as_mut().unwrap().steps[0]
        .decision
        .as_mut()
        .unwrap()
        .reason = "fake".into();
    assert!(replay.verify_report(&bad).is_err());
}
#[test]
fn shapes_and_static_budgets_are_predeclared_and_reproducible() {
    let mut p = plan().adaptive_step_plan("explore-0", None).unwrap();
    let legacy = p.fingerprint();
    assert!(serde_json::to_value(&p).unwrap().get("shape").is_none());
    p.shape = LoadShape::Uniform;
    assert_eq!(p.fingerprint(), legacy);
    p.count = 5;
    p.fps = 10;
    p.shape = LoadShape::Burst { frames: 4 };
    assert_eq!(p.compile().unwrap().duration_ns, 500_000_000);
    p.count = 20;
    p.fps = 20;
    p.shape = LoadShape::Burst { frames: 8 };
    let c = p.compile().unwrap();
    assert_eq!(c.max_fps, 800);
    assert_eq!(
        (0..8).map(|n| c.target_ns(n)).collect::<Vec<_>>(),
        vec![0; 8]
    );
    assert_eq!(c.target_ns(8), 400_000_000);
    p.shape = LoadShape::SeededIntervals {
        seed: 42,
        jitter_ppm: 500000,
    };
    let c = p.compile().unwrap();
    let times = (0..20).map(|n| c.deadline_ns(n)).collect::<Vec<_>>();
    assert!(times.windows(2).all(|w| w[1] > w[0]));
    assert_eq!(
        times,
        (0..20)
            .map(|n| p.compile().unwrap().deadline_ns(n))
            .collect::<Vec<_>>()
    );
    assert!(times.windows(2).any(|w| w[1] - w[0] != 50_000_000));
    p.shape = LoadShape::SeededIntervals {
        seed: 43,
        jitter_ppm: 500000,
    };
    assert_ne!(
        times,
        (0..20)
            .map(|n| p.compile().unwrap().deadline_ns(n))
            .collect::<Vec<_>>()
    );
    for shape in [
        LoadShape::Burst { frames: 0 },
        LoadShape::Burst { frames: 201 },
        LoadShape::SeededIntervals {
            seed: 0,
            jitter_ppm: 500001,
        },
    ] {
        p.shape = shape;
        assert!(p.compile().is_err());
    }
    let mut p = plan();
    p.adaptive.as_mut().unwrap().rates = vec![4, 2];
    assert!(p.compile().is_err());
    let mut p = plan();
    p.count = 2;
    assert!(p.compile().is_err());
    let mut p = plan().adaptive_step_plan("explore-0", None).unwrap();
    p.traffic = Traffic::FdV1 {
        data_bitrate: 2000000,
        stream: 1,
        epoch: 1,
        seed: 42,
        variants: vec![FdVariant {
            length: 32,
            brs: false,
        }],
    };
    p.rtt = Some(RttOptions {
        role: RttRole::Loopback,
        response_can_id: 292,
        timeout_ms: 100,
        max_inflight: 4,
    });
    assert!(p.compile().is_ok());
    p.shape = LoadShape::Burst { frames: 2 };
    assert!(p.compile().unwrap_err().contains("asynchronous RTT"));
}
#[tokio::test(start_paused = true)]
async fn stopping_exploration_never_selects_or_starts_a_measurement() {
    let p = plan();
    let rt = Runtime::new(Arc::new(VirtualBus::new(8192)));
    let (sink, mut rx) = EvidenceSink::channel(4096);
    let run = Executor::new(rt.clone())
        .start_recorded(
            p.clone(),
            true,
            uuid::Uuid::new_v4().to_string(),
            Some(sink),
            None,
        )
        .unwrap();
    loop {
        if run.snapshot().submitted >= 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    run.stop();
    let mut r = end(&run).await;
    r.state = r.state.strip_prefix("saving_").unwrap_or(&r.state).into();
    assert_ne!(r.verdict, "pass");
    assert_eq!(r.adaptive.as_ref().unwrap().steps.len(), 1);
    assert!(r.adaptive.as_ref().unwrap().selected_fps.is_none());
    assert!(rt.list().await.is_empty());
    let mut replay = Replay::new(&p).unwrap();
    while let Ok(e) = rx.try_recv() {
        replay.event(&e).unwrap();
    }
    replay.verify_report(&r).unwrap();
}

struct FaultBus {
    bus: VirtualBus,
    mode: u8,
    gate: tokio::sync::Semaphore,
}
impl canharbor_core::Backend for FaultBus {
    fn channels(&self) -> Vec<canharbor_core::Channel> {
        self.bus.channels()
    }
    fn capabilities(&self, c: &str) -> Result<canharbor_core::Capabilities, canharbor_core::Error> {
        self.bus.capabilities(c)
    }
    fn subscribe(
        &self,
        c: &str,
    ) -> Result<
        tokio::sync::broadcast::Receiver<Arc<canharbor_core::BackendEvent>>,
        canharbor_core::Error,
    > {
        self.bus.subscribe(c)
    }
    fn check_channel(&self, c: &str) -> Result<u64, canharbor_core::Error> {
        self.bus.check_channel(c)
    }
    fn configure<'a>(
        &'a self,
        c: &'a str,
        v: canharbor_core::Config,
    ) -> canharbor_core::BackendFuture<'a, ()> {
        self.bus.configure(c, v)
    }
    fn start<'a>(
        &'a self,
        c: &'a str,
        p: canharbor_core::control::Permit,
    ) -> canharbor_core::BackendFuture<'a, ()> {
        self.bus.start(c, p)
    }
    fn stop<'a>(&'a self, c: &'a str) -> canharbor_core::BackendFuture<'a, ()> {
        self.bus.stop(c)
    }
    fn close<'a>(&'a self, c: &'a str) -> canharbor_core::BackendFuture<'a, ()> {
        self.bus.close(c)
    }
    fn submit<'a>(
        &'a self,
        c: &'a str,
        f: canharbor_core::Frame,
        p: canharbor_core::control::Permit,
    ) -> canharbor_core::BackendFuture<'a, ()> {
        Box::pin(async move {
            if self.mode == 4 {
                tokio::time::sleep(Duration::from_millis(600)).await;
            }
            if self.mode == 2 {
                let _ = self.gate.acquire().await.unwrap();
            }
            p.check()?;
            if self.mode == 3 {
                return Err(canharbor_core::Error::Unsupported);
            }
            self.transmit(c, f)
        })
    }
    fn transmit(&self, c: &str, f: canharbor_core::Frame) -> Result<(), canharbor_core::Error> {
        let tag = u32::from_be_bytes(f.data()[..4].try_into().unwrap());
        if self.mode == 0 && tag == 19 || self.mode == 1 && tag == 21 {
            return Ok(());
        }
        self.bus.transmit(c, f)
    }
}
#[tokio::test(start_paused = true)]
async fn a_failed_candidate_selects_the_previous_rate_and_measurement_never_downgrades() {
    for mode in [0, 1] {
        let p = plan();
        let rt = Runtime::new(Arc::new(FaultBus {
            bus: VirtualBus::new(8192),
            mode,
            gate: tokio::sync::Semaphore::new(0),
        }));
        let (sink, mut rx) = EvidenceSink::channel(4096);
        let run = Executor::new(rt.clone())
            .start_recorded(
                p.clone(),
                true,
                uuid::Uuid::new_v4().to_string(),
                Some(sink),
                None,
            )
            .unwrap();
        let mut r = end(&run).await;
        r.state = "completed".into();
        let a = r.adaptive.as_ref().unwrap();
        if mode == 0 {
            assert_eq!(a.selected_fps, Some(2));
            assert_eq!(a.steps.len(), 3);
            assert_eq!(
                a.steps[1].decision.as_ref().unwrap().reason,
                "frozen_quality_or_workload_not_met"
            );
            assert_eq!(r.verdict, "pass", "{r:?}");
        } else {
            assert_eq!(a.selected_fps, Some(8));
            assert_eq!(a.steps.len(), 4);
            assert_eq!(a.steps.last().unwrap().report.plan.fps, 8);
            assert_eq!(r.verdict, "fail");
        }
        let mut replay = Replay::new(&p).unwrap();
        while let Ok(e) = rx.try_recv() {
            replay.event(&e).unwrap();
        }
        replay.verify_report(&r).unwrap();
        assert!(rt.list().await.is_empty());
    }
}
#[tokio::test(start_paused = true)]
async fn unknown_step_holds_original_devices_and_global_slot_and_does_not_probe_again() {
    let p = plan();
    let bus = Arc::new(FaultBus {
        bus: VirtualBus::new(8192),
        mode: 2,
        gate: tokio::sync::Semaphore::new(0),
    });
    let rt = Runtime::new(bus.clone());
    let executor = Executor::new(rt.clone());
    let run = executor.start(p.clone(), true).unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while run.pending_io() == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    run.stop();
    assert!(!Executor::new(rt.clone()).available());
    assert!(executor.start(p, true).is_err());
    assert!(
        rt.reserve_devices(&["virtual:0".into(), "virtual:1".into()])
            .await
            .is_err()
    );
    bus.gate.add_permits(1);
    let r = end(&run).await;
    assert_ne!(r.verdict, "pass");
    assert_eq!(r.submitted, 0);
    assert_eq!(r.adaptive.as_ref().unwrap().steps.len(), 1);
    assert!(rt.list().await.is_empty());
    tokio::time::timeout(Duration::from_secs(2), async {
        while !executor.available() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test(start_paused = true)]
async fn observed_rate_floor_is_independent_of_submission_and_rx_quality() {
    let p = plan();
    let rt = Runtime::new(Arc::new(FaultBus {
        bus: VirtualBus::new(8192),
        mode: 4,
        gate: tokio::sync::Semaphore::new(0),
    }));
    let run = Executor::new(rt).start(p, true).unwrap();
    let r = end(&run).await;
    let a = r.adaptive.unwrap();
    assert_eq!(a.steps.len(), 1);
    assert_eq!(a.selected_fps, None);
    assert_eq!(a.steps[0].report.verdict, "pass");
    assert_eq!(
        a.steps[0].decision.as_ref().unwrap().reason,
        "observed_host_rate_below_declared_floor"
    );
    assert_eq!(r.verdict, "fail");
}
#[tokio::test(start_paused = true)]
async fn stop_after_ready_before_future_release_preserves_zero_submission_and_evidence() {
    let mut p = plan();
    p.coordination=Some(serde_json::from_value(serde_json::json!({"group_id":uuid::Uuid::new_v4().to_string(),"participant":"test","clock_quality":"unsynchronized","lease_ms":5000,"start_delay_ms":2000})).unwrap());
    let (sink, mut rx) = EvidenceSink::channel(4096);
    let run = Executor::new(Runtime::new(Arc::new(VirtualBus::new(8192))))
        .start_recorded(
            p.clone(),
            true,
            uuid::Uuid::new_v4().to_string(),
            Some(sink),
            None,
        )
        .unwrap();
    loop {
        if run.state() == "running"
            && run
                .snapshot()
                .adaptive
                .as_ref()
                .is_some_and(|a| a.steps.first().is_some_and(|s| s.report.state == "running"))
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    run.stop();
    let mut r = end(&run).await;
    r.state = r.state.strip_prefix("saving_").unwrap_or(&r.state).into();
    assert_eq!(r.submitted, 0);
    let mut replay = Replay::new(&p).unwrap();
    while let Ok(e) = rx.try_recv() {
        replay.event(&e).unwrap();
    }
    replay.verify_report(&r).unwrap();
}
#[test]
fn classic_pattern_payload_domains_are_disjoint_between_adaptive_steps() {
    let mut p = plan();
    p.traffic = Traffic::ClassicPatternV1 {
        checkpoint_can_id: 292,
        seed: 42,
        stream: 7,
        max_pending: 4,
    };
    let a = p.adaptive_step_plan("explore-0", None).unwrap();
    let b = p.adaptive_step_plan("measurement", Some(4)).unwrap();
    for i in 0..16 {
        for j in 0..16 {
            assert_ne!(a.pattern_bytes(i), b.pattern_bytes(j));
        }
    }
}

#[tokio::test(start_paused = true)]
async fn bounded_burst_and_seeded_flows_share_global_peak_and_replay_windows() {
    let mut p = plan().adaptive_step_plan("explore-0", None).unwrap();
    p.count = 20;
    p.fps = 20;
    p.shape = LoadShape::Burst { frames: 4 };
    p.flows.push(serde_json::from_value(serde_json::json!({"name":"reverse","sender":"virtual:1","receivers":["virtual:0"],"can_id":128,"extended":false,"run_tag":99,"count":20,"fps":20,"shape":{"format":"seeded_intervals","seed":42,"jitter_ppm":500000}})).unwrap());
    assert_eq!(p.group_budget().unwrap().peak_transmit_fps, 600);
    let (sink, mut rx) = EvidenceSink::channel(4096);
    let run = Executor::new(Runtime::new(Arc::new(VirtualBus::new(8192))))
        .start_recorded(
            p.clone(),
            true,
            uuid::Uuid::new_v4().to_string(),
            Some(sink),
            None,
        )
        .unwrap();
    let mut r = end(&run).await;
    r.state = "completed".into();
    assert_eq!(r.verdict, "pass", "{r:?}");
    assert_eq!(r.submitted, 40);
    assert!(r.passing_invariants());
    for f in &r.group.as_ref().unwrap().flows {
        assert!(f.report.load.as_ref().unwrap().completed);
    }
    let mut replay = Replay::new(&p).unwrap();
    while let Ok(e) = rx.try_recv() {
        replay.event(&e).unwrap();
    }
    replay.verify_report(&r).unwrap();
}
