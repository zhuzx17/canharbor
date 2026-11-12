// SPDX-License-Identifier: AGPL-3.0-or-later
use canharbor_client::{ApiFailure, LocalClient, api};
use canharbor_core::runtime::Runtime;
use canharbor_server::local::RunningServer;
use canharbor_virtual::VirtualBus;
use serde_json::json;
use std::sync::Arc;
#[tokio::test]
async fn typed_clients_share_contract_and_keep_independent_control_identities() {
    let dir = std::path::PathBuf::from(if cfg!(windows) {
        std::env::var_os("TEMP").expect("TEMP")
    } else {
        std::ffi::OsString::from("/tmp")
    })
    .join(format!("ch-sdk-{}", uuid::Uuid::new_v4().simple()));
    let server = RunningServer::start(
        Runtime::new(Arc::new(VirtualBus::default())),
        &dir,
        0,
        false,
    )
    .await
    .unwrap();
    let first = LocalClient::new(dir.clone()).unwrap();
    let second = LocalClient::new(dir.clone()).unwrap();
    assert_ne!(first.client_id(), second.client_id());
    assert_eq!(first.version().await.unwrap().api_version, "1.0");
    assert!(!first.devices().await.unwrap().devices.is_empty());
    let created = first
        .create(&api::CreateRequest {
            channel_id: "virtual:0".into(),
            config: api::Config {
                mode: api::SessionMode::Active,
                nominal_bitrate: 500_000,
                fd: None,
            },
        })
        .await
        .unwrap();
    let id = created.id.as_str();
    let acquired = first.acquire(id).await.unwrap();
    let generation = acquired.generation.as_str().to_owned();
    let conflict = second.acquire(id).await.err().unwrap();
    let conflict = conflict.downcast_ref::<ApiFailure>().unwrap();
    assert_eq!(conflict.status, 409);
    assert!(conflict.detail.is_some());
    first.running(id, &generation, true).await.unwrap();
    let tx = api::Tx {
        client_request_id: "sdk-one".into(),
        frame: api::FrameDto {
            kind: api::FrameKind::ClassicData,
            id: 0x123,
            extended: false,
            brs: false,
            data_hex: "0102".into(),
            dlc: None,
        },
    };
    let receipt = first.submit(id, &generation, &tx).await.unwrap();
    let duplicate = first.submit(id, &generation, &tx).await.unwrap();
    assert_eq!(receipt.receipt, duplicate.receipt);
    let queried = first
        .receipt(id, receipt.receipt.parse().unwrap())
        .await
        .unwrap();
    assert_eq!(queried.receipt, receipt.receipt);
    let lookup = second
        .receipt_by_request(id, "sdk-one", Some(&generation))
        .await
        .unwrap();
    assert_eq!(lookup.receipt, receipt.receipt);
    let mut odd = tx.clone();
    odd.client_request_id = "space + / ? & = 中文".into();
    let expected = first.submit(id, &generation, &odd).await.unwrap();
    let encoded = second
        .receipt_by_request(id, &odd.client_request_id, Some(&generation))
        .await
        .unwrap();
    assert_eq!(encoded.receipt, expected.receipt);
    let mut boundary = tx.clone();
    boundary.client_request_id = "🚀".repeat(128);
    let expected = first.submit(id, &generation, &boundary).await.unwrap();
    let lookup = second
        .receipt_by_request(id, &boundary.client_request_id, None)
        .await
        .unwrap();
    assert_eq!(lookup.receipt, expected.receipt);
    assert!(
        second
            .receipt_by_request(id, &"🚀".repeat(129), None)
            .await
            .is_err()
    );

    first.running(id, &generation, false).await.unwrap();
    assert!(
        first
            .request("GET", "/../version", json!({}), None)
            .await
            .is_err()
    );
    assert!(server.shutdown().await.failures.is_empty());
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn response_bounds_and_redirect_refusal_are_enforced_over_uds() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let root = std::path::PathBuf::from(if cfg!(windows) {
        std::env::var_os("TEMP").expect("TEMP")
    } else {
        std::ffi::OsString::from("/tmp")
    })
    .join(format!("ch-sdk-bound-{}", uuid::Uuid::new_v4().simple()));
    canharbor_platform::private::directory(&root).unwrap();
    for kind in 0..4 {
        let mut listener = canharbor_ipc::Listener::bind(&root).unwrap();
        let peer = tokio::spawn(async move {
            let mut socket = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            let _ = socket.read(&mut request).await.unwrap();
            match kind {
                0 => {
                    let _=socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1048577\r\nConnection: close\r\n\r\n").await;
                }
                1 => {
                    let _=socket.write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n100001\r\n").await;
                    let _ = socket.write_all(&vec![b' '; 1048577]).await;
                    let _ = socket.write_all(b"\r\n0\r\n\r\n").await;
                }
                2 => {
                    let _=socket.write_all(b"HTTP/1.1 302 Found\r\nLocation: http://example.invalid/other\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}").await;
                }
                _ => {
                    let _=socket.write_all(b"HTTP/1.1 503 Unavailable\r\nContent-Length: 4\r\nConnection: close\r\n\r\noops").await;
                }
            }
        });
        let client = LocalClient::new(root.clone()).unwrap();
        let error = client
            .request("GET", "/version", json!(null), None)
            .await
            .err()
            .unwrap();
        if kind == 2 {
            assert_eq!(error.downcast_ref::<ApiFailure>().unwrap().status, 302)
        } else if kind == 3 {
            let api = error.downcast_ref::<ApiFailure>().unwrap();
            assert_eq!(api.status, 503);
            assert!(api.detail.is_none());
            assert!(api.submission_is_unknown());
        } else {
            assert!(error.to_string().contains("JSON limit"));
        }
        peer.await.unwrap();
        #[cfg(unix)]
        std::fs::remove_file(root.join("service.sock")).unwrap();
    }
    std::fs::remove_dir(root).unwrap();
}

#[tokio::test]
async fn failed_binary_downloads_do_not_publish_partial_files() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let root = std::path::PathBuf::from(if cfg!(windows) {
        std::env::var_os("TEMP").expect("TEMP")
    } else {
        std::ffi::OsString::from("/tmp")
    })
    .join(format!("ch-download-{}", uuid::Uuid::new_v4().simple()));
    canharbor_platform::private::directory(&root).unwrap();
    for oversized in [false, true] {
        let mut listener = canharbor_ipc::Listener::bind(&root).unwrap();
        let peer = tokio::spawn(async move {
            let mut stream = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            let _ = stream.read(&mut request).await.unwrap();
            let response = if oversized {
                b"HTTP/1.1 200 OK\r\nContent-Length: 67108865\r\nConnection: close\r\n\r\n"
                    .as_slice()
            } else {
                b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\nabc".as_slice()
            };
            let _ = stream.write_all(response).await;
        });
        let client = LocalClient::new(root.clone()).unwrap();
        let destination = root.join("capture.ndjson");
        assert!(
            client
                .download(uuid::Uuid::new_v4(), destination.clone())
                .await
                .is_err()
        );
        assert!(!destination.exists());
        assert!(
            !std::fs::read_dir(&root).unwrap().any(|e| e
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".part"))
        );
        peer.await.unwrap();
        #[cfg(unix)]
        std::fs::remove_file(root.join("service.sock")).unwrap();
    }
    std::fs::remove_dir(root).unwrap();
}
