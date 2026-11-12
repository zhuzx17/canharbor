// SPDX-License-Identifier: AGPL-3.0-or-later
use canharbor_client::{LocalClient, TaskStartUnknown, api};
use canharbor_core::runtime::Runtime;
use canharbor_server::local::RunningServer;
use canharbor_virtual::VirtualBus;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
fn plan(id: &str, count: u32) -> api::PeriodicRequest {
    api::PeriodicRequest {
        client_request_id: id.into(),
        frame: api::FrameDto {
            kind: api::FrameKind::ClassicData,
            id: 0x123,
            extended: false,
            brs: false,
            data_hex: "0102".into(),
            dlc: None,
        },
        count,
        interval_ms: 1000,
    }
}
#[tokio::test]
async fn typed_tasks_keep_explicit_control_and_read_only_recovery() {
    let root = std::path::PathBuf::from(if cfg!(windows) {
        std::env::var_os("TEMP").expect("TEMP")
    } else {
        std::ffi::OsString::from("/tmp")
    })
    .join(format!("ch-task-sdk-{}", uuid::Uuid::new_v4().simple()));
    let server = RunningServer::start(
        Runtime::new(Arc::new(VirtualBus::default())),
        &root,
        0,
        false,
    )
    .await
    .unwrap();
    let client = LocalClient::new(root.clone()).unwrap();
    let other = LocalClient::new(root.clone()).unwrap();
    let created = client
        .create(&api::CreateRequest {
            channel_id: "virtual:0".into(),
            config: api::Config {
                mode: api::SessionMode::Active,
                nominal_bitrate: 500000,
                fd: None,
            },
        })
        .await
        .unwrap();
    let lease = client.acquire(&created.id).await.unwrap();
    let g = lease.generation.as_str();
    client.running(&created.id, g, true).await.unwrap();
    assert_eq!(
        client
            .renew(&created.id, g)
            .await
            .unwrap()
            .generation
            .as_str(),
        g
    );
    let task = client
        .start_periodic(&created.id, g, &plan("sdk", 1))
        .await
        .unwrap();
    assert_eq!(
        client
            .start_periodic(&created.id, g, &plan("sdk", 1))
            .await
            .unwrap()
            .id,
        task.id
    );
    assert_eq!(
        other
            .recover_periodic(&created.id, "sdk", Some(g))
            .await
            .unwrap()
            .unwrap()
            .id,
        task.id
    );
    let id = task.id.parse().unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        let t = other.periodic_task(id).await.unwrap();
        if matches!(t.state, api::TaskState::Completed) {
            assert_eq!(t.submitted.as_str(), "1");
            break;
        }
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let mut long = plan("cancel", 2);
    long.interval_ms = 60000;
    let task = client.start_periodic(&created.id, g, &long).await.unwrap();
    other
        .cancel_periodic(task.id.parse().unwrap())
        .await
        .unwrap();
    assert!(
        client
            .recover_periodic(&created.id, "absent", Some(g))
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        other
            .start_periodic(&created.id, g, &plan("wrong-owner", 1))
            .await
            .err()
            .unwrap()
            .downcast_ref::<TaskStartUnknown>()
            .is_none()
    );
    assert!(
        client
            .start_periodic(&created.id, "01", &plan("invalid", 1))
            .await
            .is_err()
    );
    assert!(
        client
            .start_periodic(&created.id, g, &plan("invalid", 0))
            .await
            .is_err()
    );
    client.release(&created.id, g).await.unwrap();
    assert!(server.shutdown().await.failures.is_empty());
    std::fs::remove_dir_all(root).unwrap();
}
#[tokio::test]
async fn lost_start_response_retains_identity_and_recovery_never_posts() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let root = std::path::PathBuf::from(if cfg!(windows) {
        std::env::var_os("TEMP").expect("TEMP")
    } else {
        std::ffi::OsString::from("/tmp")
    })
    .join(format!("ch-task-loss-{}", uuid::Uuid::new_v4().simple()));
    canharbor_platform::private::directory(&root).unwrap();
    let mut listener = canharbor_ipc::Listener::bind(&root).unwrap();
    let service = tokio::spawn(async move {
        for index in 0..2 {
            let mut socket = listener.accept().await.unwrap();
            let mut bytes = vec![];
            let mut part = [0; 4096];
            loop {
                let n = socket.read(&mut part).await.unwrap();
                assert!(n > 0);
                bytes.extend_from_slice(&part[..n]);
                if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                    let header = std::str::from_utf8(&bytes[..end]).unwrap();
                    let length = header
                        .lines()
                        .find_map(|l| {
                            let (k, v) = l.split_once(':')?;
                            k.eq_ignore_ascii_case("content-length")
                                .then(|| v.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    if bytes.len() >= end + 4 + length {
                        break;
                    }
                }
            }
            if index == 0 {
                assert!(bytes.starts_with(b"POST /api/v1/sessions/session-1/tasks "));
                continue;
            }
            assert!(bytes.starts_with(b"GET /api/v1/tasks "));
            let body=json!({"tasks":[{"id":uuid::Uuid::nil().to_string(),"session_id":"session-1","client_request_id":"lost","generation":"1","simulated":true,"state":"completed","planned":"1","attempted":"1","submitted":"1","failed":"0","unknown":"0","skipped":"0","max_dispatch_lateness_ns":"0","total_dispatch_lateness_ns":"0","reason":Value::Null}]}).to_string();
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        }
    });
    let client = LocalClient::new(root.clone()).unwrap();
    let error = client
        .start_periodic("session-1", "1", &plan("lost", 1))
        .await
        .err()
        .unwrap();
    let error = error.downcast_ref::<TaskStartUnknown>().unwrap();
    assert_eq!(error.session, "session-1");
    assert_eq!(error.generation, "1");
    assert_eq!(error.request_id, "lost");
    assert_eq!(
        client
            .recover_periodic(&error.session, &error.request_id, Some(&error.generation))
            .await
            .unwrap()
            .unwrap()
            .submitted
            .as_str(),
        "1"
    );
    service.await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}
