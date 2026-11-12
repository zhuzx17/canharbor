// SPDX-License-Identifier: AGPL-3.0-or-later
//! The Rust wire contract. JSON Schema and TypeScript are generated from these types.
use canharbor_core::{
    CanId, Frame as CoreFrame,
    runtime::{Event, Mode, SessionView},
};
use schemars::{JsonSchema, Schema};
use serde::{Deserialize, Serialize};

/// Request IDs follow JSON Schema string length semantics (Unicode scalars).
/// At most 512 UTF-8 bytes can be stored for a valid 128-scalar ID.
pub fn valid_client_request_id(id: &str) -> bool {
    !id.is_empty() && id.chars().take(129).count() <= 128
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FrameKind {
    ClassicData,
    ClassicRemote,
    FdData,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "FrameInput", transform = frame_constraints)]
pub struct FrameDto {
    pub kind: FrameKind,
    #[schemars(range(min = 0, max = 536870911))]
    pub id: u32,
    #[serde(default)]
    pub extended: bool,
    #[serde(default)]
    pub brs: bool,
    #[serde(default)]
    #[schemars(length(max = 128), pattern("^([0-9A-Fa-f]{2})*$"))]
    pub data_hex: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 0, max = 15))]
    pub dlc: Option<u8>,
}
// Conditional wire constraints cannot be expressed by field annotations alone.
// Keep them attached to the DTO so every schema consumer gets the same rules.
fn frame_constraints(schema: &mut Schema) {
    schema.insert("allOf".into(), serde_json::json!([
        {"if":{"properties":{"extended":{"const":true}},"required":["extended"]},
         "else":{"properties":{"id":{"maximum":2047}}}},
        {"if":{"properties":{"kind":{"enum":["classic_data","classic_remote"]}}},
         "then":{"properties":{"brs":{"const":false},"data_hex":{"maxLength":16},"dlc":{"type":["integer","null"],"maximum":8}}}},
        {"if":{"properties":{"kind":{"const":"classic_remote"}}},
         "then":{"required":["dlc"],"properties":{"data_hex":{"const":""},"dlc":{"type":"integer","minimum":0,"maximum":8}}}},
        {"if":{"properties":{"kind":{"const":"fd_data"}}},
         "then":{"properties":{"data_hex":{"pattern":"^(([0-9A-Fa-f]{2}){0,8}|([0-9A-Fa-f]{2}){12}|([0-9A-Fa-f]{2}){16}|([0-9A-Fa-f]{2}){20}|([0-9A-Fa-f]{2}){24}|([0-9A-Fa-f]{2}){32}|([0-9A-Fa-f]{2}){48}|([0-9A-Fa-f]{2}){64})$"}}}}
    ]));
    let constraints = schema.get_mut("allOf").unwrap().as_array_mut().unwrap();
    // DLC must agree with wire payload length when supplied on a data frame.
    for (dlc, len) in [0, 1, 2, 3, 4, 5, 6, 7, 8, 12, 16, 20, 24, 32, 48, 64]
        .into_iter()
        .enumerate()
    {
        let mut then =
            serde_json::json!({"properties":{"data_hex":{"minLength":len*2,"maxLength":len*2}}});
        if len != 0 {
            then["required"] = serde_json::json!(["data_hex"]);
        }
        constraints.push(serde_json::json!({
            "if":{"required":["dlc"],"properties":{"kind":{"enum":["classic_data","fd_data"]},"dlc":{"const":dlc}}},"then":then
        }));
    }
}
impl FrameDto {
    pub fn into_frame(self) -> Result<CoreFrame, String> {
        let id = CanId::new(self.id, self.extended).map_err(|e| e.to_string())?;
        if self.data_hex.len() > 128
            || !self.data_hex.len().is_multiple_of(2)
            || !self.data_hex.is_ascii()
        {
            return Err("data_hex must contain complete hex bytes".into());
        }
        let data = (0..self.data_hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&self.data_hex[i..i + 2], 16))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| "invalid hex data")?;
        let frame = match self.kind {
            FrameKind::ClassicData if !self.brs => CoreFrame::classic(id, data),
            FrameKind::ClassicRemote if !self.brs && data.is_empty() => {
                CoreFrame::remote(id, self.dlc.ok_or("RTR requires dlc")?)
            }
            FrameKind::FdData => CoreFrame::fd(id, data, self.brs),
            _ => return Err("invalid frame kind or flags".into()),
        }
        .map_err(|e| e.to_string())?;
        if self.dlc.is_some_and(|d| d != frame.dlc()) {
            return Err("DLC does not match payload".into());
        }
        Ok(frame)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[schemars(transform = frame_constraints)]
