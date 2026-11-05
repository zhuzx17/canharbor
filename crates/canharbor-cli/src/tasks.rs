// SPDX-License-Identifier: AGPL-3.0-or-later
use crate::{print, status};
use canharbor_client::{LocalClient, api};
use clap::Subcommand;
use serde_json::json;
use std::{error::Error, time::Duration};
use uuid::Uuid;
type Result<T> = std::result::Result<T, Box<dyn Error>>;
#[derive(Subcommand)]
pub enum Command {
    /// Create a foreground task; maintain its existing lease until it finishes.
    Create {
        #[arg(long)]
        channel: String,
        #[arg(long)]
        active: bool,
        #[arg(long)]
        id: String,
        #[arg(long, default_value = "")]
        data: String,
        #[arg(long)]
        extended: bool,
        #[arg(long)]
        fd: bool,
        #[arg(long, requires = "fd")]
        brs: bool,
        #[arg(long,conflicts_with_all=["fd","data"])]
        rtr_dlc: Option<u8>,
        #[arg(long, default_value_t = 500000)]
        bitrate: u32,
        #[arg(long, requires = "fd")]
        data_bitrate: Option<u32>,
        #[arg(long,default_value_t=100,value_parser=clap::value_parser!(u32).range(1..=6000))]
        count: u32,
        #[arg(long,default_value_t=10,value_parser=clap::value_parser!(u32).range(10..=60000))]
        interval_ms: u32,
        #[arg(long)]
        request_id: Option<String>,
    },
    List,
    Show {
        id: Uuid,
    },
    Cancel {
        id: Uuid,
    },
    /// Query retained results; never starts a task or renews control.
    Recover {
        #[arg(long)]
        session: String,
        #[arg(long)]
        request: String,
        #[arg(long)]
        generation: Option<String>,
    },
}
fn terminal(t: &api::PeriodicTask) -> bool {
    !matches!(
        t.state,
        api::TaskState::Starting | api::TaskState::Running | api::TaskState::Cancelling
    )
}
pub async fn run(client: &LocalClient, command: Command) -> Result<()> {
    match command {
        Command::List => print(serde_json::to_value(client.periodic_tasks().await?)?),
        Command::Show { id } => print(serde_json::to_value(client.periodic_task(id).await?)?),
        Command::Cancel { id } => {
            let task = match client.cancel_periodic(id).await {
                Ok(task) => task,
                Err(error)
                    if error
                        .downcast_ref::<canharbor_client::ApiFailure>()
                        .is_some_and(|api| !api.submission_is_unknown()) =>
                {
                    return Err(error);
                }
                Err(error) => {
                    return Err(status::unknown(format!(
                        "task {id} cancellation outcome unknown: {error}; inspect with canharbor task show {id}; do not repeat cancellation automatically"
                    )));
                }
            };
            print(serde_json::to_value(task)?);
        }
        Command::Recover {
            session,
            request,
            generation,
        } => {
            match client.recover_periodic(&session,&request,generation.as_deref()).await? {
                Some(task)=>print(serde_json::to_value(task)?),
                None=>return Err(Box::new(status::Failure::Reported{code:4,message:"no retained task matches; expiry or restart can remove results; do not infer nonexecution or resend automatically".into()})),
            }
        }
        Command::Create {
            channel,
            active,
            id,
            data,
            extended,
            fd,
            brs,
            rtr_dlc,
            bitrate,
            data_bitrate,
            count,
            interval_ms,
            request_id,
        } => {
            if !active {
                return Err(status::input("task create requires explicit --active"));
            }
            let request_id = request_id.unwrap_or_else(|| Uuid::new_v4().to_string());
            if !api::valid_client_request_id(&request_id)
                || u64::from(count - 1) * u64::from(interval_ms) > 60000
            {
                return Err(status::input("task plan exceeds its bounds"));
            }
            if id.is_empty() || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(status::input("--id must be hexadecimal without a prefix"));
            }
            let frame = api::FrameDto {
                kind: if fd {
                    api::FrameKind::FdData
                } else if rtr_dlc.is_some() {
                    api::FrameKind::ClassicRemote
                } else {
                    api::FrameKind::ClassicData
                },
                id: u32::from_str_radix(&id, 16)
                    .map_err(|_| status::input("invalid CAN identifier"))?,
                extended,
                brs,
                data_hex: data,
                dlc: rtr_dlc,
            };
            frame
                .clone()
                .into_frame()
                .map_err(|e| status::input(format!("invalid frame: {e}")))?;
            let session = client
                .create(&api::CreateRequest {
                    channel_id: channel,
                    config: api::Config {
                        mode: api::SessionMode::Active,
                        nominal_bitrate: bitrate,
                        fd: fd.then_some(api::FdConfig {
                            enabled: true,
                            data_bitrate: data_bitrate.unwrap_or(2000000),
                            iso: true,
                        }),
                    },
                })
                .await?;
            // Anchor before the acquire request: its response may itself be slow.
            let acquired_at = tokio::time::Instant::now();
            let lease = client.acquire(&session.id).await?;
            let g = lease.generation.as_str();
            let plan = api::PeriodicRequest {
                client_request_id: request_id.clone(),
                frame,
                count,
                interval_ms,
            };
            let signal = tokio::signal::ctrl_c();
            tokio::pin!(signal);
            let task_id = std::cell::Cell::<Option<Uuid>>::new(None);
            let work = async {
                client.running(&session.id, g, true).await?;
                let task = client.start_periodic(&session.id, g, &plan).await?;
                let id = task.id.parse()?;
                task_id.set(Some(id));
                eprintln!(
                    "task {}: session {}, generation {g}, request {request_id}; foreground control renews every 5 seconds",
                    task.id, session.id
                );
                let observe = async {
                    loop {
                        let task = client.periodic_task(id).await?;
                        if terminal(&task) {
                            return Ok::<_, Box<dyn Error>>(task);
                        }
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                };
                match tokio::time::timeout(Duration::from_secs(65), observe).await {
                    Ok(Ok(task)) => finish(task),
                    Ok(Err(error)) => Err(status::unknown(format!(
                        "task {id} observation failed: {error}; use task show; do not retry"
                    ))),
                    Err(_) => {
                        // Cleanup below fences the session even if this cancel
                        // response cannot be established.
                        let _ = client.cancel_periodic(id).await;
                        Err(status::unknown(format!(
                            "task {id} observation deadline exceeded; query its result; do not retry"
                        )))
                    }
                }
            };
            let renew = async {
                let mut timer = tokio::time::interval_at(
                    acquired_at + Duration::from_secs(5),
                    Duration::from_secs(5),
                );
                timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                loop {
                    timer.tick().await;
                    if let Err(error) = client.renew(&session.id, g).await {
                        break error;
                    }
                }
            };
            let result = tokio::select! {
                biased;
                _=&mut signal=>match task_id.get() {
                    Some(id)=>cancel_and_wait(client,id).await,
                    None=>Err(status::unknown(format!("interrupted during task setup/admission; session {}, generation {g}, request {request_id}; use task recover; do not resend automatically",session.id))),
                },
                error=renew=>Err(status::unknown(format!("task control renewal failed: {error}; session {}, generation {g}, request {request_id}; use task recover; do not retry",session.id))),
                result=work=>result,
            };
            let cleanup = client
                .request(
                    "DELETE",
                    &format!("/sessions/{}", session.id),
                    json!({}),
                    Some(g),
                )
                .await;
            if let Err(e) = &cleanup {
                eprintln!("session {} cleanup failed: {e}", session.id);
            }
            result?;
            cleanup?;
        }
    }
    Ok(())
}

