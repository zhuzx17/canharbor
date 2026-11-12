// SPDX-License-Identifier: AGPL-3.0-or-later
use canharbor_core::runtime::Runtime;
use canharbor_stress::*;
use canharbor_virtual::VirtualBus;
use std::{sync::Arc, time::Duration};
fn plan() -> Plan {
    serde_json::from_value(serde_json::json!({"sender":"virtual:0","receivers":["virtual:1"],"can_id":291,"extended":false,"run_tag":17,"count":12,"fps":20,"bitrate":500000,"receivers_active":true,"drain_ms":20,"evidence":{"sample_every":1,"max_bytes":268435456,"segment_bytes":4194304},"traffic":{"format":"classic_pattern_v1","checkpoint_can_id":292,"seed":42,"stream":7,"max_pending":4}})).unwrap()
}
async fn end(run: &Run) -> Report {
    tokio::time::timeout(Duration::from_secs(10), async {
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
fn replay_report(p: &Plan, mut r: Report, events: &[EvidenceEvent]) {
    r.state = r.state.strip_prefix("saving_").unwrap_or(&r.state).into();
    let mut replay = Replay::new(p).unwrap();
    for e in events {
        replay.event(e).unwrap();
    }
    replay.verify_report(&r).unwrap();
    let mut forged = r.clone();
    forged
        .classic_pattern_transmit
        .as_mut()
        .unwrap()
        .checkpoint_submitted -= 1;
    assert!(replay.verify_report(&forged).is_err());
    let mut forged = r;
    forged.receivers[0]
        .classic_pattern
        .as_mut()
        .unwrap()
        .data_verified_unique += 1;
    assert!(replay.verify_report(&forged).is_err());
}
#[tokio::test(start_paused = true)]
async fn physical_slots_and_full_raw_replay_include_checkpoints_and_host_timing() {
    let mut p = plan();
    p.timing = Some(TimingOptions {
        sample_every: 1,
        max_pending: 4,
    });
    let rt = Runtime::new(Arc::new(VirtualBus::new(8192)));
    let (sink, mut rx) = EvidenceSink::channel(2048);
    let run = Executor::new(rt.clone())
        .start_recorded(
            p.clone(),
            true,
            uuid::Uuid::new_v4().to_string(),
            Some(sink),
            None,
        )
        .unwrap();
    let r = end(&run).await;
    assert_eq!(r.verdict, "pass", "{r:?}");
    assert_eq!(r.version, 9);
    assert_eq!(r.planned, 12);
    assert_eq!(r.submitted, 12);
    let tx = r.classic_pattern_transmit.as_ref().unwrap();
    assert_eq!(tx.checkpoint_submitted, 6);
    assert_eq!(tx.data_submitted, 6);
    let v = r.receivers[0].classic_pattern.as_ref().unwrap();
    assert_eq!(v.checkpoints_unique, 6);
    assert_eq!(v.data_verified_unique, 6);
    assert!(v.passed());
    assert!(v.window_high_water <= 4);
    assert_eq!(r.timing.as_ref().unwrap().receivers[0].matched, 12);
    assert!(rt.list().await.is_empty());
    let mut events = vec![];
    while let Ok(e) = rx.try_recv() {
        events.push(e);
    }
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, EvidenceEvent::PatternFrame { .. }))
            .count(),
        12
    );
    replay_report(&p, r.clone(), &events);
    // Removing the first checkpoint cannot be hidden behind an aggregate that still claims verification.
    let mut replay = Replay::new(&p).unwrap();
    let mut removed = false;
    for e in &events {
        if !removed && matches!(e,EvidenceEvent::PatternFrame{wire,..} if wire.can_id==292) {
            removed = true;
            continue;
        }
        replay.event(e).unwrap();
    }
    let mut r = r;
    r.state = "completed".into();
    assert!(replay.verify_report(&r).is_err());
}
#[test]
fn malformed_plans_and_control_producer_collisions_fail_before_io() {
    let p = plan();
    assert_eq!(p.compile().unwrap().max_fps, 20);
    assert_eq!(p.producers().iter().map(|p| p.3).sum::<u32>(), 20);
    for (count, fps, sample) in [
        (11, 20, 1),
        (12, 21, 1),
        (12, 20, 0),
        (12, 20, 2),
        (12, 2002, 1),
    ] {
        let mut p = p.clone();
        p.count = count;
        p.fps = fps;
        p.evidence.sample_every = sample;
        assert!(p.compile().is_err());
    }
    let mut p = p.clone();
    p.traffic = Traffic::ClassicPatternV1 {
        checkpoint_can_id: 291,
        seed: 42,
        stream: 7,
        max_pending: 4,
    };
    assert!(p.compile().is_err());
    let mut p = plan();
    p.flows.push(serde_json::from_value(serde_json::json!({"name":"reverse","sender":"virtual:1","receivers":["virtual:0"],"can_id":292,"extended":false,"run_tag":18,"count":12,"fps":20})).unwrap());
    assert!(p.compile().unwrap_err().contains("producers"));
}
#[tokio::test(start_paused = true)]
async fn pattern_and_sequence_flows_keep_independent_physical_ledgers() {
    let mut p = plan();
    p.flows.push(serde_json::from_value(serde_json::json!({"name":"reverse","sender":"virtual:1","receivers":["virtual:0"],"can_id":128,"extended":false,"run_tag":18,"count":12,"fps":20})).unwrap());
    assert_eq!(p.group_budget().unwrap().peak_transmit_fps, 40);
    let (sink, mut rx) = EvidenceSink::channel(2048);
    let rt = Runtime::new(Arc::new(VirtualBus::new(8192)));
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
    assert_eq!(r.verdict, "pass", "{r:?}");
    assert_eq!(r.submitted, 24);
    assert!(r.classic_pattern_transmit.is_none());
    let flows = &r.group.as_ref().unwrap().flows;
    assert!(
        flows
            .iter()
            .find(|f| f.name == "reverse")
            .unwrap()
            .report
            .receivers[0]
            .classic_pattern
            .is_none()
    );
    let mut replay = Replay::new(&p).unwrap();
    while let Ok(e) = rx.try_recv() {
        replay.event(&e).unwrap();
    }
    replay.verify_report(&r).unwrap();
    assert!(rt.list().await.is_empty());
}
#[tokio::test(start_paused = true)]
async fn stop_between_checkpoint_and_data_revokes_both_lanes_without_retry() {
    let mut p = plan();
    p.fps = 2;
    p.count = 20;
    let rt = Runtime::new(Arc::new(VirtualBus::new(8192)));
    let (sink, mut rx) = EvidenceSink::channel(2048);
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
    run.stop();
    let r = end(&run).await;
    assert_eq!(r.submitted, 1);
    assert_eq!(
        r.classic_pattern_transmit
            .as_ref()
            .unwrap()
            .checkpoint_submitted,
        1
    );
    assert_eq!(
        r.classic_pattern_transmit.as_ref().unwrap().data_submitted,
        0
    );
    assert_ne!(r.verdict, "pass");
    assert!(rt.list().await.is_empty());
    let mut events = vec![];
    while let Ok(e) = rx.try_recv() {
        events.push(e);
    }
    replay_report(&p, r, &events);
}

