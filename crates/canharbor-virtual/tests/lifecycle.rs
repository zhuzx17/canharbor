// SPDX-License-Identifier: AGPL-3.0-or-later
use canharbor_core::{
    Backend, BackendEvent, BackendFuture, CanId, Channel, Config, Error, Frame,
    control::Permit,
    runtime::{Mode, Runtime},
};
use canharbor_virtual::VirtualBus;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use tokio::sync::{Notify, broadcast};
struct Checked {
    bus: VirtualBus,
    split_devices: bool,
    hold_configure: AtomicBool,
    stops: AtomicUsize,
    closes: AtomicUsize,
    queued: Notify,
    execute: Notify,
    hold: AtomicBool,
    submissions: AtomicUsize,
    panic_after_write: AtomicBool,
    fail_stop: AtomicBool,
    fail_start: AtomicBool,
    delayed_fault: AtomicBool,
    hide_channels: AtomicBool,
    report_failure: AtomicBool,
    report_calls: AtomicUsize,
    hold_report: AtomicBool,
    hold_start: AtomicBool,
    start_permit: std::sync::Mutex<Option<Permit>>,
    report_entered: Notify,
    report_release: Notify,
}
impl Default for Checked {
    fn default() -> Self {
        Self {
            bus: VirtualBus::default(),
            split_devices: false,
            hold_configure: AtomicBool::new(false),
            stops: AtomicUsize::new(0),
            closes: AtomicUsize::new(0),
            queued: Notify::new(),
            execute: Notify::new(),
            hold: AtomicBool::new(false),
            submissions: AtomicUsize::new(0),
            panic_after_write: AtomicBool::new(false),
            fail_stop: AtomicBool::new(false),
            fail_start: AtomicBool::new(false),
            delayed_fault: AtomicBool::new(false),
            hide_channels: AtomicBool::new(false),
            report_failure: AtomicBool::new(false),
            report_calls: AtomicUsize::new(0),
            hold_report: AtomicBool::new(false),
            hold_start: AtomicBool::new(false),
            start_permit: std::sync::Mutex::new(None),
            report_entered: Notify::new(),
            report_release: Notify::new(),
        }
    }
}
impl Backend for Checked {
    fn capabilities(&self, ch: &str) -> Result<canharbor_core::Capabilities, Error> {
        let mut c = self.bus.capabilities(ch)?;
        if self.split_devices {
            c.device_id = format!(
                "fake-device-{}",
                ch.rsplit(':').next().unwrap().parse::<u8>().unwrap() % 2
            );
        }
        Ok(c)
    }
    fn shutdown_report(&self) -> BackendFuture<'_, canharbor_core::CloseSummary> {
        self.report_calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            if self.hold_report.load(Ordering::Acquire) {
                self.report_entered.notify_one();
                self.report_release.notified().await;
            }
            Ok(if self.report_failure.load(Ordering::SeqCst) {
                canharbor_core::CloseSummary {
                    unknown_transmissions: 2,
                    failures: vec!["fake shutdown failure".into()],
                }
            } else {
                canharbor_core::CloseSummary::default()
            })
        })
    }
    fn channels(&self) -> Vec<Channel> {
        if self.hide_channels.load(Ordering::SeqCst) {
            Vec::new()
        } else {
            self.bus.channels()
        }
    }
    fn subscribe(&self, ch: &str) -> Result<broadcast::Receiver<Arc<BackendEvent>>, Error> {
        self.bus.subscribe(ch)
    }
    fn check_channel(&self, ch: &str) -> Result<u64, Error> {
        self.bus.check_channel(ch)
    }
    fn transmit(&self, ch: &str, f: Frame) -> Result<(), Error> {
        self.bus.transmit(ch, f)
    }
    fn configure<'a>(&'a self, ch: &'a str, c: Config) -> BackendFuture<'a, ()> {
        Box::pin(async move {
            if ch == "virtual:0" && self.hold_configure.load(Ordering::Acquire) {
                self.queued.notify_one();
                self.execute.notified().await;
            }
            self.bus.configure(ch, c).await
        })
    }
    fn start<'a>(&'a self, ch: &'a str, p: Permit) -> BackendFuture<'a, ()> {
        if self.fail_start.load(Ordering::SeqCst) {
            return Box::pin(async { Err(Error::Fault(canharbor_core::BackendFault::BusOff)) });
        }
        if self.hold_start.load(Ordering::Acquire) {
            *self.start_permit.lock().unwrap() = Some(p.clone());
            return Box::pin(async move {
                self.queued.notify_one();
                self.execute.notified().await;
                p.check()?;
                self.bus.start(ch, p).await
            });
        }
        self.bus.start(ch, p)
    }
    fn stop<'a>(&'a self, ch: &'a str) -> BackendFuture<'a, ()> {
        self.stops.fetch_add(1, Ordering::SeqCst);
        if self.fail_stop.load(Ordering::SeqCst) {
            return Box::pin(async { Err(Error::Fault(canharbor_core::BackendFault::StopFailed)) });
        }
        self.bus.stop(ch)
    }
    fn close<'a>(&'a self, ch: &'a str) -> BackendFuture<'a, ()> {
        self.closes.fetch_add(1, Ordering::SeqCst);
        self.bus.close(ch)
    }
    fn submit<'a>(&'a self, ch: &'a str, f: Frame, p: Permit) -> BackendFuture<'a, ()> {
        self.submissions.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            if self.hold.load(Ordering::SeqCst) {
                self.queued.notify_one();
                self.execute.notified().await;
            }
            if self.delayed_fault.load(Ordering::SeqCst) {
                return Err(Error::Fault(canharbor_core::BackendFault::BusOff));
            }
            p.check()?;
            self.bus.transmit(ch, f)?;
            assert!(
                !self.panic_after_write.load(Ordering::SeqCst),
                "injected panic after write"
            );
            Ok(())
        })
    }
}
#[tokio::test(start_paused = true)]
async fn expired_lease_stops_backend_and_idle_reclamation_closes_it() {
    let b = Arc::new(Checked::default());
    let r = Runtime::new(b.clone());
    let s = r.create("virtual:0", Mode::Active).await.unwrap();
    let lease = r.acquire(&s.id, "owner").await.unwrap();
    r.running(&s.id, "owner", lease.generation, true)
        .await
        .unwrap();
    tokio::time::advance(std::time::Duration::from_secs(16)).await;
    r.maintain().await;
    assert!(!r.get(&s.id).await.unwrap().running);
    assert!(b.stops.load(Ordering::SeqCst) >= 1);
    tokio::time::advance(std::time::Duration::from_secs(61)).await;
    r.maintain().await;
    assert!(r.list().await.is_empty());
    assert_eq!(b.closes.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn revocation_invalidates_a_queued_send_before_backend_execution() {
    let b = Arc::new(Checked::default());
    let r = Runtime::new(b.clone());
    let s = r.create("virtual:0", Mode::Active).await.unwrap();
    let lease = r.acquire(&s.id, "owner").await.unwrap();
    r.running(&s.id, "owner", lease.generation, true)
        .await
        .unwrap();
    let mut events = r.subscribe(&s.id).await.unwrap();
    b.hold.store(true, Ordering::SeqCst);
    let task = {
        let r = r.clone();
        let id = s.id.clone();
        tokio::spawn(async move {
            r.send(
                &id,
                "owner",
                lease.generation,
                Frame::classic(CanId::new(1, false).unwrap(), vec![]).unwrap(),
            )
            .await
        })
    };
    b.queued.notified().await;
    r.revoke("owner").await;
    b.execute.notify_one();
    assert!(matches!(
        task.await.unwrap(),
        Err(canharbor_core::runtime::RuntimeError::Backend(
            Error::ControlExpired
        ))
    ));
    assert!(events.try_recv().is_err());
    assert!(!r.get(&s.id).await.unwrap().running);
}
#[tokio::test]
async fn release_close_and_service_shutdown_call_backend_lifecycle() {
    let b = Arc::new(Checked::default());
    let r = Runtime::new(b.clone());
    let s = r.create("virtual:0", Mode::Active).await.unwrap();
    let lease = r.acquire(&s.id, "a").await.unwrap();
    r.running(&s.id, "a", lease.generation, true).await.unwrap();
    r.release(&s.id, "a", lease.generation).await.unwrap();
    assert!(b.stops.load(Ordering::SeqCst) > 0);
    let lease = r.acquire(&s.id, "b").await.unwrap();
    r.close(&s.id, "b", lease.generation).await.unwrap();
    r.create("virtual:1", Mode::ListenOnly).await.unwrap();
    r.shutdown().await;
    assert_eq!(b.closes.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn failed_stop_is_faulted_and_does_not_retain_control() {
    let backend = Arc::new(Checked::default());
    let runtime = Runtime::new(backend.clone());
    let session = runtime.create("virtual:0", Mode::Active).await.unwrap();
    let lease = runtime.acquire(&session.id, "owner").await.unwrap();
    runtime
        .running(&session.id, "owner", lease.generation, true)
        .await
        .unwrap();
    backend.fail_stop.store(true, Ordering::SeqCst);
    assert!(
        runtime
            .release(&session.id, "owner", lease.generation)
            .await
            .is_err()
    );
    let session = runtime.get(&session.id).await.unwrap();
    assert!(session.controller.is_none());
    assert!(!session.running);
    assert_eq!(
        session.fault,
        Some(canharbor_core::BackendFault::StopFailed)
    );
}

#[tokio::test]
async fn delayed_fault_does_not_invalidate_restart_under_same_lease() {
    let backend = Arc::new(Checked::default());
    let runtime = Runtime::new(backend.clone());
    let session = runtime.create("virtual:0", Mode::Active).await.unwrap();
    let lease = runtime.acquire(&session.id, "owner").await.unwrap();
    runtime
        .running(&session.id, "owner", lease.generation, true)
        .await
        .unwrap();
    backend.hold.store(true, Ordering::SeqCst);
    backend.delayed_fault.store(true, Ordering::SeqCst);
    let sending = {
        let runtime = runtime.clone();
        let id = session.id.clone();
        tokio::spawn(async move {
            runtime
                .send(
                    &id,
                    "owner",
                    lease.generation,
                    Frame::classic(CanId::new(1, false).unwrap(), vec![]).unwrap(),
                )
                .await
        })
    };
    backend.queued.notified().await;
    runtime
        .running(&session.id, "owner", lease.generation, false)
        .await
        .unwrap();
    runtime
        .running(&session.id, "owner", lease.generation, true)
        .await
        .unwrap();
    let mut events = runtime.subscribe(&session.id).await.unwrap();
    backend.execute.notify_one();
    assert!(sending.await.unwrap().is_err());
    let view = runtime.get(&session.id).await.unwrap();
    assert!(view.running);
    assert!(view.fault.is_none());
    assert!(view.controller.is_some());
    assert!(matches!(
        events.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));
}
#[tokio::test]
async fn startup_fault_revokes_control_and_closes_stream() {
    let backend = Arc::new(Checked::default());
    let runtime = Runtime::new(backend.clone());
    let session = runtime.create("virtual:0", Mode::Active).await.unwrap();
    let lease = runtime.acquire(&session.id, "owner").await.unwrap();
    let mut events = runtime.subscribe(&session.id).await.unwrap();
    backend.fail_start.store(true, Ordering::SeqCst);
    assert!(
        runtime
            .running(&session.id, "owner", lease.generation, true)
            .await
            .is_err()
    );
    let view = runtime.get(&session.id).await.unwrap();
    assert!(!view.running);
    assert!(view.controller.is_none());
    assert_eq!(view.fault, Some(canharbor_core::BackendFault::BusOff));
    assert!(matches!(
        events.try_recv(),
        Err(broadcast::error::TryRecvError::Closed)
    ));
    assert!(backend.stops.load(Ordering::SeqCst) > 0);
}

#[tokio::test]
async fn shutdown_reports_unknown_outcomes_and_permanently_closes_mutations() {
    let backend = Arc::new(Checked::default());
    let runtime = Runtime::new(backend.clone());
    let session = runtime.create("virtual:0", Mode::Active).await.unwrap();
    backend.report_failure.store(true, Ordering::SeqCst);
    let summary = runtime.shutdown().await;
    assert_eq!(summary.unknown_transmissions, 2);
    assert_eq!(summary.failures, vec!["fake shutdown failure"]);
    assert!(matches!(
        runtime.create("virtual:1", Mode::Active).await,
        Err(canharbor_core::runtime::RuntimeError::Closed)
    ));
    assert!(matches!(
        runtime.acquire(&session.id, "owner").await,
        Err(canharbor_core::runtime::RuntimeError::Closed)
    ));
    let again = runtime.shutdown().await;
    assert_eq!(again.failures, summary.failures);
    assert_eq!(backend.report_calls.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn registry_collects_every_backend_shutdown_failure() {
    let a = Arc::new(Checked::default());
    let b = Arc::new(Checked::default());
    for backend in [&a, &b] {
        backend.hide_channels.store(true, Ordering::SeqCst);
        backend.report_failure.store(true, Ordering::SeqCst);
    }
    let registry = canharbor_core::registry::Registry::new(vec![a.clone(), b.clone()]).unwrap();
    let summary = registry.shutdown_report().await.unwrap();
    assert_eq!(summary.failures.len(), 2);
    assert_eq!(summary.unknown_transmissions, 4);
    assert_eq!(a.report_calls.load(Ordering::SeqCst), 1);
    assert_eq!(b.report_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn cancelling_shutdown_waiter_keeps_one_cleanup_and_its_actual_summary() {
    let backend = Arc::new(Checked::default());
    let runtime = Runtime::new(backend.clone());
    let session = runtime.create("virtual:0", Mode::Active).await.unwrap();
    let lease = runtime.acquire(&session.id, "owner").await.unwrap();
    runtime
        .running(&session.id, "owner", lease.generation, true)
        .await
        .unwrap();
    backend.hold_report.store(true, Ordering::Release);
    backend.report_failure.store(true, Ordering::Release);
    let copy = runtime.clone();
    let first = tokio::spawn(async move { copy.shutdown().await });
    backend.report_entered.notified().await;
    first.abort();
    assert!(first.await.unwrap_err().is_cancelled());
    let copy = runtime.clone();
    let second = tokio::spawn(async move { copy.shutdown().await });
    tokio::task::yield_now().await;
    assert!(!second.is_finished());
    backend.report_release.notify_one();
    let report = second.await.unwrap();
    assert_eq!(report.unknown_transmissions, 2);
    assert_eq!(report.failures, vec!["fake shutdown failure"]);
    assert_eq!(runtime.shutdown().await.failures, report.failures);
    assert_eq!(backend.closes.load(Ordering::Acquire), 1);
    assert_eq!(backend.report_calls.load(Ordering::Acquire), 1);
}
#[tokio::test]
async fn shutdown_fences_startup_before_waiting_for_the_runtime_lock() {
    let backend = Arc::new(Checked::default());
    let runtime = Runtime::new(backend.clone());
    let session = runtime.create("virtual:0", Mode::Active).await.unwrap();
    let lease = runtime.acquire(&session.id, "owner").await.unwrap();
    backend.hold_start.store(true, Ordering::Release);
    let copy = runtime.clone();
    let id = session.id.clone();
    let start =
        tokio::spawn(async move { copy.running(&id, "owner", lease.generation, true).await });
    backend.queued.notified().await;
    let copy = runtime.clone();
    let closing = tokio::spawn(async move { copy.shutdown().await });
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if backend
                .start_permit
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .check()
                .is_err()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    backend.execute.notify_one();
    assert!(start.await.unwrap().is_err());
    assert!(closing.await.unwrap().failures.is_empty());
    assert!(runtime.acquire(&session.id, "owner").await.is_err());
}

#[tokio::test]
async fn prepared_transmissions_are_bound_to_runtime_and_acquisition() {
    let backend = Arc::new(Checked::default());
    let runtime = Runtime::new(backend.clone());
    let session = runtime.create("virtual:0", Mode::Active).await.unwrap();
    let lease = runtime.acquire(&session.id, "owner").await.unwrap();
    runtime
        .running(&session.id, "owner", lease.generation, true)
        .await
        .unwrap();
    let frame = Frame::classic(CanId::new(1, false).unwrap(), vec![]).unwrap();
    let wrong = runtime
        .prepare_send(&session.id, "owner", lease.generation, frame.clone())
        .await
        .unwrap();
    let other = Runtime::new(Arc::new(VirtualBus::default()));
    assert!(matches!(
        other.submit_prepared(wrong).await,
        Err(canharbor_core::runtime::RuntimeError::InvalidState)
    ));
    let stale = runtime
        .prepare_send(&session.id, "owner", lease.generation, frame.clone())
        .await
        .unwrap();
    runtime
        .running(&session.id, "owner", lease.generation, false)
        .await
        .unwrap();
    runtime
        .running(&session.id, "owner", lease.generation, true)
        .await
        .unwrap();
    assert!(matches!(
        runtime.submit_prepared(stale).await,
        Err(canharbor_core::runtime::RuntimeError::Backend(
            Error::ControlExpired
        ))
    ));
    assert_eq!(backend.submissions.load(Ordering::SeqCst), 0);
    runtime
        .send(&session.id, "owner", lease.generation, frame)
        .await
        .unwrap();
    assert_eq!(backend.submissions.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn panic_after_possible_submission_revokes_control_and_reports_unknown() {
    let backend = Arc::new(Checked::default());
    let runtime = Runtime::new(backend.clone());
    let session = runtime.create("virtual:0", Mode::Active).await.unwrap();
    let lease = runtime.acquire(&session.id, "owner").await.unwrap();
    runtime
        .running(&session.id, "owner", lease.generation, true)
        .await
        .unwrap();
    backend.panic_after_write.store(true, Ordering::SeqCst);
    let result = runtime
        .send(
            &session.id,
            "owner",
            lease.generation,
            Frame::classic(CanId::new(1, false).unwrap(), vec![]).unwrap(),
        )
        .await;
    assert!(matches!(
        result,
        Err(canharbor_core::runtime::RuntimeError::SubmissionUnknown)
    ));
    let state = runtime.get(&session.id).await.unwrap();
    assert!(!state.running);
    assert!(state.controller.is_none());
    assert_eq!(
        state.fault,
        Some(canharbor_core::BackendFault::TransmitFailed)
    );
    assert_eq!(backend.submissions.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn queued_submission_deadline_is_not_extended_by_lease_renewal() {
    let backend = Arc::new(Checked::default());
    let runtime = Runtime::new(backend.clone());
    let session = runtime.create("virtual:0", Mode::Active).await.unwrap();
    let lease = runtime.acquire(&session.id, "owner").await.unwrap();
    runtime
        .running(&session.id, "owner", lease.generation, true)
        .await
        .unwrap();
    let ticket = runtime
        .prepare_send(
            &session.id,
            "owner",
            lease.generation,
            Frame::classic(CanId::new(1, false).unwrap(), vec![]).unwrap(),
        )
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(1050)).await;
    runtime
        .renew(&session.id, "owner", lease.generation)
        .await
        .unwrap();
    assert!(matches!(
        runtime.submit_prepared(ticket).await,
        Err(canharbor_core::runtime::RuntimeError::Backend(
            Error::ControlExpired
        ))
    ));
    assert_eq!(backend.submissions.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn independent_device_controls_progress_while_same_device_channels_wait() {
    use std::time::Duration;
    let backend = Arc::new(Checked {
        bus: VirtualBus::with_channel_count(256, 4),
        split_devices: true,
        ..Checked::default()
    });
    let runtime = Runtime::new(backend.clone());
    let b = runtime.create("virtual:1", Mode::Active).await.unwrap();
    let lease = runtime.acquire(&b.id, "B").await.unwrap();
    runtime
        .running(&b.id, "B", lease.generation, true)
        .await
        .unwrap();
    backend.hold_configure.store(true, Ordering::Release);
    let rt = runtime.clone();
    let blocked = tokio::spawn(async move { rt.create("virtual:0", Mode::Active).await });
    backend.queued.notified().await;
    let rt = runtime.clone();
    let mut same = tokio::spawn(async move { rt.create("virtual:2", Mode::ListenOnly).await });
    assert!(
        tokio::time::timeout(Duration::from_millis(30), &mut same)
            .await
            .is_err()
    );
    tokio::time::timeout(Duration::from_secs(1), async {
        runtime.renew(&b.id, "B", lease.generation).await.unwrap();
        runtime.sequence(&b.id).await.unwrap();
        let prepared = runtime
            .prepare_send(
                &b.id,
                "B",
                lease.generation,
                Frame::classic(CanId::new(291, false).unwrap(), vec![1]).unwrap(),
            )
            .await
            .unwrap();
        runtime
            .running(&b.id, "B", lease.generation, false)
            .await
            .unwrap();
        assert!(runtime.submit_prepared(prepared).await.is_err());
        runtime.close(&b.id, "B", lease.generation).await.unwrap();
        assert!(
            runtime
                .reserve_devices(&["virtual:0".into(), "virtual:1".into()])
                .await
                .is_err()
        );
        let reserve = runtime
            .reserve_devices(&["virtual:1".into()])
            .await
            .unwrap();
        drop(reserve);
    })
    .await
    .unwrap();
    blocked.abort(); // The detached configure must still finish and remain owned by the runtime.
    backend.execute.notify_one();
    same.await.unwrap().unwrap();
    runtime.shutdown().await;
    assert_eq!(backend.submissions.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn device_shards_keep_global_session_limit_and_shutdown_fence() {
    let backend = Arc::new(Checked {
        bus: VirtualBus::with_channel_count(256, 16),
        split_devices: true,
        ..Checked::default()
    });
    let runtime = Runtime::new(backend);
    let mut tasks = tokio::task::JoinSet::new();
    for n in 0..16 {
        let rt = runtime.clone();
        tasks.spawn(async move { rt.create(&format!("virtual:{n}"), Mode::ListenOnly).await });
    }
    let mut results = vec![];
    while let Some(r) = tasks.join_next().await {
        results.push(r.unwrap());
    }
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 8);
    assert_eq!(runtime.list().await.len(), 8);
    runtime.fence_shutdown();
    assert!(runtime.create("virtual:15", Mode::Active).await.is_err());
    runtime.shutdown().await;
    assert!(runtime.list().await.is_empty());
}

#[tokio::test]
async fn cancelled_creator_cannot_use_a_dropped_reservation_token() {
    use std::{
        future::{Future, poll_fn},
        task::Poll,
    };
    let backend = Arc::new(Checked::default());
    let rt = Runtime::new(backend.clone());
    let reservation = rt.reserve_devices(&["virtual:0".into()]).await.unwrap();
    let mut creating =
        Box::pin(rt.create_reserved("virtual:0", Config::default(), Some(&reservation)));
    poll_fn(|cx| {
        assert!(creating.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    drop(creating);
    drop(reservation);
    tokio::task::yield_now().await;
    assert!(rt.list().await.is_empty());
    let fresh = rt.reserve_devices(&["virtual:0".into()]).await.unwrap();
    rt.create_reserved("virtual:0", Config::default(), Some(&fresh))
        .await
        .unwrap();
    rt.shutdown().await;
    assert_eq!(backend.submissions.load(Ordering::SeqCst), 0);
}
