// SPDX-License-Identifier: AGPL-3.0-or-later
//! Wire contract re-export and server-owned capture conversion.
pub use canharbor_api::*;
pub fn capture(info: crate::capture::Info) -> Capture {
    use crate::capture::Status;
    Capture {
        name: info.name,
        channel_id: info.channel_id,
        created_unix_ms: info.created_unix_ms,
        id: info.id,
        session_id: info.session_id,
        state: match info.state {
            Status::Starting => CaptureState::Starting,
            Status::Recording => CaptureState::Recording,
            Status::Completed => CaptureState::Completed,
            Status::Incomplete => CaptureState::Incomplete,
            Status::Failed => CaptureState::Failed,
        },
        complete: info.state == Status::Completed,
        loss_count_known: info.loss_count_known,
        expires_unix_ms: info.expires_unix_ms,
        events: info.events.into(),
        dropped: info.dropped.into(),
        bytes: info.bytes.into(),
        reason: info.reason.and_then(|reason| {
            serde_json::to_value(reason)
                .ok()?
                .as_str()
                .map(str::to_owned)
        }),
    }
}
