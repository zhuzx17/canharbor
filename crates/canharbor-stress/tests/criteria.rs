// SPDX-License-Identifier: AGPL-3.0-or-later
//! Virtual-only criteria and independently recomputed evidence.
use canharbor_core::{
    Backend, BackendEvent, BackendFuture, Capabilities, Channel, Config, Error, Frame,
    control::Permit, runtime::Runtime,
};
use canharbor_stress::*;
use canharbor_virtual::VirtualBus;
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::broadcast;
fn plan() -> Plan {
    serde_json::from_value(serde_json::json!({"sender":"virtual:0","receivers":["virtual:1"],"can_id":291,"extended":false,"run_tag":17,"count":10,"fps":20,"bitrate":500000,"receivers_active":true,"drain_ms":20})).unwrap()
}
fn complete() -> Report {
    let mut r = Report::initial(&plan(), true, "test".into()).unwrap();
    r.version = 6;
    r.state = "completed".into();
    r.verdict = "pass".into();
    r.submitted = 10;
    r.attempted = 10;
    r.workload_achieved = true;
    r.measurement_complete = true;
    r.receivers = vec![ReceiverReport {
        channel: "virtual:1".into(),
        unique: 10,
        evidence_complete: true,
        ..Default::default()
    }];
    r.stages[0].submitted = 10;
    r.stages[0].received_unique = vec![10];
    r.assessment = Some(r.assess());
    r
}
#[test]
fn default_criteria_preserve_fingerprint_and_invalid_criteria_are_rejected() {
    let p = plan();
    let hash = p.fingerprint();
    let mut value = serde_json::to_value(&p).unwrap();
    assert!(value.get("criteria").is_none());
    value["criteria"] = serde_json::json!({});
    assert_eq!(
        serde_json::from_value::<Plan>(value.clone())
            .unwrap()
            .fingerprint(),
        hash
    );
    for bad in [
        serde_json::json!({"min_submitted_ppm":0}),
        serde_json::json!({"min_submitted_ppm":1000001}),
        serde_json::json!({"max_missing_expected_ppm":1000000}),
        serde_json::json!({"max_missing_submitted_ppm":1000000}),
        serde_json::json!({"max_dispatch_lateness_ns":259200000000001u64}),
    ] {
        value["criteria"] = bad;
        assert!(
            serde_json::from_value::<Plan>(value.clone())
                .unwrap()
                .compile()
                .is_err()
        );
    }
    for bad in [
        serde_json::json!({"max_unknown":1}),
        serde_json::json!({"max_duplicates":-1}),
        serde_json::json!({"max_reordered":1.5}),
        serde_json::json!({"max_payload_errors":4294967296u64}),
    ] {
        value["criteria"] = bad;
        assert!(serde_json::from_value::<Plan>(value.clone()).is_err());
    }
    value["criteria"] = serde_json::json!({"max_duplicates":1});
    assert_ne!(
        serde_json::from_value::<Plan>(value).unwrap().fingerprint(),
        hash
    );
}
#[test]
fn ppm_comparisons_have_exact_boundaries_without_rounding_or_pooled_receivers() {
    let mut r = complete();
    r.planned = 3;
    r.submitted = 3;
    r.attempted = 3;
    r.stages[0].count = 3;
    r.stages[0].submitted = 3;
    r.receivers[0].unique = 2;
    r.receivers[0].missing_expected = 1;
    r.receivers[0].missing_submitted = 1;
    r.plan.criteria.max_missing_expected_ppm = 333333;
    r.plan.criteria.max_missing_submitted_ppm = 333333;
    assert!(!r.assess().passed);
    r.plan.criteria.max_missing_expected_ppm = 333334;
    r.plan.criteria.max_missing_submitted_ppm = 333334;
    assert!(r.assess().passed);
    let mut peer = r.receivers[0].clone();
    peer.channel = "virtual:2".into();
    peer.unique = 3;
    peer.missing_expected = 0;
    peer.missing_submitted = 0;
    r.receivers.push(peer);
    r.plan.criteria.max_missing_expected_ppm = 250000;
    let a = r.assess();
    assert!(!a.passed);
    assert_eq!(a.failures.len(), 1);
    assert_eq!(a.failures[0].channel.as_deref(), Some("virtual:1"));
}
#[test]
fn submission_rate_is_checked_in_every_declared_stage_not_only_the_whole_run() {
    let mut r = complete();
    r.planned = 110;
    r.submitted = 100;
    r.stages[0].count = 10;
    r.stages[0].submitted = 5;
    let mut next = r.stages[0].clone();
    next.first = 10;
    next.count = 100;
    next.submitted = 95;
    r.stages.push(next);
    r.plan.criteria.min_submitted_ppm = 900000;
    let a = r.assess();
    assert!(!a.workload_met);
    assert_eq!(a.failures.len(), 1);
    assert_eq!(a.failures[0].stage, Some(0));
    r.plan.criteria.min_submitted_ppm = 500000;
    assert!(r.assess().workload_met);
    r.skipped = 1;
    assert!(!r.assess().passed);
    assert!(!r.assess().workload_met);
    r.plan.criteria.max_scheduler_skipped = 1;
    assert!(r.assess().passed);
    assert!(r.assess().workload_met);
}
#[test]
fn error_budgets_and_lateness_are_inclusive_but_never_relax_execution_safety() {
    let mut r = complete();
    r.receivers[0].duplicates = 1;
    r.receivers[0].reordered = 2;
    r.receivers[0].crc_errors = 1;
    r.receivers[0].pattern_errors = 2;
    r.receivers[0].out_of_range = 1;
    assert_eq!(r.assess().failures.len(), 4);
    assert!(r.known_violation());
    r.plan.criteria.max_duplicates = 1;
    r.plan.criteria.max_reordered = 2;
    r.plan.criteria.max_payload_errors = 3;
    r.plan.criteria.max_out_of_range = 1;
    r.max_dispatch_lateness_ns = 100;
    r.plan.criteria.max_dispatch_lateness_ns = Some(100);
    r.assessment = Some(r.assess());
    assert!(r.passing_invariants());
    assert!(!r.known_violation());
    r.max_dispatch_lateness_ns += 1;
    assert!(r.known_violation());
    assert!(!r.assess().passed);
    r.max_dispatch_lateness_ns = 100;
    for fault in [
        "unknown",
        "incomplete",
        "aborted",
        "failed",
        "bus_off",
        "legacy",
        "stale_assessment",
    ] {
        let mut v = r.clone();
        v.assessment = Some(v.assess());
        match fault {
            "unknown" => {
                v.unknown = 1;
                v.unknown_sequence = Some(0);
            }
            "incomplete" => v.measurement_complete = false,
            "aborted" => v.state = "aborted".into(),
            "failed" => v.failed = 1,
            "bus_off" => {
                v.lifecycle.termination = Some(Termination {
                    code: "bus_off".into(),
                    ..Default::default()
                })
            }
            "legacy" => v.version = 5,
            _ => v
                .assessment
                .as_mut()
                .unwrap()
                .failures
                .push(CriterionFailure {
                    criterion: "forged".into(),
                    actual: 0,
                    limit: 0,
                    denominator: None,
                    channel: None,
                    stage: None,
                }),
        }
        assert!(!v.passing_invariants(), "{fault}");
    }
    // Malicious persisted counters cannot overflow a sum into an allowed budget.
    r.receivers[0].crc_errors = u64::MAX;
    r.receivers[0].pattern_errors = u64::MAX;
    assert!(r.known_violation());
    assert!(!r.assess().passed);
}
struct Faulty {
    slow: bool,
    bus: VirtualBus,
    calls: AtomicUsize,
}
impl Backend for Faulty {
    fn capabilities(&self, c: &str) -> Result<Capabilities, Error> {
        self.bus.capabilities(c)
    }
    fn channels(&self) -> Vec<Channel> {
        self.bus.channels()
    }
    fn subscribe(&self, c: &str) -> Result<broadcast::Receiver<Arc<BackendEvent>>, Error> {
        self.bus.subscribe(c)
    }
    fn check_channel(&self, c: &str) -> Result<u64, Error> {
        self.bus.check_channel(c)
    }
    fn configure<'a>(&'a self, c: &'a str, v: Config) -> BackendFuture<'a, ()> {
        self.bus.configure(c, v)
    }
    fn start<'a>(&'a self, c: &'a str, p: Permit) -> BackendFuture<'a, ()> {
        self.bus.start(c, p)
    }
    fn stop<'a>(&'a self, c: &'a str) -> BackendFuture<'a, ()> {
        self.bus.stop(c)
    }
    fn close<'a>(&'a self, c: &'a str) -> BackendFuture<'a, ()> {
        self.bus.close(c)
    }
    fn submit<'a>(&'a self, c: &'a str, f: Frame, p: Permit) -> BackendFuture<'a, ()> {
        Box::pin(async move {
            if self.slow {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            p.check()?;
            self.transmit(c, f)
        })
    }
    fn transmit(&self, c: &str, f: Frame) -> Result<(), Error> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        if !self.slow && n == 0 {
            return Ok(());
        } // Known submission with no peer observation.
        if !self.slow && n == 1 {
            self.bus.transmit(c, f.clone())?;
        } // Explicit duplicate injection.
        self.bus.transmit(c, f)
    }
}
#[tokio::test(start_paused = true)]
async fn tolerated_virtual_loss_and_duplicates_pass_only_with_frozen_criteria_and_replay() {
    for tolerant in [false, true] {
        let backend = Arc::new(Faulty {
            slow: false,
            bus: VirtualBus::default(),
            calls: AtomicUsize::new(0),
        });
        let mut p = plan();
        p.evidence.sample_every = 1;
        if tolerant {
            p.criteria.max_missing_expected_ppm = 100000;
            p.criteria.max_missing_submitted_ppm = 100000;
            p.criteria.max_duplicates = 1;
        }
        let (sink, mut rx) = EvidenceSink::channel(256);
        let run = Executor::new(Runtime::new(backend.clone()))
            .start_recorded(
                p.clone(),
                true,
                uuid::Uuid::new_v4().to_string(),
                Some(sink),
                None,
            )
            .unwrap();
        let r = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let r = run.snapshot();
                if r.state == "saving_completed" {
                    break r;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(backend.calls.load(Ordering::SeqCst), 10);
        assert_eq!(r.submitted, 10);
        assert_eq!(r.receivers[0].unique, 9);
        assert_eq!(r.receivers[0].duplicates, 1);
        assert_eq!(r.receivers[0].missing_submitted, 1);
        assert_eq!(r.verdict, if tolerant { "pass" } else { "fail" });
        let mut r = r;
        r.state = "completed".into();
        let mut replay = Replay::new(&p).unwrap();
        while let Ok(e) = rx.try_recv() {
            replay.event(&e).unwrap();
        }
        replay.verify_report(&r).unwrap();
        if tolerant {
            assert!(r.passing_invariants());
            let mut forged = r.clone();
            forged.assessment.as_mut().unwrap().passed = false;
            assert!(replay.verify_report(&forged).is_err());
            let mut forged = r.clone();
            forged.max_dispatch_lateness_ns += 1;
            assert!(replay.verify_report(&forged).is_err());
            let mut forged = r.clone();
            forged.plan.criteria.max_duplicates = 2;
            forged.plan_hash = forged.plan.fingerprint();
            assert!(replay.verify_report(&forged).is_err());
            let mut forged = r.clone();
            forged.version = 5;
            assert!(replay.verify_report(&forged).is_err());
        }
    }
}

#[tokio::test(start_paused = true)]
async fn scheduled_skips_require_all_frozen_workload_and_loss_budgets_and_reconcile() {
    for tolerant in [false, true] {
        let backend = Arc::new(Faulty {
            slow: true,
            bus: VirtualBus::default(),
            calls: AtomicUsize::new(0),
        });
        let mut p = plan();
        p.count = 20;
        p.fps = 100;
        p.evidence.sample_every = 1;
        if tolerant {
            p.criteria.min_submitted_ppm = 250000;
            p.criteria.max_scheduler_skipped = 20;
            p.criteria.max_missing_expected_ppm = 999999;
        }
        let (sink, mut rx) = EvidenceSink::channel(256);
        let run = Executor::new(Runtime::new(backend))
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
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert!(r.skipped > 0);
        assert!(r.submitted >= 5);
        assert_eq!(r.skipped + r.submitted, 20);
        assert_eq!(r.receivers[0].missing_expected, r.skipped);
        assert_eq!(r.receivers[0].missing_submitted, 0);
        assert_eq!(r.verdict, if tolerant { "pass" } else { "fail" });
        r.state = "completed".into();
        let mut replay = Replay::new(&p).unwrap();
        while let Ok(e) = rx.try_recv() {
            replay.event(&e).unwrap();
        }
        replay.verify_report(&r).unwrap();
        if tolerant {
            assert!(r.passing_invariants());
        }
    }
}
