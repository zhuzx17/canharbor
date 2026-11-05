// SPDX-License-Identifier: AGPL-3.0-or-later
// Generated from Rust wire DTOs. Run bun run --cwd web api:generate. Do not edit.

/**
 * Canonical u64 encoded as a decimal string to avoid JavaScript rounding.
 *
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "Decimal".
 */
export type Decimal = string;
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "CaptureState".
 */
export type CaptureState =
  "starting" | "recording" | "completed" | "incomplete" | "failed";
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "SessionMode".
 */
export type SessionMode = "active" | "listen_only";
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "FaultCode".
 */
export type FaultCode =
  | "device_disconnected"
  | "bus_off"
  | "receive_overflow"
  | "transmit_failed"
  | "stop_failed";
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "SessionState".
 */
export type SessionState =
  "configured" | "running" | "faulted" | "disconnected";
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "FrameKind".
 */
export type FrameKind = "classic_data" | "classic_remote" | "fd_data";
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "TaskState".
 */
export type TaskState =
  | "starting"
  | "running"
  | "cancelling"
  | "completed"
  | "cancelled"
  | "failed"
  | "outcome_unknown";
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "StreamMessage".
 */
export type StreamMessage =
  | {
      api_version: string;
      capacity: number;
      server_instance_id: string;
      session_epoch: string;
      type: "hello";
    }
  | {
      events: BusEvent[];
      server_instance_id: string;
      session_epoch: string;
      type: "events";
    }
  | {
      cause: GapCause;
      count: Decimal;
      type: "gap";
    }
  | {
      type: "resume_unavailable";
    }
  | {
      code: FaultCode;
      state: SessionState;
      type: "session_fault";
    };
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "BusEvent".
 */
export type BusEvent =
  | {
      device_raw_timestamp_us?: number | null;
      device_timestamp_us?: Decimal | null;
      direction: Direction;
      frame: Frame;
      host_monotonic_ns: Decimal;
      raw_dlc?: number | null;
      sequence: Decimal;
      timestamp_epoch: Decimal;
      timestamp_quality: TimestampQuality;
      type: "frame_received";
    }
  | {
      device_raw_timestamp_us?: number | null;
      device_timestamp_us?: Decimal | null;
      error_classes: number;
      host_monotonic_ns: Decimal;
      overflow: boolean;
      raw_data_hex: string;
      sequence: Decimal;
      timestamp_epoch: Decimal;
      timestamp_quality: TimestampQuality;
      type: "bus_diagnostic";
    };
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "Direction".
 */
export type Direction = "rx" | "tx";
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "TimestampQuality".
 */
export type TimestampQuality = "host_only" | "hardware_unmapped" | "uncertain";
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "GapCause".
 */
export type GapCause = "subscriber_lag";
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "SubscribeKind".
 */
export type SubscribeKind = "subscribe";
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "Confirmation".
 */
export type Confirmation =
  "none" | "virtual_bus_accepted" | "backend_submitted";
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "TxState".
 */
export type TxState = "queued" | "submitted" | "failed" | "outcome_unknown";

/**
 * Catalog of wire types, not an HTTP response. Exported definitions describe
 * implemented M0 payloads; routing and authentication remain in the API design.
 */
