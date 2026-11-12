// SPDX-License-Identifier: AGPL-3.0-or-later
use crate::{AppState, capabilities, dto};
use axum::{
    Json,
    extract::{Path, State},
};
use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

#[cfg(unix)]
fn kernel_release() -> Option<String> {
    let mut value = std::mem::MaybeUninit::<libc::utsname>::uninit();
    // SAFETY: uname initializes this correctly sized output on success.
    if unsafe { libc::uname(value.as_mut_ptr()) } != 0 {
        return None;
    }
    // SAFETY: successful uname initialized the structure and NUL-terminated release.
    let value = unsafe { value.assume_init() };
    Some(
        unsafe { std::ffi::CStr::from_ptr(value.release.as_ptr()) }
            .to_string_lossy()
            .into_owned(),
    )
}

#[cfg(windows)]
fn kernel_release() -> Option<String> {
    None
}

pub(crate) async fn report(State(s): State<Arc<AppState>>) -> Json<dto::DeviceReport> {
    // This snapshot does not expire leases or perform device I/O. No control identity,
    // library path, hostname, token, or frame payload is included in the report.
    let sessions = s.runtime.session_snapshot().await;
    let mut channels = Vec::new();
    for channel in s.runtime.channels() {
        let (caps, error) = match capabilities(State(s.clone()), Path(channel.id.clone())).await {
            Ok(Json(caps)) => (Some(caps), None),
            Err(_) => (None, Some("capabilities_unavailable".into())),
        };
        let verification = match caps.as_ref() {
            Some(c) if c.simulated => "simulation_only",
            Some(_) => "hardware_unverified",
            None => "unknown",
        };
        let mut limitations = vec![
            "Inventory only; this request does not open, configure, transmit, or probe hardware.".into(),
            "Registration is not proof of current presence or channel availability.".into(),
            "Session configuration is requested configuration, not measured bus timing.".into(),
            "Cached session state can lag lease expiry or a device disconnect; no liveness probe is performed.".into(),
            "Device model, firmware and loaded library version are not exposed by the backend contract.".into(),
        ];
        if caps.as_ref().is_some_and(|c| c.backend == "peak") {
            limitations.push(
                "PEAK adapter targets open-source libPeakUSB ABI 1.0.2; PCBUSB is not supported."
                    .into(),
            );
        }
        channels.push(dto::ChannelReport {
            sessions: sessions
                .iter()
                .filter(|v| v.channel == channel.id)
                .cloned()
                .map(|v| {
                    let v = dto::session(v);
                    dto::DiagnosticSession {
                        state: v.state,
                        config: v.config,
                        fault: v.fault,
                    }
                })
                .collect(),
            channel_id: channel.id,
            label: channel.label,
            registered: true,
            capabilities: caps,
            capability_error: error,
            verification: verification.into(),
            communication_verified: false,
            device_model: None,
            firmware_version: None,
            library_version: None,
            limitations,
        });
    }
    Json(dto::DeviceReport {
        report_version: "1".into(),
        generated_unix_ms: u64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
        )
        .unwrap_or(u64::MAX)
        .into(),
        service_version: env!("CARGO_PKG_VERSION").into(),
        server_instance_id: s.instance.clone(),
        host: dto::DiagnosticHost {
            os: std::env::consts::OS.into(),
            architecture: std::env::consts::ARCH.into(),
            kernel_release: kernel_release(),
        },
        channels,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use canharbor_core::{
        Backend, BackendEvent, BackendFuture, Channel, Config, Error, Frame, control::Permit,
        runtime::Runtime,
    };
    use tokio::sync::broadcast;
    struct Unavailable;
    impl Backend for Unavailable {
        fn channels(&self) -> Vec<Channel> {
            vec![Channel {
                id: "unavailable:0".into(),
                label: "Unavailable capabilities".into(),
            }]
        }
        fn subscribe(&self, _: &str) -> Result<broadcast::Receiver<Arc<BackendEvent>>, Error> {
            panic!("diagnostics must not subscribe")
        }
        fn check_channel(&self, _: &str) -> Result<u64, Error> {
            panic!("diagnostics must not probe")
        }
        fn transmit(&self, _: &str, _: Frame) -> Result<(), Error> {
            panic!("diagnostics must not transmit")
        }
        fn configure<'a>(&'a self, _: &'a str, _: Config) -> BackendFuture<'a, ()> {
            panic!("diagnostics must not configure")
        }
        fn start<'a>(&'a self, _: &'a str, _: Permit) -> BackendFuture<'a, ()> {
            panic!("diagnostics must not start")
        }
        fn stop<'a>(&'a self, _: &'a str) -> BackendFuture<'a, ()> {
            panic!("diagnostics must not stop")
        }
        fn close<'a>(&'a self, _: &'a str) -> BackendFuture<'a, ()> {
            panic!("diagnostics must not close")
        }
    }
    #[tokio::test]
    async fn unsupported_capabilities_do_not_hide_registered_channels() {
        let state = AppState::new(
            Runtime::new(Arc::new(Unavailable)),
            "http://127.0.0.1:32180".into(),
        );
        let Json(value) = report(State(state)).await;
        assert_eq!(value.channels.len(), 1);
        assert!(value.channels[0].capabilities.is_none());
        assert_eq!(
            value.channels[0].capability_error.as_deref(),
            Some("capabilities_unavailable")
        );
        assert_eq!(value.channels[0].verification, "unknown");
        assert!(!value.channels[0].communication_verified);
    }
    #[tokio::test]
    async fn empty_registry_is_a_valid_empty_report() {
        let backend = canharbor_core::registry::Registry::new(vec![]).unwrap();
        let state = AppState::new(
            Runtime::new(Arc::new(backend)),
            "http://127.0.0.1:32180".into(),
        );
        let Json(value) = report(State(state)).await;
        assert!(value.channels.is_empty());
    }
}
