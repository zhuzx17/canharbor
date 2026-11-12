// SPDX-License-Identifier: AGPL-3.0-or-later
//! Bounded foreground communication tests. Uses the ordinary authenticated API.
use crate::status;
use canharbor_client::{LocalClient, api};
use clap::{Args, Subcommand};
use serde_json::{Value, json};
use std::{error::Error, time::Duration};
use tokio::time::{Instant, MissedTickBehavior};
use uuid::Uuid;
type Result<T> = std::result::Result<T, Box<dyn Error>>;

#[derive(Subcommand)]
pub enum Command {
    /// Submit a bounded sequence; success does not establish peer reception.
    Send {
        #[command(flatten)]
        options: Options,
        #[arg(long)]
        #[arg(help = "Explicitly authorize active CAN operation; sending requires this flag")]
        active: bool,
        #[arg(long, default_value_t=10, value_parser=clap::value_parser!(u32).range(10..=60000))]
        #[arg(help = "Interval between planned frames in milliseconds")]
        interval_ms: u32,
    },
    /// Receive matching RX frames; defaults to listen-only.
    Receive {
        /// Enable controller participation (including ACK); never sends test data.
        #[arg(long)]
        #[arg(help = "Enable receiver participation in CAN ACK; does not send test data")]
        active: bool,
        #[command(flatten)]
        options: Options,
        #[arg(long, default_value_t=5000, value_parser=clap::value_parser!(u32).range(1..=60000))]
        timeout_ms: u32,
    },
}
#[derive(Args)]
pub struct Options {
    #[arg(long)]
    #[arg(help = "Channel ID from canharbor devices, for example virtual:0")]
    channel: String,
    /// CAN identifier as hexadecimal without 0x.
    #[arg(long, value_parser=parse_hex)]
    id: u32,
    #[arg(long)]
    #[arg(help = "Use a 29-bit identifier instead of the default 11-bit identifier")]
    extended: bool,
    /// Caller-selected hexadecimal test tag. Use the same value at both ends.
    #[arg(long, value_parser=parse_hex)]
    run_id: u32,
    #[arg(long, default_value_t=100, value_parser=clap::value_parser!(u32).range(1..=6000))]
    #[arg(help = "Maximum number of frames; see the command description for bounds")]
    count: u32,
    #[arg(long, default_value_t = 0)]
    first: u32,
    #[arg(long, default_value_t = 500000)]
    #[arg(help = "Nominal CAN bitrate in bit/s, for example 500000")]
    bitrate: u32,
    #[arg(long)]
    #[arg(help = "Enable CAN FD in this session")]
    fd: bool,
    #[arg(long, requires = "fd", default_value_t = 2000000)]
    #[arg(help = "CAN FD data bitrate in bit/s; requires --fd (default 2000000)")]
    data_bitrate: u32,
    #[arg(long, requires = "fd")]
    #[arg(help = "Use CAN FD bit-rate switching; requires --fd")]
    brs: bool,
}
fn parse_hex(value: &str) -> std::result::Result<u32, String> {
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("expected hexadecimal digits without a prefix".into());
    }
    u32::from_str_radix(value, 16).map_err(|_| "hexadecimal value exceeds u32".into())
}
impl Options {
    fn validate(&self) -> Result<()> {
        if self.count == 0 || self.count > 6000 || self.first.checked_add(self.count - 1).is_none()
        {
            return Err(status::input(
                "sequence range must contain 1..6000 values without u32 wrap",
            ));
        }
        canharbor_core::CanId::new(self.id, self.extended)
            .map_err(|_| status::input("CAN identifier exceeds selected identifier width"))?;
        Ok(())
    }
    fn frame(&self, sequence: u32) -> api::FrameDto {
        api::FrameDto {
            kind: if self.fd {
                api::FrameKind::FdData
            } else {
                api::FrameKind::ClassicData
            },
            id: self.id,
            extended: self.extended,
            brs: self.brs,
            data_hex: format!("{:08X}{sequence:08X}", self.run_id),
            dlc: None,
        }
    }
}

