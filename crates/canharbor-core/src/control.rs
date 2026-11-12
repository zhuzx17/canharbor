// SPDX-License-Identifier: AGPL-3.0-or-later
//! Revocable permits checked by a backend immediately before mutating I/O.
use crate::Error;
use std::{
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
/// One-way fence shared by all executions belonging to a single job.
#[derive(Clone, Default)]
pub struct ExecutionGate(Arc<GateState>);
#[derive(Default)]
struct GateState {
    revoked: AtomicBool,
    parent: Option<ExecutionGate>,
}
impl ExecutionGate {
    pub fn child(&self) -> Self {
        Self(Arc::new(GateState {
            revoked: AtomicBool::new(false),
            parent: Some(self.clone()),
        }))
    }
    pub fn revoke(&self) {
        self.0.revoked.store(true, Ordering::Release);
    }
    pub fn is_revoked(&self) -> bool {
        self.0.revoked.load(Ordering::Acquire)
            || self.0.parent.as_ref().is_some_and(Self::is_revoked)
    }
}
#[derive(Clone)]
pub struct Permit(Arc<Inner>);
struct Inner {
    revoked: AtomicBool,
    shutdown: Arc<AtomicBool>,
    until: Mutex<Instant>,
    parent: Option<Permit>,
    suspend: Option<crate::suspend::Sample>,
    gate: OnceLock<ExecutionGate>,
}
impl Permit {
    pub(crate) fn new(ttl: Duration, shutdown: Arc<AtomicBool>) -> Self {
        Self(Arc::new(Inner {
            revoked: AtomicBool::new(false),
            gate: OnceLock::new(),
            shutdown,
            until: Mutex::new(Instant::now() + ttl),
            parent: None,
            suspend: crate::suspend::sample(),
        }))
    }
    // Reconfiguration and stop invalidate old queue entries without granting a
    // new acquisition's clock baseline. Only `new` establishes that baseline.
    pub(crate) fn replacement(&self, ttl: Duration) -> Self {
        Self(Arc::new(Inner {
            revoked: AtomicBool::new(false),
            gate: OnceLock::new(),
            shutdown: self.0.shutdown.clone(),
            until: Mutex::new(Instant::now() + ttl),
            parent: None,
            suspend: self.0.suspend,
        }))
    }
    pub(crate) fn expiring_child(&self, ttl: Duration) -> Self {
        Self(Arc::new(Inner {
            revoked: AtomicBool::new(false),
            gate: OnceLock::new(),
            shutdown: self.0.shutdown.clone(),
            until: Mutex::new(Instant::now() + ttl),
            parent: Some(self.clone()),
            suspend: None,
        }))
    }
    pub(crate) fn same_as(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
    pub(crate) fn bind_gate(&self, gate: &ExecutionGate) -> Result<(), Error> {
        self.0
            .gate
            .set(gate.clone())
            .map_err(|_| Error::InvalidState)
    }
    pub fn check(&self) -> Result<(), Error> {
        if let Some(parent) = &self.0.parent {
            parent.check()?;
        }
        if self.0.parent.is_none()
            && !crate::suspend::valid(self.0.suspend, crate::suspend::sample())
        {
            self.revoke();
        }
        if self.0.gate.get().is_some_and(ExecutionGate::is_revoked)
            || self.0.shutdown.load(Ordering::Acquire)
            || self.0.revoked.load(Ordering::Acquire)
            || Instant::now() >= *self.0.until.lock().unwrap()
        {
            Err(Error::ControlExpired)
        } else {
            Ok(())
        }
    }
    pub(crate) fn restrict_until(&self, deadline: Instant) {
        let mut until = self.0.until.lock().unwrap();
        *until = (*until).min(deadline);
    }
    pub(crate) fn renew(&self, ttl: Duration) {
        *self.0.until.lock().unwrap() = Instant::now() + ttl;
    }
    pub(crate) fn revoke(&self) {
        self.0.revoked.store(true, Ordering::Release);
    }
}
impl Drop for Inner {
    fn drop(&mut self) {
        self.revoked.store(true, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_clock_discontinuity_revokes_children_and_renewal_cannot_restore_them() {
        let mut root = Permit::new(Duration::from_secs(15), Arc::new(AtomicBool::new(false)));
        assert!(root.check().is_ok());
        Arc::get_mut(&mut root.0).unwrap().suspend = crate::suspend::shifted_sample();
        let child = root.expiring_child(Duration::from_secs(1));
        assert!(child.check().is_err());
        root.renew(Duration::from_secs(15));
        assert!(root.check().is_err());
        assert!(child.check().is_err());
        assert!(root.replacement(Duration::from_secs(15)).check().is_err());
        let replacement = Permit::new(Duration::from_secs(15), Arc::new(AtomicBool::new(false)));
        assert!(replacement.check().is_ok());
    }
}

#[cfg(test)]
mod replacement_tests {
    use super::*;
    #[test]
    fn a_normal_stop_can_replace_queue_authorization_without_a_new_acquisition() {
        let root = Permit::new(Duration::from_secs(15), Arc::new(AtomicBool::new(false)));
        root.revoke();
        let next = root.replacement(Duration::from_secs(15));
        assert!(root.check().is_err());
        assert!(next.check().is_ok());
    }
}