pub struct Frame {
    pub kind: FrameKind,
    #[schemars(range(min = 0, max = 536870911))]
    pub id: u32,
    pub extended: bool,
    pub brs: bool,
    pub esi: bool,
    #[schemars(length(max = 128), pattern("^([0-9A-Fa-f]{2})*$"))]
    pub data_hex: String,
    pub dlc: u8,
}
impl From<&CoreFrame> for Frame {
    fn from(f: &CoreFrame) -> Self {
        Self {
            kind: match f {
                CoreFrame::ClassicData { .. } => FrameKind::ClassicData,
                CoreFrame::ClassicRemote { .. } => FrameKind::ClassicRemote,
                CoreFrame::FdData { .. } => FrameKind::FdData,
            },
            id: f.id().value(),
            extended: f.id().extended(),
            brs: matches!(f, CoreFrame::FdData { brs: true, .. }),
            esi: false,
            data_hex: f.data().iter().map(|b| format!("{b:02X}")).collect(),
            dlc: f.dlc(),
        }
    }
}

/// Canonical u64 encoded as a decimal string to avoid JavaScript rounding.
#[derive(Clone, Debug, Serialize, JsonSchema)]
#[schemars(transform = decimal_constraints)]
pub struct Decimal(String);
impl Decimal {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl<'de> Deserialize<'de> for Decimal {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        let number = value.parse::<u64>().map_err(serde::de::Error::custom)?;
        if value != number.to_string() {
            return Err(serde::de::Error::custom(
                "expected a canonical u64 decimal string",
            ));
        }
        Ok(number.into())
    }
}
fn decimal_constraints(schema: &mut Schema) {
    let max = u64::MAX.to_string();
    let mut alternatives = vec!["0".to_owned(), "[1-9][0-9]{0,18}".to_owned(), max.clone()];
    for (i, digit) in max.bytes().enumerate() {
        let lower = if i == 0 { b'1' } else { b'0' };
        if digit > lower {
            let upper = digit - 1;
            let range = if lower == upper {
                (lower as char).to_string()
            } else {
                format!("[{}-{}]", lower as char, upper as char)
            };
            let remaining = max.len() - i - 1;
            let suffix = if remaining == 0 {
                String::new()
            } else {
                format!("[0-9]{{{remaining}}}")
            };
            alternatives.push(format!("{}{range}{suffix}", &max[..i]));
        }
    }
    schema.insert(
        "pattern".into(),
        format!("^({})$", alternatives.join("|")).into(),
    );
}
impl From<u64> for Decimal {
    fn from(n: u64) -> Self {
        Self(n.to_string())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SessionMode {
    Active,
    ListenOnly,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    Configured,
    Running,
    Faulted,
    Disconnected,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FaultCode {
    DeviceDisconnected,
    BusOff,
    ReceiveOverflow,
    TransmitFailed,
    StopFailed,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[schemars(transform = required_properties)]
pub struct Session {
    pub id: String,
    pub channel_id: String,
    pub mode: SessionMode,
    pub state: SessionState,
    pub revision: Decimal,
    pub generation: Decimal,
    pub controller: Option<String>,
    pub fault: Option<FaultCode>,
    pub config: EffectiveConfig,
}
pub fn session(s: SessionView) -> Session {
    Session {
        id: s.id,
        channel_id: s.channel,
        mode: if s.mode == Mode::Active {
            SessionMode::Active
        } else {
            SessionMode::ListenOnly
        },
        state: match s.fault {
            Some(canharbor_core::BackendFault::Disconnected) => SessionState::Disconnected,
            Some(_) => SessionState::Faulted,
            None if s.running => SessionState::Running,
            None => SessionState::Configured,
        },
        revision: s.revision.into(),
        generation: s.generation.into(),
        controller: s.controller,
        fault: s.fault.map(Into::into),
        config: EffectiveConfig {
            mode: if s.config.listen_only {
                SessionMode::ListenOnly
            } else {
                SessionMode::Active
            },
            nominal_bitrate: s.config.nominal_bitrate,
            data_bitrate: s.config.data_bitrate,
        },
    }
}
impl From<canharbor_core::BackendFault> for FaultCode {
    fn from(f: canharbor_core::BackendFault) -> Self {
        match f {
            canharbor_core::BackendFault::Disconnected => Self::DeviceDisconnected,
            canharbor_core::BackendFault::BusOff => Self::BusOff,
            canharbor_core::BackendFault::ReceiveOverflow => Self::ReceiveOverflow,
            canharbor_core::BackendFault::TransmitFailed => Self::TransmitFailed,
            canharbor_core::BackendFault::StopFailed => Self::StopFailed,
        }
    }
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Rx,
    Tx,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TimestampQuality {
    HostOnly,
    HardwareUnmapped,
    Uncertain,
}
impl From<canharbor_core::timestamp::Quality> for TimestampQuality {
    fn from(v: canharbor_core::timestamp::Quality) -> Self {
        match v {
            canharbor_core::timestamp::Quality::HostOnly => Self::HostOnly,
            canharbor_core::timestamp::Quality::HardwareUnmapped => Self::HardwareUnmapped,
            canharbor_core::timestamp::Quality::Uncertain => Self::Uncertain,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BusEvent {
    FrameReceived {
        sequence: Decimal,
        host_monotonic_ns: Decimal,
        timestamp_quality: TimestampQuality,
        device_raw_timestamp_us: Option<u32>,
        device_timestamp_us: Option<Decimal>,
        timestamp_epoch: Decimal,
        raw_dlc: Option<u8>,
        direction: Direction,
        frame: Frame,
    },
    BusDiagnostic {
        sequence: Decimal,
        host_monotonic_ns: Decimal,
        timestamp_quality: TimestampQuality,
        device_raw_timestamp_us: Option<u32>,
        device_timestamp_us: Option<Decimal>,
        timestamp_epoch: Decimal,
        error_classes: u32,
        raw_data_hex: String,
        overflow: bool,
    },
}
pub fn event(e: Event) -> BusEvent {
    let mut frame: Frame = (&e.frame).into();
    frame.esi = e.metadata.esi;
    let stamp = e.metadata.stamp;
    BusEvent::FrameReceived {
        sequence: e.sequence.into(),
        host_monotonic_ns: e.elapsed_ns.into(),
        timestamp_quality: stamp.quality.into(),
        device_raw_timestamp_us: stamp.raw_us,
        device_timestamp_us: stamp.extended_us.map(Into::into),
        timestamp_epoch: stamp.epoch.into(),
        raw_dlc: e.metadata.raw_dlc,
        direction: if e.direction == "tx" {
            Direction::Tx
        } else {
            Direction::Rx
        },
        frame,
    }
}
pub fn notice(n: canharbor_core::runtime::Notice) -> BusEvent {
    match n {
        canharbor_core::runtime::Notice::Frame(e) => event(e),
        canharbor_core::runtime::Notice::Diagnostic {
            sequence,
            elapsed_ns,
            diagnostic,
        } => BusEvent::BusDiagnostic {
            sequence: sequence.into(),
            host_monotonic_ns: elapsed_ns.into(),
            timestamp_quality: diagnostic.stamp.quality.into(),
            device_raw_timestamp_us: diagnostic.stamp.raw_us,
            device_timestamp_us: diagnostic.stamp.extended_us.map(Into::into),
            timestamp_epoch: diagnostic.stamp.epoch.into(),
            error_classes: diagnostic.classes,
            raw_data_hex: diagnostic.data.iter().map(|b| format!("{b:02X}")).collect(),
            overflow: diagnostic.overflow,
        },
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamMessage {
    Hello {
        api_version: String,
        server_instance_id: String,
        session_epoch: String,
        capacity: u32,
    },
    Events {
        server_instance_id: String,
        session_epoch: String,
        events: Vec<BusEvent>,
    },
    Gap {
        cause: GapCause,
        count: Decimal,
    },
    ResumeUnavailable,
    SessionFault {
        state: SessionState,
        code: FaultCode,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GapCause {
    SubscriberLag,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Filter {
    #[serde(default)]
    #[schemars(length(max = 256))]
    pub ids: Vec<u32>,
    #[serde(default)]
    pub extended: Option<bool>,
    #[serde(default)]
    pub direction: Option<Direction>,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SubscribeKind {
    Subscribe,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Subscribe {
    pub r#type: SubscribeKind,
    pub protocol_version: String,
    #[serde(default)]
    pub csrf_token: Option<String>,
    #[serde(default)]
    pub after_sequence: Option<Decimal>,
    #[serde(default)]
    pub filter: Filter,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PairRequest {
    pub code: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[schemars(transform = required_properties)]
pub struct Auth {
    pub client_id: String,
    pub csrf_token: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FdConfig {
    pub enabled: bool,
    pub data_bitrate: u32,
    pub iso: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub mode: SessionMode,
    pub nominal_bitrate: u32,
    #[serde(default)]
    pub fd: Option<FdConfig>,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateRequest {
    pub channel_id: String,
    pub config: Config,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Tx {
    #[schemars(length(min = 1, max = 128))]
    pub client_request_id: String,
    pub frame: FrameDto,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct EffectiveConfig {
    pub mode: SessionMode,
    pub nominal_bitrate: u32,
    pub data_bitrate: Option<u32>,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct TimingLimits {
    pub clock_hz: u32,
    pub tseg1_min: u32,
    pub tseg1_max: u32,
    pub tseg2_min: u32,
    pub tseg2_max: u32,
    pub sjw_max: u32,
    pub brp_min: u32,
    pub brp_max: u32,
    pub brp_inc: u32,
}
impl From<canharbor_core::TimingLimits> for TimingLimits {
    fn from(v: canharbor_core::TimingLimits) -> Self {
        Self {
            clock_hz: v.clock_hz,
            tseg1_min: v.tseg1_min,
            tseg1_max: v.tseg1_max,
            tseg2_min: v.tseg2_min,
            tseg2_max: v.tseg2_max,
            sjw_max: v.sjw_max,
            brp_min: v.brp_min,
            brp_max: v.brp_max,
            brp_inc: v.brp_inc,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Capabilities {
    pub channel_id: String,
    pub backend: String,
    pub simulated: bool,
    pub fd: bool,
    pub listen_only: bool,
    pub hardware_timestamps: bool,
    pub nominal_timing: Option<TimingLimits>,
    pub data_timing: Option<TimingLimits>,
    pub fixed_nominal_bitrates: Vec<u32>,
    pub fixed_data_bitrates: Vec<u32>,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Channel {
    pub id: String,
    pub label: String,
    pub fd: bool,
    pub listen_only: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Device {
    pub id: String,
    pub label: String,
    pub backend: String,
    pub simulated: bool,
    pub verification: String,
    pub channels: Vec<Channel>,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Devices {
    pub devices: Vec<Device>,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Sessions {
    pub sessions: Vec<Session>,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Version {
    pub version: String,
    pub api_version: String,
    pub server_instance_id: String,
    pub stage: String,
    pub web_ui: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct PairLink {
    pub url: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReceiptLookup {
    #[schemars(length(min = 1, max = 128))]
    pub client_request_id: String,
    pub generation: Option<Decimal>,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TxState {
    Queued,
    Submitted,
    Failed,
    OutcomeUnknown,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Confirmation {
    None,
    VirtualBusAccepted,
    BackendSubmitted,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct TxReceipt {
    pub receipt: String,
    #[serde(default)]
    pub session_id: String,
    pub state: TxState,
    pub simulated: bool,
    pub confirmation: Confirmation,
    pub error: Option<ErrorDetail>,
}
/// A read-only inventory snapshot, never a hardware communication certificate.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct DeviceReport {
    pub report_version: String,
    pub generated_unix_ms: Decimal,
    pub service_version: String,
    pub server_instance_id: String,
    pub host: DiagnosticHost,
    pub channels: Vec<ChannelReport>,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[schemars(transform = required_properties)]
pub struct DiagnosticHost {
    pub os: String,
    pub architecture: String,
    /// Kernel release, not the macOS product version. No hostname is collected.
    pub kernel_release: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[schemars(transform = required_properties)]
pub struct ChannelReport {
    pub channel_id: String,
    pub label: String,
    pub registered: bool,
    pub capabilities: Option<Capabilities>,
    pub capability_error: Option<String>,
    pub verification: String,
    /// Always false: exporting inventory does not perform a communication test.
    pub communication_verified: bool,
    pub device_model: Option<String>,
    pub firmware_version: Option<String>,
    pub library_version: Option<String>,
    pub sessions: Vec<DiagnosticSession>,
    pub limitations: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[schemars(transform = required_properties)]
pub struct DiagnosticSession {
    pub state: SessionState,
    pub config: EffectiveConfig,
    pub fault: Option<FaultCode>,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Diagnostics {
    pub stage: String,
    pub hardware_backends: Vec<String>,
    pub session_count: u32,
    pub stream_capacity: u32,
    pub ws_slots_available: u32,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[schemars(transform = required_properties)]
pub struct Legal {
    pub license: String,
    pub source_url: String,
    pub stage: String,
    pub exact_source_archive: Option<String>,
    pub notice: String,
}
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub enum UiLocale {
    #[default]
    #[serde(rename = "auto")]
    Auto,
    #[serde(rename = "en")]
    En,
    #[serde(rename = "zh-CN")]
    ZhCn,
}
impl UiLocale {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::En => "en",
            Self::ZhCn => "zh-CN",
        }
    }
}
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UiPreferences {
    pub locale: UiLocale,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ErrorDetail {
    pub code: String,
    pub message: String,
    pub retryable: bool,
    pub request_id: String,
    pub receipt: Option<String>,
    #[serde(default)]
    pub outcome_unknown: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ApiErrorBody {
    pub error: ErrorDetail,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CaptureRequest {
    #[serde(default)]
    #[schemars(length(max = 120))]
    pub name: String,
    pub session_id: String,
    #[schemars(length(min = 1, max = 128))]
    pub client_request_id: String,
    #[serde(default = "capture_duration")]
    #[schemars(range(min = 1, max = 86400))]
    pub max_duration_seconds: u32,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CaptureRename {
    #[schemars(length(max = 120))]
    pub name: String,
    #[schemars(length(max = 120))]
    pub expected_name: String,
}
fn capture_duration() -> u32 {
    3600
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CaptureState {
    Starting,
    Recording,
    Completed,
    Incomplete,
    Failed,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Capture {
    pub name: String,
    pub channel_id: Option<String>,
    pub created_unix_ms: Option<String>,
    pub id: String,
    pub session_id: String,
    pub state: CaptureState,
    pub complete: bool,
    pub loss_count_known: bool,
    pub expires_unix_ms: String,
    pub events: Decimal,
    pub dropped: Decimal,
    pub bytes: Decimal,
    pub reason: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Captures {
    pub captures: Vec<Capture>,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PeriodicRequest {
    #[schemars(length(min = 1, max = 128))]
    pub client_request_id: String,
    pub frame: FrameDto,
    #[schemars(range(min = 10, max = 60000))]
    pub interval_ms: u32,
    #[schemars(range(min = 1, max = 6000))]
    pub count: u32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    Starting,
    Running,
    Cancelling,
    Completed,
    Cancelled,
    Failed,
    OutcomeUnknown,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[schemars(transform = required_properties)]
pub struct PeriodicTask {
    pub simulated: bool,
    pub id: String,
    pub session_id: String,
    pub client_request_id: String,
    pub generation: Decimal,
    pub state: TaskState,
    pub planned: Decimal,
    pub attempted: Decimal,
    pub submitted: Decimal,
    pub failed: Decimal,
    pub unknown: Decimal,
    pub skipped: Decimal,
    pub max_dispatch_lateness_ns: Decimal,
    pub total_dispatch_lateness_ns: Decimal,
    pub reason: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct PeriodicTasks {
    pub tasks: Vec<PeriodicTask>,
}
/// Catalog of wire types, not an HTTP response. Exported definitions describe
/// implemented M0 payloads; routing and authentication remain in the API design.
#[derive(JsonSchema)]
pub struct Contract {
    pub ui_preferences: UiPreferences,
    pub stress_events: canharbor_stress::Events,
    pub stress_runs: canharbor_stress::Runs,
    pub stress_artifacts: canharbor_stress::Artifacts,
    pub stress_metric: canharbor_stress::Metric,
    pub stress_validation: canharbor_stress::Validation,
    pub stress_request: canharbor_stress::Request,
    pub stress_coord_request: canharbor_stress::CoordRequest,
    pub stress_coord_status: canharbor_stress::CoordStatus,
    pub stress_plan: canharbor_stress::Plan,
    pub stress_report: canharbor_stress::Report,
    pub periodic_request: PeriodicRequest,
    pub periodic_task: PeriodicTask,
    pub periodic_tasks: PeriodicTasks,
    pub counter: Decimal,
    pub capture_request: CaptureRequest,
    pub capture_rename: CaptureRename,
    pub capture: Capture,
    pub captures: Captures,
    pub session: Session,
    pub frame_input: FrameDto,
    pub auth: Auth,
    pub create_request: CreateRequest,
    pub tx: Tx,
    pub subscribe: Subscribe,
    pub stream_message: StreamMessage,
    pub devices: Devices,
    pub capabilities: Capabilities,
    pub config: Config,
    pub sessions: Sessions,
    pub version: Version,
    pub pair_request: PairRequest,
    pub pair_link: PairLink,
    pub tx_receipt: TxReceipt,
    pub receipt_lookup: ReceiptLookup,
    pub diagnostics: Diagnostics,
    pub device_report: DeviceReport,
    pub legal: Legal,
    pub error: ApiErrorBody,
}
fn required_properties(schema: &mut Schema) {
    let required = schema
        .get("properties")
        .unwrap()
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    schema.insert("required".into(), serde_json::json!(required));
}
pub fn schema() -> Schema {
    schemars::generate::SchemaSettings::draft07()
        .into_generator()
        .into_root_schema_for::<Contract>()
}

/// Empty names use the capture ID; named recordings must be printable and bounded.
pub fn valid_capture_name(name: &str) -> bool {
    name.chars().count() <= 120
        && !name.chars().any(char::is_control)
        && (name.is_empty() || name.trim() == name && !name.trim().is_empty())
}
