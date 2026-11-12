// SPDX-License-Identifier: AGPL-3.0-or-later
use canharbor_core::{
    Backend, BackendEvent, BackendFuture, Capabilities, Channel, Config, Error, Frame,
    control::Permit, runtime::Runtime,
};
use canharbor_stress::*;
use canharbor_virtual::VirtualBus;
use std::{sync::Arc, time::Duration};
use tokio::sync::broadcast;
fn plan() -> Plan {
    serde_json::from_value(serde_json::json!({"sender":"virtual:0","receivers":["virtual:1"],"can_id":291,"extended":false,"run_tag":17,"count":5,"fps":20,"bitrate":500000,"receivers_active":true,"drain_ms":20,"timing":{"sample_every":1,"max_pending":2},"topology":{"bus":"virtual bench","wiring":"manual declaration only","aliases":[{"channel":"virtual:0","alias":"A"},{"channel":"virtual:1","alias":"B"}]}})).unwrap()
}
fn points() -> Points {
    Points {
        preparation_started_ns: 0,
        prepared_ns: 10,
        dispatched_ns: Some(20),
        enqueued_ns: Some(30),
        worker_started_ns: Some(40),
        call_started_ns: Some(50),
        call_completed_ns: Some(100),
        completed_ns: Some(110),
        scope: "native_worker_driver_call".into(),
    }
}
fn begin(n: u32) -> TimingEvent {
    TimingEvent::Begin {
        sequence: n,
        response: false,
    }
}
fn settled(n: u32) -> TimingEvent {
    TimingEvent::Settled {
        sequence: n,
        response: false,
        outcome: "submitted".into(),
        points: points(),
    }
}
fn rx(n: u32, at: u64) -> TimingEvent {
    TimingEvent::Received {
        sequence: n,
        channel: "virtual:1".into(),
        received_ns: at,
    }
}
#[test]
fn signed_latency_queue_and_completion_distributions_retain_rx_before_completion() {
    let mut t = TimingState::new(&plan()).unwrap();
    for (n, at) in [(0, 90), (1, 150)] {
        t.apply(&begin(n)).unwrap();
        t.apply(&rx(n, at)).unwrap();
        t.apply(&settled(n)).unwrap();
        t.expect(n, 1, false, "submitted");
    }
    t.apply(&TimingEvent::Finish).unwrap();
    t.verify_outcomes().unwrap();
    let r = t.snapshot();
    assert_eq!(r.request.preparation.max_ns, 10);
    assert_eq!(r.request.worker_queue.max_ns, 10);
    assert_eq!(r.request.submit_call.max_ns, 50);
    assert_eq!(r.request.completion_wait.max_ns, 10);
    assert_eq!(r.request.total.max_ns, 110);
    let p = &r.receivers[0];
    assert_eq!(p.matched, 2);
    assert_eq!(p.unmatched_rx, 0);
    assert_eq!(p.missing_sampled_submitted, 0);
    assert_eq!(p.latency.negative_magnitude.samples, 1);
    assert_eq!(p.latency.nonnegative.samples, 1);
    assert_eq!(p.latency.min_ns, Some(-10));
    assert_eq!(p.latency.max_ns, Some(50));
    assert_eq!(p.latency.p50_upper_ns, Some(0));
    assert_eq!(p.latency.p99_upper_ns, Some(1000));
    assert!(r.finalized);
    assert_eq!(r.pending, 0);
}
#[test]
fn window_eviction_and_very_late_rx_have_explicit_coverage_without_resurrecting_history() {
    let mut p = plan();
    p.timing.as_mut().unwrap().max_pending = 1;
    let mut t = TimingState::new(&p).unwrap();
    t.apply(&begin(0)).unwrap();
    t.apply(&begin(1)).unwrap();
    t.apply(&settled(0)).unwrap();
    t.apply(&rx(0, 200)).unwrap();
    t.apply(&settled(1)).unwrap();
    t.apply(&TimingEvent::Finish).unwrap();
    for n in [0, 1] {
        t.expect(n, 1, false, "submitted");
    }
    t.verify_outcomes().unwrap();
    let r = t.snapshot();
    assert_eq!(r.evicted, 1);
    assert_eq!(r.abandoned, 1);
    assert_eq!(r.window_high_water, 1);
    assert_eq!(r.receivers[0].matched, 0);
    assert_eq!(r.receivers[0].unmatched_rx, 1);
    assert_eq!(r.receivers[0].missing_sampled_submitted, 2);
}
#[test]
fn corrupt_timing_records_are_rejected_transactionally_and_outcomes_are_independently_checked() {
    let mut t = TimingState::new(&plan()).unwrap();
    t.apply(&begin(0)).unwrap();
    let before = t.snapshot();
    let mut p = points();
    p.worker_started_ns = Some(29);
    assert!(
        t.apply(&TimingEvent::Settled {
            sequence: 0,
            response: false,
            outcome: "submitted".into(),
            points: p
        })
        .is_err()
    );
    assert_eq!(before, t.snapshot());
    assert!(
        t.apply(&TimingEvent::Begin {
            sequence: 0,
            response: false
        })
        .is_err()
    );
    assert!(
        t.apply(&TimingEvent::Begin {
            sequence: 1,
            response: true
        })
        .is_err()
    );
    assert!(t.apply(&rx(5, 100)).is_err());
    assert_eq!(before, t.snapshot());
    t.apply(&settled(0)).unwrap();
    t.apply(&rx(0, 90)).unwrap();
    assert!(t.apply(&rx(0, 90)).is_err());
    assert!(t.apply(&settled(0)).is_err());
    t.apply(&TimingEvent::Finish).unwrap();
    assert!(t.apply(&TimingEvent::Finish).is_err());
    assert!(t.verify_outcomes().is_err());
    t.expect(0, 1, false, "submitted");
    t.verify_outcomes().unwrap();
}
#[test]
fn frozen_options_aliases_sampling_and_memory_budgets_are_validated_before_io() {
    let mut p = plan();
    let hash = p.fingerprint();
    assert!(p.compile().is_ok());
    p.timing = None;
    p.topology = None;
    assert_ne!(hash, p.fingerprint());
    let value = serde_json::to_value(&p).unwrap();
    assert!(value.get("timing").is_none());
    assert!(value.get("topology").is_none());
    for sample in [0, 1] {
        let mut p = plan();
        p.fps = 20000;
        p.timing.as_mut().unwrap().sample_every = sample;
        assert!(p.compile().is_err());
    }
    for capacity in [0, 4097] {
        let mut p = plan();
        p.timing.as_mut().unwrap().max_pending = capacity;
        assert!(p.compile().is_err());
    }
    let mut p = plan();
    p.topology.as_mut().unwrap().aliases[1].alias = "A".into();
    assert!(p.compile().is_err());
    p.topology.as_mut().unwrap().aliases[1].alias = "B".into();
    p.topology.as_mut().unwrap().aliases[1].channel = "unselected".into();
    assert!(p.compile().is_err());
}
struct RxBeforeCompletion(VirtualBus);
impl Backend for RxBeforeCompletion {
    fn capabilities(&self, c: &str) -> Result<Capabilities, Error> {
        self.0.capabilities(c)
    }
    fn channels(&self) -> Vec<Channel> {
        self.0.channels()
    }
    fn check_channel(&self, c: &str) -> Result<u64, Error> {
        self.0.check_channel(c)
    }
    fn subscribe(&self, c: &str) -> Result<broadcast::Receiver<Arc<BackendEvent>>, Error> {
        self.0.subscribe(c)
    }
    fn configure<'a>(&'a self, c: &'a str, v: Config) -> BackendFuture<'a, ()> {
        self.0.configure(c, v)
    }
    fn start<'a>(&'a self, c: &'a str, p: Permit) -> BackendFuture<'a, ()> {
        self.0.start(c, p)
    }
    fn stop<'a>(&'a self, c: &'a str) -> BackendFuture<'a, ()> {
        self.0.stop(c)
    }
    fn close<'a>(&'a self, c: &'a str) -> BackendFuture<'a, ()> {
        self.0.close(c)
    }
    fn transmit(&self, c: &str, f: Frame) -> Result<(), Error> {
        self.0.transmit(c, f)
    }
    fn submit<'a>(&'a self, c: &'a str, f: Frame, p: Permit) -> BackendFuture<'a, ()> {
        Box::pin(async move {
            p.check()?;
            self.0.transmit(c, f)?;
            tokio::time::sleep(Duration::from_millis(2)).await;
            Ok(())
        })
    }
}
#[tokio::test(start_paused = true)]
async fn virtual_negative_rx_latency_sources_and_full_independent_replay_agree() {
    let p = plan();
    let runtime = Runtime::new(Arc::new(RxBeforeCompletion(VirtualBus::default())));
    let (sink, mut events) = EvidenceSink::channel(256);
    let run = Executor::new(runtime)
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
            if r.state == "saving_completed" {
                break r;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(r.verdict, "pass", "{r:?}");
    let timing = r.timing.as_ref().unwrap();
    assert_eq!(timing.request.submitted, 5);
    assert_eq!(timing.request.worker_queue.samples, 0);
    assert_eq!(timing.request.worker_queue_unknown, 5);
    assert_eq!(timing.receivers[0].matched, 5);
    assert_eq!(timing.receivers[0].latency.negative_magnitude.samples, 5);
    assert_eq!(timing.receivers[0].latency.min_ns, Some(-2_000_000));
    assert_eq!(timing.evicted, 0);
    let source = r.provenance.as_ref().unwrap();
    assert!(source.valid(&p));
    assert!(
        source
            .executable
            .sha256
            .as_ref()
            .is_some_and(|h| h.len() == 64)
    );
    assert_eq!(source.channels.len(), 2);
    assert!(
        source
            .channels
            .iter()
            .all(|c| c.libraries.is_empty() && c.simulated)
    );
    let mut replay = Replay::new(&p).unwrap();
    while let Ok(e) = events.try_recv() {
        replay.event(&e).unwrap();
    }
    r.state = "completed".into();
    replay.verify_report(&r).unwrap();
    assert!(r.passing_invariants());
    let mut forged = r.clone();
    forged.timing.as_mut().unwrap().receivers[0].matched -= 1;
    assert!(replay.verify_report(&forged).is_err());
    let mut forged = r.clone();
    forged.provenance.as_mut().unwrap().executable.sha256 = Some("0".repeat(64));
    assert!(replay.verify_report(&forged).is_err());
    let mut forged = r.clone();
    forged.version = 6;
    assert!(replay.verify_report(&forged).is_err());
}

#[test]
fn probe_and_responder_roles_cannot_claim_each_others_timing_evidence() {
    let mut p = plan();
    p.traffic = Traffic::FdV1 {
        data_bitrate: 2_000_000,
        stream: 1,
        epoch: 1,
        seed: 42,
        variants: vec![FdVariant {
            length: 32,
            brs: true,
        }],
    };
    p.rtt = Some(RttOptions {
        role: RttRole::Probe,
        response_can_id: 292,
        timeout_ms: 100,
        max_inflight: 32,
    });
    let mut t = TimingState::new(&p).unwrap();
    assert!(
        t.apply(&TimingEvent::Begin {
            sequence: 0,
            response: true
        })
        .is_err()
    );
    let mut replay = Replay::new(&p).unwrap();
    assert!(
        replay
            .event(&EvidenceEvent::Reply {
                sequence: 0,
                outcome: "submitted".into()
            })
            .is_err()
    );
    p.rtt.as_mut().unwrap().role = RttRole::Responder;
    let mut t = TimingState::new(&p).unwrap();
    assert!(t.apply(&begin(0)).is_err());
    t.apply(&TimingEvent::Begin {
        sequence: 0,
        response: true,
    })
    .unwrap();
    let mut replay = Replay::new(&p).unwrap();
    assert!(
        replay
            .event(&EvidenceEvent::Rtt {
                sequence: 0,
                sent_ns: 10,
                received_ns: 20
            })
            .is_err()
    );
}
