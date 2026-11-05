// SPDX-License-Identifier: AGPL-3.0-or-later
use canharbor_core::{Backend, CanId, Config, Frame, runtime::Runtime};
use canharbor_slcan::{
    Decoder, Error as ProtocolError, MAX_CANABLE_LINE, Profile, Response,
    backend::SlcanBackend,
    encode_with_profile,
    engine::{Engine, Error, FirmwareVersion},
    parse, parse_with_profile,
    serial::Transport,
};
use std::{
    collections::VecDeque,
    io,
    sync::{Arc, Mutex},
    time::Duration,
};

const RX: &[u8] = b"d402D0100199800000000979299149DABADB3A89F00ABA7AEAEAEACBFA2A100000000";

#[test]
fn fd_codec_handles_all_dlc_id_widths_brs_and_fragmented_maximum_records() {
    // Independent wire fixtures prevent encode/decode from sharing a mistake.
    for (extended, brs, expected) in [
        (false, false, b"d12390102030405060708090A0B0C\r".as_slice()),
        (false, true, b"b12390102030405060708090A0B0C\r"),
        (true, false, b"D0123ABCD90102030405060708090A0B0C\r"),
        (true, true, b"B0123ABCD90102030405060708090A0B0C\r"),
    ] {
        let frame = Frame::fd(
            CanId::new(if extended { 0x123abcd } else { 0x123 }, extended).unwrap(),
            (1..=12).collect(),
            brs,
        )
        .unwrap();
        assert_eq!(
            encode_with_profile(&frame, Profile::CanableFd).unwrap(),
            expected
        );
    }
    for extended in [false, true] {
        for brs in [false, true] {
            for length in (0..=8).chain([12, 16, 20, 24, 32, 48, 64]) {
                let frame = Frame::fd(
                    CanId::new(if extended { 0x123abcd } else { 0x123 }, extended).unwrap(),
                    (0..length).map(|n| n as u8).collect(),
                    brs,
                )
                .unwrap();
                let encoded = encode_with_profile(&frame, Profile::CanableFd).unwrap();
                assert_eq!(encoded.len(), if extended { 11 } else { 6 } + length * 2);
                assert!(parse(&encoded[..encoded.len() - 1]).is_err());
                let mut decoder = Decoder::with_profile(Profile::CanableFd);
                for byte in &encoded[..encoded.len() - 1] {
                    assert!(decoder.push(*byte).is_none());
                }
                assert_eq!(
                    decoder.push(b'\r'),
                    Some(Ok(Response::Frame {
                        frame,
                        timestamp_ms: None
                    }))
                );
            }
        }
    }
    let Response::Frame {
        frame,
        timestamp_ms,
    } = parse_with_profile(RX, Profile::CanableFd).unwrap()
    else {
        panic!()
    };
    assert_eq!(frame.id().value(), 0x402);
    assert_eq!(frame.data().len(), 32);
    assert!(matches!(frame, Frame::FdData { brs: false, .. }));
    assert_eq!(timestamp_ms, None);
}

#[test]
fn invalid_fd_and_oversized_input_are_bounded_and_default_remains_strict() {
    for line in [
        b"d8000".as_slice(),
        b"D200000000",
        b"d0019",
        b"d0011AG",
        b"d0010FFFF",
        b"b0011",
        b"d001G",
    ] {
        assert!(
            parse_with_profile(line, Profile::CanableFd).is_err(),
            "{line:?}"
        );
    }
    let invalid = Frame::FdData {
        id: CanId::new(1, false).unwrap(),
        data: vec![0; 9],
        brs: false,
    };
    assert!(encode_with_profile(&invalid, Profile::CanableFd).is_err());
    let mut decoder = Decoder::with_profile(Profile::CanableFd);
    for _ in 0..MAX_CANABLE_LINE + 10_000 {
        assert!(decoder.push(b'X').is_none());
    }
    assert_eq!(decoder.push(b'\r'), Some(Err(ProtocolError::TooLong)));
    assert_eq!(decoder.push(7), Some(Err(ProtocolError::Rejected)));
    for byte in b"d0010" {
        assert!(decoder.push(*byte).is_none());
    }
    assert!(matches!(
        decoder.push(b'\r'),
        Some(Ok(Response::Frame { .. }))
    ));
    assert!(parse_with_profile(b"custom-build", Profile::CanableFd).is_err());
    assert_eq!(Profile::Lawicel.bitrate(800_000).unwrap(), *b"S7\r");
    assert!(Profile::CanableFd.bitrate(800_000).is_err());
    assert_eq!(Profile::CanableFd.bitrate(750_000).unwrap(), *b"S7\r");
    assert_eq!(Profile::CanableFd.bitrate(83_300).unwrap(), *b"S9\r");
}

