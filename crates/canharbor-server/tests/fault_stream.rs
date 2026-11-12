// SPDX-License-Identifier: AGPL-3.0-or-later
mod common;
use canharbor_core::{
    CanId, Frame,
    runtime::{Mode, Runtime},
};
use canharbor_server::local::RunningServer;
use canharbor_virtual::{Fault, VirtualBus};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio_tungstenite::{
    client_async,
    tungstenite::{Message, client::IntoClientRequest},
};
async fn next_text(
    ws: &mut tokio_tungstenite::WebSocketStream<canharbor_ipc::StreamClient>,
) -> Value {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match ws.next().await.unwrap().unwrap() {
                Message::Text(t) => return serde_json::from_str(&t).unwrap(),
                Message::Ping(p) => ws.send(Message::Pong(p)).await.unwrap(),
                other => panic!("expected text, got {other:?}"),
            }
        }
    })
    .await
    .unwrap()
}
#[tokio::test]
async fn injected_backend_faults_emit_valid_wire_messages_before_closing() {
    for (fault, expected) in [
        (Fault::Disconnect, "device_disconnected"),
        (Fault::BusOff, "bus_off"),
        (Fault::ReceiveOverflow, "receive_overflow"),
        (Fault::FailNextTransmit, "transmit_failed"),
    ] {
        let bus = Arc::new(VirtualBus::default());
        let runtime = Runtime::new(bus.clone());
        let client = uuid::Uuid::new_v4().to_string();
        let owner = format!("native:{client}");
        let s = runtime.create("virtual:0", Mode::Active).await.unwrap();
        let lease = runtime.acquire(&s.id, &owner).await.unwrap();
        runtime
            .running(&s.id, &owner, lease.generation, true)
            .await
            .unwrap();
        let dir = std::path::PathBuf::from(if cfg!(windows) {
            std::env::var_os("TEMP").expect("TEMP")
        } else {
            std::ffi::OsString::from("/tmp")
        })
        .join(format!("ch-ws-{}", uuid::Uuid::new_v4().simple()));
        let server = RunningServer::start(runtime.clone(), &dir, 0, false)
            .await
            .unwrap();
        let mut req = format!("ws://localhost/api/v1/sessions/{}/stream", s.id)
            .into_client_request()
            .unwrap();
        req.headers_mut()
            .insert("x-canharbor-client", client.parse().unwrap());
        let (mut ws, _) = client_async(req, canharbor_ipc::connect(&dir).await.unwrap())
            .await
            .unwrap();
        let subscribe = json!({"type":"subscribe","protocol_version":"1.0","filter":{}});
        common::validate("Subscribe", &subscribe);
        ws.send(Message::Text(subscribe.to_string().into()))
            .await
            .unwrap();
        let hello = next_text(&mut ws).await;
        common::validate("StreamMessage", &hello);
        assert_eq!(hello["type"], "hello");
        let mut observed = runtime.subscribe(&s.id).await.unwrap();
        runtime
            .send(
                &s.id,
                &owner,
                lease.generation,
                Frame::classic(CanId::new(0x123, false).unwrap(), vec![0xAB]).unwrap(),
            )
            .await
            .unwrap();
        // Wait for forwarding, then fault without yielding on this thread.
        // The WebSocket must flush this final frame before its terminal message.
        observed.recv().await.unwrap();
        bus.inject(fault);
        if matches!(fault, Fault::FailNextTransmit) {
            assert!(
                runtime
                    .send(
                        &s.id,
                        &owner,
                        lease.generation,
                        Frame::classic(CanId::new(1, false).unwrap(), vec![]).unwrap()
                    )
                    .await
                    .is_err()
            );
        }
        let event = next_text(&mut ws).await;
        common::validate("StreamMessage", &event);
        assert_eq!(event["type"], "events");
        assert_eq!(event["events"].as_array().unwrap().len(), 1);
        let event = next_text(&mut ws).await;
        common::validate("StreamMessage", &event);
        assert_eq!(event["type"], "session_fault");
        assert_eq!(event["code"], expected);
        assert!(!runtime.get(&s.id).await.unwrap().running);
        assert!(
            runtime
                .running(&s.id, &owner, lease.generation, true)
                .await
                .is_err()
        );
        drop(ws);
        server.shutdown().await;
        std::fs::remove_dir_all(dir).unwrap();
    }
}
