// SPDX-License-Identifier: AGPL-3.0-or-later
use canharbor_core::{
    Backend, BackendEvent, BackendFuture, Capabilities, Channel, Config, Error, Frame,
    control::Permit, runtime::Runtime,
};
use canharbor_stress::{Executor, Plan, Run};
use canharbor_virtual::{Fault, VirtualBus};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Notify, Semaphore, broadcast};
struct Device {
    bus: VirtualBus,
    mode: u8,
    entered: Notify,
    release: Semaphore,
    finished: AtomicBool,
    done: Notify,
    writes: AtomicUsize,
}
impl Device {
    fn new(mode: u8) -> Arc<Self> {
        Arc::new(Self {
            bus: VirtualBus::new(8192),
            mode,
            entered: Notify::new(),
            release: Semaphore::new(0),
            finished: AtomicBool::new(false),
            done: Notify::new(),
            writes: AtomicUsize::new(0),
        })
    }
    async fn gate(&self) {
        self.entered.notify_one();
        self.release.acquire().await.unwrap().forget();
    }
}
impl Backend for Device {
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
    fn transmit(&self, c: &str, f: Frame) -> Result<(), Error> {
        self.bus.transmit(c, f)
    }
    fn configure<'a>(&'a self, c: &'a str, v: Config) -> BackendFuture<'a, ()> {
        Box::pin(async move {
            if self.mode == 3 && c == "virtual:0" {
                self.gate().await;
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
        Box::pin(async move {
            if self.mode == 2 {
                while !self.finished.load(Ordering::Acquire) {
                    self.done.notified().await;
                }
            }
            self.bus.close(c).await
        })
    }
    fn submit<'a>(&'a self, c: &'a str, f: Frame, p: Permit) -> BackendFuture<'a, ()> {
        Box::pin(async move {
            if self.mode == 1 {
                self.gate().await;
            }
            p.check()?;
            self.writes.fetch_add(1, Ordering::SeqCst);
            self.bus.transmit(c, f)?;
            if self.mode == 2 {
                self.gate().await;
                self.finished.store(true, Ordering::Release);
                self.done.notify_one();
            }
            Ok(())
        })
    }
}
fn plan() -> Plan {
    serde_json::from_value(serde_json::json!({"sender":"virtual:0","receivers":["virtual:1"],"can_id":291,"extended":false,"run_tag":55,"count":1000,"fps":100,"bitrate":500000,"receivers_active":true,"drain_ms":20})).unwrap()
}
async fn terminal(run: &Run) -> canharbor_stress::Report {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let r = run.snapshot();
            if matches!(r.state.as_str(), "completed" | "failed" | "aborted") {
                return r;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}
async fn running(run: &Run) {
    while run.state() != "running" {
        tokio::task::yield_now().await;
    }
}
#[tokio::test(start_paused = true)]
async fn stop_before_queued_write_revokes_synchronously_and_is_idempotent() {
    let d = Device::new(1);
    let rt = Runtime::new(d.clone());
    let run = Executor::new(rt).start(plan(), true).unwrap();
    d.entered.notified().await;
    run.stop();
    let revision = run.snapshot().lifecycle.revision;
    run.stop();
    assert_eq!(run.snapshot().lifecycle.revision, revision);
    d.release.add_permits(1);
    let r = terminal(&run).await;
    assert_eq!(d.writes.load(Ordering::SeqCst), 0);
    assert_eq!(r.state, "aborted");
    assert!(r.lifecycle.stop_requested_ns.is_some());
    assert!(r.lifecycle.execution_end_ns >= r.lifecycle.stop_requested_ns);
    assert_ne!(r.verdict, "pass");
}
#[tokio::test(start_paused = true)]
async fn opaque_inflight_and_cleanup_stay_reserved_after_stop_deadline() {
    let d = Device::new(2);
    let rt = Runtime::new(d.clone());
    let executor = Executor::new(rt);
    let mut timed = plan();
    timed.timing = Some(canharbor_stress::TimingOptions {
        sample_every: 1,
        max_pending: 4,
    });
    let run = executor.start(timed, true).unwrap();
    d.entered.notified().await;
    run.stop();
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(4)).await;
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    assert_eq!(run.state(), "stopping_blocked");
    assert!(!executor.available());
    assert!(executor.start(plan(), true).is_err());
    assert_eq!(d.writes.load(Ordering::SeqCst), 1);
    assert_eq!(run.pending_io(), 1);
    d.release.add_permits(1);
    let r = terminal(&run).await;
    assert_eq!(r.unknown, 1);
    assert_eq!(r.timing.as_ref().unwrap().request.unknown, 1);
    assert_eq!(r.timing.as_ref().unwrap().request.submit_call.samples, 0);
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    assert_eq!(run.pending_io(), 0);
    assert_eq!(r.unknown_sequence, Some(0));
    assert_eq!(d.writes.load(Ordering::SeqCst), 1);
    assert_eq!(r.state, "aborted");
    assert_ne!(r.verdict, "pass");
}
#[tokio::test(start_paused = true)]
async fn startup_call_blocking_does_not_hide_stop_or_start_a_replacement() {
    let d = Device::new(3);
    let executor = Executor::new(Runtime::new(d.clone()));
    let run = executor.start(plan(), true).unwrap();
    d.entered.notified().await;
    run.stop();
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(3)).await;
    tokio::task::yield_now().await;
    assert_eq!(run.state(), "stopping_blocked");
    assert!(!executor.available());
    assert_eq!(d.writes.load(Ordering::SeqCst), 0);
    d.release.add_permits(1);
    let r = terminal(&run).await;
    assert_eq!(r.state, "aborted");
    assert_eq!(r.attempted, 0);
    assert_eq!(d.writes.load(Ordering::SeqCst), 0);
}
#[tokio::test(start_paused = true)]
async fn disconnect_bus_off_and_overflow_never_resume_after_health_returns() {
    for fault in [Fault::Disconnect, Fault::BusOff, Fault::ReceiveOverflow] {
        let d = Device::new(0);
        let rt = Runtime::new(d.clone());
        let run = Executor::new(rt).start(plan(), true).unwrap();
        running(&run).await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        d.bus.inject(fault);
        let r = terminal(&run).await;
        assert_ne!(r.verdict, "pass");
        assert!(r.lifecycle.termination.is_some());
        let sent = d.writes.load(Ordering::SeqCst);
        d.bus.clear_faults();
        tokio::time::sleep(Duration::from_secs(30)).await;
        assert_eq!(d.writes.load(Ordering::SeqCst), sent);
    }
}
#[tokio::test(start_paused = true)]
async fn restarted_epoch_is_not_closed_by_the_old_stress_cleanup() {
    let d = Device::new(0);
    let rt = Runtime::new(d);
    let owner = "owner";
    let run = Executor::new(rt.clone())
        .start_recorded(
            plan(),
            true,
            uuid::Uuid::new_v4().to_string(),
            None,
            Some((owner.into(), Arc::new(AtomicBool::new(false)))),
        )
        .unwrap();
    running(&run).await;
    let tx = rt
        .list()
        .await
        .into_iter()
        .find(|s| s.channel == "virtual:0")
        .unwrap();
    assert!(rt.acquire(&tx.id, "other").await.is_err());
    let owner = tx.controller.as_deref().unwrap();
    rt.running(&tx.id, owner, tx.generation, false)
        .await
        .unwrap();
    rt.running(&tx.id, owner, tx.generation, true)
        .await
        .unwrap();
    let r = terminal(&run).await;
    assert_ne!(r.verdict, "pass");
    let current = rt.get(&tx.id).await.unwrap();
    assert!(current.running);
    assert_eq!(current.controller.as_deref(), Some(owner));
    rt.close(&tx.id, owner, tx.generation).await.unwrap();
}
#[tokio::test(start_paused = true)]
async fn lease_takeover_is_not_renewed_or_closed_by_the_previous_job() {
    let d = Device::new(0);
    let rt = Runtime::new(d);
    let owner = "owner";
    let run = Executor::new(rt.clone())
        .start_recorded(
            plan(),
            true,
            uuid::Uuid::new_v4().to_string(),
            None,
            Some((owner.into(), Arc::new(AtomicBool::new(false)))),
        )
        .unwrap();
    running(&run).await;
    let tx = rt
        .list()
        .await
        .into_iter()
        .find(|s| s.channel == "virtual:0")
        .unwrap();
    let owner = tx.controller.as_deref().unwrap();
    rt.release(&tx.id, owner, tx.generation).await.unwrap();
    let next = rt.acquire(&tx.id, "other").await.unwrap();
    rt.running(&tx.id, "other", next.generation, true)
        .await
        .unwrap();
    assert_ne!(terminal(&run).await.verdict, "pass");
    let current = rt.get(&tx.id).await.unwrap();
    assert!(current.running);
    assert_eq!(current.controller.as_deref(), Some("other"));
    rt.close(&tx.id, "other", next.generation).await.unwrap();
}
#[tokio::test(start_paused = true)]
async fn service_fence_and_stop_race_never_grant_more_submissions() {
    let d = Device::new(1);
    let rt = Runtime::new(d.clone());
    let run = Executor::new(rt.clone()).start(plan(), true).unwrap();
    d.entered.notified().await;
    rt.fence_shutdown();
    run.stop();
    d.release.add_permits(1);
    assert_ne!(terminal(&run).await.verdict, "pass");
    assert_eq!(d.writes.load(Ordering::SeqCst), 0);
}
#[tokio::test(start_paused = true)]
async fn twenty_four_hour_virtual_clock_run_is_bounded_and_reconciled() {
    let mut p = plan();
    p.count = 86400;
    p.fps = 1;
    p.evidence.max_bytes = 1024 * 1024 * 1024;
    let rt = Runtime::new(Device::new(0));
    let run = Executor::new(rt.clone()).start(p, true).unwrap();
    loop {
        let r = run.snapshot();
        if matches!(r.state.as_str(), "completed" | "failed") {
            assert_eq!(r.verdict, "pass", "{r:?}");
            assert_eq!(r.submitted, 86400);
            assert_eq!(r.receivers[0].unique, 86400);
            assert!(r.history.len() <= 120);
            break;
        }
        tokio::time::sleep(Duration::from_secs(3600)).await;
    }
    assert!(rt.list().await.is_empty());
}
#[test]
fn rss_growth_policy_is_frozen_and_unavailable_samples_cannot_pass() {
    use canharbor_stress::{StabilityLimits, StabilityReport};
    let limits = StabilityLimits {
        max_rss_growth_bytes: Some(100),
        warmup_seconds: 1,
    };
    let mut r = StabilityReport::default();
    r.observe(&limits, Duration::ZERO, Some(1000));
    assert_eq!(r.status, "warming");
    r.observe(&limits, Duration::from_secs(2), Some(1000));
    r.observe(&limits, Duration::from_secs(3), Some(1101));
    assert!(r.limit_exceeded);
    r.observe(&limits, Duration::from_secs(4), Some(900));
    assert!(r.limit_exceeded);
    let mut r = StabilityReport::default();
    r.observe(&limits, Duration::from_secs(2), None);
    assert_eq!(r.status, "unsupported");
    assert!(canharbor_stress::current_rss().is_some());
}

#[tokio::test]
#[ignore = "actual 24-hour virtual-only soak; explicit release gate, no hardware"]
async fn actual_twenty_four_hour_virtual_soak_with_rss_limit() {
    let mut p = plan();
    p.count = 86400;
    p.fps = 1;
    p.evidence.max_bytes = 1024 * 1024 * 1024;
    p.stability.max_rss_growth_bytes = Some(32 * 1024 * 1024);
    p.stability.warmup_seconds = 60;
    let rt = Runtime::new(Device::new(0));
    let run = Executor::new(rt.clone()).start(p, true).unwrap();
    let result = tokio::time::timeout(Duration::from_secs(86430), async {
        loop {
            let r = run.snapshot();
            if matches!(r.state.as_str(), "completed" | "failed" | "aborted") {
                break r;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(result.verdict, "pass", "{result:?}");
    assert_eq!(result.submitted, 86400);
    assert_eq!(result.receivers[0].unique, 86400);
    assert!(!result.stability.limit_exceeded);
    assert!(result.history.len() <= 120);
    assert!(rt.list().await.is_empty());
}

#[tokio::test(start_paused = true)]
async fn delayed_arming_cannot_reset_the_execution_authorization_deadline() {
    let d = Device::new(3);
    let run = Executor::new(Runtime::new(d.clone()))
        .start(plan(), true)
        .unwrap();
    d.entered.notified().await;
    let budget = run.snapshot().execution_timeout_ns;
    tokio::time::advance(Duration::from_nanos(budget) + Duration::from_secs(1)).await;
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    d.release.add_permits(1);
    let r = terminal(&run).await;
    assert_eq!(r.state, "aborted");
    assert_eq!(r.submitted, 0);
    assert_eq!(d.writes.load(Ordering::SeqCst), 0);
    assert_eq!(r.lifecycle.termination.unwrap().code, "execution_deadline");
}

#[tokio::test(start_paused = true)]
async fn creating_user_cannot_reuse_private_stress_transmission_lease() {
    let d = Device::new(0);
    let rt = Runtime::new(d.clone());
    let run = Executor::new(rt.clone())
        .start_recorded(
            plan(),
            true,
            uuid::Uuid::new_v4().to_string(),
            None,
            Some(("creator".into(), Arc::new(AtomicBool::new(false)))),
        )
        .unwrap();
    running(&run).await;
    let tx = rt
        .list()
        .await
        .into_iter()
        .find(|s| s.channel == "virtual:0")
        .unwrap();
    assert!(tx.controller.as_deref().unwrap().starts_with("stress:"));
    let before = d.writes.load(Ordering::SeqCst);
    let frame =
        Frame::classic(canharbor_core::CanId::new(291, false).unwrap(), vec![0; 8]).unwrap();
    assert!(
        rt.prepare_send(&tx.id, "creator", tx.generation, frame)
            .await
            .is_err()
    );
    assert!(rt.acquire(&tx.id, "creator").await.is_err());
    assert_eq!(d.writes.load(Ordering::SeqCst), before);
    run.stop();
    assert_eq!(terminal(&run).await.state, "aborted");
}
