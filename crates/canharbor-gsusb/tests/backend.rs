// SPDX-License-Identifier: AGPL-3.0-or-later
mod common;
use canharbor_core::{
    Backend, CanId, Config, Error, Frame,
    runtime::{Runtime, RuntimeError},
};
use canharbor_gsusb::{backend::GsUsbBackend, wire};
use common::{Fake, Gate};
use std::{
    future::Future,
    sync::Arc,
    task::{Context, Waker},
    time::Duration,
};
fn config() -> Config {
    Config {
        nominal_bitrate: 500_000,
        data_bitrate: None,
        listen_only: false,
    }
}
fn frame() -> Frame {
    Frame::classic(CanId::new(0x123, false).unwrap(), vec![1, 2]).unwrap()
}
#[tokio::test]
async fn runtime_configures_receives_transmits_stops_and_closes_usb_actor() {
    let fake = Fake::default();
    let backend = Arc::new(
        GsUsbBackend::open(fake.clone(), 0, "test-epoch".into())
            .await
            .unwrap(),
    );
    let channel = backend.channels()[0].id.clone();
    assert!(!backend.capabilities(&channel).unwrap().simulated);
    let runtime = Runtime::new(backend.clone());
    let session = runtime.create_configured(&channel, config()).await.unwrap();
    assert_eq!(fake.0.lock().unwrap().writes, 0);
    let lease = runtime.acquire(&session.id, "owner").await.unwrap();
    let mut rx = runtime.subscribe(&session.id).await.unwrap();
    runtime
        .running(&session.id, "owner", lease.generation, true)
        .await
        .unwrap();
    let start = tokio::time::Instant::now();
    let mut prepared = runtime
        .prepare_send(&session.id, "owner", lease.generation, frame())
        .await
        .unwrap();
    let trace = prepared.observe(start, start);
    runtime.submit_prepared(prepared).await.unwrap();
    let p = trace.snapshot();
    assert_eq!(p.scope, "device_worker_submit");
    assert!(p.enqueued_ns.unwrap() <= p.worker_started_ns.unwrap());
    assert!(p.call_started_ns.unwrap() <= p.call_completed_ns.unwrap());
    assert!(p.completed_ns.unwrap() >= p.call_completed_ns.unwrap());
    let event = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(event.direction, "tx");
    assert_eq!(event.frame, frame());
    let mut bytes = wire::encode(&frame(), 0, 0, false).unwrap();
    bytes[..4].copy_from_slice(&wire::RX_ECHO.to_le_bytes());
    fake.0.lock().unwrap().reads.push_back(Ok(bytes));
    let event = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(event.direction, "rx");
    runtime
        .running(&session.id, "owner", lease.generation, false)
        .await
        .unwrap();
    assert!(
        runtime
            .send(&session.id, "owner", lease.generation, frame())
            .await
            .is_err()
    );
    runtime
        .close(&session.id, "owner", lease.generation)
        .await
        .unwrap();
    runtime.shutdown().await;
    assert_eq!(fake.0.lock().unwrap().cancels, 1);
}
#[tokio::test]
async fn queued_usb_tx_observes_revocation_before_writing() {
    let fake = Fake::default();
    let backend = Arc::new(
        GsUsbBackend::open(fake.clone(), 0, "revocation".into())
            .await
            .unwrap(),
    );
    let runtime = Runtime::new(backend.clone());
    let channel = backend.channels()[0].id.clone();
    let session = runtime.create_configured(&channel, config()).await.unwrap();
    let lease = runtime.acquire(&session.id, "owner").await.unwrap();
    runtime
        .running(&session.id, "owner", lease.generation, true)
        .await
        .unwrap();
    let gate = Arc::new(Gate::default());
    fake.0.lock().unwrap().gate = Some(gate.clone());
    gate.entered();
    let mut cx = Context::from_waker(Waker::noop());
    let mut send = Box::pin(runtime.send(&session.id, "owner", lease.generation, frame()));
    assert!(send.as_mut().poll(&mut cx).is_pending());
    let mut revoke = Box::pin(runtime.revoke("owner"));
    assert!(revoke.as_mut().poll(&mut cx).is_pending());
    gate.release();
    revoke.await;
    assert!(matches!(
        send.await,
        Err(RuntimeError::Backend(Error::ControlExpired))
    ));
    assert_eq!(fake.0.lock().unwrap().writes, 0);
    runtime.shutdown().await;
}
#[tokio::test]
async fn raw_bus_off_is_retained_and_faults_runtime_without_becoming_data() {
    let fake = Fake::default();
    let backend = Arc::new(
        GsUsbBackend::open(fake.clone(), 0, "busoff".into())
            .await
            .unwrap(),
    );
    let runtime = Runtime::new(backend.clone());
    let session = runtime
        .create_configured(&backend.channels()[0].id, config())
        .await
        .unwrap();
    let lease = runtime.acquire(&session.id, "owner").await.unwrap();
    let mut rx = runtime.subscribe(&session.id).await.unwrap();
    runtime
        .running(&session.id, "owner", lease.generation, true)
        .await
        .unwrap();
    let mut bytes = vec![0; 20];
    bytes[..4].copy_from_slice(&wire::RX_ECHO.to_le_bytes());
    bytes[4..8].copy_from_slice(&0x20000040u32.to_le_bytes());
    bytes[8] = 8;
    bytes[12] = 7;
    fake.0.lock().unwrap().reads.push_back(Ok(bytes));
    assert!(
        tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .is_err()
    );
    assert_eq!(
        runtime.get(&session.id).await.unwrap().fault,
        Some(canharbor_core::BackendFault::BusOff)
    );
    assert_eq!(
        backend.diagnostics().raw_errors[0],
        (0, 0x40, [7, 0, 0, 0, 0, 0, 0, 0])
    );
    runtime.shutdown().await;
}

