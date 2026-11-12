// SPDX-License-Identifier: AGPL-3.0-or-later
use canharbor_core::{CanId, Config, Frame};
use canharbor_slcan::{
    engine::{Engine, Error},
    serial::Transport,
};
use std::{
    collections::VecDeque,
    io,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
#[derive(Default)]
struct State {
    writes: Vec<Vec<u8>>,
    replies: VecDeque<Vec<u8>>,
    failure: Option<usize>,
    quiet: bool,
    reject_listen: bool,
    extra_ack: bool,
}
struct Fake(Arc<Mutex<State>>);
impl Transport for Fake {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let mut state = self.0.lock().unwrap();
        state.writes.push(bytes.to_vec());
        if let Some(n) = state.failure {
            return Ok(n.min(bytes.len()));
        }
        let reply = match bytes {
            b"C\r" if state.extra_ack => b"\r\r".as_slice(),
            b"V\r" => b"V0234\r".as_slice(),
            b"L\r" if state.reject_listen => b"\x07",
            b"t5311BA\r" if state.quiet => b"",
            b"t5311BA\r" => b"t5321CA\rz\r",
            b"P\r" => b"t5331DA\r",
            b"F\r" => b"F09\r",
            _ => b"\r",
        };
        state.replies.push_back(reply.to_vec());
        Ok(bytes.len())
    }
    fn read(&mut self, bytes: &mut [u8], timeout: Duration) -> io::Result<usize> {
        let mut state = self.0.lock().unwrap();
        if let Some(mut reply) = state.replies.pop_front() {
            let n = reply.len().min(bytes.len()).min(2);
            bytes[..n].copy_from_slice(&reply[..n]);
            reply.drain(..n);
            if !reply.is_empty() {
                state.replies.push_front(reply);
            }
            return Ok(n);
        }
        drop(state);
        std::thread::sleep(timeout.min(Duration::from_millis(1)));
        Ok(0)
    }
}
fn config(listen_only: bool) -> Config {
    Config {
        nominal_bitrate: 500_000,
        data_bitrate: None,
        listen_only,
    }
}
fn frame() -> Frame {
    Frame::classic(CanId::new(0x531, false).unwrap(), vec![0xba]).unwrap()
}
fn engine() -> (Engine<Fake>, Arc<Mutex<State>>) {
    let state = Arc::new(Mutex::new(State::default()));
    let mut engine = Engine::new(Fake(state.clone()));
    assert!(engine.start(config(false), || Ok(())).is_err());
    assert_eq!(
        engine.initialize().unwrap(),
        canharbor_slcan::engine::FirmwareVersion::Lawicel {
            hardware: 2,
            software: 0x34
        }
    );
    (engine, state)
}
#[test]
fn handshake_start_tx_rx_status_and_stop_are_serialized() {
    let (mut engine, state) = engine();
    engine.start(config(false), || Ok(())).unwrap();
    engine.transmit(&frame(), || Ok(())).unwrap();
    assert_eq!(engine.unknown_transmissions(), 0);
    assert_eq!(engine.frames().next().unwrap().frame.id().value(), 0x532);
    engine.poll(true, || Ok(())).unwrap();
    assert_eq!(engine.frames().next().unwrap().frame.id().value(), 0x533);
    engine.status(|| Ok(())).unwrap();
    assert_eq!(engine.statuses().collect::<Vec<_>>(), vec![9]);
    engine.stop().unwrap();
    assert_eq!(
        state.lock().unwrap().writes,
        vec![
            b"V\r".to_vec(),
            b"C\r".to_vec(),
            b"S6\r".to_vec(),
            b"O\r".to_vec(),
            b"t5311BA\r".to_vec(),
            b"P\r".to_vec(),
            b"F\r".to_vec(),
            b"C\r".to_vec()
        ]
    );
}
#[test]
fn listen_only_never_falls_back_to_active_and_rejects_tx() {
    let (mut engine, state) = engine();
    state.lock().unwrap().reject_listen = true;
    assert!(engine.start(config(true), || Ok(())).is_err());
    assert!(
        !state
            .lock()
            .unwrap()
            .writes
            .iter()
            .any(|command| command == b"O\r")
    );
    assert!(engine.transmit(&frame(), || Ok(())).is_err());
    state.lock().unwrap().reject_listen = false;
    engine.start(config(true), || Ok(())).unwrap();
    assert!(engine.transmit(&frame(), || Ok(())).is_err());
    let before = state.lock().unwrap().writes.len();
    let mut fd = config(false);
    fd.data_bitrate = Some(2_000_000);
    assert!(engine.start(fd, || Ok(())).is_err());
    assert_eq!(state.lock().unwrap().writes.len(), before);
}
#[test]
fn revoked_control_prevents_each_mutating_io_and_known_non_attempt_is_not_unknown() {
    let (mut engine, state) = engine();
    let checks = AtomicUsize::new(0);
    assert!(
        engine
            .start(config(false), || {
                if checks.fetch_add(1, Ordering::SeqCst) < 2 {
                    Ok(())
                } else {
                    Err(canharbor_core::Error::ControlExpired)
                }
            })
            .is_err()
    );
    assert_eq!(
        state.lock().unwrap().writes,
        vec![b"V\r".to_vec(), b"C\r".to_vec(), b"S6\r".to_vec()]
    );
    engine.start(config(false), || Ok(())).unwrap();
    let before = state.lock().unwrap().writes.len();
    assert!(
        engine
            .transmit(&frame(), || Err(canharbor_core::Error::ControlExpired))
            .is_err()
    );
    assert_eq!(engine.unknown_transmissions(), 0);
    assert_eq!(state.lock().unwrap().writes.len(), before);
}
#[test]
fn short_write_poisoning_never_completes_or_retries_a_partial_tx() {
    let (mut engine, state) = engine();
    engine.start(config(false), || Ok(())).unwrap();
    state.lock().unwrap().failure = Some(5);
    assert!(engine.transmit(&frame(), || Ok(())).is_err());
    assert_eq!(engine.unknown_transmissions(), 1);
    let before = state.lock().unwrap().writes.len();
    state.lock().unwrap().failure = None;
    assert!(matches!(engine.stop(), Err(Error::Poisoned)));
    assert!(engine.transmit(&frame(), || Ok(())).is_err());
    assert_eq!(state.lock().unwrap().writes.len(), before);
    assert_eq!(engine.unknown_transmissions(), 1);
}
#[test]
fn missing_firmware_reply_is_unknown_and_not_retried() {
    let (mut engine, state) = engine();
    engine.start(config(false), || Ok(())).unwrap();
    state.lock().unwrap().quiet = true;
    assert!(matches!(
        engine.transmit(&frame(), || Ok(())),
        Err(Error::Timeout)
    ));
    assert_eq!(engine.unknown_transmissions(), 1);
    assert!(engine.poisoned());
    let before = state.lock().unwrap().writes.len();
    assert!(engine.stop().is_err());
    assert_eq!(state.lock().unwrap().writes.len(), before);
}

#[test]
fn extra_reply_cannot_confirm_a_later_command() {
    let (mut engine, state) = engine();
    state.lock().unwrap().extra_ack = true;
    assert!(engine.start(config(false), || Ok(())).is_err());
    assert_eq!(
        state.lock().unwrap().writes,
        vec![b"V\r".to_vec(), b"C\r".to_vec()]
    );
    assert!(engine.poisoned());
    assert_eq!(engine.unknown_transmissions(), 0);
}