#[derive(Default)]
struct State {
    writes: Vec<Vec<u8>>,
    input: VecDeque<u8>,
    short: bool,
}
struct Fake(Arc<Mutex<State>>);
impl Transport for Fake {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let mut state = self.0.lock().unwrap();
        state.writes.push(bytes.to_vec());
        if state.short {
            return Ok(bytes.len() - 1);
        }
        if bytes == b"V\r" {
            state.input.extend(b"custom-build\r");
        }
        // No ACK/NACK and no TX echo, just like the tested firmware.
        Ok(bytes.len())
    }
    fn read(&mut self, bytes: &mut [u8], timeout: Duration) -> io::Result<usize> {
        let mut state = self.0.lock().unwrap();
        let n = bytes.len().min(state.input.len()).min(2);
        for byte in &mut bytes[..n] {
            *byte = state.input.pop_front().unwrap();
        }
        drop(state);
        if n == 0 {
            std::thread::sleep(timeout.min(Duration::from_millis(2)));
        }
        Ok(n)
    }
}
fn config() -> Config {
    Config {
        nominal_bitrate: 1_000_000,
        data_bitrate: Some(5_000_000),
        listen_only: false,
    }
}
fn frame() -> Frame {
    Frame::fd(CanId::new(1, false).unwrap(), vec![0xaa; 32], false).unwrap()
}
fn engine() -> (Engine<Fake>, Arc<Mutex<State>>) {
    let state = Arc::new(Mutex::new(State::default()));
    let mut engine = Engine::with_profile(Fake(state.clone()), Profile::CanableFd);
    assert_eq!(
        engine.initialize().unwrap(),
        FirmwareVersion::Canable("custom-build".into())
    );
    (engine, state)
}
#[test]
fn no_ack_commands_stream_rx_and_retain_unknown_tx_without_retry() {
    let (mut engine, state) = engine();
    engine.start(config(), || Ok(())).unwrap();
    engine.transmit(&frame(), || Ok(())).unwrap();
    assert_eq!(engine.unknown_transmissions(), 1);
    state
        .lock()
        .unwrap()
        .input
        .extend(RX.iter().chain(b"\r").copied());
    for _ in 0..40 {
        engine.poll(false, || Ok(())).unwrap();
    }
    assert_eq!(engine.frames().next().unwrap().frame.id().value(), 0x402);
    assert_eq!(
        engine.unknown_transmissions(),
        1,
        "remote RX cannot confirm a TX command"
    );
    let before = state.lock().unwrap().writes.len();
    assert!(engine.poll(true, || Ok(())).is_err());
    assert!(engine.status(|| Ok(())).is_err());
    assert_eq!(state.lock().unwrap().writes.len(), before);
    engine.stop().unwrap();
    let writes = &state.lock().unwrap().writes;
    assert_eq!(
        &writes[..6],
        &[
            b"V\r".to_vec(),
            b"C\r".to_vec(),
            b"S8\r".to_vec(),
            b"Y5\r".to_vec(),
            b"M0\r".to_vec(),
            b"O\r".to_vec()
        ]
    );
    assert_eq!(writes.len(), 8);
    assert_eq!(
        writes[6],
        encode_with_profile(&frame(), Profile::CanableFd).unwrap()
    );
    assert_eq!(writes[7], b"C\r");
}
#[test]
fn listen_only_classic_mode_invalid_rates_and_revocation_prevent_tx() {
    let (mut engine, state) = engine();
    let mut invalid = config();
    invalid.data_bitrate = Some(4_000_000);
    assert!(engine.start(invalid, || Ok(())).is_err());
    assert_eq!(state.lock().unwrap().writes.len(), 1);
    let mut listen = config();
    listen.listen_only = true;
    engine.start(listen, || Ok(())).unwrap();
    assert_eq!(state.lock().unwrap().writes[4], b"M1\r");
    assert!(engine.transmit(&frame(), || Ok(())).is_err());
    engine.stop().unwrap();
    let mut classic = config();
    classic.data_bitrate = None;
    engine.start(classic, || Ok(())).unwrap();
    assert!(engine.transmit(&frame(), || Ok(())).is_err());
    engine.stop().unwrap();
    engine.start(config(), || Ok(())).unwrap();
    let before = state.lock().unwrap().writes.len();
    assert!(
        engine
            .transmit(&frame(), || Err(canharbor_core::Error::ControlExpired))
            .is_err()
    );
    assert_eq!(state.lock().unwrap().writes.len(), before);
    assert_eq!(engine.unknown_transmissions(), 0);
    engine.stop().unwrap();
    for allowed in 0..5 {
        let checks = std::cell::Cell::new(0);
        let before = state.lock().unwrap().writes.len();
        assert!(
            engine
                .start(config(), || {
                    let count = checks.get();
                    checks.set(count + 1);
                    if count < allowed {
                        Ok(())
                    } else {
                        Err(canharbor_core::Error::ControlExpired)
                    }
                })
                .is_err()
        );
        assert_eq!(state.lock().unwrap().writes.len() - before, allowed);
        assert_ne!(state.lock().unwrap().writes.last().unwrap(), b"O\r");
    }
}
#[test]
fn partial_fd_write_remains_unknown_and_never_gets_a_cleanup_terminator() {
    let (mut engine, state) = engine();
    engine.start(config(), || Ok(())).unwrap();
    state.lock().unwrap().short = true;
    assert!(engine.transmit(&frame(), || Ok(())).is_err());
    assert_eq!(engine.unknown_transmissions(), 1);
    let before = state.lock().unwrap().writes.len();
    assert!(matches!(engine.stop(), Err(Error::Poisoned)));
    assert!(engine.transmit(&frame(), || Ok(())).is_err());
    assert_eq!(state.lock().unwrap().writes.len(), before);
}
#[tokio::test]
async fn fd_backend_runtime_reports_capabilities_rx_and_unconfirmed_shutdown() {
    let state = Arc::new(Mutex::new(State::default()));
    assert!(
        SlcanBackend::open_with_profile(
            Fake(state.clone()),
            "fake".into(),
            true,
            Profile::CanableFd
        )
        .await
        .is_err()
    );
    assert!(state.lock().unwrap().writes.is_empty());
    let backend = Arc::new(
        SlcanBackend::open_with_profile(
            Fake(state.clone()),
            "fake".into(),
            false,
            Profile::CanableFd,
        )
        .await
        .unwrap(),
    );
    let caps = backend.capabilities("slcan:fake:0").unwrap();
    assert!(caps.fd);
    assert!(!caps.timestamps);
    assert_eq!(caps.fixed_data_bitrates, [2_000_000, 5_000_000]);
    let runtime = Runtime::new(backend);
    let session = runtime
        .create_configured("slcan:fake:0", config())
        .await
        .unwrap();
    let lease = runtime.acquire(&session.id, "owner").await.unwrap();
    runtime
        .running(&session.id, "owner", lease.generation, true)
        .await
        .unwrap();
    let mut events = runtime.subscribe(&session.id).await.unwrap();
    runtime
        .send(&session.id, "owner", lease.generation, frame())
        .await
        .unwrap();
    assert!(events.try_recv().is_err(), "no synthetic TX echo");
    state
        .lock()
        .unwrap()
        .input
        .extend(RX.iter().chain(b"\r").copied());
    let event = tokio::time::timeout(Duration::from_secs(2), events.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(event.direction, "rx");
    assert_eq!(event.frame.id().value(), 0x402);
    // Span the LAWICEL periodic status interval: no F command is valid here.
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(runtime.get(&session.id).await.unwrap().fault.is_none());
    let summary = runtime.shutdown().await;
    assert_eq!(summary.unknown_transmissions, 1);
    assert!(summary.failures.is_empty());
    let writes = &state.lock().unwrap().writes;
    assert!(!writes.iter().any(|w| w == b"F\r" || w == b"P\r"));
}
