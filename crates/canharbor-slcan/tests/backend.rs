// SPDX-License-Identifier: AGPL-3.0-or-later
use canharbor_core::{Backend, BackendFault, CanId, Config, Frame, runtime::Runtime};
use canharbor_slcan::{backend::SlcanBackend, serial::Transport};
use std::{
    collections::VecDeque,
    io,
    sync::{Arc, Mutex},
    time::Duration,
};
#[derive(Default)]
struct State {
    writes: Vec<Vec<u8>>,
    input: VecDeque<u8>,
    quiet: bool,
    short: bool,
    disconnected: bool,
    status: u8,
    closed: bool,
    drop_gate: Option<Arc<DropGate>>,
}
#[derive(Default)]
struct DropGate {
    entered: std::sync::atomic::AtomicBool,
    released: std::sync::atomic::AtomicBool,
}
struct Fake(Arc<Mutex<State>>);
impl Drop for Fake {
    fn drop(&mut self) {
        use std::sync::atomic::Ordering;
        let gate = self.0.lock().unwrap().drop_gate.clone();
        if let Some(gate) = gate {
            gate.entered.store(true, Ordering::Release);
            let until = std::time::Instant::now() + Duration::from_secs(2);
            while !gate.released.load(Ordering::Acquire) && std::time::Instant::now() < until {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        self.0.lock().unwrap().closed = true;
    }
}
impl Transport for Fake {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let mut state = self.0.lock().unwrap();
        state.writes.push(bytes.to_vec());
        if state.short && bytes.starts_with(b"t") {
            return Ok(5);
        }
        if bytes == b"F\r" {
            let response = format!("F{:02X}\r", state.status);
            state.input.extend(response.bytes());
            return Ok(bytes.len());
        }
        let response = match bytes {
            b"V\r" => b"V0234\r".as_slice(),
            b"F\r" => b"F00\r",
            b"t5311BA\r" if state.quiet => b"",
            b"t5311BA\r" => b"z\r",
            _ => b"\r",
        };
        state.input.extend(response);
        Ok(bytes.len())
    }
    fn read(&mut self, bytes: &mut [u8], timeout: Duration) -> io::Result<usize> {
        let mut state = self.0.lock().unwrap();
        if state.disconnected {
            return Err(io::Error::from(io::ErrorKind::BrokenPipe));
        }
        if state.input.is_empty() {
            drop(state);
            std::thread::sleep(timeout.min(Duration::from_millis(2)));
            return Ok(0);
        }
        let n = bytes.len().min(state.input.len());
        for byte in &mut bytes[..n] {
            *byte = state.input.pop_front().unwrap();
        }
        Ok(n)
    }
}
async fn setup() -> (
    Arc<Runtime>,
    Arc<SlcanBackend>,
    Arc<Mutex<State>>,
    String,
    u64,
) {
    let state = Arc::new(Mutex::new(State::default()));
    let backend = Arc::new(
        SlcanBackend::open(Fake(state.clone()), "fake-epoch".into(), false)
            .await
            .unwrap(),
    );
    let runtime = Runtime::new(backend.clone());
    let session = runtime
        .create_configured(
            "slcan:fake-epoch:0",
            Config {
                nominal_bitrate: 500_000,
                data_bitrate: None,
                listen_only: false,
            },
        )
        .await
        .unwrap();
    let lease = runtime.acquire(&session.id, "owner").await.unwrap();
    runtime
        .running(&session.id, "owner", lease.generation, true)
        .await
        .unwrap();
    (runtime, backend, state, session.id, lease.generation)
}
fn frame() -> Frame {
    Frame::classic(CanId::new(0x531, false).unwrap(), vec![0xba]).unwrap()
}
#[tokio::test]
async fn runtime_receives_raw_timestamps_and_serializes_transmit_stop_and_restart() {
    let (runtime, backend, state, session, g) = setup().await;
    let mut events = runtime.subscribe(&session).await.unwrap();
    state.lock().unwrap().input.extend(b"t5341CA0123\r");
    let event = tokio::time::timeout(Duration::from_secs(2), events.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(event.direction, "rx");
    assert_eq!(event.frame.id().value(), 0x534);
    assert_eq!(event.metadata.stamp.raw_us, Some(0x123 * 1000));
    assert_eq!(event.metadata.stamp.extended_us, None);
    let start = tokio::time::Instant::now();
    let mut prepared = runtime
        .prepare_send(&session, "owner", g, frame())
        .await
        .unwrap();
    let trace = prepared.observe(start, start);
    runtime.submit_prepared(prepared).await.unwrap();
    let p = trace.snapshot();
    assert_eq!(p.scope, "device_worker_submit");
    assert!(p.enqueued_ns.unwrap() <= p.worker_started_ns.unwrap());
    assert!(p.call_started_ns.unwrap() <= p.call_completed_ns.unwrap());
    assert!(p.completed_ns.unwrap() >= p.call_completed_ns.unwrap());
    assert!(
        events.try_recv().is_err(),
        "firmware acceptance must not fabricate a TX echo"
    );
    runtime.running(&session, "owner", g, false).await.unwrap();
    assert!(runtime.send(&session, "owner", g, frame()).await.is_err());
    runtime.running(&session, "owner", g, true).await.unwrap();
    runtime.send(&session, "owner", g, frame()).await.unwrap();
    runtime.close(&session, "owner", g).await.unwrap();
    let report = runtime.shutdown().await;
    assert!(report.failures.is_empty());
    assert_eq!(report.unknown_transmissions, 0);
    assert!(backend.check_channel("slcan:fake-epoch:0").is_err());
    assert_eq!(
        backend
            .shutdown_report()
            .await
            .unwrap()
            .unknown_transmissions,
        0
    );
}
#[tokio::test]
async fn partial_and_missing_replies_are_unknown_with_no_retry_or_cleanup_tx() {
    for short in [false, true] {
        let (runtime, _backend, state, session, g) = setup().await;
        {
            let mut state = state.lock().unwrap();
            state.short = short;
            state.quiet = !short;
        }
        assert!(runtime.send(&session, "owner", g, frame()).await.is_err());
        let before = state.lock().unwrap().writes.len();
        let summary = runtime.shutdown().await;
        assert_eq!(summary.unknown_transmissions, 1);
        assert!(!summary.failures.is_empty());
        assert_eq!(state.lock().unwrap().writes.len(), before);
        assert_eq!(
            state
                .lock()
                .unwrap()
                .writes
                .iter()
                .filter(|write| write.starts_with(b"t"))
                .count(),
            1
        );
    }
}
#[tokio::test]
async fn disconnect_faults_session_and_requires_explicit_reopen() {
    let (runtime, backend, state, session, _g) = setup().await;
    state.lock().unwrap().disconnected = true;
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if runtime.get(&session).await.unwrap().fault == Some(BackendFault::Disconnected) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert!(backend.check_channel("slcan:fake-epoch:0").is_err());
    let view = runtime.get(&session).await.unwrap();
    assert!(!view.running);
    assert!(view.controller.is_none());
    assert!(!runtime.shutdown().await.failures.is_empty());
}

#[tokio::test]
async fn source_overflow_publishes_diagnostics_before_faulting() {
    let (runtime, _backend, state, session, _g) = setup().await;
    let mut notices = runtime.subscribe_all(&session).await.unwrap();
    state.lock().unwrap().status = 9;
    let notice = tokio::time::timeout(Duration::from_secs(2), notices.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(notice,canharbor_core::runtime::Notice::Diagnostic{diagnostic,..} if diagnostic.overflow && diagnostic.data[0]==9)
    );
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if runtime.get(&session).await.unwrap().fault == Some(BackendFault::ReceiveOverflow) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    runtime.shutdown().await;
}
#[tokio::test]
async fn dropping_owner_requests_worker_cleanup_without_an_explicit_shutdown() {
    let state = Arc::new(Mutex::new(State::default()));
    let backend = SlcanBackend::open(Fake(state.clone()), "drop-epoch".into(), false)
        .await
        .unwrap();
    drop(backend);
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if state.lock().unwrap().closed {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        !state
            .lock()
            .unwrap()
            .writes
            .iter()
            .any(|line| line.starts_with(b"t"))
    );
}

#[tokio::test]
async fn cancelled_shutdown_waiter_retains_unknown_tx_and_cleanup_failures() {
    use std::{
        future::Future,
        sync::atomic::Ordering,
        task::{Context, Waker},
    };
    let (runtime, backend, state, session, g) = setup().await;
    let gate = Arc::new(DropGate::default());
    {
        let mut state = state.lock().unwrap();
        state.short = true;
        state.drop_gate = Some(gate.clone());
    }
    assert!(runtime.send(&session, "owner", g, frame()).await.is_err());
    tokio::time::timeout(Duration::from_secs(2), async {
        while !gate.entered.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let mut context = Context::from_waker(Waker::noop());
    let mut first = Box::pin(backend.shutdown_report());
    assert!(first.as_mut().poll(&mut context).is_pending());
    drop(first);
    let mut second = Box::pin(backend.shutdown_report());
    assert!(second.as_mut().poll(&mut context).is_pending());
    gate.released.store(true, Ordering::Release);
    let report = second.await.unwrap();
    assert_eq!(report.unknown_transmissions, 1);
    assert!(!report.failures.is_empty());
    let repeated = backend.shutdown_report().await.unwrap();
    assert_eq!(repeated.unknown_transmissions, 1);
    assert_eq!(repeated.failures, report.failures);
    runtime.shutdown().await;
}
