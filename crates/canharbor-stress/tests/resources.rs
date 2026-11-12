// SPDX-License-Identifier: AGPL-3.0-or-later
use canharbor_core::runtime::Runtime;
use canharbor_stress::*;
use canharbor_virtual::VirtualBus;
use std::{sync::Arc, time::Duration};
fn plan() -> Plan {
    serde_json::from_value(serde_json::json!({"sender":"virtual:0","receivers":["virtual:1"],"can_id":291,"extended":false,"run_tag":17,"count":160,"fps":100,"bitrate":500000,"receivers_active":true,"drain_ms":20,"tracking":{"format":"sliding","capacity":64},"queues":{"sample_every":3},"evidence":{"max_bytes":268435456,"segment_bytes":4194304,"sample_every":1}})).unwrap()
}
async fn recorded(p: &Plan) -> (Report, Vec<EvidenceEvent>) {
    let executor = Executor::new(Runtime::new(Arc::new(VirtualBus::new(8192))));
    let (sink, mut rx) = EvidenceSink::channel(8192);
    let run = executor
        .start_recorded(
            p.clone(),
            true,
            uuid::Uuid::new_v4().to_string(),
            Some(sink),
            None,
        )
        .unwrap();
    let r = tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let r = run.snapshot();
            if r.state.starts_with("saving_") {
                break r;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let mut events = vec![];
    while let Ok(e) = rx.try_recv() {
        events.push(e);
    }
    (r, events)
}
fn replay(p: &Plan, events: &[EvidenceEvent]) -> Result<Replay, String> {
    let mut r = Replay::new(p)?;
    for e in events {
        r.event(e)?;
    }
    Ok(r)
}
#[tokio::test(start_paused = true)]
async fn ordered_window_queue_evidence_replays_and_rejects_tampering() {
    let p = plan();
    let (mut r, events) = recorded(&p).await;
    r.state = "completed".into();
    assert_eq!(r.version, 11);
    assert_eq!(r.verdict, "pass", "{r:?}");
    assert!(r.passing_invariants());
    assert_eq!(r.receivers[0].unique, 160);
    let w = r.receivers[0].tracking.as_ref().unwrap();
    assert!(w.base > 0);
    assert_eq!(w.finalized_missing_submitted, 0);
    let q = r.receivers[0].queues.as_ref().unwrap();
    assert_eq!(q.samples, 54);
    assert!(q.observed_high_water > 0);
    assert_eq!(q.residence.samples, 54);
    replay(&p, &events).unwrap().verify_report(&r).unwrap();
    for mode in 0..4 {
        let mut changed = events.clone();
        let mut altered = false;
        for e in &mut changed {
            if let EvidenceEvent::TrackingRecord { event } = e {
                match event.as_mut() {
                    EvidenceEvent::TrackingAdvance { next_sequence, .. } if mode == 0 => {
                        *next_sequence += 1;
                        altered = true;
                    }
                    EvidenceEvent::QueueSample { ordinal, .. } if mode == 1 => {
                        *ordinal += 1;
                        altered = true;
                    }
                    EvidenceEvent::QueueSample { depth, .. } if mode == 2 => {
                        *depth = 0;
                        altered = true;
                    }
                    EvidenceEvent::QueueSample { processed_ns, .. } if mode == 3 => {
                        *processed_ns = 0;
                        altered = true;
                    }
                    _ => {}
                }
            }
            if altered {
                break;
            }
        }
        assert!(altered);
        assert!(
            replay(&p, &changed)
                .and_then(|v| v.verify_report(&r))
                .is_err()
        );
    }
    let mut missing = events.clone();
    let index=missing.iter().position(|e|matches!(e,EvidenceEvent::TrackingRecord{event} if matches!(event.as_ref(),EvidenceEvent::TrackingAdvance{..}))).unwrap();
    missing.remove(index);
    assert!(replay(&p, &missing).is_err());
    let raw=events.iter().find(|e|matches!(e,EvidenceEvent::TrackingRecord{event} if matches!(event.as_ref(),EvidenceEvent::Sample{..}))).unwrap();
    assert!(replay(&p, &events).unwrap().event(raw).is_err());
    let unwrapped = events
        .iter()
        .find_map(|e| {
            if let EvidenceEvent::TrackingRecord { event } = e {
                Some(event.as_ref())
            } else {
                None
            }
        })
        .unwrap();
    assert!(Replay::new(&p).unwrap().event(unwrapped).is_err());
    let mut forged = r.clone();
    forged.receivers[0]
        .tracking
        .as_mut()
        .unwrap()
        .late_after_window = 1;
    assert!(!forged.passing_invariants());
    assert!(replay(&p, &events).unwrap().verify_report(&forged).is_err());
}
#[tokio::test(start_paused = true)]
async fn window_without_raw_samples_preserves_aggregate_scope_and_exact_queue_replay() {
    let mut p = plan();
    p.evidence.sample_every = 0;
    let (mut r, events) = recorded(&p).await;
    r.state = "completed".into();
    assert_eq!(r.verdict, "pass");
    let replay = replay(&p, &events).unwrap();
    replay.verify_report(&r).unwrap();
    assert!(replay.sample_receivers().is_empty());
    let mut changed = r.clone();
    changed.receivers[0]
        .queues
        .as_mut()
        .unwrap()
        .observed_high_water += 1;
    assert!(replay.verify_report(&changed).is_err());
}
#[tokio::test(start_paused = true)]
async fn mixed_exact_and_window_flows_retain_independent_statistics() {
    let mut p = plan();
    p.count = 80;
    p.flows=serde_json::from_value(serde_json::json!([{"name":"reverse","sender":"virtual:1","receivers":["virtual:0"],"can_id":128,"extended":false,"run_tag":18,"count":80,"fps":100,"queues":{"sample_every":2}}])).unwrap();
    let (mut r, events) = recorded(&p).await;
    r.state = "completed".into();
    assert_eq!(r.verdict, "pass", "{r:?}");
    assert!(r.group_passing_invariants());
    let outcome=events.iter().find(|e|matches!(e,EvidenceEvent::FlowRecord{event,..} if matches!(event.as_ref(),EvidenceEvent::TrackingRecord{event} if matches!(event.as_ref(),EvidenceEvent::Submissions{..})))).unwrap();
    assert!(Replay::new(&p).unwrap().event(outcome).is_err());
    let g = r.group.as_ref().unwrap();
    assert!(g.flows[0].report.receivers[0].tracking.is_some());
    assert!(g.flows[1].report.receivers[0].tracking.is_none());
    replay(&p, &events).unwrap().verify_report(&r).unwrap();
}
#[tokio::test(start_paused = true)]
async fn classic_checkpoint_window_and_adaptive_children_replay() {
    let mut p = plan();
    p.traffic = Traffic::ClassicPatternV1 {
        checkpoint_can_id: 292,
        stream: 1,
        seed: 17,
        max_pending: 64,
    };
    let (mut r, events) = recorded(&p).await;
    r.state = "completed".into();
    assert_eq!(r.verdict, "pass", "{r:?}");
    replay(&p, &events).unwrap().verify_report(&r).unwrap();
    let mut p = plan();
    p.count = 0;
    p.fps = 0;
    p.adaptive = Some(AdaptiveOptions {
        rates: vec![100, 120],
        exploration_seconds: 1,
        measurement_seconds: 1,
        min_observed_ppm: 900000,
    });
    let (mut r, events) = recorded(&p).await;
    r.state = "completed".into();
    assert_eq!(r.verdict, "pass", "{r:?}");
    replay(&p, &events).unwrap().verify_report(&r).unwrap();
}

#[tokio::test(start_paused = true)]
async fn window_stop_is_terminal_and_never_resumes_after_lease_time() {
    let mut p = plan();
    p.count = 1000;
    p.evidence.sample_every = 0;
    let rt = Runtime::new(Arc::new(VirtualBus::new(8192)));
    let executor = Executor::new(rt.clone());
    let run = executor.start(p, true).unwrap();
    loop {
        if run.snapshot().submitted >= 80 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    run.stop();
    run.stop();
    let final_report = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let r = run.snapshot();
            if r.state == "aborted" {
                break r;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_ne!(final_report.verdict, "pass");
    assert!(
        final_report.receivers[0]
            .tracking
            .as_ref()
            .unwrap()
            .finalized
    );
    assert!(!final_report.passing_invariants());
    tokio::time::advance(Duration::from_secs(60)).await;
    run.stop();
    assert_eq!(run.snapshot().submitted, final_report.submitted);
    assert!(rt.list().await.is_empty());
}

#[tokio::test(start_paused = true)]
async fn fd_mixed_brs_window_rtt_replays_and_responder_keeps_exact_fallback() {
    let mut p = plan();
    p.traffic = Traffic::FdV1 {
        data_bitrate: 2_000_000,
        stream: 1,
        epoch: 1,
        seed: 17,
        variants: vec![
            FdVariant {
                length: 32,
                brs: false,
            },
            FdVariant {
                length: 64,
                brs: true,
            },
        ],
    };
    p.rtt = Some(RttOptions {
        response_can_id: 292,
        timeout_ms: 200,
        max_inflight: 64,
        role: RttRole::Loopback,
    });
    let (mut r, events) = recorded(&p).await;
    r.state = "completed".into();
    assert_eq!(r.verdict, "pass", "{r:?}");
    assert_eq!(r.rtt_report.as_ref().unwrap().latency.samples, 160);
    assert!(r.passing_invariants());
    replay(&p, &events).unwrap().verify_report(&r).unwrap();
    p.rtt.as_mut().unwrap().role = RttRole::Responder;
    assert!(p.compile().unwrap_err().contains("watermark"));
    p.tracking = Tracking::Exact;
    assert!(p.compile().is_ok());
}
