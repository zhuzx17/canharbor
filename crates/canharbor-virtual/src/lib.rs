// SPDX-License-Identifier: AGPL-3.0-or-later
//! Two simulated channels on one virtual bus. No synthetic background traffic.
use canharbor_core::{Backend, BackendEvent, BackendFault, BusFrame, Channel, Error, Frame};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use tokio::sync::broadcast;

/// Explicit, deterministic controls for simulation/test harnesses. Not exposed over HTTP.
#[derive(Clone, Copy, Debug)]
pub enum Fault {
    Disconnect,
    BusOff,
    ReceiveOverflow,
    FailNextTransmit,
}
#[derive(Default)]
struct FaultState {
    active: Option<BackendFault>,
    generation: u64,
    fail_next_tx: bool,
}
pub struct VirtualBus {
    faults: Mutex<FaultState>,
    channels: BTreeMap<String, broadcast::Sender<Arc<BackendEvent>>>,
}
impl Default for VirtualBus {
    fn default() -> Self {
        Self::new(256)
    }
}
impl VirtualBus {
    pub fn new(capacity: usize) -> Self {
        Self::with_channel_count(capacity, 2)
    }
    /// Explicit simulation topology for multi-agent and protocol tests.
    pub fn with_channel_count(capacity: usize, count: u8) -> Self {
        assert!(capacity > 0 && (2..=16).contains(&count));
        Self {
            faults: Mutex::new(FaultState::default()),
            channels: (0..count)
                .map(|n| (format!("virtual:{n}"), broadcast::channel(capacity).0))
                .collect(),
        }
    }
    pub fn inject(&self, fault: Fault) {
        let mut state = self.faults.lock().unwrap();
        let fault = match fault {
            Fault::FailNextTransmit => {
                state.fail_next_tx = true;
                return;
            }
            Fault::Disconnect => BackendFault::Disconnected,
            Fault::BusOff => BackendFault::BusOff,
            Fault::ReceiveOverflow => BackendFault::ReceiveOverflow,
        };
        state.active = Some(fault);
        state.generation += 1;
        for tx in self.channels.values() {
            let _ = tx.send(Arc::new(BackendEvent::Fault {
                fault,
                generation: state.generation,
            }));
        }
    }
    /// Restore backend health only. Sessions and leases are not restarted.
    pub fn clear_faults(&self) {
        let mut state = self.faults.lock().unwrap();
        state.active = None;
        state.fail_next_tx = false;
    }
}
impl Backend for VirtualBus {
    fn cleanup_observation(&self, channel: &str) -> Option<canharbor_core::CloseSummary> {
        self.channels.contains_key(channel).then(Default::default)
    }
    fn capabilities(&self, channel: &str) -> Result<canharbor_core::Capabilities, Error> {
        if !self.channels.contains_key(channel) {
            return Err(Error::UnknownChannel);
        }
        Ok(canharbor_core::Capabilities {
            device_id: "virtual".into(),
            backend: "virtual".into(),
            simulated: true,
            fd: true,
            listen_only: true,
            timestamps: false,
            nominal_timing: None,
            data_timing: None,
            fixed_nominal_bitrates: vec![500_000],
            fixed_data_bitrates: vec![2_000_000],
        })
    }
    fn channels(&self) -> Vec<Channel> {
        self.channels
            .keys()
            .map(|id| Channel {
                id: id.clone(),
                label: format!("Virtual CAN {}", id.rsplit(':').next().unwrap()),
            })
            .collect()
    }
    fn subscribe(&self, channel: &str) -> Result<broadcast::Receiver<Arc<BackendEvent>>, Error> {
        self.check_channel(channel)?;
        self.channels
            .get(channel)
            .map(|tx| tx.subscribe())
            .ok_or(Error::UnknownChannel)
    }
    fn check_channel(&self, channel: &str) -> Result<u64, Error> {
        if !self.channels.contains_key(channel) {
            return Err(Error::UnknownChannel);
        }
        let state = self.faults.lock().unwrap();
        match state.active {
            Some(f) => Err(Error::Fault(f)),
            None => Ok(state.generation),
        }
    }
    fn configure<'a>(
        &'a self,
        channel: &'a str,
        config: canharbor_core::Config,
    ) -> canharbor_core::BackendFuture<'a, ()> {
        Box::pin(async move {
            self.check_channel(channel)?;
            if config.nominal_bitrate != 500_000
                || config.data_bitrate.is_some_and(|rate| rate != 2_000_000)
            {
                return Err(Error::Unsupported);
            }
            Ok(())
        })
    }
    fn start<'a>(
        &'a self,
        channel: &'a str,
        permit: canharbor_core::control::Permit,
    ) -> canharbor_core::BackendFuture<'a, ()> {
        Box::pin(async move {
            permit.check()?;
            self.check_channel(channel).map(|_| ())
        })
    }
    fn stop<'a>(&'a self, channel: &'a str) -> canharbor_core::BackendFuture<'a, ()> {
        Box::pin(async move {
            if self.channels.contains_key(channel) {
                Ok(())
            } else {
                Err(Error::UnknownChannel)
            }
        })
    }
    fn close<'a>(&'a self, channel: &'a str) -> canharbor_core::BackendFuture<'a, ()> {
        self.stop(channel)
    }
    fn transmit(&self, channel: &str, frame: Frame) -> Result<(), Error> {
        if !self.channels.contains_key(channel) {
            return Err(Error::UnknownChannel);
        }
        frame.validate()?;
        let mut faults = self.faults.lock().unwrap();
        if let Some(fault) = faults.active {
            return Err(Error::Fault(fault));
        }
        if std::mem::take(&mut faults.fail_next_tx) {
            faults.generation += 1;
            return Err(Error::Fault(BackendFault::TransmitFailed));
        }
        let event = Arc::new(BackendEvent::Frame(BusFrame {
            received_at: tokio::time::Instant::now(),
            frame,
            source_channel: channel.into(),
            generation: faults.generation,
            metadata: canharbor_core::FrameMetadata::default(),
        }));
        for tx in self.channels.values() {
            let _ = tx.send(event.clone());
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use canharbor_core::CanId;
    #[tokio::test]
    async fn cross_channel_delivery_and_slow_receiver_are_bounded() {
        let bus = VirtualBus::new(2);
        let mut slow = bus.subscribe("virtual:1").unwrap();
        let frame = Frame::classic(CanId::new(1, false).unwrap(), vec![42]).unwrap();
        for _ in 0..4 {
            bus.transmit("virtual:0", frame.clone()).unwrap();
        }
        assert!(matches!(
            slow.recv().await,
            Err(broadcast::error::RecvError::Lagged(2))
        ));
        let event = slow.recv().await.unwrap();
        let BackendEvent::Frame(event) = event.as_ref() else {
            panic!("expected frame")
        };
        assert_eq!(event.source_channel, "virtual:0");
        assert_eq!(event.frame, frame);
    }
    #[test]
    fn invalid_enum_cannot_bypass_validation() {
        let bus = VirtualBus::default();
        assert!(
            bus.transmit(
                "virtual:0",
                Frame::ClassicData {
                    id: CanId::new(0, false).unwrap(),
                    data: vec![0; 9]
                }
            )
            .is_err()
        );
    }
}

#[cfg(test)]
mod runtime_tests {
    use super::*;
    use canharbor_core::{
        CanId,
        runtime::{Mode, Runtime, RuntimeError},
    };
    use std::time::Duration;
    fn frame() -> Frame {
        Frame::classic(CanId::new(0x123, false).unwrap(), vec![1, 2, 3]).unwrap()
    }
    #[tokio::test(start_paused = true)]
    async fn lease_expiry_fences_old_controller_and_stops_channel() {
        let r = Runtime::new(Arc::new(VirtualBus::default()));
        let s = r.create("virtual:0", Mode::Active).await.unwrap();
        let a = r.acquire(&s.id, "a").await.unwrap();
        assert!(matches!(
            r.acquire(&s.id, "b").await,
            Err(RuntimeError::LeaseConflict)
        ));
        r.running(&s.id, "a", a.generation, true).await.unwrap();
        tokio::time::advance(Duration::from_secs(16)).await;
        assert!(matches!(
            r.send(&s.id, "a", a.generation, frame()).await,
            Err(RuntimeError::LeaseExpired)
        ));
        assert!(!r.get(&s.id).await.unwrap().running);
        let b = r.acquire(&s.id, "b").await.unwrap();
        assert!(b.generation > a.generation);
        assert!(r.running(&s.id, "a", a.generation, true).await.is_err());
        r.running(&s.id, "b", b.generation, true).await.unwrap();
        r.send(&s.id, "b", b.generation, frame()).await.unwrap();
    }
    #[tokio::test]
    async fn monitor_receives_from_other_channel_and_cannot_transmit() {
        let r = Runtime::new(Arc::new(VirtualBus::default()));
        let monitor = r.create("virtual:1", Mode::ListenOnly).await.unwrap();
        let tx = r.create("virtual:0", Mode::Active).await.unwrap();
        let m = r.acquire(&monitor.id, "monitor").await.unwrap();
        let t = r.acquire(&tx.id, "sender").await.unwrap();
        r.running(&monitor.id, "monitor", m.generation, true)
            .await
            .unwrap();
        r.running(&tx.id, "sender", t.generation, true)
            .await
            .unwrap();
        let mut input = r.subscribe(&monitor.id).await.unwrap();
        assert!(matches!(
            r.send(&monitor.id, "monitor", m.generation, frame()).await,
            Err(RuntimeError::ListenOnly)
        ));
        r.send(&tx.id, "sender", t.generation, frame())
            .await
            .unwrap();
        let event = tokio::time::timeout(Duration::from_secs(1), input.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(event.frame, frame());
        assert_eq!(event.direction, "rx");
        assert_eq!(event.sequence, 1);
        r.revoke("sender").await;
        assert!(!r.get(&tx.id).await.unwrap().running);
        r.shutdown().await;
        assert!(r.list().await.is_empty());
    }
    #[tokio::test]
    async fn channel_reservation_is_exclusive() {
        let r = Runtime::new(Arc::new(VirtualBus::default()));
        r.create("virtual:0", Mode::Active).await.unwrap();
        assert!(matches!(
            r.create("virtual:0", Mode::ListenOnly).await,
            Err(RuntimeError::Busy)
        ));
        assert!(r.create("missing", Mode::Active).await.is_err());
    }
}

#[cfg(test)]
mod fault_tests {
    use super::*;
    use canharbor_core::{
        CanId,
        runtime::{Mode, Runtime, RuntimeError},
    };
    use std::time::Duration;
    fn frame() -> Frame {
        Frame::classic(CanId::new(0x123, false).unwrap(), vec![1, 2, 3]).unwrap()
    }
    async fn start(
        r: &Arc<Runtime>,
        channel: &str,
        owner: &str,
    ) -> canharbor_core::runtime::SessionView {
        let s = r.create(channel, Mode::Active).await.unwrap();
        let s = r.acquire(&s.id, owner).await.unwrap();
        r.running(&s.id, owner, s.generation, true).await.unwrap()
    }
    async fn closed(rx: &mut broadcast::Receiver<canharbor_core::runtime::Event>) {
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(1), rx.recv())
                .await
                .unwrap(),
            Err(broadcast::error::RecvError::Closed)
        ));
    }
    #[tokio::test]
    async fn persistent_faults_stop_all_channels_and_require_explicit_recovery() {
        for (fault, expected) in [
            (Fault::Disconnect, BackendFault::Disconnected),
            (Fault::BusOff, BackendFault::BusOff),
            (Fault::ReceiveOverflow, BackendFault::ReceiveOverflow),
        ] {
            let bus = Arc::new(VirtualBus::default());
            let r = Runtime::new(bus.clone());
            let a = start(&r, "virtual:0", "a").await;
            let b = start(&r, "virtual:1", "b").await;
            let mut old_a = r.subscribe(&a.id).await.unwrap();
            let mut old_b = r.subscribe(&b.id).await.unwrap();
            bus.inject(fault);
            closed(&mut old_a).await;
            closed(&mut old_b).await;
            for s in [&a, &b] {
                let view = r.get(&s.id).await.unwrap();
                assert_eq!(view.fault, Some(expected));
                assert!(!view.running);
                assert!(view.controller.is_none());
            }
            assert!(matches!(
                r.send(&a.id, "a", a.generation, frame()).await,
                Err(RuntimeError::LeaseExpired)
            ));
            assert!(r.create("missing", Mode::Active).await.is_err());
            let failed = r.acquire(&a.id, "new").await.unwrap();
            assert!(
                matches!(r.running(&a.id,"new",failed.generation,true).await,Err(RuntimeError::Backend(Error::Fault(f))) if f==expected)
            );
            bus.clear_faults();
            assert!(!r.get(&a.id).await.unwrap().running);
            let renewed = r.acquire(&a.id, "new").await.unwrap();
            assert!(renewed.generation > a.generation);
            r.running(&a.id, "new", renewed.generation, true)
                .await
                .unwrap();
            assert!(matches!(
                old_a.try_recv(),
                Err(broadcast::error::TryRecvError::Closed)
            ));
            let mut fresh = r.subscribe(&a.id).await.unwrap();
            r.send(&a.id, "new", renewed.generation, frame())
                .await
                .unwrap();
            let event = tokio::time::timeout(Duration::from_secs(1), fresh.recv())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(event.frame, frame());
            // Backend health alone did not restart the second channel or renew its controller.
            assert!(!r.get(&b.id).await.unwrap().running);
            assert!(r.running(&a.id, "a", a.generation, true).await.is_err());
        }
    }
    #[tokio::test]
    async fn one_shot_send_failure_delivers_no_frame_and_revokes_only_the_sender() {
        let bus = Arc::new(VirtualBus::default());
        let r = Runtime::new(bus.clone());
        let a = start(&r, "virtual:0", "a").await;
        let b = start(&r, "virtual:1", "b").await;
        let mut input = r.subscribe(&b.id).await.unwrap();
        let mut sender = r.subscribe(&a.id).await.unwrap();
        bus.inject(Fault::FailNextTransmit);
        assert!(matches!(
            r.send(&a.id, "a", a.generation, frame()).await,
            Err(RuntimeError::Backend(Error::Fault(
                BackendFault::TransmitFailed
            )))
        ));
        closed(&mut sender).await;
        assert!(matches!(
            input.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
        assert!(r.get(&b.id).await.unwrap().running);
        assert_eq!(
            r.get(&a.id).await.unwrap().fault,
            Some(BackendFault::TransmitFailed)
        );
        let new = r.acquire(&a.id, "a").await.unwrap();
        r.running(&a.id, "a", new.generation, true).await.unwrap();
        r.send(&a.id, "a", new.generation, frame()).await.unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), input.recv())
                .await
                .unwrap()
                .unwrap()
                .frame,
            frame()
        );
    }
    #[tokio::test]
    async fn actual_backend_buffer_overflow_is_visible_and_closes_old_stream() {
        let bus = Arc::new(VirtualBus::new(2));
        let r = Runtime::new(bus.clone());
        let a = start(&r, "virtual:0", "a").await;
        let mut rx = r.subscribe(&a.id).await.unwrap();
        // No yield: overflow the backend receiver before its forwarding task can run.
        for _ in 0..4 {
            bus.transmit("virtual:0", frame()).unwrap();
        }
        closed(&mut rx).await;
        assert_eq!(
            r.get(&a.id).await.unwrap().fault,
            Some(BackendFault::ReceiveOverflow)
        );
        assert!(!r.get(&a.id).await.unwrap().running);
    }
    #[test]
    fn malformed_frames_do_not_consume_a_pending_transmit_failure() {
        let bus = VirtualBus::default();
        bus.inject(Fault::FailNextTransmit);
        let bad = Frame::ClassicData {
            id: CanId::new(0, false).unwrap(),
            data: vec![0; 9],
        };
        assert_eq!(bus.transmit("virtual:0", bad), Err(Error::InvalidFrame));
        assert_eq!(
            bus.transmit("virtual:0", frame()),
            Err(Error::Fault(BackendFault::TransmitFailed))
        );
        assert!(bus.transmit("virtual:0", frame()).is_ok());
    }
}

