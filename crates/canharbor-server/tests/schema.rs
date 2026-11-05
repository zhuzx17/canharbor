// SPDX-License-Identifier: AGPL-3.0-or-later
mod common;
use canharbor_server::dto;
use serde_json::{Value, json};
#[test]
fn committed_schema_matches_the_actual_wire_dtos() {
    let expected = serde_json::to_value(dto::schema()).unwrap();
    let actual: Value = serde_json::from_str(include_str!("../../../api/schema.json")).unwrap();
    assert_eq!(expected, actual, "run bun run --cwd web api:generate");
}
#[test]
fn shared_frames_agree_with_schema_deserialization_and_core_validation() {
    let vectors: Value = serde_json::from_str(include_str!("../../../api/fixtures.json")).unwrap();
    let validator = common::validator("FrameInput");
    for (group, expected) in [("valid_frames", true), ("invalid_frames", false)] {
        for value in vectors[group].as_array().unwrap() {
            assert_eq!(validator.is_valid(value), expected, "schema: {value}");
            let parsed = serde_json::from_value::<dto::FrameDto>(value.clone())
                .ok()
                .and_then(|f| f.into_frame().ok());
            assert_eq!(parsed.is_some(), expected, "Rust: {value}");
            if let Some(f) = parsed {
                common::validate(
                    "Frame",
                    &serde_json::to_value(dto::Frame::from(&f)).unwrap(),
                );
            }
        }
    }
}
#[test]
fn stream_messages_preserve_discriminators_nullable_fields_and_large_counters() {
    let event = dto::event(canharbor_core::runtime::Event {
        sequence: 9007199254740993,
        elapsed_ns: u64::MAX,
        frame: canharbor_core::Frame::classic(
            canharbor_core::CanId::new(1, false).unwrap(),
            vec![],
        )
        .unwrap(),
        direction: "rx",
        metadata: canharbor_core::FrameMetadata::default(),
    });
    let wire = serde_json::to_value(dto::StreamMessage::Events {
        server_instance_id: "srv".into(),
        session_epoch: "s1".into(),
        events: vec![event],
    })
    .unwrap();
    common::validate("StreamMessage", &wire);
    assert_eq!(wire["events"][0]["sequence"], "9007199254740993");
    assert_eq!(wire["events"][0]["host_monotonic_ns"], u64::MAX.to_string());
    let mut bad = wire.clone();
    bad["events"][0]["sequence"] = json!(9007199254740993u64);
    assert!(!common::validator("StreamMessage").is_valid(&bad));
    assert!(!common::validator("StreamMessage").is_valid(&json!({"type":"unknown"})));
    for state in [dto::SessionState::Faulted, dto::SessionState::Disconnected] {
        common::validate(
            "StreamMessage",
            &serde_json::to_value(dto::StreamMessage::SessionFault {
                state,
                code: dto::FaultCode::DeviceDisconnected,
            })
            .unwrap(),
        );
    }
    common::validate("Auth", &json!({"client_id":"native","csrf_token":null}));
    assert!(!common::validator("Auth").is_valid(&json!({"client_id":"missing"})));
}

#[test]
fn shared_counter_boundaries_agree_with_rust_and_json_schema() {
    let fixtures: Value = serde_json::from_str(include_str!("../../../api/fixtures.json")).unwrap();
    let validator = common::validator("Decimal");
    for (group, expected) in [("valid_counters", true), ("invalid_counters", false)] {
        for value in fixtures[group].as_array().unwrap() {
            assert_eq!(validator.is_valid(value), expected, "schema: {value}");
            assert_eq!(
                serde_json::from_value::<dto::Decimal>(value.clone()).is_ok(),
                expected,
                "Rust: {value}"
            );
        }
    }
}

#[test]
fn request_id_unicode_limits_match_schema_and_runtime_validation() {
    for atom in ["A", "中", "🚀"] {
        for count in [0, 1, 128, 129] {
            let id = atom.repeat(count);
            let expected = (1..=128).contains(&count);
            assert_eq!(dto::valid_client_request_id(&id), expected);
            for (name, value) in [
                ("ReceiptLookup", json!({"client_request_id":id})),
                (
                    "Tx",
                    json!({"client_request_id":id,"frame":{"kind":"classic_data","id":1,"data_hex":""}}),
                ),
                (
                    "CaptureRequest",
                    json!({"client_request_id":id,"session_id":"s_test"}),
                ),
            ] {
                assert_eq!(
                    common::validator(name).is_valid(&value),
                    expected,
                    "{name}, {count} scalars"
                );
            }
        }
    }
}