/// Final missing count is calculated against the finite requested range, not a
/// high-water mark: a late arrival fills a hole without counting a loss twice.
struct Receiver {
    seen: Vec<bool>,
    highest: Option<usize>,
    unique: u32,
    duplicates: u64,
    reordered: u32,
    unrelated: u64,
    out_of_range: u64,
    diagnostics: u64,
}
impl Receiver {
    fn new(count: u32) -> Self {
        Self {
            seen: vec![false; count as usize],
            highest: None,
            unique: 0,
            duplicates: 0,
            reordered: 0,
            unrelated: 0,
            out_of_range: 0,
            diagnostics: 0,
        }
    }
    fn event(&mut self, options: &Options, event: api::BusEvent) {
        let api::BusEvent::FrameReceived {
            direction: api::Direction::Rx,
            frame,
            ..
        } = event
        else {
            if matches!(event, api::BusEvent::BusDiagnostic { .. }) {
                self.diagnostics = self.diagnostics.saturating_add(1);
            }
            return;
        };
        if frame.id != options.id
            || frame.extended != options.extended
            || !matches!(
                (options.fd, frame.kind),
                (false, api::FrameKind::ClassicData) | (true, api::FrameKind::FdData)
            )
            || frame.brs != options.brs
            || frame.data_hex.len() != 16
            || !frame.data_hex.bytes().all(|b| b.is_ascii_hexdigit())
        {
            self.unrelated = self.unrelated.saturating_add(1);
            return;
        }
        let Ok(tag) = u32::from_str_radix(&frame.data_hex[..8], 16) else {
            self.unrelated = self.unrelated.saturating_add(1);
            return;
        };
        let Ok(sequence) = u32::from_str_radix(&frame.data_hex[8..], 16) else {
            self.unrelated = self.unrelated.saturating_add(1);
            return;
        };
        if tag != options.run_id {
            self.unrelated = self.unrelated.saturating_add(1);
            return;
        }
        let Some(index) = sequence
            .checked_sub(options.first)
            .filter(|v| *v < options.count)
            .map(|v| v as usize)
        else {
            self.out_of_range = self.out_of_range.saturating_add(1);
            return;
        };
        if self.seen[index] {
            self.duplicates = self.duplicates.saturating_add(1);
            return;
        }
        self.seen[index] = true;
        self.unique += 1;
        if self.highest.is_some_and(|v| index < v) {
            self.reordered += 1;
        }
        self.highest = Some(self.highest.map_or(index, |v| v.max(index)));
    }
}
struct Progress {
    attempted: u32,
    submitted: u32,
    pending: Option<String>,
    unknown: u32,
    failed: u32,
    received: Receiver,
}

