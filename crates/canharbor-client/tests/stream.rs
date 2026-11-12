// SPDX-License-Identifier: AGPL-3.0-or-later
use canharbor_client::{LocalClient, api};
use canharbor_core::runtime::Runtime;
use canharbor_server::local::RunningServer;
use canharbor_virtual::VirtualBus;
use futures_util::{SinkExt, StreamExt};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio_tungstenite::tungstenite::Message;
fn directory() -> PathBuf {
    std::path::PathBuf::from(if cfg!(windows) {
        std::env::var_os("TEMP").expect("TEMP")
    } else {
        std::ffi::OsString::from("/tmp")
    })
    .join(format!("ch-stream-{}", uuid::Uuid::new_v4().simple()))
}
fn subscription() -> api::Subscribe {
    api::Subscribe {
        r#type: api::SubscribeKind::Subscribe,
        protocol_version: "1.0".into(),
        csrf_token: None,
        after_sequence: None,
        filter: api::Filter::default(),
    }
}
#[tokio::test]
async fn observer_receives_without_control_and_cancelled_wait_does_not_resubscribe() {
    let dir = directory();
    let server = RunningServer::start(
        Runtime::new(Arc::new(VirtualBus::default())),
        &dir,
        0,
        false,
    )
    .await
    .unwrap();
    let sender = LocalClient::new(dir.clone()).unwrap();
    let observer = LocalClient::new(dir.clone()).unwrap();
    let session = sender
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
    let acquired = sender.acquire(&session.id).await.unwrap();
    let mut stream = observer
        .subscribe(&session.id, &subscription())
        .await
        .unwrap();
    assert!(matches!(
        stream.next().await.unwrap(),
        Some(api::StreamMessage::Hello { .. })
    ));
    assert!(
        tokio::time::timeout(Duration::from_millis(20), stream.next())
            .await
            .is_err()
    );
    sender
        .running(&session.id, acquired.generation.as_str(), true)
        .await
        .unwrap();
    sender
        .submit(
            &session.id,
            acquired.generation.as_str(),
            &api::Tx {
                client_request_id: "stream-test".into(),
                frame: api::FrameDto {
                    kind: api::FrameKind::ClassicData,
                    id: 0x123,
                    extended: false,
                    brs: false,
                    data_hex: "AA".into(),
                    dlc: None,
                },
            },
        )
        .await
        .unwrap();
    let event = tokio::time::timeout(Duration::from_secs(2), stream.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let api::StreamMessage::Events { events, .. } = event else {
        panic!("missing events");
    };
    assert!(
        events
            .iter()
            .any(|e| matches!(e, api::BusEvent::FrameReceived {frame, ..} if frame.id == 0x123))
    );
    // Subscription did not acquire another lease or change the owner.
    assert_eq!(
        sender
            .request("GET", "/sessions", serde_json::Value::Null, None)
            .await
            .unwrap()["sessions"][0]["controller"]
            .as_str()
            .map(str::to_owned),
        acquired.controller
    );
    stream.close().await.unwrap();
    assert!(stream.next().await.unwrap().is_none());
    assert!(server.shutdown().await.failures.is_empty());
    std::fs::remove_dir_all(dir).unwrap();
}
#[tokio::test]
async fn malformed_and_oversized_streams_terminate_without_reconnect() {
    for payload in ["{}".to_owned(), "X".repeat(1024 * 1024 + 1), serde_json::json!({"type":"events","server_instance_id":"other","session_epoch":"other","events":[]}).to_string()] {
        let dir = directory(); canharbor_platform::private::directory(&dir).unwrap();
        let mut listener = canharbor_ipc::Listener::bind(&dir).unwrap();
        let peer = tokio::spawn(async move {
            let socket = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(socket).await.unwrap();
            assert!(matches!(ws.next().await, Some(Ok(Message::Text(_)))));
            let _ = ws.send(Message::Text(payload.into())).await;
        });
        let client = LocalClient::new(dir.clone()).unwrap();
        let mut stream = client.subscribe("s_fixture", &subscription()).await.unwrap();
        assert!(tokio::time::timeout(Duration::from_secs(2), stream.next()).await.unwrap().is_err());
        assert!(stream.next().await.unwrap().is_none());
        peer.await.unwrap(); std::fs::remove_dir_all(dir).unwrap();
    }
}
