// SPDX-License-Identifier: AGPL-3.0-or-later
import { useTranslation } from "./i18n";
import { errorDetail } from "./i18n/errors";
import { LocalizedError, text } from "./i18n/messages";
import { recoveryAdvice, type RecoveryContext } from "./recovery";
export function ErrorRecovery({
  error,
  context = "general",
  busy,
  onCheck,
  onDismiss,
}: {
  error: unknown;
  context?: RecoveryContext;
  busy: boolean;
  onCheck: () => void;
  onDismiss: () => void;
}) {
  useTranslation();
  const advice = recoveryAdvice(error, context);
  return (
    <section
      role="alert"
      className="alert"
      aria-label={text("errorRecovery:operation_recovery")}
      style={{ position: "sticky", top: 8, zIndex: 5 }}
    >
      <strong>{advice.message}</strong>
      <p>{advice.next}</p>
      {!(error instanceof LocalizedError) && (
        <details>
          <summary>{text("errors:technical")}</summary>
          <pre>{errorDetail(error)}</pre>
        </details>
      )}
      {advice.check && (
        <button disabled={busy} onClick={onCheck}>
          {context === "recording"
            ? text("errorRecovery:refresh_recordings")
            : context === "stress"
              ? text("errorRecovery:refresh_stress_status")
              : text("errorRecovery:check_connection_and_session_status")}
        </button>
      )}
      <button onClick={onDismiss}>{text("errorRecovery:dismiss_error")}</button>
    </section>
  );
}
