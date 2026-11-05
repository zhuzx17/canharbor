// SPDX-License-Identifier: AGPL-3.0-or-later
use crate::{ApiFailure, InvalidArgument, LocalClient, api, session_id};
use serde_json::{Value, json};
use std::{error::Error, fmt};
use uuid::Uuid;
type Result<T> = std::result::Result<T, Box<dyn Error>>;
/// A start response did not establish whether admission occurred. Recover with
/// read-only lookup; a fresh request ID could duplicate physical transmission.
#[derive(Debug)]
pub struct TaskStartUnknown {
    pub session: String,
    pub generation: String,
    pub request_id: String,
    source: Box<dyn Error>,
}
impl fmt::Display for TaskStartUnknown {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "task start outcome unknown; session {}, generation {}, request {}: {}; use read-only task lookup; do not resend automatically",
            self.session, self.generation, self.request_id, self.source
        )
    }
}
impl Error for TaskStartUnknown {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(self.source.as_ref())
    }
}
#[derive(Debug)]
pub struct AmbiguousTask;
impl fmt::Display for AmbiguousTask {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("multiple tasks match; specify the original lease generation")
    }
}
impl Error for AmbiguousTask {}
fn generation(value: &str) -> Result<()> {
    if !value.parse::<u64>().is_ok_and(|v| v.to_string() == value) {
        return Err(Box::new(InvalidArgument(
            "generation must be a canonical u64 string",
        )));
    }
    Ok(())
}
impl LocalClient {
    pub async fn renew(&self, session: &str, g: &str) -> Result<api::Session> {
        session_id(session)?;
        generation(g)?;
        self.request_typed(
            "POST",
            &format!("/sessions/{session}/lease/renew"),
            json!({}),
            Some(g),
        )
        .await
    }
    pub async fn release(&self, session: &str, g: &str) -> Result<()> {
        session_id(session)?;
        generation(g)?;
        self.request(
            "DELETE",
            &format!("/sessions/{session}/lease"),
            json!({}),
            Some(g),
        )
        .await?;
        Ok(())
    }
    pub async fn start_periodic(
        &self,
        session: &str,
        g: &str,
        plan: &api::PeriodicRequest,
    ) -> Result<api::PeriodicTask> {
        session_id(session)?;
        generation(g)?;
        if !api::valid_client_request_id(&plan.client_request_id)
            || !(1..=6000).contains(&plan.count)
            || !(10..=60000).contains(&plan.interval_ms)
            || u64::from(plan.count - 1) * u64::from(plan.interval_ms) > 60000
            || plan.frame.clone().into_frame().is_err()
        {
            return Err(Box::new(InvalidArgument("invalid periodic plan")));
        }
        match self
            .request_typed(
                "POST",
                &format!("/sessions/{session}/tasks"),
                serde_json::to_value(plan)?,
                Some(g),
            )
            .await
        {
            Ok(task) => Ok(task),
            Err(error)
                if error
                    .downcast_ref::<ApiFailure>()
                    .is_some_and(|api| !api.submission_is_unknown()) =>
            {
                Err(error)
            }
            Err(error) => Err(Box::new(TaskStartUnknown {
                session: session.into(),
                generation: g.into(),
                request_id: plan.client_request_id.clone(),
                source: error,
            })),
        }
    }
    pub async fn periodic_tasks(&self) -> Result<api::PeriodicTasks> {
        self.request_typed("GET", "/tasks", Value::Null, None).await
    }
    pub async fn periodic_task(&self, id: Uuid) -> Result<api::PeriodicTask> {
        self.request_typed("GET", &format!("/tasks/{id}"), Value::Null, None)
            .await
    }
    pub async fn cancel_periodic(&self, id: Uuid) -> Result<api::PeriodicTask> {
        self.request_typed("POST", &format!("/tasks/{id}/cancel"), json!({}), None)
            .await
    }
    /// No match is not proof of nonexecution: results expire and service restarts
    /// clear the registry. This method never starts, renews or retries a task.
    pub async fn recover_periodic(
        &self,
        session: &str,
        request: &str,
        g: Option<&str>,
    ) -> Result<Option<api::PeriodicTask>> {
        session_id(session)?;
        if !api::valid_client_request_id(request) {
            return Err(Box::new(InvalidArgument("invalid request ID")));
        }
        if let Some(g) = g {
            generation(g)?;
        }
        let mut tasks = self.periodic_tasks().await?.tasks.into_iter().filter(|v| {
            v.session_id == session
                && v.client_request_id == request
                && g.is_none_or(|g| v.generation.as_str() == g)
        });
        let first = tasks.next();
        if tasks.next().is_some() {
            return Err(Box::new(AmbiguousTask));
        }
        Ok(first)
    }
}