export interface Contract {
  auth: Auth;
  capabilities: Capabilities;
  capture: Capture;
  capture_request: CaptureRequest;
  captures: Captures;
  config: Config;
  counter: Decimal;
  create_request: CreateRequest;
  device_report: DeviceReport;
  devices: Devices;
  diagnostics: Diagnostics;
  error: ApiErrorBody;
  frame_input: FrameInput;
  legal: Legal;
  pair_link: PairLink;
  pair_request: PairRequest;
  periodic_request: PeriodicRequest;
  periodic_task: PeriodicTask;
  periodic_tasks: PeriodicTasks;
  receipt_lookup: ReceiptLookup;
  session: Session;
  sessions: Sessions;
  stream_message: StreamMessage;
  subscribe: Subscribe;
  tx: Tx;
  tx_receipt: TxReceipt;
  version: Version;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "Auth".
 */
export interface Auth {
  client_id: string;
  csrf_token: string | null;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "Capabilities".
 */
export interface Capabilities {
  backend: string;
  channel_id: string;
  data_timing?: TimingLimits | null;
  fd: boolean;
  fixed_data_bitrates: number[];
  fixed_nominal_bitrates: number[];
  hardware_timestamps: boolean;
  listen_only: boolean;
  nominal_timing?: TimingLimits | null;
  simulated: boolean;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "TimingLimits".
 */
export interface TimingLimits {
  brp_inc: number;
  brp_max: number;
  brp_min: number;
  clock_hz: number;
  sjw_max: number;
  tseg1_max: number;
  tseg1_min: number;
  tseg2_max: number;
  tseg2_min: number;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "Capture".
 */
export interface Capture {
  bytes: Decimal;
  complete: boolean;
  dropped: Decimal;
  events: Decimal;
  expires_unix_ms: string;
  id: string;
  loss_count_known: boolean;
  reason?: string | null;
  session_id: string;
  state: CaptureState;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "CaptureRequest".
 */
export interface CaptureRequest {
  client_request_id: string;
  max_duration_seconds?: number;
  session_id: string;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "Captures".
 */
export interface Captures {
  captures: Capture[];
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "Config".
 */
export interface Config {
  fd?: FdConfig | null;
  mode: SessionMode;
  nominal_bitrate: number;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "FdConfig".
 */
export interface FdConfig {
  data_bitrate: number;
  enabled: boolean;
  iso: boolean;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "CreateRequest".
 */
export interface CreateRequest {
  channel_id: string;
  config: Config;
}
/**
 * A read-only inventory snapshot, never a hardware communication certificate.
 *
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "DeviceReport".
 */
export interface DeviceReport {
  channels: ChannelReport[];
  generated_unix_ms: Decimal;
  host: DiagnosticHost;
  report_version: string;
  server_instance_id: string;
  service_version: string;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "ChannelReport".
 */
export interface ChannelReport {
  capabilities: Capabilities | null;
  capability_error: string | null;
  channel_id: string;
  /**
   * Always false: exporting inventory does not perform a communication test.
   */
  communication_verified: boolean;
  device_model: string | null;
  firmware_version: string | null;
  label: string;
  library_version: string | null;
  limitations: string[];
  registered: boolean;
  sessions: DiagnosticSession[];
  verification: string;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "DiagnosticSession".
 */
export interface DiagnosticSession {
  config: EffectiveConfig;
  fault: FaultCode | null;
  state: SessionState;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "EffectiveConfig".
 */
export interface EffectiveConfig {
  data_bitrate?: number | null;
  mode: SessionMode;
  nominal_bitrate: number;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "DiagnosticHost".
 */
export interface DiagnosticHost {
  architecture: string;
  /**
   * Kernel release, not the macOS product version. No hostname is collected.
   */
  kernel_release: string | null;
  os: string;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "Devices".
 */
export interface Devices {
  devices: Device[];
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "Device".
 */
export interface Device {
  backend: string;
  channels: Channel[];
  id: string;
  label: string;
  simulated: boolean;
  verification: string;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "Channel".
 */
export interface Channel {
  fd: boolean;
  id: string;
  label: string;
  listen_only: boolean;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "Diagnostics".
 */
export interface Diagnostics {
  hardware_backends: string[];
  session_count: number;
  stage: string;
  stream_capacity: number;
  ws_slots_available: number;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "ApiErrorBody".
 */
export interface ApiErrorBody {
  error: ErrorDetail;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "ErrorDetail".
 */
export interface ErrorDetail {
  code: string;
  message: string;
  outcome_unknown?: boolean;
  receipt?: string | null;
  request_id: string;
  retryable: boolean;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "FrameInput".
 */
export interface FrameInput {
  brs?: boolean;
  data_hex?: string;
  dlc?: number | null;
  extended?: boolean;
  id: number;
  kind: FrameKind;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "Legal".
 */
export interface Legal {
  exact_source_archive: string | null;
  license: string;
  notice: string;
  source_url: string;
  stage: string;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "PairLink".
 */
export interface PairLink {
  url: string;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "PairRequest".
 */
export interface PairRequest {
  code: string;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "PeriodicRequest".
 */
export interface PeriodicRequest {
  client_request_id: string;
  count: number;
  frame: FrameInput;
  interval_ms: number;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "PeriodicTask".
 */
export interface PeriodicTask {
  attempted: Decimal;
  client_request_id: string;
  failed: Decimal;
  generation: Decimal;
  id: string;
  max_dispatch_lateness_ns: Decimal;
  planned: Decimal;
  reason: string | null;
  session_id: string;
  simulated: boolean;
  skipped: Decimal;
  state: TaskState;
  submitted: Decimal;
  total_dispatch_lateness_ns: Decimal;
  unknown: Decimal;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "PeriodicTasks".
 */
export interface PeriodicTasks {
  tasks: PeriodicTask[];
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "ReceiptLookup".
 */
export interface ReceiptLookup {
  client_request_id: string;
  generation?: Decimal | null;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "Session".
 */
export interface Session {
  channel_id: string;
  config: EffectiveConfig;
  controller: string | null;
  fault: FaultCode | null;
  generation: Decimal;
  id: string;
  mode: SessionMode;
  revision: Decimal;
  state: SessionState;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "Sessions".
 */
export interface Sessions {
  sessions: Session[];
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "Frame".
 */
export interface Frame {
  brs: boolean;
  data_hex: string;
  dlc: number;
  esi: boolean;
  extended: boolean;
  id: number;
  kind: FrameKind;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "Subscribe".
 */
export interface Subscribe {
  after_sequence?: Decimal | null;
  csrf_token?: string | null;
  filter?: Filter;
  protocol_version: string;
  type: SubscribeKind;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "Filter".
 */
export interface Filter {
  direction?: Direction | null;
  extended?: boolean | null;
  /**
   * @maxItems 256
   */
  ids?: number[];
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "Tx".
 */
export interface Tx {
  client_request_id: string;
  frame: FrameInput;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "TxReceipt".
 */
export interface TxReceipt {
  confirmation: Confirmation;
  error?: ErrorDetail | null;
  receipt: string;
  session_id?: string;
  simulated: boolean;
  state: TxState;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "Version".
 */
export interface Version {
  api_version: string;
  server_instance_id: string;
  stage: string;
  version: string;
  web_ui: boolean;
}
