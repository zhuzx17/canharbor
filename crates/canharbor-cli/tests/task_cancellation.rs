// SPDX-License-Identifier: AGPL-3.0-or-later
#![cfg(unix)]
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::Command,
};
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lost_or_malformed_cancellation_responses_are_unknown_but_rejections_are_preserved() {
    for mode in 0..3 {
        let root =
            test_temp_dir().join(format!("ch-cancel-loss-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir(&root).unwrap();
        let listener = tokio::net::UnixListener::bind(root.join("service.sock")).unwrap();
        let id = uuid::Uuid::new_v4();
        let service = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = vec![];
            let mut part = [0; 4096];
            loop {
                let n = socket.read(&mut part).await.unwrap();
                assert!(n > 0);
                bytes.extend_from_slice(&part[..n]);
                if let Some(end) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
                    let headers = std::str::from_utf8(&bytes[..end]).unwrap();
                    let len = headers
                        .lines()
                        .find_map(|l| {
                            let (k, v) = l.split_once(':')?;
                            k.eq_ignore_ascii_case("content-length")
                                .then(|| v.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    if bytes.len() >= end + 4 + len {
                        break;
                    }
                }
            }
            assert!(bytes.starts_with(format!("POST /api/v1/tasks/{id}/cancel ").as_bytes()));
            match mode {
                0 => {}
                1 => {
                    let body = "{}";
                    socket.write_all(format!("HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
                }
                _ => {
                    let body = r#"{"error":{"code":"lease_conflict","message":"rejected","retryable":false,"request_id":"test","receipt":null,"outcome_unknown":false}}"#;
                    socket.write_all(format!("HTTP/1.1 409 Conflict\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
                }
            }
            drop(socket);
            assert!(
                tokio::time::timeout(Duration::from_millis(100), listener.accept())
                    .await
                    .is_err(),
                "cancellation was retried"
            );
        });
        let out = tokio::time::timeout(
            Duration::from_secs(5),
            Command::new(env!("CARGO_BIN_EXE_canharbor"))
                .arg("--runtime-dir")
                .arg(&root)
                .args(["task", "cancel", &id.to_string()])
                .kill_on_drop(true)
                .output(),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            out.status.code(),
            Some(if mode == 2 { 5 } else { 7 }),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        if mode != 2 {
            let error = String::from_utf8(out.stderr).unwrap();
            assert!(error.contains("cancellation outcome unknown"));
            assert!(error.contains(&id.to_string()));
            assert!(error.contains("task show"));
        }
        assert!(out.stdout.is_empty());
        service.await.unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
}

fn test_temp_dir() -> std::path::PathBuf {
    if cfg!(windows) {
        std::env::temp_dir()
    } else {
        std::path::PathBuf::from("/tmp")
    }
}
