// SPDX-License-Identifier: AGPL-3.0-or-later
use canharbor_core::runtime::Runtime;
use canharbor_stress::*;
use canharbor_virtual::VirtualBus;
use std::{sync::Arc, time::Duration};
fn plan() -> Plan {
    serde_json::from_value(serde_json::json!({"sender":"virtual:0","receivers":["virtual:1"],"can_id":291,"extended":false,"run_tag":17,"count":10,"fps":20,"bitrate":500000,"receivers_active":true,"drain_ms":20,"flows":[{"name":"reverse","sender":"virtual:1","receivers":["virtual:0"],"can_id":128,"extended":false,"run_tag":18,"count":10,"fps":20}]})).unwrap()
}
async fn end(run: &Run) -> Report {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let r = run.snapshot();
            if matches!(r.state.as_str(), "completed" | "failed" | "aborted")
                || r.state.starts_with("saving_")
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
async fn bidirectional_flows_share_two_acquisitions_and_replay_independently() {
    let p = plan();
    let rt = Runtime::new(Arc::new(VirtualBus::new(8192)));
    let executor = Executor::new(rt.clone());
    let (sink, mut events) = EvidenceSink::channel(1024);
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
    assert_eq!(r.verdict, "pass", "{r:?}");
    assert_eq!(r.submitted, 20);
    assert_eq!(r.planned, 20);
    assert_eq!(r.group.as_ref().unwrap().flows.len(), 2);
    assert_eq!(r.backend_cleanup.len(), 2);
    for f in &r.group.as_ref().unwrap().flows {
        assert_eq!(f.report.submitted, 10);
        assert_eq!(f.report.receivers[0].unique, 10);
        assert!(f.report.passing_invariants(), "{f:?}");
    }
    assert!(rt.list().await.is_empty());
    r.state = "completed".into();
    assert!(r.group_passing_invariants());
    let mut replay = Replay::new(&p).unwrap();
    let mut recorded = vec![];
    while let Ok(e) = events.try_recv() {
        replay.event(&e).unwrap();
        recorded.push(e);
    }
    replay.verify_report(&r).unwrap();
    let mut forged = r.clone();
    forged.group.as_mut().unwrap().flows[0].report.receivers[0].unique -= 1;
    assert!(replay.verify_report(&forged).is_err());
    let mut forged = r.clone();
    forged.group.as_mut().unwrap().budget.peak_transmit_fps += 1;
    assert!(replay.verify_report(&forged).is_err());
    let mut forged = r.clone();
    forged.group.as_mut().unwrap().flows[0].report.id = uuid::Uuid::new_v4().to_string();
    assert!(!forged.group_passing_invariants());
    assert!(replay.verify_report(&forged).is_err());
    let mut missing_release = Replay::new(&p).unwrap();
    assert!(
        recorded
            .iter()
            .filter(|e| !matches!(e, EvidenceEvent::GroupRelease { .. }))
            .any(|e| missing_release.event(e).is_err())
    );
    let unwrapped = recorded
        .iter()
        .find_map(|e| match e {
            EvidenceEvent::FlowRecord { event, .. }
                if matches!(event.as_ref(), EvidenceEvent::Submissions { .. }) =>
            {
                Some(event.as_ref())
            }
            _ => None,
        })
        .unwrap();
    assert!(Replay::new(&p).unwrap().event(unwrapped).is_err());
}
#[test]
fn compiler_rejects_collisions_and_aggregate_budget_bypasses() {
    let p = plan();
    let b = p.group_budget().unwrap();
    assert_eq!(b.peak_transmit_fps, 40);
    assert_eq!(b.channels.len(), 2);
    let mut p = plan();
    p.flows[0].can_id = p.can_id;
    assert!(p.compile().is_err());
    let mut p = plan();
    p.flows[0].run_tag = p.run_tag;
    assert!(p.compile().is_err());
    let mut p = plan();
    p.fps = 12000;
    p.flows[0].fps = 12000;
    assert!(p.compile().is_err());
    let mut p = plan();
    p.flows[0].name = "primary".into();
    assert!(p.compile().is_err());
    let mut p = plan();
    p.evidence.segment_bytes = 4096;
    assert!(p.compile().is_err());
}

#[tokio::test(start_paused = true)]
async fn stopping_one_flow_preserves_other_flow_and_group_stop_is_idempotent() {
    let mut p = plan();
    p.count = 40;
    p.flows[0].count = 40;
    let rt = Runtime::new(Arc::new(VirtualBus::new(8192)));
    let executor = Executor::new(rt.clone());
    let (sink, mut rx) = EvidenceSink::channel(1024);
    let run = executor
        .start_recorded(
            p.clone(),
            true,
            uuid::Uuid::new_v4().to_string(),
            Some(sink),
            None,
        )
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let r = run.snapshot();
            if r.group.as_ref().is_some_and(|g| {
                g.flows.len() == 2 && g.flows.iter().all(|f| f.report.submitted >= 2)
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(rt.list().await.len(), 2);
    let other = Executor::new(rt.clone());
    assert!(!other.available());
    assert!(other.start(plan(), true).is_err());
    run.stop_flow("reverse").unwrap();
    run.stop_flow("reverse").unwrap();
    assert!(run.stop_flow("unknown").is_err());
    let mut r = end(&run).await;
    assert_ne!(r.verdict, "pass");
    let g = r.group.as_ref().unwrap();
    assert_eq!(g.stop_requests.len(), 1);
    let primary = g.flows.iter().find(|f| f.name == "primary").unwrap();
    let reverse = g.flows.iter().find(|f| f.name == "reverse").unwrap();
    assert_eq!(primary.report.submitted, 40);
    assert!(reverse.report.submitted < 40);
    assert_eq!(primary.report.receivers[0].unique, 40);
    assert_eq!(reverse.report.state, "aborted");
    r.state = r.state.strip_prefix("saving_").unwrap_or(&r.state).into();
    let mut replay = Replay::new(&p).unwrap();
    while let Ok(e) = rx.try_recv() {
        replay.event(&e).unwrap();
    }
    replay.verify_report(&r).unwrap();
    assert!(rt.list().await.is_empty());
    run.stop_flow("primary").unwrap();
    run.stop();
    run.stop();
    assert!(executor.available());
}
#[tokio::test(start_paused = true)]
async fn stopping_during_arming_never_releases_a_partial_group() {
    let p = plan();
    let run = Executor::new(Runtime::new(Arc::new(VirtualBus::new(8192))))
        .start(p, true)
        .unwrap();
    run.stop_flow("reverse").unwrap();
    let r = end(&run).await;
    assert_eq!(r.submitted, 0);
    assert!(r.group.unwrap().start_ns.is_none());
    assert_ne!(r.verdict, "pass");
}
#[tokio::test(start_paused = true)]
async fn mixed_classic_fd_and_explicit_response_flows_share_the_fd_configuration() {
    let mut p = plan();
    p.flows[0].traffic = Traffic::FdV1 {
        data_bitrate: 2_000_000,
        stream: 2,
        epoch: 3,
        seed: 42,
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
    p.flows[0].rtt = Some(RttOptions {
        role: RttRole::Loopback,
        response_can_id: 129,
        timeout_ms: 100,
        max_inflight: 32,
    });
    p.flows[0].timing = Some(TimingOptions {
        sample_every: 1,
        max_pending: 4,
    });
    let (sink, mut rx) = EvidenceSink::channel(2048);
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
    assert_eq!(r.verdict, "pass", "{r:?}");
    assert_eq!(r.planned, 30);
    assert_eq!(r.submitted, 30);
    assert_eq!(r.group.as_ref().unwrap().budget.peak_transmit_fps, 60);
    let reverse = r
        .group
        .as_ref()
        .unwrap()
        .flows
        .iter()
        .find(|f| f.name == "reverse")
        .unwrap();
    assert_eq!(
        reverse.report.timing.as_ref().unwrap().response.submitted,
        10
    );
    r.state = "completed".into();
    let mut replay = Replay::new(&p).unwrap();
    while let Ok(e) = rx.try_recv() {
        replay.event(&e).unwrap();
    }
    replay.verify_report(&r).unwrap();
}
#[test]
fn global_memory_disk_and_sampling_budgets_cannot_be_split_between_flows() {
    let mut p = plan();
    p.count = 1_000_000;
    p.fps = 10_000;
    p.flows[0].count = 1_000_000;
    p.flows[0].fps = 10_000;
    p.evidence.max_bytes = 8 * 1024 * 1024;
    assert!(p.compile().unwrap_err().contains("evidence"));
    let mut p = plan();
    p.fps = 400;
    p.flows[0].fps = 400;
    p.timing = Some(TimingOptions {
        sample_every: 1,
        max_pending: 1,
    });
    p.flows[0].timing = p.timing.clone();
    assert!(p.compile().unwrap_err().contains("2000"));
    let mut p = plan();
    p.count = 32_000_000;
    p.fps = 1000;
    p.evidence.max_bytes = 2 * 1024 * 1024 * 1024;
    p.receivers = (1..8).map(|n| format!("virtual:{n}")).collect();
    p.flows = (1..8)
        .map(|n| Flow {
            tracking: Default::default(),
            queues: None,
            shape: LoadShape::default(),
            name: format!("flow{n}"),
            sender: format!("virtual:{n}"),
            receivers: (0..8)
                .filter(|c| *c != n)
                .map(|c| format!("virtual:{c}"))
                .collect(),
            can_id: 291 + n,
            extended: false,
            run_tag: 17 + n,
            count: 32_000_000,
            fps: 1000,
            stages: vec![],
            traffic: Traffic::default(),
            criteria: Default::default(),
            timing: None,
            rtt: None,
        })
        .collect();
    assert!(p.compile().unwrap_err().contains("tracking"));
}

use canharbor_core::{
    Backend, BackendEvent, BackendFuture, Capabilities, Channel, Config, Error, Frame,
    control::Permit,
};
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::{Notify, Semaphore, broadcast};
struct DeviceBus {
    bus: VirtualBus,
    configured: AtomicUsize,
    writes: AtomicUsize,
    block: bool,
    fail_config: bool,
    entered: Notify,
    release: Semaphore,
}
impl DeviceBus {
    fn new(block: bool, fail_config: bool) -> Arc<Self> {
        Arc::new(Self {
            bus: VirtualBus::new(8192),
            configured: Default::default(),
            writes: Default::default(),
            block,
            fail_config,
            entered: Default::default(),
            release: Semaphore::new(0),
        })
    }
}
impl Backend for DeviceBus {
    fn capabilities(&self, c: &str) -> Result<Capabilities, Error> {
        let mut caps = self.bus.capabilities(c)?;
        caps.device_id = c.into();
        Ok(caps)
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
        Box::pin(async move {
            self.configured.fetch_add(1, Ordering::SeqCst);
            if self.fail_config && c == "virtual:1" {
                return Err(Error::InvalidState);
            }
            self.bus.configure(c, v).await
        })
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
    fn transmit(&self, c: &str, f: Frame) -> Result<(), Error> {
        self.writes.fetch_add(1, Ordering::SeqCst);
        self.bus.transmit(c, f)
    }
    fn submit<'a>(&'a self, c: &'a str, f: Frame, p: Permit) -> BackendFuture<'a, ()> {
        Box::pin(async move {
            if self.block && f.id().value() == 291 {
                self.entered.notify_one();
                let _ = self.release.acquire().await.unwrap();
            }
            p.check()?;
            self.transmit(c, f)
        })
    }
}
#[tokio::test(start_paused = true)]
async fn busy_device_union_and_partial_configuration_fail_without_any_transmit() {
    for fail_config in [false, true] {
        let b = DeviceBus::new(false, fail_config);
        let rt = Runtime::new(b.clone());
        let prior = if !fail_config {
            Some(
                rt.create_configured("virtual:1", Config::default())
                    .await
                    .unwrap(),
            )
        } else {
            None
        };
        let run = Executor::new(rt.clone()).start(plan(), true).unwrap();
        let r = end(&run).await;
        assert_ne!(r.verdict, "pass");
        assert_eq!(b.writes.load(Ordering::SeqCst), 0);
        assert_eq!(
            b.configured.load(Ordering::SeqCst),
            if fail_config { 2 } else { 1 }
        );
        assert_eq!(rt.list().await.len(), usize::from(prior.is_some()));
        let reservation = rt.reserve_devices(&["virtual:0".into()]).await.unwrap();
        drop(reservation);
    }
}
#[tokio::test(start_paused = true)]
async fn queued_flow_stop_cannot_revoke_the_other_flows_shared_acquisition() {
    let b = DeviceBus::new(true, false);
    let rt = Runtime::new(b.clone());
    let executor = Executor::new(rt.clone());
    let run = executor.start(plan(), true).unwrap();
    b.entered.notified().await;
    run.stop_flow("primary").unwrap();
    b.release.add_permits(1);
    let r = end(&run).await;
    let g = r.group.unwrap();
    let reverse = g.flows.iter().find(|f| f.name == "reverse").unwrap();
    assert_eq!(reverse.report.submitted, 10);
    assert_eq!(b.writes.load(Ordering::SeqCst), 10);
    assert_eq!(r.unknown, 0);
    assert!(rt.list().await.is_empty());
}
#[tokio::test(start_paused = true)]
async fn unknown_inflight_keeps_the_entire_group_reserved_until_it_settles() {
    let b = DeviceBus::new(true, false);
    let rt = Runtime::new(b.clone());
    let executor = Executor::new(rt.clone());
    let mut p = plan();
    p.count = 200;
    p.flows[0].count = 200;
    let run = executor.start(p, true).unwrap();
    b.entered.notified().await;
    run.stop_flow("primary").unwrap();
    tokio::time::sleep(Duration::from_secs(2)).await;
    let r = end(&run).await;
    assert_eq!(r.unknown, 1);
    assert_eq!(run.pending_io(), 1);
    assert!(!executor.available());
    assert!(
        rt.reserve_devices(&["virtual:0".into(), "virtual:1".into()])
            .await
            .is_err()
    );
    let writes = b.writes.load(Ordering::SeqCst);
    b.release.add_permits(1);
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    assert_eq!(b.writes.load(Ordering::SeqCst), writes);
    assert_eq!(run.pending_io(), 0);
    assert!(executor.available());
}

#[tokio::test(start_paused = true)]
async fn responder_does_not_send_for_matching_external_rx_before_the_group_start_gate() {
    let b = DeviceBus::new(false, false);
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
        role: RttRole::Loopback,
        response_can_id: 292,
        timeout_ms: 100,
        max_inflight: 32,
    });
    p.coordination = Some(Coordination {
        group_id: uuid::Uuid::new_v4().to_string(),
        participant: "guard".into(),
        lease_ms: 3000,
        start_delay_ms: 2000,
        clock_quality: "unsynchronized".into(),
    });
    let run = Executor::new(Runtime::new(b.clone()))
        .start(p.clone(), true)
        .unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        while run.snapshot().group.as_ref().unwrap().start_ns.is_none() {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
    // A fake external request is deliberately injected through the simulation bus, not the executor.
    b.bus.transmit("virtual:0", p.frame(0)).unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(b.writes.load(Ordering::SeqCst), 0);
    run.stop();
    let r = end(&run).await;
    assert_eq!(r.submitted, 0);
    assert_eq!(b.writes.load(Ordering::SeqCst), 0);
    assert_ne!(r.verdict, "pass");
}