struct FaultBus {
    bus: VirtualBus,
    mode: u8,
}
impl canharbor_core::Backend for FaultBus {
    fn capabilities(&self, c: &str) -> Result<canharbor_core::Capabilities, canharbor_core::Error> {
        self.bus.capabilities(c)
    }
    fn channels(&self) -> Vec<canharbor_core::Channel> {
        self.bus.channels()
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
            p.check()?;
            self.transmit(c, f)
        })
    }
    fn transmit(&self, c: &str, mut f: canharbor_core::Frame) -> Result<(), canharbor_core::Error> {
        if self.mode == 0 && f.id().value() == 292 && f.data()[4..] == 0u32.to_be_bytes() {
            return Ok(());
        }
        if self.mode == 1 && f.id().value() == 291 {
            let canharbor_core::Frame::ClassicData { data, .. } = &mut f else {
                unreachable!()
            };
            data[7] ^= 1;
        }
        self.bus.transmit(c, f)
    }
}
#[tokio::test(start_paused = true)]
async fn lost_controls_and_corrupted_ambiguous_data_are_hard_failures_even_with_quality_tolerance()
{
    for mode in [0, 1] {
        let mut p = plan();
        p.criteria.max_missing_expected_ppm = 999999;
        p.criteria.max_missing_submitted_ppm = 999999;
        let rt = Runtime::new(Arc::new(FaultBus {
            bus: VirtualBus::new(8192),
            mode,
        }));
        let (sink, mut rx) = EvidenceSink::channel(2048);
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
        assert_eq!(r.submitted, 12);
        assert_eq!(r.verdict, "fail", "{r:?}");
        assert!(!r.passing_invariants());
        let v = r.receivers[0].classic_pattern.as_ref().unwrap();
        assert!(
            r.assessment.as_ref().unwrap().passed,
            "quality tolerance must not mask a checkpoint integrity failure"
        );
        if mode == 0 {
            assert_eq!(v.data_verified_unique, 5);
            assert_eq!(v.unverified_data, 1);
            assert_eq!(v.missing_checkpoints, 1);
        } else {
            assert_eq!(v.data_verified_unique, 0);
            assert_eq!(v.unverified_data, 6);
            assert!(v.ambiguous_data > 0);
            assert_eq!(v.missing_checkpoints, 0);
        }
        r.state = "completed".into();
        let mut replay = Replay::new(&p).unwrap();
        while let Ok(e) = rx.try_recv() {
            replay.event(&e).unwrap();
        }
        replay.verify_report(&r).unwrap();
        assert!(rt.list().await.is_empty());
    }
}