#[cfg(test)]
mod recovery_order_tests {
    use super::*;
    use canharbor_core::{
        CanId,
        runtime::{Mode, Runtime},
    };
    use std::time::Duration;
    #[tokio::test]
    async fn start_and_restart_discard_frames_queued_before_acquisition() {
        let bus = Arc::new(VirtualBus::default());
        let r = Runtime::new(bus.clone());
        let s = r.create("virtual:0", Mode::ListenOnly).await.unwrap();
        let lease = r.acquire(&s.id, "owner").await.unwrap();
        let mut rx = r.subscribe(&s.id).await.unwrap();
        let id = CanId::new(1, false).unwrap();
        for _ in 0..2 {
            // On this current-thread runtime, the forwarding task cannot run
            // until recv yields. Both pre-start frames are still queued then.
            bus.transmit("virtual:1", Frame::classic(id, vec![0xAA]).unwrap())
                .unwrap();
            r.running(&s.id, "owner", lease.generation, true)
                .await
                .unwrap();
            let fresh = Frame::classic(id, vec![0xBB]).unwrap();
            bus.transmit("virtual:1", fresh.clone()).unwrap();
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(1), rx.recv())
                    .await
                    .unwrap()
                    .unwrap()
                    .frame,
                fresh
            );
            assert!(matches!(
                rx.try_recv(),
                Err(broadcast::error::TryRecvError::Empty)
            ));
            r.running(&s.id, "owner", lease.generation, false)
                .await
                .unwrap();
        }
    }
    #[tokio::test]
    async fn explicit_recovery_fences_queued_faults_and_pre_fault_frames() {
        let bus = Arc::new(VirtualBus::default());
        let r = Runtime::new(bus.clone());
        let s = r.create("virtual:0", Mode::Active).await.unwrap();
        let old = r.acquire(&s.id, "old").await.unwrap();
        r.running(&s.id, "old", old.generation, true).await.unwrap();
        let id = CanId::new(1, false).unwrap();
        bus.transmit("virtual:0", Frame::classic(id, vec![0xAA]).unwrap())
            .unwrap();
        bus.inject(Fault::Disconnect);
        // The synchronous TX failure is observed before the forwarding task
        // has dequeued the earlier frame and fault notification.
        assert!(
            r.send(
                &s.id,
                "old",
                old.generation,
                Frame::classic(id, vec![]).unwrap()
            )
            .await
            .is_err()
        );
        bus.clear_faults();
        let new = r.acquire(&s.id, "new").await.unwrap();
        r.running(&s.id, "new", new.generation, true).await.unwrap();
        let mut rx = r.subscribe(&s.id).await.unwrap();
        let fresh = Frame::classic(id, vec![0xBB]).unwrap();
        r.send(&s.id, "new", new.generation, fresh.clone())
            .await
            .unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), rx.recv())
                .await
                .unwrap()
                .unwrap()
                .frame,
            fresh
        );
        assert!(matches!(
            rx.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
        assert!(r.get(&s.id).await.unwrap().running);
    }
}
