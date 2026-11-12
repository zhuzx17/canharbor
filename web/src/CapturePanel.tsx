// SPDX-License-Identifier: AGPL-3.0-or-later
import { useEffect, useRef, useState } from "react";
import { api, ApiFailure } from "./api";
import { ErrorRecovery } from "./ErrorRecovery";
import type { Auth, Capture, Session } from "./generated/api";
import { useTranslation } from "./i18n";
import { formatDateTime } from "./i18n/format";
import { stateText, text } from "./i18n/messages";
import { recordingFile } from "./recording";
function readableBytes(value: string) {
  const bytes = BigInt(value);
  if (bytes < 1024n) return `${bytes} B`;
  if (bytes < 1048576n)
    return `${Number(bytes) / 1024 >= 10 ? (Number(bytes) / 1024).toFixed(0) : (Number(bytes) / 1024).toFixed(1)} KiB`;
  return `${(Number(bytes) / 1048576).toFixed(1)} MiB`;
}
export function CapturePanel({
  auth,
  session,
  onInspect,
}: {
  auth: Auth;
  session?: Session;
  onInspect: (file: File) => void;
}) {
  useTranslation();
  const [captures, setCaptures] = useState<Capture[]>([]);
  const [name, setName] = useState("");
  const [duration, setDuration] = useState(3600);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const [search, setSearch] = useState("");
  const [channelFilter, setChannelFilter] = useState("");
  const [editing, setEditing] = useState<{
    id: string;
    original: string;
    name: string;
    conflicted?: boolean;
    currentName?: string;
  } | null>(null);
  const pending = useRef<{
    session: string;
    duration: number;
    name: string;
    id: string;
  } | null>(null);
  function acceptCaptures(next: Capture[]) {
    setCaptures(next);
    setEditing((draft) => {
      if (!draft?.conflicted) return draft;
      const current = next.find((item) => item.id === draft.id);
      return { ...draft, currentName: current?.name };
    });
  }
  async function refresh() {
    const result = await api("captures", "/captures");
    acceptCaptures(result.captures);
  }
  useEffect(() => {
    let alive = true;
    const refresh = async () => {
      try {
        const result = await api("captures", "/captures");
        if (alive) acceptCaptures(result.captures);
      } catch (e) {
        if (alive) setError(e);
      }
    };
    void refresh();
    const timer = setInterval(() => void refresh(), 2000);
    return () => {
      alive = false;
      clearInterval(timer);
    };
  }, [auth.client_id]);
  async function act(action: () => Promise<unknown>) {
    setBusy(true);
    setError(null);
    try {
      await action();
      await refresh();
    } catch (e) {
      setError(e);
    } finally {
      setBusy(false);
    }
  }
  const validDuration =
    Number.isInteger(duration) && duration >= 1 && duration <= 86400;
  return (
    <section aria-label={text("capture:captures")} className="capture-panel">
      <h2>{text("capture:captures")}</h2>
      <p>
        {text(
          "capture:record_one_running_acquisition_recording_ends_when_acquisition_stops",
        )}
      </p>
      <p>
        {text("capture:service_recordings_and_downloads_are_limited_to_64_mib")}
      </p>
      {error != null && (
        <ErrorRecovery
          error={error}
          context="recording"
          busy={busy}
          onCheck={() => void act(refresh)}
          onDismiss={() => setError(null)}
        />
      )}
      <label>
        {text("capture:recording_name_optional")}
        <input
          value={name}
          maxLength={120}
          onChange={(e) => setName(e.target.value)}
          placeholder={text("capture:bench_run")}
        />
      </label>
      <label>
        {text("capture:recording_duration_seconds")}
        <input
          type="number"
          min="1"
          max="86400"
          value={duration}
          onChange={(e) => setDuration(Number(e.target.value))}
        />
      </label>
      <button
        disabled={busy || session?.state !== "running" || !validDuration}
        onClick={() =>
          void act(async () => {
            if (!session) return;
            if (
              pending.current?.session !== session.id ||
              pending.current?.duration !== duration ||
              pending.current?.name !== name.trim()
            )
              pending.current = {
                session: session.id,
                duration,
                name: name.trim(),
                id: crypto.randomUUID(),
              };
            await api(
              "capture",
              "/captures",
              "POST",
              {
                session_id: session.id,
                name: pending.current.name,
                client_request_id: pending.current.id,
                max_duration_seconds: duration,
              },
              auth,
            );
            pending.current = null;
          })
        }
      >
        {text("capture:start_recording")}
      </button>
      <p>
        {session
          ? text("capture:selected_source_value_session_value", {
              channel_id: session.channel_id,
              id: session.id,
            })
          : text("capture:select_a_session_in_the_connection_panel_first")}
        {session && session.state !== "running"
          ? text("capture:start_the_channel_before_recording")
          : ""}
      </p>
      {!validDuration && (
        <p role="alert">
          {text("capture:enter_a_duration_from_1_to_86400_seconds")}
        </p>
      )}
      {captures.length === 0 && <p>{text("capture:no_recordings")}</p>}
      <label>
        {text("capture:filter_recordings_by_name")}
        <input value={search} onChange={(e) => setSearch(e.target.value)} />
      </label>
      <label>
        {text("capture:filter_recordings_by_channel")}
        <select
          value={channelFilter}
          onChange={(e) => setChannelFilter(e.target.value)}
        >
          <option value="">{text("capture:all_channels")}</option>
          {[...new Set(captures.map((c) => c.channel_id ?? "unknown"))]
            .sort()
            .map((channel) => (
              <option key={channel} value={channel}>
                {channel === "unknown"
                  ? text("capture:unknown_channel")
                  : channel}
              </option>
            ))}
        </select>
      </label>
      <ul>
        {captures
          .filter(
            (c) =>
              (!channelFilter ||
                (c.channel_id ?? "unknown") === channelFilter) &&
              (c.name || text("capture:unnamed_recording"))
                .toLowerCase()
                .includes(search.trim().toLowerCase()),
          )
          .map((capture) => {
            const active =
              capture.state === "starting" || capture.state === "recording";
            return (
              <li key={capture.id} data-capture-id={capture.id}>
                <strong>
                  {capture.name || text("capture:unnamed_recording")}
                </strong>{" "}
                <code>{capture.id}</code> — {stateText(capture.state)};{" "}
                {capture.events} {text("capture:events")}
                {capture.dropped}
                {text("capture:known_drops")}
                {capture.loss_count_known
                  ? ""
                  : text("capture:total_loss_unknown")}
                ;{" "}
                <span
                  title={text("capture:value_bytes", { bytes: capture.bytes })}
                >
                  {readableBytes(capture.bytes)}
                </span>
                <p>
                  {text("capture:source_session")}
                  <code>{capture.session_id}</code>
                  {capture.channel_id
                    ? text("capture:value", { channel_id: capture.channel_id })
                    : ""}
                  {capture.created_unix_ms && (
                    <>
                      {" "}
                      {text("capture:created")}{" "}
                      {formatDateTime(
                        new Date(Number(capture.created_unix_ms)),
                      )}
                    </>
                  )}
                  {" · "}
                  {text("capture:duration_deadline")}{" "}
                  {formatDateTime(new Date(Number(capture.expires_unix_ms)))}
                </p>
                {capture.reason && <span> ({capture.reason})</span>}
                {editing?.id === capture.id ? (
                  <div>
                    <label>
                      {text("capture:new_recording_name")}
                      <input
                        value={editing.name}
                        maxLength={120}
                        onChange={(e) =>
                          setEditing({ ...editing, name: e.target.value })
                        }
                      />
                    </label>
                    <button
                      disabled={busy || editing.conflicted}
                      onClick={() =>
                        void act(async () => {
                          try {
                            await api(
                              "capture",
                              `/captures/${capture.id}`,
                              "PATCH",
                              {
                                name: editing.name.trim(),
                                expected_name: editing.original,
                              },
                              auth,
                            );
                            setEditing(null);
                          } catch (failure) {
                            if (
                              failure instanceof ApiFailure &&
                              failure.status === 409
                            ) {
                              setEditing((draft) =>
                                draft?.id === capture.id
                                  ? {
                                      ...draft,
                                      conflicted: true,
                                      currentName: undefined,
                                    }
                                  : draft,
                              );
                              // Read the current name without retrying the mutation.
                              await refresh();
                            }
                            throw failure;
                          }
                        })
                      }
                    >
                      {text("capture:save_recording_name")}
                    </button>
                    {editing.conflicted && (
                      <div aria-label={text("capture:rename_conflict")}>
                        <p>
                          {text(
                            "capture:your_draft_is_preserved_another_client_changed_this_recording",
                          )}
                        </p>
                        <p>
                          {text("capture:current_server_name")}{" "}
                          {editing.currentName === undefined
                            ? text(
                                "capture:unavailable_refresh_recordings_to_check_again",
                              )
                            : editing.currentName ||
                              text("capture:unnamed_recording")}
                        </p>
                        {editing.currentName !== undefined && (
                          <>
                            <button
                              disabled={busy}
                              onClick={() => {
                                setEditing({
                                  ...editing,
                                  name: editing.currentName!,
                                  original: editing.currentName!,
                                  conflicted: false,
                                });
                                setError(null);
                              }}
                            >
                              {text("capture:load_current_name")}
                            </button>
                            <button
                              disabled={busy}
                              onClick={() => {
                                setEditing({
                                  ...editing,
                                  original: editing.currentName!,
                                  conflicted: false,
                                });
                                setError(null);
                              }}
                            >
                              {text("capture:keep_draft_against_current_name")}
                            </button>
                            <p>
                              {text(
                                "capture:keeping_the_draft_prepares_a_new_comparison_save_recording",
                              )}
                            </p>
                          </>
                        )}
                      </div>
                    )}
                    <button disabled={busy} onClick={() => setEditing(null)}>
                      {text("capture:cancel_rename")}
                    </button>
                  </div>
                ) : (
                  <button
                    disabled={busy}
                    onClick={() =>
                      setEditing({
                        id: capture.id,
                        original: capture.name,
                        name: capture.name,
                      })
                    }
                  >
                    {text("capture:rename_recording")}
                  </button>
                )}
                {active ? (
                  <button
                    disabled={busy}
                    onClick={() =>
                      void act(() =>
                        api(
                          "capture",
                          `/captures/${capture.id}/stop`,
                          "POST",
                          {},
                          auth,
                        ),
                      )
                    }
                  >
                    {text("capture:stop_recording")}
                  </button>
                ) : (
                  <>
                    <button
                      disabled={busy}
                      onClick={() =>
                        void act(async () =>
                          onInspect(
                            await recordingFile(
                              capture.id,
                              AbortSignal.timeout(30_000),
                            ),
                          ),
                        )
                      }
                    >
                      {text("capture:analyze_recording")}
                    </button>
                    <a
                      href={`/api/v1/captures/${capture.id}/download`}
                      download
                    >
                      {text("capture:download_capture")}
                    </a>
                    <button
                      disabled={busy}
                      onClick={() => {
                        if (
                          !window.confirm(
                            text("capture:confirmDelete", {
                              name:
                                capture.name ||
                                text("capture:unnamed_recording"),
                              id: capture.id,
                            }),
                          )
                        )
                          return;
                        void act(() =>
                          api(
                            "empty",
                            `/captures/${capture.id}`,
                            "DELETE",
                            {},
                            auth,
                          ),
                        );
                      }}
                    >
                      {text("capture:delete_recording")}
                    </button>
                  </>
                )}
              </li>
            );
          })}
      </ul>
    </section>
  );
}
