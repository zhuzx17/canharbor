// SPDX-License-Identifier: AGPL-3.0-or-later
mod common;
use canharbor_core::{Backend, Config, runtime::Runtime};
use canharbor_gsusb::{backend::GsUsbBackend, wire};
use canharbor_server::local::RunningServer;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio_tungstenite::{
    client_async,
    tungstenite::{Message, client::IntoClientRequest},
};
#[tokio::test]
async fn diagnostic_survives_data_filter_and_precedes_terminal_fault() {
    let fake = common::Fake::default();
    let backend = Arc::new(
        GsUsbBackend::open(fake.clone(), 0, "diagnostic".into())
            .await
            .unwrap(),
    );
    let runtime = Runtime::new(backend.clone());
    let session = runtime
        .create_configured(
            &backend.channels()[0].id,
            Config {
                nominal_bitrate: 500_000,
                data_bitrate: None,
                listen_only: true,
            },
        )
        .await
        .unwrap();
    let lease = runtime.acquire(&session.id, "owner").await.unwrap();
    runtime
        .running(&session.id, "owner", lease.generation, true)
        .await
        .unwrap();
    let dir = std::path::PathBuf::from(if cfg!(windows) {
        std::env::var_os("TEMP").expect("TEMP")
    } else {
        std::ffi::OsString::from("/tmp")
    })
    .join(format!("ch-diagnostic-{}", std::process::id()));
    let server = RunningServer::start(runtime.clone(), &dir, 0, false)
        .await
        .unwrap();
    let mut request = format!("ws://localhost/api/v1/sessions/{}/stream", session.id)
        .into_client_request()
        .unwrap();
    request.headers_mut().insert(
        "x-canharbor-client",
        "00000000-0000-4000-8000-000000000001".parse().unwrap(),
    );
    let (mut ws, _) = client_async(request, canharbor_ipc::connect(&dir).await.unwrap())
        .await
        .unwrap();
    ws.send(Message::Text(
        json!({"type":"subscribe","protocol_version":"1.0","filter":{"ids":[999]}})
            .to_string()
            .into(),
    ))
    .await
    .unwrap();
    let mut injected = false;
    let mut diagnostic = false;
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match ws.next().await.unwrap().unwrap() {
                Message::Ping(bytes) => ws.send(Message::Pong(bytes)).await.unwrap(),
                Message::Text(text) => {
                    let _: canharbor_server::dto::StreamMessage =
                        serde_json::from_str(&text).unwrap();
                    let value: Value = serde_json::from_str(&text).unwrap();
                    match value["type"].as_str().unwrap() {
                        "hello" => {
                            let mut bytes = vec![0; 20];
                            bytes[..4].copy_from_slice(&wire::RX_ECHO.to_le_bytes());
                            bytes[4..8].copy_from_slice(&0x20000040u32.to_le_bytes());
                            bytes[8] = 8;
                            bytes[12] = 7;
                            fake.0.lock().unwrap().reads.push_back(Ok(bytes));
                            injected = true;
                        }
                        "events" => {
                            assert!(injected);
                            assert_eq!(value["events"][0]["type"], "bus_diagnostic");
                            assert_eq!(value["events"][0]["error_classes"], 64);
                            assert_eq!(value["events"][0]["raw_data_hex"], "0700000000000000");
                            diagnostic = true;
                        }
                        "session_fault" => {
                            assert!(diagnostic);
                            assert_eq!(value["code"], "bus_off");
                            break;
                        }
                        other => panic!("unexpected {other}"),
                    }
                }
                other => panic!("unexpected {other:?}"),
            }
        }
    })
    .await
    .unwrap();
    drop(ws);
    server.shutdown().await;
    std::fs::remove_dir_all(dir).unwrap();
}
