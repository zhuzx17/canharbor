// SPDX-License-Identifier: AGPL-3.0-or-later
use crate::{
    Backend, BackendEvent, BackendFuture, Capabilities, Channel, Config, Error, Frame,
    control::Permit,
};
use std::{collections::BTreeMap, sync::Arc};
use tokio::sync::broadcast;
/// Static composition with unique channel IDs. Each physical backend retains
/// its device actor; this router never shares or duplicates a USB receiver.
pub struct Registry {
    backends: Vec<Arc<dyn Backend>>,
    routes: BTreeMap<String, usize>,
}
impl Registry {
    pub fn new(backends: Vec<Arc<dyn Backend>>) -> Result<Self, Error> {
        let mut routes = BTreeMap::new();
        for (index, backend) in backends.iter().enumerate() {
            for channel in backend.channels() {
                if routes.insert(channel.id, index).is_some() {
                    return Err(Error::InvalidState);
                }
            }
        }
        Ok(Self { backends, routes })
    }
    fn route(&self, id: &str) -> Result<&dyn Backend, Error> {
        self.routes
            .get(id)
            .map(|i| self.backends[*i].as_ref())
            .ok_or(Error::UnknownChannel)
    }
}
impl Backend for Registry {
    fn source_evidence(&self, ch: &str) -> Vec<crate::measurement::SourceEvidence> {
        self.route(ch)
            .map_or_else(|_| vec![], |b| b.source_evidence(ch))
    }
    fn submit_observed<'a>(
        &'a self,
        ch: &'a str,
        f: Frame,
        p: Permit,
        t: crate::measurement::SubmissionTrace,
    ) -> BackendFuture<'a, ()> {
        Box::pin(async move { self.route(ch)?.submit_observed(ch, f, p, t).await })
    }

    fn cleanup_observation(&self, ch: &str) -> Option<crate::CloseSummary> {
        self.route(ch).ok()?.cleanup_observation(ch)
    }
    fn channels(&self) -> Vec<Channel> {
        self.backends.iter().flat_map(|b| b.channels()).collect()
    }
    fn capabilities(&self, ch: &str) -> Result<Capabilities, Error> {
        self.route(ch)?.capabilities(ch)
    }
    fn subscribe(&self, ch: &str) -> Result<broadcast::Receiver<Arc<BackendEvent>>, Error> {
        self.route(ch)?.subscribe(ch)
    }
    fn check_channel(&self, ch: &str) -> Result<u64, Error> {
        self.route(ch)?.check_channel(ch)
    }
    fn transmit(&self, ch: &str, f: Frame) -> Result<(), Error> {
        self.route(ch)?.transmit(ch, f)
    }
    fn configure<'a>(&'a self, ch: &'a str, c: Config) -> BackendFuture<'a, ()> {
        Box::pin(async move { self.route(ch)?.configure(ch, c).await })
    }
    fn start<'a>(&'a self, ch: &'a str, p: Permit) -> BackendFuture<'a, ()> {
        Box::pin(async move { self.route(ch)?.start(ch, p).await })
    }
    fn stop<'a>(&'a self, ch: &'a str) -> BackendFuture<'a, ()> {
        Box::pin(async move { self.route(ch)?.stop(ch).await })
    }
    fn close<'a>(&'a self, ch: &'a str) -> BackendFuture<'a, ()> {
        Box::pin(async move { self.route(ch)?.close(ch).await })
    }
    fn submit<'a>(&'a self, ch: &'a str, f: Frame, p: Permit) -> BackendFuture<'a, ()> {
        Box::pin(async move { self.route(ch)?.submit(ch, f, p).await })
    }
    fn shutdown_report(&self) -> BackendFuture<'_, crate::CloseSummary> {
        Box::pin(async move {
            let mut summary = crate::CloseSummary::default();
            for backend in &self.backends {
                match backend.shutdown_report().await {
                    Ok(report) => {
                        summary.unknown_transmissions = summary
                            .unknown_transmissions
                            .saturating_add(report.unknown_transmissions);
                        summary.failures.extend(report.failures);
                    }
                    Err(error) => summary.failures.push(format!("backend close: {error}")),
                }
            }
            Ok(summary)
        })
    }
    fn shutdown(&self) -> BackendFuture<'_, ()> {
        Box::pin(async move {
            let mut result = Ok(());
            for backend in &self.backends {
                if let Err(e) = backend.shutdown().await {
                    result = Err(e);
                }
            }
            result
        })
    }
}
