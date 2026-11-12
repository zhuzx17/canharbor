// SPDX-License-Identifier: AGPL-3.0-or-later
//! Native-only two-phase admission. Service-owned commits survive a lost HTTP waiter.
use super::*;
use canharbor_stress::{CoordRequest, CoordStatus, Request as Start};
use std::sync::atomic::Ordering;
#[derive(Clone)]
pub(super) struct Prepared {
    pub group: String,
    pub token: String,
    pub request: Start,
    pub expires: Instant,
    pub run: Option<String>,
    pub committing: bool,
}
fn conflict(message: &str) -> ApiError {
    ApiError::new(StatusCode::CONFLICT, "coordination_conflict", message)
}
// A closed send gate is irreversible. Poll finalization without renewing its lease.
fn renew_heartbeat(run: &canharbor_stress::Run) -> Result<bool, String> {
    if super::stress::terminal(&run.state()) || run.execution_closed() {
        return Ok(false);
    }
    match run.renew_coordination() {
        Ok(()) => Ok(true),
        Err(_) if run.execution_closed() => Ok(false),
        Err(e) => Err(e),
    }
}
pub(super) async fn action(
    State(s): State<Arc<AppState>>,
    Extension(i): Extension<Identity>,
    WireJson(p): WireJson<CoordRequest>,
) -> Result<Json<CoordStatus>, ApiError> {
    if capture_owner(&i) != "native" {
        return Err(ApiError::forbidden());
    }
    let permit = s
        .stress
        .coord_commands
        .clone()
        .try_acquire_owned()
        .map_err(|_| ApiError::limit())?;
    tokio::spawn(async move {
        let _permit = permit;
        owned(s, i, p).await
    })
    .await
    .map_err(|e| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "coordination_worker",
            e.to_string(),
        )
    })?
}
async fn owned(
    s: Arc<AppState>,
    i: Identity,
    p: CoordRequest,
) -> Result<Json<CoordStatus>, ApiError> {
    if uuid::Uuid::parse_str(&p.group_id).is_err() {
        return Err(conflict("invalid group UUID"));
    }
    s.stress.ensure().await?;
    let _operation = s.stress.coord_operations.lock().await;
    if !matches!(
        p.operation.as_str(),
        "prepare" | "commit" | "heartbeat" | "abort" | "status"
    ) {
        return Err(conflict("unknown operation"));
    }
    if p.operation == "prepare" {
        let request = p
            .request
            .ok_or_else(|| conflict("missing immutable request"))?;
        let coord = request
            .plan
            .coordination
            .as_ref()
            .ok_or_else(|| conflict("missing coordination plan"))?;
        coord.validate().map_err(|e| conflict(&e))?;
        request
            .plan
            .validate(&s.runtime)
            .map_err(|e| conflict(&e))?;
        if coord.group_id != p.group_id
            || !request.active
            || !dto::valid_client_request_id(&request.client_request_id)
            || request.plan_hash != request.plan.fingerprint()
        {
            return Err(conflict("prepare identity/authorization mismatch"));
        }
        let _admission = s.stress.admission.lock().await;
        if s.stress.closing.load(Ordering::Acquire)
            || !s.stress.recovery_issues.lock().unwrap().is_empty()
        {
            return Err(conflict("service closing or evidence quarantined"));
        }
        let entries = s.stress.entries.lock().unwrap();
        if entries.iter().any(|e| {
            e.plan
                .coordination
                .as_ref()
                .is_some_and(|c| c.group_id == p.group_id)
        }) {
            return Err(conflict(
                "group already recorded; read status, never prepare again",
            ));
        }
        if !s.stress.executor.available()
            || entries.iter().any(|e| {
                !super::stress::terminal(&e.run.state()) || e.writing.load(Ordering::Acquire)
            })
        {
            return Err(conflict("stress executor busy"));
        }
        let mut slot = s.stress.coordinated.lock().unwrap();
        if slot.as_ref().is_some_and(|c| {
            c.run.as_ref().map_or_else(
                || Instant::now() < c.expires,
                |id| {
                    entries.iter().any(|e| {
                        e.run.id() == *id
                            && (!super::stress::terminal(&e.run.state())
                                || e.writing.load(Ordering::Acquire))
                    })
                },
            )
        }) {
            return Err(conflict("another coordinator holds admission"));
        }
        *slot = Some(Prepared {
            group: p.group_id.clone(),
            token: uuid::Uuid::new_v4().to_string(),
            expires: Instant::now() + Duration::from_millis(u64::from(coord.lease_ms)),
            request,
            run: None,
            committing: false,
        });
    } else if p.operation != "status" {
        let mut slot = {
            let guard = s.stress.coordinated.lock().unwrap();
            guard
                .as_ref()
                .filter(|c| c.group == p.group_id && p.token.as_deref() == Some(c.token.as_str()))
                .cloned()
                .ok_or_else(|| conflict("unknown group or token"))?
        };
        if p.operation == "abort" {
            if let Some(id) = &slot.run {
                s.stress.find("native", id)?.0.stop();
            }
            slot.expires = Instant::now();
            // Retain the group/token until the next prepare; commits can never revive it.
            slot.committing = false;
            *s.stress.coordinated.lock().unwrap() = Some(slot);
        } else {
            if Instant::now() >= slot.expires {
                if let Some(id) = &slot.run {
                    s.stress.find("native", id)?.0.stop();
                }
                return Err(conflict("group lease expired; no automatic resume"));
            }
            let mut renew_slot = p.operation == "heartbeat" && slot.run.is_none();
            if p.operation == "commit" && slot.run.is_none() {
                slot.committing = true;
                *s.stress.coordinated.lock().unwrap() = Some(slot.clone());
                let (_, report) =
                    super::stress::start_owned(s.clone(), i, slot.request.clone(), true).await?;
                slot.run = Some(report.id.clone());
                slot.committing = false;
            } else if p.operation == "heartbeat"
                && let Some(id) = &slot.run
            {
                let run = s.stress.find("native", id)?.0;
                renew_slot = renew_heartbeat(&run).map_err(|e| conflict(&e))?;
            }
            if renew_slot {
                slot.expires = Instant::now()
                    + Duration::from_millis(u64::from(
                        slot.request.plan.coordination.as_ref().unwrap().lease_ms,
                    ));
            }
            *s.stress.coordinated.lock().unwrap() = Some(slot);
        }
    }
    let slot = s
        .stress
        .coordinated
        .lock()
        .unwrap()
        .as_ref()
        .filter(|c| c.group == p.group_id)
        .cloned();
    let run = if let Some(id) = slot.as_ref().and_then(|c| c.run.as_ref()) {
        Some(s.stress.find("native", id)?.0.snapshot())
    } else {
        s.stress
            .entries
            .lock()
            .unwrap()
            .iter()
            .find(|e| {
                e.plan
                    .coordination
                    .as_ref()
                    .is_some_and(|c| c.group_id == p.group_id)
            })
            .map(|e| e.run.snapshot())
    };
    let state = run.as_ref().map(|r| r.state.clone()).unwrap_or_else(|| {
        if slot.as_ref().is_some_and(|c| Instant::now() < c.expires) {
            "prepared"
        } else {
            "expired_or_absent"
        }
        .into()
    });
    Ok(Json(CoordStatus {
        group_id: p.group_id,
        state,
        token: slot.as_ref().map(|c| c.token.clone()),
        expires_in_ms: slot.as_ref().map_or(0, |c| {
            c.expires
                .saturating_duration_since(Instant::now())
                .as_millis() as u64
        }),
        run_id: run.as_ref().map(|r| r.id.clone()),
        report: run,
        server_instance_id: s.instance.clone(),
        clock_quality: "unsynchronized; no cross-host one-way latency".into(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn coordinator_waiters_are_bounded_and_cancellation_does_not_free_running_slots() {
        let root = std::path::PathBuf::from(if cfg!(windows) {
            std::env::var_os("TEMP").expect("TEMP")
        } else {
            std::ffi::OsString::from("/tmp")
        })
        .join(format!("ch-coord-limit-{}", uuid::Uuid::new_v4()));
        canharbor_platform::private::directory(&root).unwrap();
        let s = AppState::with_capture_dir(
            Runtime::new(Arc::new(canharbor_virtual::VirtualBus::new(8192))),
            "http://127.0.0.1:32180".into(),
            false,
            Some(root.join("captures")),
        );
        let lock = s.stress.coord_operations.lock().await;
        let mut handles = vec![];
        let identity = || Identity {
            revoked: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            id: "native:audit".into(),
            cookie: None,
            csrf: None,
        };
        for _ in 0..8 {
            let state = s.clone();
            let i = identity();
            handles.push(tokio::spawn(async move {
                action(
                    State(state),
                    Extension(i),
                    WireJson(CoordRequest {
                        operation: "status".into(),
                        group_id: uuid::Uuid::new_v4().to_string(),
                        token: None,
                        request: None,
                    }),
                )
                .await
            }));
        }
        tokio::time::timeout(Duration::from_secs(2), async {
            while s.stress.coord_commands.available_permits() > 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        handles.pop().unwrap().abort();
        tokio::task::yield_now().await;
        assert_eq!(s.stress.coord_commands.available_permits(), 0);
        let response = action(
            State(s.clone()),
            Extension(identity()),
            WireJson(CoordRequest {
                operation: "status".into(),
                group_id: uuid::Uuid::new_v4().to_string(),
                token: None,
                request: None,
            }),
        )
        .await;
        assert_eq!(
            response.err().unwrap().into_response().status(),
            StatusCode::TOO_MANY_REQUESTS
        );
        drop(lock);
        for h in handles {
            assert!(h.await.unwrap().is_ok());
        }
        tokio::time::timeout(Duration::from_secs(2), async {
            while s.stress.coord_commands.available_permits() != 8 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
mod heartbeat_tests {
    use super::*;
    #[tokio::test]
    async fn closed_execution_is_read_only_and_never_renews_or_reopens() {
        let plan:canharbor_stress::Plan=serde_json::from_value(serde_json::json!({"sender":"virtual:0","receivers":["virtual:1"],"can_id":291,"extended":false,"run_tag":17,"count":4,"fps":2,"bitrate":500000,"receivers_active":true,"drain_ms":20,"coordination":{"group_id":uuid::Uuid::new_v4().to_string(),"participant":"test","lease_ms":5000,"start_delay_ms":1000,"clock_quality":"unsynchronized"}})).unwrap();
        let runtime = canharbor_core::runtime::Runtime::new(Arc::new(
            canharbor_virtual::VirtualBus::default(),
        ));
        let run = canharbor_stress::Executor::new(runtime)
            .start(plan, true)
            .unwrap();
        run.stop();
        assert!(run.execution_closed());
        let state = run.state();
        assert!(!renew_heartbeat(&run).unwrap());
        assert_eq!(run.state(), state);
        assert!(run.renew_coordination().is_err());
        assert!(run.execution_closed());
    }
}
