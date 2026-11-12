// SPDX-License-Identifier: AGPL-3.0-or-later
use canharbor_core::runtime::Runtime;
use canharbor_server::local::RunningServer;
use canharbor_virtual::{Fault, VirtualBus};
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    process::{Output, Stdio},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::Command,
};
fn command(root: &Path, args: &[&str]) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_canharbor"));
    c.arg("--runtime-dir")
        .arg(root)
        .args(args)
        .kill_on_drop(true);
    c
}
async fn output(root: &Path, args: &[&str]) -> Output {
    tokio::time::timeout(Duration::from_secs(10), command(root, args).output())
        .await
        .unwrap()
        .unwrap()
}
fn report(o: &Output) -> Value {
    serde_json::from_slice(&o.stdout).unwrap_or_else(|e| {
        panic!(
            "{e}: {} / {}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        )
    })
}
async fn setup() -> (PathBuf, Arc<VirtualBus>, RunningServer) {
    let root = test_temp_dir().join(format!("ch-seq-{}", uuid::Uuid::new_v4().simple()));
    let bus = Arc::new(VirtualBus::default());
    let service = RunningServer::start(Runtime::new(bus.clone()), &root, 0, false)
        .await
        .unwrap();
    (root, bus, service)
}
async fn receiver(root: &Path, extra: &[&str]) -> tokio::process::Child {
    let mut args = vec![
        "sequence",
        "receive",
        "--channel",
        "virtual:1",
        "--id",
        "123",
        "--run-id",
        "ABCDEF01",
        "--count",
        "4",
        "--timeout-ms",
        "3000",
    ];
    args.extend_from_slice(extra);
    let mut child = command(root, &args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stderr.take().unwrap()).lines();
    let line = tokio::time::timeout(Duration::from_secs(5), lines.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(line.contains("receiver ready"), "{line}");
    child.stderr = Some(lines.into_inner().into_inner());
    child
}
async fn finish(child: tokio::process::Child) -> Output {
    tokio::time::timeout(Duration::from_secs(10), child.wait_with_output())
        .await
        .unwrap()
        .unwrap()
}
async fn clean(root: PathBuf, service: RunningServer) {
    assert!(
        service
            .state
            .runtime
            .session_snapshot()
            .await
            .iter()
            .all(|v| !v.running)
    );
    service.shutdown().await;
    std::fs::remove_dir_all(root).unwrap();
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn classic_and_fd_sequences_cross_virtual_channels_without_hardware() {
    for fd in [false, true] {
        let (root, _, service) = setup().await;
        let extra = if fd { vec!["--fd", "--brs"] } else { vec![] };
        let mut receive_extra = extra.clone();
        receive_extra.push("--active");
        let child = receiver(&root, &receive_extra).await;
        let mut args = vec![
            "sequence",
            "send",
            "--channel",
            "virtual:0",
            "--id",
            "123",
            "--run-id",
            "ABCDEF01",
            "--count",
            "4",
            "--active",
        ];
        args.extend_from_slice(&extra);
        let sent = output(&root, &args).await;
        assert!(
            sent.status.success(),
            "{}",
            String::from_utf8_lossy(&sent.stderr)
        );
        let tx = report(&sent);
        assert_eq!(tx["submitted"], 4);
        assert_eq!(tx["confirmation"], "backend_submission_only");
        assert_eq!(tx["received_unique"], 0);
        assert!(tx["missing"].is_null());
        let received = finish(child).await;
        assert!(
            received.status.success(),
            "{}",
            String::from_utf8_lossy(&received.stderr)
        );
        let rx = report(&received);
        assert_eq!(rx["received_unique"], 4);
        assert_eq!(rx["missing"], 0);
        assert_eq!(rx["passed"], true);
        assert_eq!(rx["simulated"], true);
        clean(root, service).await;
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_duplicate_and_out_of_order_sequences_are_reported() {
    let (root, _, service) = setup().await;
    let child = receiver(&root, &[]).await;
    // Send crafted sequence traffic through the ordinary authenticated API.
    let client = canharbor_client::LocalClient::new(root.clone()).unwrap();
    for sequence in [0, 2, 2, 1] {
        let payload = format!("ABCDEF01{sequence:08X}");
        let result = output(
            &root,
            &[
                "send",
                "--channel",
                "virtual:0",
                "--active",
                "--id",
                "123",
                "--data",
                &payload,
            ],
        )
        .await;
        assert!(result.status.success());
    }
    let received = finish(child).await;
    assert_eq!(received.status.code(), Some(6));
    let rx = report(&received);
    assert_eq!(rx["received_unique"], 3);
    assert_eq!(rx["missing"], 1);
    assert_eq!(rx["duplicates"], 1);
    assert_eq!(rx["out_of_order"], 1);
    assert_eq!(rx["state"], "incomplete");
    assert_eq!(rx["passed"], false);
    assert!(
        client
            .request("GET", "/sessions", Value::Null, None)
            .await
            .unwrap()["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s["state"] != "running")
    );
    clean(root, service).await;
}
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancellation_and_device_fault_do_not_leave_a_running_receiver() {
    for fault in [false, true] {
        let (root, bus, service) = setup().await;
        let child = receiver(&root, &[]).await;
        if fault {
            bus.inject(Fault::Disconnect);
        } else {
            assert_eq!(
                unsafe { libc::kill(child.id().unwrap() as libc::pid_t, libc::SIGINT) },
                0
            );
        }
        let received = finish(child).await;
        assert_eq!(received.status.code(), Some(6));
        let rx = report(&received);
        assert_eq!(rx["state"], "incomplete");
        assert_eq!(rx["missing"], 4);
        clean(root, service).await;
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_submission_is_not_retried_and_active_opt_in_is_required() {
    let (root, bus, service) = setup().await;
    let args = [
        "sequence",
        "send",
        "--channel",
        "virtual:0",
        "--id",
        "123",
        "--run-id",
        "ABCDEF01",
        "--count",
        "4",
    ];
    let invalid = output(&root, &args).await;
    assert_eq!(invalid.status.code(), Some(2));
    assert!(service.state.runtime.session_snapshot().await.is_empty());
    bus.inject(Fault::FailNextTransmit);
    let mut args = args.to_vec();
    args.push("--active");
    let failed = output(&root, &args).await;
    assert!(!failed.status.success());
    let tx = report(&failed);
    assert_eq!(tx["attempted"], 1);
    assert_eq!(tx["submitted"], 0);
    assert_eq!(tx["failed"], 1);
    assert_eq!(tx["unknown"], 0);
    clean(root, service).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ranges_and_schedules_are_rejected_before_device_configuration() {
    let (root, _, service) = setup().await;
    for extra in [
        vec!["--first", "4294967295", "--count", "2"],
        vec!["--count", "6001"],
        vec!["--count", "4", "--interval-ms", "60000"],
    ] {
        let mut args = vec![
            "sequence",
            "send",
            "--channel",
            "virtual:0",
            "--id",
            "123",
            "--run-id",
            "1",
            "--active",
        ];
        args.extend(extra);
        let result = output(&root, &args).await;
        assert_eq!(result.status.code(), Some(2));
        assert!(service.state.runtime.session_snapshot().await.is_empty());
    }
    clean(root, service).await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelling_sender_prevents_later_sequence_submissions() {
    use canharbor_core::Backend;
    let (root, bus, service) = setup().await;
    let mut events = bus.subscribe("virtual:0").unwrap();
    let child = command(
        &root,
        &[
            "sequence",
            "send",
            "--channel",
            "virtual:0",
            "--id",
            "123",
            "--run-id",
            "1",
            "--count",
            "4",
            "--interval-ms",
            "1000",
            "--active",
        ],
    )
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()
    .unwrap();
    tokio::time::timeout(Duration::from_secs(5), events.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        unsafe { libc::kill(child.id().unwrap() as libc::pid_t, libc::SIGINT) },
        0
    );
    let result = finish(child).await;
    assert!(matches!(result.status.code(), Some(6 | 7)));
    let tx = report(&result);
    assert_eq!(tx["attempted"], 1);
    assert_eq!(tx["passed"], false);
    assert!(
        tokio::time::timeout(Duration::from_millis(1100), events.recv())
            .await
            .is_err()
    );
    clean(root, service).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn foreground_receiver_renews_control_and_cleans_up_at_its_deadline() {
    let (root, _, service) = setup().await;
    let mut child = command(
        &root,
        &[
            "sequence",
            "receive",
            "--channel",
            "virtual:1",
            "--id",
            "123",
            "--run-id",
            "1",
            "--count",
            "1",
            "--timeout-ms",
            "17100",
        ],
    )
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()
    .unwrap();
    let mut lines = BufReader::new(child.stderr.take().unwrap()).lines();
    let ready = tokio::time::timeout(Duration::from_secs(5), lines.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(ready.contains("receiver ready"), "{ready}");
    child.stderr = Some(lines.into_inner().into_inner());
    // The original lease expires after 15 seconds. Force maintenance after that
    // boundary: only successful foreground renewals can preserve acquisition.
    tokio::time::sleep(Duration::from_secs(16)).await;
    let sessions = service.state.runtime.list().await;
    assert_eq!(sessions.len(), 1);
    assert!(
        sessions[0].running,
        "foreground renewal failed to preserve control"
    );
    let result = finish(child).await;
    assert_eq!(result.status.code(), Some(6));
    let rx = report(&result);
    assert_eq!(rx["missing"], 1);
    assert!(rx["cleanup_error"].is_null());
    clean(root, service).await;
}

fn test_temp_dir() -> std::path::PathBuf {
    if cfg!(windows) {
        std::env::temp_dir()
    } else {
        std::path::PathBuf::from("/tmp")
    }
}