pub async fn run(client: &LocalClient, command: Command) -> Result<()> {
    let (options, interval, timeout, active) = match &command {
        Command::Send {
            options,
            active,
            interval_ms,
        } => {
            if !active {
                return Err(status::input("sequence send requires explicit --active"));
            }
            if u64::from(options.count.saturating_sub(1)) * u64::from(*interval_ms) > 60000 {
                return Err(status::input("sequence send schedule exceeds 60 seconds"));
            }
            (options, Some(*interval_ms), 60000, true)
        }
        Command::Receive {
            options,
            timeout_ms,
            active,
        } => (options, None, *timeout_ms, *active),
    };
    options.validate()?;
    let capabilities = client
        .request_typed::<api::Capabilities>(
            "GET",
            &format!("/channels/{}/capabilities", options.channel),
            Value::Null,
            None,
        )
        .await?;
    let session = client
        .create(&api::CreateRequest {
            channel_id: options.channel.clone(),
            config: api::Config {
                mode: if active {
                    api::SessionMode::Active
                } else {
                    api::SessionMode::ListenOnly
                },
                nominal_bitrate: options.bitrate,
                fd: options.fd.then_some(api::FdConfig {
                    enabled: true,
                    data_bitrate: options.data_bitrate,
                    iso: true,
                }),
            },
        })
        .await?;
    let id = session.id;
    let lease = client.acquire(&id).await?;
    let generation = lease.generation.as_str().to_owned();
    let mut progress = Progress {
        attempted: 0,
        submitted: 0,
        pending: None,
        unknown: 0,
        failed: 0,
        received: Receiver::new(options.count),
    };
    let started = Instant::now();
    let deadline = started + Duration::from_millis(u64::from(timeout));
    // Preserve the in-flight work across renewal ticks. Dropping a TX future
    // cannot establish whether it executed: its request ID remains in progress.
    let mut work = Box::pin(execute(
        client,
        &id,
        &generation,
        options,
        interval,
        &mut progress,
    ));
    let mut renew =
        tokio::time::interval_at(started + Duration::from_secs(5), Duration::from_secs(5));
    renew.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let signal = tokio::signal::ctrl_c();
    tokio::pin!(signal);
    let result = loop {
        tokio::select! {
            biased;
            signal = &mut signal => break Err(incomplete(if signal.is_ok() { "interrupted; no automatic resume" } else { "signal monitoring failed" })),
            _ = tokio::time::sleep_until(deadline) => break Err(incomplete("communication test deadline exceeded")),
            result = &mut work => break result,
            _ = renew.tick() => {
                let renew_path = format!("/sessions/{id}/lease/renew");
                let renewal = client.request("POST", &renew_path, json!({}), Some(&generation));
                tokio::select! {
                    biased;
                    _ = &mut signal => break Err(incomplete("interrupted during renewal")),
                    _ = tokio::time::sleep_until(deadline) => break Err(incomplete("communication test deadline exceeded during renewal")),
                    result = renewal => if let Err(error) = result { break Err(error); },
                }
            }
        }
    };
    drop(work);
    if progress.pending.is_some() && progress.unknown == 0 {
        progress.unknown = 1;
    }
    let cleanup = client
        .request(
            "DELETE",
            &format!("/sessions/{id}"),
            json!({}),
            Some(&generation),
        )
        .await;
    let elapsed = started.elapsed().as_secs_f64();
    let missing = options.count - progress.received.unique;
    let success = result.is_ok() && cleanup.is_ok();
    crate::print(json!({
        "report_version":"1", "test":if interval.is_some(){"send"}else{"receive"},
        "run_id":format!("{:08X}",options.run_id), "first":options.first, "expected":options.count,
        "session_id":id, "generation":generation, "simulated":capabilities.simulated,
        "passed":success && (interval.is_some() || (missing == 0 && progress.received.duplicates == 0 && progress.received.reordered == 0)),
        "state":if progress.unknown>0{"outcome_unknown"}else if success{"completed"}else{"incomplete"},
        "attempted":progress.attempted, "submitted":progress.submitted, "failed":progress.failed, "unknown":progress.unknown,
        "pending_request_id":progress.pending, "confirmation":if interval.is_some(){"backend_submission_only"}else{"rx_observed_only"},
        "hardware_verified":false,
        "received_unique":progress.received.unique, "missing":if interval.is_none(){Some(missing)}else{None},
        "duplicates":progress.received.duplicates, "out_of_order":progress.received.reordered,
        "unrelated":progress.received.unrelated, "out_of_range":progress.received.out_of_range, "diagnostics":progress.received.diagnostics,
        "elapsed_seconds":elapsed, "unique_frames_per_second":if elapsed>0.0{f64::from(progress.received.unique)/elapsed}else{0.0},
        "error":result.as_ref().err().map(|e|e.to_string()), "cleanup_error":cleanup.as_ref().err().map(|e|e.to_string()),
    }));
    if progress.unknown > 0 {
        return Err(status::unknown(format!(
            "transmission outcome unknown; session {id}, request {}; inspect with canharbor receipt --session {id} --request {} --generation {generation}; do not resend automatically",
            progress.pending.as_deref().unwrap_or("unknown"),
            progress.pending.as_deref().unwrap_or("unknown")
        )));
    }
    result?;
    cleanup?;
    if interval.is_none() && (progress.received.duplicates > 0 || progress.received.reordered > 0) {
        return Err(incomplete("sequence integrity errors detected"));
    }
    Ok(())
}
fn incomplete(message: &str) -> Box<dyn Error> {
    Box::new(status::Failure::Reported {
        code: 6,
        message: message.into(),
    })
}
async fn execute(
    client: &LocalClient,
    id: &str,
    generation: &str,
    options: &Options,
    interval: Option<u32>,
    progress: &mut Progress,
) -> Result<()> {
    if let Some(interval) = interval {
        client.running(id, generation, true).await?;
        for offset in 0..options.count {
            if offset > 0 {
                tokio::time::sleep(Duration::from_millis(u64::from(interval))).await;
            }
            let request_id = Uuid::new_v4().to_string();
            progress.pending = Some(request_id.clone());
            progress.attempted += 1;
            let result = client
                .submit(
                    id,
                    generation,
                    &api::Tx {
                        client_request_id: request_id,
                        frame: options.frame(options.first + offset),
                    },
                )
                .await;
            match result {
                Ok(receipt) if matches!(receipt.state, api::TxState::Submitted) => {
                    progress.submitted += 1;
                    progress.pending = None;
                }
                Ok(_) => {
                    progress.unknown = 1;
                    return Err(status::unknown(
                        "submission did not return a final submitted receipt",
                    ));
                }
                Err(error) => {
                    if error
                        .downcast_ref::<canharbor_client::ApiFailure>()
                        .is_some_and(|v| !v.submission_is_unknown())
                    {
                        progress.failed += 1;
                        progress.pending = None;
                    } else {
                        progress.unknown = 1;
                    }
                    return Err(error);
                }
            }
        }
        Ok(())
    } else {
        let mut stream = client
            .subscribe(
                id,
                &api::Subscribe {
                    r#type: api::SubscribeKind::Subscribe,
                    protocol_version: "1.0".into(),
                    csrf_token: None,
                    after_sequence: None,
                    filter: api::Filter {
                        ids: vec![options.id],
                        extended: Some(options.extended),
                        direction: Some(api::Direction::Rx),
                    },
                },
            )
            .await?;
        match stream.next().await? {
            Some(api::StreamMessage::Hello { .. }) => {}
            _ => return Err("missing stream greeting".into()),
        }
        client.running(id, generation, true).await?;
        if crate::i18n::chinese() {
            eprintln!(
                "序列接收端已就绪：运行 {:08X}，通道 {}",
                options.run_id, options.channel
            );
        } else {
            eprintln!(
                "sequence receiver ready: run {:08X}, channel {}",
                options.run_id, options.channel
            );
        }
        loop {
            match stream.next().await? {
                Some(api::StreamMessage::Events { events, .. }) => {
                    for event in events {
                        if matches!(event, api::BusEvent::BusDiagnostic { overflow: true, .. }) {
                            progress.received.diagnostics =
                                progress.received.diagnostics.saturating_add(1);
                            return Err(incomplete("receive overflow; results are incomplete"));
                        }
                        progress.received.event(options, event);
                    }
                    if progress.received.unique == options.count {
                        return Ok(());
                    }
                }
                Some(api::StreamMessage::Gap { .. } | api::StreamMessage::ResumeUnavailable) => {
                    return Err("event stream loss; results are incomplete".into());
                }
                Some(api::StreamMessage::SessionFault { .. }) => {
                    return Err("device fault; results are incomplete".into());
                }
                Some(api::StreamMessage::Hello { .. }) => {
                    return Err("unexpected stream greeting".into());
                }
                None => {
                    return Err(Box::new(canharbor_client::StreamUnavailable(
                        "event stream closed; no automatic resume".into(),
                    )));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn options() -> Options {
        Options {
            channel: "virtual:0".into(),
            id: 0x123,
            extended: false,
            run_id: 0xabcdef01,
            count: 4,
            first: 10,
            bitrate: 500000,
            fd: false,
            data_bitrate: 2000000,
            brs: false,
        }
    }
    fn event(o: &Options, seq: u32) -> api::BusEvent {
        let frame = o.frame(seq).into_frame().unwrap();
        api::BusEvent::FrameReceived {
            sequence: 1u64.into(),
            host_monotonic_ns: 0u64.into(),
            timestamp_quality: api::TimestampQuality::HostOnly,
            device_raw_timestamp_us: None,
            device_timestamp_us: None,
            timestamp_epoch: 0u64.into(),
            raw_dlc: None,
            direction: api::Direction::Rx,
            frame: (&frame).into(),
        }
    }
    #[test]
    fn late_frames_fill_holes_duplicates_do_not_and_bounds_are_exact() {
        let o = options();
        let mut r = Receiver::new(o.count);
        for seq in [10, 12, 12, 11, 9, 14] {
            r.event(&o, event(&o, seq));
        }
        assert_eq!(r.unique, 3);
        assert_eq!(o.count - r.unique, 1);
        assert_eq!(r.duplicates, 1);
        assert_eq!(r.reordered, 1);
        assert_eq!(r.out_of_range, 2);
        r.event(&o, event(&o, 13));
        assert_eq!(r.unique, 4);
    }
    #[test]
    fn only_matching_rx_data_frames_contribute_to_sequence_statistics() {
        let o = options();
        let mut r = Receiver::new(o.count);
        let mut other = options();
        other.run_id += 1;
        r.event(&o, event(&other, 10));
        other = options();
        other.extended = true;
        r.event(&o, event(&other, 10));
        other = options();
        other.fd = true;
        r.event(&o, event(&other, 10));
        let mut tx = event(&o, 10);
        if let api::BusEvent::FrameReceived { direction, .. } = &mut tx {
            *direction = api::Direction::Tx;
        }
        r.event(&o, tx);
        let mut malformed = event(&o, 10);
        if let api::BusEvent::FrameReceived { frame, .. } = &mut malformed {
            frame.data_hex = "éééééééé".into();
        }
        r.event(&o, malformed);
        assert_eq!(r.unique, 0);
        assert_eq!(r.unrelated, 4);
    }
    #[test]
    fn sequence_payload_is_big_endian_tag_then_u32_without_wrap() {
        let mut o = options();
        assert_eq!(o.frame(10).data_hex, "ABCDEF010000000A");
        o.first = u32::MAX - 3;
        assert!(o.validate().is_ok());
        o.first += 1;
        assert!(o.validate().is_err());
        assert!(parse_hex("0x123").is_err());
        assert!(parse_hex("+123").is_err());
        assert!(parse_hex("100000000").is_err());
    }
}
