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
 * via the `definition` "StressRttRole".
 */
export type StressRttRole = "loopback" | "probe" | "responder";
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "StressLoadShape".
 */
export type StressLoadShape =
  | {
      format: "uniform";
    }
  | {
      format: "burst";
      frames: number;
    }
  | {
      format: "seeded_intervals";
      jitter_ppm: number;
      seed: number;
    };
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "StressTracking".
 */
export type StressTracking =
  | {
      format: "exact";
    }
  | {
      capacity: number;
      format: "sliding";
    };
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "StressTraffic".
 */
export type StressTraffic =
  | {
      format: "classic";
    }
  | {
      checkpoint_can_id: number;
      format: "classic_pattern_v1";
      max_pending: number;
      seed: number;
      stream: number;
    }
  | {
      data_bitrate: number;
      epoch: number;
      format: "fd_v1";
      seed: number;
      stream: number;
      variants: StressFdVariant[];
    };
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
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "UiLocale".
 */
export type UiLocale = "auto" | "en" | "zh-CN";

/**
 * Catalog of wire types, not an HTTP response. Exported definitions describe
 * implemented M0 payloads; routing and authentication remain in the API design.
 */
export interface Contract {
  auth: Auth;
  capabilities: Capabilities;
  capture: Capture;
  capture_rename: CaptureRename;
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
  stress_artifacts: StressArtifacts;
  stress_coord_request: StressCoordRequest;
  stress_coord_status: StressCoordStatus;
  stress_events: StressEvents;
  stress_metric: StressMetric;
  stress_plan: StressPlan;
  stress_report: StressReport;
  stress_request: StressRequest;
  stress_runs: StressRuns;
  stress_validation: StressValidation;
  subscribe: Subscribe;
  tx: Tx;
  tx_receipt: TxReceipt;
  ui_preferences: UiPreferences;
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
  channel_id?: string | null;
  complete: boolean;
  created_unix_ms?: string | null;
  dropped: Decimal;
  events: Decimal;
  expires_unix_ms: string;
  id: string;
  loss_count_known: boolean;
  name: string;
  reason?: string | null;
  session_id: string;
  state: CaptureState;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "CaptureRename".
 */
export interface CaptureRename {
  expected_name: string;
  name: string;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "CaptureRequest".
 */
export interface CaptureRequest {
  client_request_id: string;
  max_duration_seconds?: number;
  name?: string;
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
 * via the `definition` "StressArtifacts".
 */
export interface StressArtifacts {
  files: StressArtifact[];
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "StressArtifact".
 */
export interface StressArtifact {
  bytes: number;
  name: string;
  sha256: string;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "StressCoordRequest".
 */
export interface StressCoordRequest {
  group_id: string;
  operation: string;
  request?: StressRequest | null;
  token?: string | null;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "StressRequest".
 */
export interface StressRequest {
  active: boolean;
  client_request_id: string;
  plan: StressPlan;
  plan_hash: string;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "StressPlan".
 */
export interface StressPlan {
  adaptive?: StressAdaptiveOptions | null;
  bitrate: number;
  can_id: number;
  coordination?: StressCoordination | null;
  count?: number;
  criteria?: StressCriteria;
  drain_ms: number;
  evidence?: StressEvidenceOptions;
  extended: boolean;
  flows?: StressFlow[];
  fps?: number;
  queues?: StressQueueOptions | null;
  receivers: string[];
  /**
   * Normal controller participation (ACK), not generation of data frames.
   */
  receivers_active: boolean;
  rtt?: StressRttOptions | null;
  run_tag: number;
  sender: string;
  shape?: StressLoadShape;
  stability?: StressStabilityLimits;
  stages?: StressStage[];
  timing?: StressTimingOptions | null;
  topology?: StressTopologyDeclaration | null;
  tracking?: StressTracking;
  traffic?: StressTraffic;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "StressAdaptiveOptions".
 */
export interface StressAdaptiveOptions {
  exploration_seconds: number;
  measurement_seconds: number;
  min_observed_ppm: number;
  rates: number[];
}
/**
 * Transport is authenticated out-of-band (same-UID socket or SSH). No clock offset is assumed.
 *
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "StressCoordination".
 */
export interface StressCoordination {
  clock_quality: string;
  group_id: string;
  lease_ms: number;
  participant: string;
  start_delay_ms: number;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "StressCriteria".
 */
export interface StressCriteria {
  max_dispatch_lateness_ns?: number | null;
  max_duplicates?: number;
  max_missing_expected_ppm?: number;
  max_missing_submitted_ppm?: number;
  max_out_of_range?: number;
  /**
   * Sum of CRC and pattern errors per receiver.
   */
  max_payload_errors?: number;
  max_reordered?: number;
  max_scheduler_skipped?: number;
  /**
   * Checked for the entire run AND every compiled stage (including warmup).
   */
  min_submitted_ppm?: number;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "StressEvidenceOptions".
 */
export interface StressEvidenceOptions {
  max_bytes: number;
  /**
   * Zero disables frame samples; online verification remains complete.
   */
  sample_every: number;
  segment_bytes: number;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "StressFlow".
 */
export interface StressFlow {
  can_id: number;
  count?: number;
  criteria?: StressCriteria;
  extended: boolean;
  fps?: number;
  name: string;
  queues?: StressQueueOptions | null;
  receivers: string[];
  rtt?: StressRttOptions | null;
  run_tag: number;
  sender: string;
  shape?: StressLoadShape;
  stages?: StressStage[];
  timing?: StressTimingOptions | null;
  tracking?: StressTracking;
  traffic?: StressTraffic;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "StressQueueOptions".
 */
export interface StressQueueOptions {
  sample_every: number;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "StressRttOptions".
 */
export interface StressRttOptions {
  max_inflight: number;
  response_can_id: number;
  role?: StressRttRole & string;
  timeout_ms: number;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "StressStage".
 */
export interface StressStage {
  duration_seconds: number;
  fps: number;
  name: string;
  warmup_seconds?: number;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "StressTimingOptions".
 */
export interface StressTimingOptions {
  max_pending?: number;
  sample_every?: number;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "StressFdVariant".
 */
export interface StressFdVariant {
  brs: boolean;
  length: number;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "StressStabilityLimits".
 */
export interface StressStabilityLimits {
  max_rss_growth_bytes?: number | null;
  warmup_seconds: number;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "StressTopologyDeclaration".
 */
export interface StressTopologyDeclaration {
  aliases?: StressRoleAlias[];
  bus?: string;
  wiring?: string;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "StressRoleAlias".
 */
export interface StressRoleAlias {
  alias: string;
  channel: string;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "StressCoordStatus".
 */
export interface StressCoordStatus {
  clock_quality: string;
  expires_in_ms: number;
  group_id: string;
  report?: StressReport | null;
  run_id?: string | null;
  server_instance_id: string;
  state: string;
  token?: string | null;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "StressReport".
 */
export interface StressReport {
  active_stage: number;
  adaptive?: StressAdaptiveReport | null;
  assessment?: StressAssessment | null;
  attempted: number;
  backend_cleanup?: StressCleanupObservation[];
  classic_pattern_transmit?: StressClassicPatternTransmit | null;
  counters_complete: boolean;
  elapsed_seconds: number;
  errors: string[];
  evidence: StressEvidenceStatus;
  execution_timeout_ns?: number;
  failed: number;
  group?: StressGroupReport | null;
  hardware_verified: boolean;
  history: StressMetric[];
  id: string;
  integrity?: string;
  lifecycle?: StressLifecycle;
  lifecycle_history?: StressLifecycleRecord[];
  load?: StressLoadObservation | null;
  max_dispatch_lateness_ns: number;
  measurement_complete: boolean;
  plan: StressPlan;
  plan_hash: string;
  planned: number;
  provenance?: StressProvenance | null;
  receivers: StressReceiverReport[];
  recovery?: StressRecovery;
  rtt_report?: StressRttReport | null;
  rx_lag_events: number;
  scheduling_quantum_ns: number;
  simulated: boolean;
  skipped: number;
  stability?: StressStabilityReport;
  stages: StressStageReport[];
  state: string;
  submission_evidence?: StressSubmissionEvidence;
  submitted: number;
  timing?: StressTimingReport | null;
  unknown: number;
  unknown_sequence?: number | null;
  verdict: string;
  version: number;
  workload_achieved: boolean;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "StressAdaptiveReport".
 */
export interface StressAdaptiveReport {
  phase: string;
  reserved_peak_fps: number;
  reserved_planned: number;
  scope: string;
  selected_fps?: number | null;
  steps: StressAdaptiveStep[];
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "StressAdaptiveStep".
 */
export interface StressAdaptiveStep {
  decision?: StressAdaptiveDecision | null;
  name: string;
  report: StressReport;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "StressAdaptiveDecision".
 */
export interface StressAdaptiveDecision {
  actual_load?: StressLoadObservation | null;
  criteria: StressAssessment;
  input_fps: number;
  name: string;
  next?: string | null;
  qualified: boolean;
  reason: string;
  selected_fps?: number | null;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "StressLoadObservation".
 */
export interface StressLoadObservation {
  actual_fps_milli: number;
  completed: boolean;
  end_ns: number;
  scheduled_duration_ns: number;
  skipped: number;
  start_ns: number;
  submitted: number;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "StressAssessment".
 */
export interface StressAssessment {
  failures: StressCriterionFailure[];
  /**
   * Workload/quality criteria only; pass ALSO requires completed execution and complete evidence.
   */
  passed: boolean;
  workload_met: boolean;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "StressCriterionFailure".
 */
export interface StressCriterionFailure {
  actual: number;
  channel?: string | null;
  criterion: string;
  /**
   * Present for ratios; limit is in ppm. Integer cross-products decide the result.
   */
  denominator?: number | null;
  limit: number;
  stage?: number | null;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "StressCleanupObservation".
 */
export interface StressCleanupObservation {
  backend_unconfirmed?: number | null;
  channel: string;
  failures: string[];
  scope: string;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "StressClassicPatternTransmit".
 */
export interface StressClassicPatternTransmit {
  checkpoint_failed: number;
  checkpoint_skipped: number;
  checkpoint_submitted: number;
  checkpoint_unknown: number;
  data_failed: number;
  data_skipped: number;
  data_submitted: number;
  data_unknown: number;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "StressEvidenceStatus".
 */
export interface StressEvidenceStatus {
  bytes: number;
  complete: boolean;
  error?: string | null;
  queue_high_water: number;
  records: number;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "StressGroupReport".
 */
export interface StressGroupReport {
  budget: StressGroupBudget;
  evidence_scope: string;
  flows: StressFlowReport[];
  start_ns?: number | null;
  stop_requests: {
    [k: string]: number;
  };
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "StressGroupBudget".
 */
export interface StressGroupBudget {
  channels: StressChannelBudget[];
  data_bitrate?: number | null;
  devices: StressDeviceBudget[];
  estimated_evidence_bytes: number;
  evidence_records_per_second: number;
  flow_count: number;
  maximum_inflight: number;
  memory_reserve_bytes: number;
  peak_transmit_fps: number;
  tracking_bytes: number;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "StressChannelBudget".
 */
export interface StressChannelBudget {
  channel: string;
  peak_transmit_fps: number;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "StressDeviceBudget".
 */
export interface StressDeviceBudget {
  channels: string[];
  device_id: string;
  peak_transmit_fps: number;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "StressFlowReport".
 */
export interface StressFlowReport {
  name: string;
  report: StressReport;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "StressMetric".
 */
export interface StressMetric {
  active_stage: number;
  attempted: number;
  elapsed_ms: number;
  evidence_queue_high_water: number;
  failed: number;
  lifecycle?: StressLifecycle;
  max_dispatch_lateness_ns?: number;
  process_cpu_seconds?: number | null;
  process_current_rss_bytes?: number | null;
  process_peak_rss_bytes?: number | null;
  receivers: StressReceiverReport[];
  skipped: number;
  stability?: StressStabilityReport;
  state: string;
  submitted: number;
  target_fps: number;
  unknown: number;
  unknown_sequence?: number | null;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "StressLifecycle".
 */
export interface StressLifecycle {
  execution_end_ns?: number | null;
  inflight_sequence?: number | null;
  last_attempt_ns?: number | null;
  last_attempt_sequence?: number | null;
  last_submitted_ns?: number | null;
  revision: number;
  stop_requested_ns?: number | null;
  termination?: StressTermination | null;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "StressTermination".
 */
export interface StressTermination {
  channel?: string | null;
  code: string;
  detail: string;
  sequence?: number | null;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "StressReceiverReport".
 */
export interface StressReceiverReport {
  channel: string;
  classic_pattern?: StressClassicPatternReceiver | null;
  crc_errors?: number;
  diagnostics: number;
  duplicates: number;
  evidence_complete: boolean;
  missing_expected: number;
  missing_submitted: number;
  out_of_range: number;
  pattern_errors?: number;
  queue_high_water: number;
  queues?: StressQueueReport | null;
  reordered: number;
  short_sequence_only?: number;
  tracking?: StressTrackingReport | null;
  unique: number;
  unrelated: number;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "StressClassicPatternReceiver".
 */
export interface StressClassicPatternReceiver {
  ambiguous_data: number;
  checkpoint_evictions: number;
  checkpoints_unique: number;
  data_verified_unique: number;
  finalized: boolean;
  invalid_envelopes: number;
  missing_checkpoints: number;
  pending_data: number;
  unverified_data: number;
  window_high_water: number;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "StressQueueReport".
 */
export interface StressQueueReport {
  invalid_timestamps: number;
  observed_high_water: number;
  residence: StressLatencyHistogram;
  samples: number;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "StressLatencyHistogram".
 */
export interface StressLatencyHistogram {
  bins: number[];
  max_ns: number;
  negative: number;
  overflow: number;
  p50_upper_ns?: number | null;
  p95_upper_ns?: number | null;
  p99_upper_ns?: number | null;
  samples: number;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "StressTrackingReport".
 */
export interface StressTrackingReport {
  base: number;
  capacity: number;
  finalized: boolean;
  finalized_missing_expected: number;
  finalized_missing_submitted: number;
  finalized_through: number;
  future_outside_window: number;
  late_after_window: number;
  unique_retired: number;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "StressStabilityReport".
 */
export interface StressStabilityReport {
  baseline_rss_bytes?: number | null;
  latest_rss_bytes?: number | null;
  limit_exceeded: boolean;
  max_growth_bytes: number;
  samples: number;
  status: string;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "StressLifecycleRecord".
 */
export interface StressLifecycleRecord {
  attempted: number;
  elapsed_ns: number;
  failed: number;
  lifecycle: StressLifecycle;
  revision: number;
  skipped: number;
  state: string;
  submitted: number;
  unknown: number;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "StressProvenance".
 */
export interface StressProvenance {
  architecture: string;
  channels: StressChannelSource[];
  executable: StressSourceFile;
  os: string;
  package_version: string;
  scope: string;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "StressChannelSource".
 */
export interface StressChannelSource {
  backend: string;
  channel: string;
  device_id: string;
  firmware: string;
  libraries: StressSourceFile[];
  simulated: boolean;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "StressSourceFile".
 */
export interface StressSourceFile {
  bytes?: number | null;
  name: string;
  sha256?: string | null;
  status: string;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "StressRecovery".
 */
export interface StressRecovery {
  audit_records: number;
  counters_are_lower_bounds: boolean;
  damaged_files: number;
  files: number;
  records: number;
  sample_coverage: string;
  sample_receivers: StressReceiverReport[];
  segments: StressSegmentRecovery[];
  segments_omitted: number;
  status: string;
  submission_coverage?: string;
  valid_prefix_bytes: number;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "StressSegmentRecovery".
 */
export interface StressSegmentRecovery {
  bytes: number;
  error?: string | null;
  name: string;
  records: number;
  valid_prefix_bytes: number;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "StressRttReport".
 */
export interface StressRttReport {
  cancelled: number;
  duplicate_responses: number;
  invalid_responses: number;
  late_or_unmatched: number;
  latency: StressLatencyHistogram;
  responder_failed: number;
  responder_submitted: number;
  responder_unknown: number;
  responder_unknown_sequence?: number | null;
  timed_out: number;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "StressStageReport".
 */
export interface StressStageReport {
  count: number;
  first: number;
  fps: number;
  name: string;
  received_unique: number[];
  skipped: number;
  start_ns: number;
  submitted: number;
  warmup: boolean;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "StressSubmissionEvidence".
 */
export interface StressSubmissionEvidence {
  covered: number;
  failed: number;
  skipped: number;
  submitted: number;
  unknown: number;
  unknown_sequence?: number | null;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "StressTimingReport".
 */
export interface StressTimingReport {
  abandoned: number;
  error?: string | null;
  evicted: number;
  finalized: boolean;
  pending: number;
  receivers: StressReceiverTiming[];
  request: StressCallTiming;
  response: StressCallTiming;
  scope: string;
  window_high_water: number;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "StressReceiverTiming".
 */
export interface StressReceiverTiming {
  channel: string;
  latency: StressSignedLatency;
  matched: number;
  missing_sampled_submitted: number;
  sampled_rx: number;
  unmatched_rx: number;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "StressSignedLatency".
 */
export interface StressSignedLatency {
  max_ns?: number | null;
  min_ns?: number | null;
  negative_magnitude: StressLatencyHistogram;
  nonnegative: StressLatencyHistogram;
  p50_upper_ns?: number | null;
  p95_upper_ns?: number | null;
  p99_upper_ns?: number | null;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "StressCallTiming".
 */
export interface StressCallTiming {
  completion_wait: StressLatencyHistogram;
  dispatch_wait: StressLatencyHistogram;
  failed: number;
  preparation: StressLatencyHistogram;
  samples: number;
  submit_call: StressLatencyHistogram;
  submitted: number;
  total: StressLatencyHistogram;
  unknown: number;
  worker_queue: StressLatencyHistogram;
  worker_queue_unknown: number;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "StressEvents".
 */
export interface StressEvents {
  events: StressLifecycleRecord[];
  gap: boolean;
  latest_revision: number;
  run_id: string;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "StressRuns".
 */
export interface StressRuns {
  recovery_issues?: StressRecoveryIssue[];
  runs: StressRunSummary[];
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "StressRecoveryIssue".
 */
export interface StressRecoveryIssue {
  detail: string;
  id: string;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "StressRunSummary".
 */
export interface StressRunSummary {
  id: string;
  planned: number;
  simulated: boolean;
  state: string;
  submitted: number;
  verdict: string;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "StressValidation".
 */
export interface StressValidation {
  group_budget?: StressGroupBudget | null;
  load_budget?: StressLoadBudget | null;
  max_active_runs: number;
  max_fps: number;
  measurement: string;
  memory_limit_bytes: number;
  plan: StressPlan;
  plan_hash: string;
  scheduling_quantum_ns: number;
  /**
   * Estimated retained tracking state; includes optional telemetry reserves. Execution queue reserve is separate.
   */
  tracking_bytes: number;
  valid: boolean;
  version: number;
}
/**
 * This interface was referenced by `Contract`'s JSON-Schema
 * via the `definition` "StressLoadBudget".
 */
export interface StressLoadBudget {
  estimated_evidence_bytes: number;
  maximum_quantum_frames: number;
  reserved_duration_ns: number;
  reserved_peak_fps: number;
  reserved_planned: number;
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
 * via the `definition` "UiPreferences".
 */
export interface UiPreferences {
  locale: UiLocale;
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
