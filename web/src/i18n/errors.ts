// SPDX-License-Identifier: AGPL-3.0-or-later
import { ApiFailure } from "../api";
import i18n from "./index";
import { LocalizedError, text, type MessageKey } from "./messages";
const codes: Record<string, MessageKey> = {
  unauthorized: "errors:unauthorized",
  forbidden: "errors:forbidden",
  lease_expired: "errors:lease_expired",
  lease_conflict: "errors:lease_conflict",
  device_busy: "errors:device_busy",
  device_disconnected: "errors:device_disconnected",
  bus_off: "errors:bus_off",
  receive_overflow: "errors:receive_overflow",
  transmit_failed: "errors:transmit_failed",
  stop_failed: "errors:stop_failed",
  invalid_state: "errors:invalid_state",
  invalid_request: "errors:invalid_request",
  invalid_frame: "errors:invalid_frame",
  unsupported: "errors:unsupported",
  resource_limit: "errors:resource_limit",
  queue_full: "errors:queue_full",
  not_found: "errors:not_found",
  channel_not_found: "errors:not_found",
  capture_not_found: "errors:not_found",
  service_closing: "errors:service_closing",
  revision_conflict: "errors:conflict",
  request_conflict: "errors:conflict",
  disk_full: "errors:disk_full",
  capture_io_error: "errors:storage",
  capture_worker_failed: "errors:storage",
  preferences_io_error: "errors:preferences",
};
export function errorDetail(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
export function errorSummary(error: unknown): string {
  if (error instanceof LocalizedError || i18n.resolvedLanguage !== "zh-CN")
    return errorDetail(error);
  if (error instanceof ApiFailure) {
    const key = error.detail?.code ? codes[error.detail.code] : undefined;
    if (key) return text(key);
    if (error.status === 401) return text("errors:unauthorized");
    if (error.status === 403) return text("errors:forbidden");
    if ([409, 412, 428].includes(error.status)) return text("errors:conflict");
    if ([400, 422].includes(error.status))
      return text("errors:invalid_request");
    if (error.status === 429) return text("errors:resource_limit");
    if (error.status >= 500) return text("errors:service");
  }
  if (
    error instanceof Error &&
    ["AbortError", "TimeoutError"].includes(error.name)
  )
    return text("errors:interrupted");
  if (error instanceof TypeError) return text("errors:network");
  return text("errors:unknown");
}
