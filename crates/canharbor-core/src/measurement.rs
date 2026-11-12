// SPDX-License-Identifier: AGPL-3.0-or-later
//! Optional per-execution host observations. These confer no execution authority.
use std::sync::{Arc, Mutex};
use tokio::time::Instant;
#[derive(Clone, Debug, Default)]
pub struct SubmissionPoints {
    pub preparation_started_ns: u64,
    pub prepared_ns: u64,
    pub dispatched_ns: Option<u64>,
    pub enqueued_ns: Option<u64>,
    pub worker_started_ns: Option<u64>,
    pub call_started_ns: Option<u64>,
    pub call_completed_ns: Option<u64>,
    pub completed_ns: Option<u64>,
    pub scope: String,
}
#[derive(Clone)]
pub struct SubmissionTrace {
    origin: Instant,
    points: Arc<Mutex<SubmissionPoints>>,
}
impl SubmissionTrace {
    pub fn new(origin: Instant, preparation_started: Instant) -> Self {
        let value = Self {
            origin,
            points: Default::default(),
        };
        {
            let mut p = value.points.lock().unwrap();
            p.preparation_started_ns = value.at(preparation_started);
            p.prepared_ns = value.now();
        }
        value
    }
    fn at(&self, at: Instant) -> u64 {
        at.saturating_duration_since(self.origin)
            .as_nanos()
            .min(u128::from(u64::MAX)) as u64
    }
    fn now(&self) -> u64 {
        self.at(Instant::now())
    }
    pub fn snapshot(&self) -> SubmissionPoints {
        self.points.lock().unwrap().clone()
    }
    pub fn dispatched(&self) {
        self.points.lock().unwrap().dispatched_ns = Some(self.now());
    }
    pub fn enqueued(&self) {
        self.points.lock().unwrap().enqueued_ns = Some(self.now());
    }
    pub fn worker_started(&self) {
        self.points.lock().unwrap().worker_started_ns = Some(self.now());
    }
    pub fn call_started(&self, scope: &str) {
        let mut p = self.points.lock().unwrap();
        p.scope = scope.into();
        p.call_started_ns = Some(self.now());
    }
    pub fn call_completed(&self) {
        self.points.lock().unwrap().call_completed_ns = Some(self.now());
    }
    pub fn completed(&self) {
        self.points.lock().unwrap().completed_ns = Some(self.now());
    }
}
#[derive(Clone, Debug)]
pub struct SourceEvidence {
    pub name: String,
    pub sha256: Option<String>,
    pub bytes: Option<u64>,
    pub status: String,
}
