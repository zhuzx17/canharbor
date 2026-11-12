// SPDX-License-Identifier: AGPL-3.0-or-later
// Generated from Rust wire DTOs. Do not edit.
import type { Contract } from "./api";
interface Validator<T> {
  (value: unknown): value is T;
  errors?: readonly { instancePath?: string; message?: string }[] | null;
}
export const auth: Validator<Contract["auth"]>;
export const capabilities: Validator<Contract["capabilities"]>;
export const capture: Validator<Contract["capture"]>;
export const capture_rename: Validator<Contract["capture_rename"]>;
export const capture_request: Validator<Contract["capture_request"]>;
export const captures: Validator<Contract["captures"]>;
export const config: Validator<Contract["config"]>;
export const counter: Validator<Contract["counter"]>;
export const create_request: Validator<Contract["create_request"]>;
export const device_report: Validator<Contract["device_report"]>;
export const devices: Validator<Contract["devices"]>;
export const diagnostics: Validator<Contract["diagnostics"]>;
export const error: Validator<Contract["error"]>;
export const frame_input: Validator<Contract["frame_input"]>;
export const legal: Validator<Contract["legal"]>;
export const pair_link: Validator<Contract["pair_link"]>;
export const pair_request: Validator<Contract["pair_request"]>;
export const periodic_request: Validator<Contract["periodic_request"]>;
export const periodic_task: Validator<Contract["periodic_task"]>;
export const periodic_tasks: Validator<Contract["periodic_tasks"]>;
export const receipt_lookup: Validator<Contract["receipt_lookup"]>;
export const session: Validator<Contract["session"]>;
export const sessions: Validator<Contract["sessions"]>;
export const stream_message: Validator<Contract["stream_message"]>;
export const stress_artifacts: Validator<Contract["stress_artifacts"]>;
export const stress_coord_request: Validator<Contract["stress_coord_request"]>;
export const stress_coord_status: Validator<Contract["stress_coord_status"]>;
export const stress_events: Validator<Contract["stress_events"]>;
export const stress_metric: Validator<Contract["stress_metric"]>;
export const stress_plan: Validator<Contract["stress_plan"]>;
export const stress_report: Validator<Contract["stress_report"]>;
export const stress_request: Validator<Contract["stress_request"]>;
export const stress_runs: Validator<Contract["stress_runs"]>;
export const stress_validation: Validator<Contract["stress_validation"]>;
export const subscribe: Validator<Contract["subscribe"]>;
export const tx: Validator<Contract["tx"]>;
export const tx_receipt: Validator<Contract["tx_receipt"]>;
export const ui_preferences: Validator<Contract["ui_preferences"]>;
export const version: Validator<Contract["version"]>;