fn finish(task: api::PeriodicTask) -> Result<()> {
    print(serde_json::to_value(&task)?);
    match task.state {
        api::TaskState::Completed => Ok(()),
        api::TaskState::OutcomeUnknown => Err(status::unknown(format!(
            "task {} transmission outcome unknown; do not retry",
            task.id
        ))),
        state => Err(Box::new(status::Failure::Reported {
            code: 6,
            message: format!(
                "task {} ended with {:?}: {}",
                task.id,
                state,
                task.reason.as_deref().unwrap_or("no reason supplied")
            ),
        })),
    }
}
async fn cancel_and_wait(client: &LocalClient, id: Uuid) -> Result<()> {
    let result = async {
        client.cancel_periodic(id).await?;
        loop {
            let task = client.periodic_task(id).await?;
            if terminal(&task) {
                return Ok::<_, Box<dyn Error>>(task);
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    };
    match tokio::time::timeout(Duration::from_secs(3), result).await {
        Ok(Ok(task)) => finish(task),
        Ok(Err(error)) => Err(status::unknown(format!(
            "task {id} cancellation outcome unknown: {error}; query its result; do not retry"
        ))),
        Err(_) => Err(status::unknown(format!(
            "task {id} cancellation outcome unknown; query its result; do not retry"
        ))),
    }
}
