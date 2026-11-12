// SPDX-License-Identifier: AGPL-3.0-or-later
import { ApiFailure } from "./api";
import { errorSummary } from "./i18n/errors";
import { text } from "./i18n/messages";
export type RecoveryContext = "general" | "channel" | "recording" | "stress";
export function recoveryAdvice(
  error: unknown,
  context: RecoveryContext = "general",
) {
  const message = errorSummary(error);
  const status = error instanceof ApiFailure ? error.status : undefined;
  const code = error instanceof ApiFailure ? error.detail?.code : undefined;
  const subject =
    context === "recording"
      ? text("recovery:recordingSubject")
      : context === "stress"
        ? text("recovery:stressSubject")
        : text("recovery:channelSubject");
  if (status === 401)
    return {
      message,
      next: text("recovery:advice_0"),
      check: true,
    };
  if (status === 403)
    return {
      message,
      next: text("recovery:advice_1"),
      check: true,
    };
  if (code === "lease_expired")
    return {
      message,
      next: text("recovery:advice_2"),
      check: true,
    };
  if (code === "lease_conflict" || code === "device_busy")
    return {
      message,
      next: text("recovery:advice_3"),
      check: true,
    };
  if (status === 409 || status === 412 || status === 428)
    return {
      message,
      next: text("recovery:advice_4"),
      check: true,
    };
  if (status === 400 || status === 422)
    return {
      message,
      next: text("recovery:validation", { subject }),
      check: false,
    };
  if (status === 404)
    return {
      message,
      next: text("recovery:advice_5"),
      check: true,
    };
  if (status === 429)
    return {
      message,
      next: text("recovery:advice_6"),
      check: true,
    };
  if (
    error instanceof TypeError ||
    (error instanceof Error &&
      ["TimeoutError", "AbortError"].includes(error.name)) ||
    (status !== undefined && status >= 500)
  )
    return {
      message,
      next: text("recovery:advice_7"),
      check: true,
    };
  return {
    message,
    next: text("recovery:advice_8"),
    check: true,
  };
}