#[tokio::test]
async fn unmatched_echo_overflow_is_counted_and_faults_the_session() {
    let fake = Fake::default();
    let backend = Arc::new(
        GsUsbBackend::open(fake.clone(), 0, "overflow".into())
            .await
            .unwrap(),
    );
    let runtime = Runtime::new(backend.clone());
    let session = runtime
        .create_configured(&backend.channels()[0].id, config())
        .await
        .unwrap();
    let lease = runtime.acquire(&session.id, "owner").await.unwrap();
    let mut rx = runtime.subscribe(&session.id).await.unwrap();
    runtime
        .running(&session.id, "owner", lease.generation, true)
        .await
        .unwrap();
    let mut bytes = wire::encode(&frame(), 0, 42, false).unwrap();
    bytes[10] |= 1;
    fake.0.lock().unwrap().reads.push_back(Ok(bytes));
    assert!(
        tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .is_err()
    );
    assert_eq!(
        runtime.get(&session.id).await.unwrap().fault,
        Some(canharbor_core::BackendFault::ReceiveOverflow)
    );
    let diagnostics = backend.diagnostics();
    assert_eq!(diagnostics.overflow, 1);
    assert_eq!(diagnostics.rejected, 1);
    runtime.shutdown().await;
}

#[tokio::test]
async fn hardware_timestamp_esi_and_raw_dlc_survive_actor_and_runtime() {
    let fake = Fake::default();
    fake.0.lock().unwrap().features = Some(0x511);
    let backend = Arc::new(
        GsUsbBackend::open(fake.clone(), 0, "metadata".into())
            .await
            .unwrap(),
    );
    let runtime = Runtime::new(backend.clone());
    let session = runtime
        .create_configured(
            &backend.channels()[0].id,
            Config {
                data_bitrate: Some(2_000_000),
                ..config()
            },
        )
        .await
        .unwrap();
    let lease = runtime.acquire(&session.id, "owner").await.unwrap();
    let mut rx = runtime.subscribe_all(&session.id).await.unwrap();
    runtime
        .running(&session.id, "owner", lease.generation, true)
        .await
        .unwrap();
    let frame = Frame::fd(CanId::new(0x123, false).unwrap(), vec![7; 12], true).unwrap();
    let mut bytes = wire::encode(&frame, 0, 1, true).unwrap();
    bytes[..4].copy_from_slice(&wire::RX_ECHO.to_le_bytes());
    bytes[10] |= 8;
    bytes.extend_from_slice(&0u32.to_le_bytes());
    fake.0.lock().unwrap().reads.push_back(Ok(bytes));
    let event = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .unwrap()
        .unwrap();
    let canharbor_core::runtime::Notice::Frame(event) = event else {
        panic!("expected frame");
    };
    assert_eq!(event.frame, frame);
    assert!(event.metadata.esi);
    assert_eq!(event.metadata.raw_dlc, Some(9));
    assert_eq!(event.metadata.stamp.raw_us, Some(0));
    assert_eq!(event.metadata.stamp.extended_us, Some(0));
    assert_eq!(
        event.metadata.stamp.quality,
        canharbor_core::timestamp::Quality::HardwareUnmapped
    );
    runtime.shutdown().await;
}

#[tokio::test]
async fn cancelled_shutdown_waiter_keeps_the_actor_close_report() {
    let fake = Fake::default();
    let backend = Arc::new(
        GsUsbBackend::open(fake.clone(), 0, "close-cancel".into())
            .await
            .unwrap(),
    );
    let gate = Arc::new(Gate::default());
    {
        let mut state = fake.0.lock().unwrap();
        state.cancel_gate = Some(gate.clone());
        state.fail_cancel = true;
    }
    let mut context = Context::from_waker(Waker::noop());
    let mut first = Box::pin(backend.shutdown_report());
    assert!(first.as_mut().poll(&mut context).is_pending());
    drop(first);
    let wait = gate.clone();
    tokio::task::spawn_blocking(move || wait.entered())
        .await
        .unwrap();
    let mut second = Box::pin(backend.shutdown_report());
    assert!(second.as_mut().poll(&mut context).is_pending());
    gate.release();
    let report = second.await.unwrap();
    assert!(!report.failures.is_empty());
    assert_eq!(
        backend.shutdown_report().await.unwrap().failures,
        report.failures
    );
    assert_eq!(fake.0.lock().unwrap().cancels, 1);
}
