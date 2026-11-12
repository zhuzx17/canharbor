// SPDX-License-Identifier: AGPL-3.0-or-later
//! Pull-based event subscription. No detached reader, control acquisition or reconnect.
use crate::{ApiFailure, InvalidArgument, LocalClient, ResourceLimit, api, session_id};
use canharbor_ipc::StreamClient;
use futures_util::{SinkExt, StreamExt};
use std::{error::Error, fmt, time::Duration};
use tokio_tungstenite::{
    WebSocketStream, client_async_with_config,
    tungstenite::{Message, client::IntoClientRequest, protocol::WebSocketConfig},
};
type Result<T> = std::result::Result<T, Box<dyn Error>>;
#[derive(Debug)]
pub struct StreamUnavailable(pub String);
impl fmt::Display for StreamUnavailable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "cannot connect to local event service: {}", self.0)
    }
}
impl Error for StreamUnavailable {}
pub struct EventStream {
    socket: Option<WebSocketStream<StreamClient>>,
    epoch: Option<(String, String)>,
}
impl LocalClient {
    /// Subscribe without acquiring or renewing a lease. Drop closes the socket;
    /// callers choose if and when to establish a new subscription.
    pub async fn subscribe(
        &self,
        session: &str,
        subscription: &api::Subscribe,
    ) -> Result<EventStream> {
        session_id(session)?;
        if subscription.protocol_version != "1.0"
            || subscription.csrf_token.is_some()
            || subscription.filter.ids.len() > 256
            || subscription.filter.ids.iter().any(|id| *id > 0x1fff_ffff)
        {
            return Err(Box::new(InvalidArgument(
                "invalid native event subscription",
            )));
        }
        tokio::time::timeout(Duration::from_secs(10), async {
            let stream = canharbor_ipc::connect(&self.dir)
                .await
                .map_err(|e| StreamUnavailable(e.to_string()))?;
            let mut request =
                format!("ws://localhost/api/v1/sessions/{session}/stream").into_client_request()?;
            request
                .headers_mut()
                .insert("x-canharbor-client", self.client_id().parse()?);
            let config = WebSocketConfig::default()
                .read_buffer_size(16 * 1024)
                .write_buffer_size(0)
                .max_write_buffer_size(64 * 1024)
                .max_frame_size(Some(1024 * 1024))
                .max_message_size(Some(1024 * 1024));
            let (mut socket, _) =
                match client_async_with_config(request, stream, Some(config)).await {
                    Ok(value) => value,
                    Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
                        return Err(Box::new(ApiFailure {
                            status: response.status().as_u16(),
                            detail: None,
                            outcome_unknown: None,
                        }) as Box<dyn Error>);
                    }
                    Err(e) => return Err(Box::new(e) as Box<dyn Error>),
                };
            socket
                .send(Message::Text(serde_json::to_string(subscription)?.into()))
                .await?;
            Ok(EventStream {
                socket: Some(socket),
                epoch: None,
            })
        })
        .await
        .map_err(|_| {
            Box::new(StreamUnavailable("subscription deadline exceeded".into())) as Box<dyn Error>
        })?
    }
}
impl EventStream {
    /// Wait for one wire message. There is no client-side queue. Slow consumers
    /// receive the server's explicit gap records. Cancelling this wait does not
    /// reconnect or transmit frames.
    pub async fn next(&mut self) -> Result<Option<api::StreamMessage>> {
        let result = self.receive().await.map_err(|error| {
            use tokio_tungstenite::tungstenite::{Error as WsError, error::ProtocolError};
            match error.downcast_ref::<WsError>() {
                Some(
                    WsError::Io(_) | WsError::Protocol(ProtocolError::ResetWithoutClosingHandshake),
                ) => Box::new(StreamUnavailable(error.to_string())) as Box<dyn Error>,
                Some(WsError::Capacity(_)) => {
                    Box::new(ResourceLimit("event message exceeds configured limit"))
                }
                _ => error,
            }
        });
        if result.is_err() || matches!(&result, Ok(None)) {
            self.socket.take();
        }
        result
    }
    async fn receive(&mut self) -> Result<Option<api::StreamMessage>> {
        let Some(socket) = self.socket.as_mut() else {
            return Ok(None);
        };
        loop {
            match socket.next().await {
                Some(Ok(Message::Text(text))) => {
                    let message: api::StreamMessage = serde_json::from_str(&text)?;
                    match &message {
                        api::StreamMessage::Hello {
                            api_version,
                            server_instance_id,
                            session_epoch,
                            capacity,
                        } => {
                            if self.epoch.is_some()
                                || api_version != "1.0"
                                || *capacity == 0
                                || *capacity > 65536
                                || server_instance_id.len() > 128
                                || session_epoch.len() > 128
                            {
                                return Err("invalid event stream greeting".into());
                            }
                            self.epoch = Some((server_instance_id.clone(), session_epoch.clone()));
                        }
                        api::StreamMessage::Events {
                            server_instance_id,
                            session_epoch,
                            events,
                        } => {
                            if events.len() > 256 {
                                return Err(Box::new(ResourceLimit(
                                    "event batch exceeds 256 entries",
                                )));
                            }
                            if self.epoch.as_ref()
                                != Some(&(server_instance_id.clone(), session_epoch.clone()))
                            {
                                return Err("event stream epoch mismatch".into());
                            }
                        }
                        api::StreamMessage::ResumeUnavailable => {}
                        _ if self.epoch.is_none() => {
                            return Err("event stream missing greeting".into());
                        }
                        _ => {}
                    }
                    return Ok(Some(message));
                }
                Some(Ok(Message::Ping(_))) => socket.flush().await?,
                Some(Ok(Message::Pong(_))) => {}
                Some(Ok(Message::Close(_))) | None => return Ok(None),
                Some(Ok(_)) => return Err("unexpected binary event stream message".into()),
                Some(Err(error)) => return Err(Box::new(error)),
            }
        }
    }
    /// Bound the close attempt to two seconds. The underlying socket is dropped
    /// even when the peer does not complete the WebSocket close handshake.
    pub async fn close(&mut self) -> Result<()> {
        if let Some(mut socket) = self.socket.take() {
            tokio::time::timeout(Duration::from_secs(2), socket.close(None)).await??;
        }
        Ok(())
    }
}
