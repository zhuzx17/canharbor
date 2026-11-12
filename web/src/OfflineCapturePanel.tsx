// SPDX-License-Identifier: AGPL-3.0-or-later
import { useEffect, useRef, useState } from "react";
import { useTranslation } from "./i18n";
import { errorSummary } from "./i18n/errors";
import { LocalizedError, text } from "./i18n/messages";
import {
  indexCapture,
  indexedCapturePage,
  MAX_LOCAL_CAPTURE_BYTES,
  type CaptureIndex,
  type OfflineCapture,
} from "./offline";
export function OfflineCapturePanel({
  recordingFile,
}: {
  recordingFile?: File | null;
}) {
  useTranslation();
  const [capture, setCapture] = useState<OfflineCapture | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const [idFilter, setIdFilter] = useState("");
  const [direction, setDirection] = useState("all");
  const [filename, setFilename] = useState("");
  const [file, setFile] = useState<File | null>(null);
  const [offset, setOffset] = useState(0);
  const [readingPage, setReadingPage] = useState(false);
  const [progress, setProgress] = useState(0);
  const [applied, setApplied] = useState({ id: "", direction: "all" });
  const [jumpPage, setJumpPage] = useState("1");
  const [timeUnit, setTimeUnit] = useState("seconds");
  const indexRef = useRef<CaptureIndex | null>(null);
  const visibleEvents = capture?.preview ?? [];
  const active = useRef<AbortController | null>(null);
  useEffect(() => () => active.current?.abort(), []);
  useEffect(() => {
    if (!recordingFile) return;
    indexRef.current = null;
    setCapture(null);
    setFile(recordingFile);
    setIdFilter("");
    setDirection("all");
    void scan(recordingFile);
  }, [recordingFile]);
  async function scan(
    file: File | undefined,
    nextOffset = 0,
    filters = { id: "", direction: "all" },
  ) {
    active.current?.abort();
    setFilename(file?.name ?? "");
    setProgress(0);
    setError("");
    if (!file) {
      setBusy(false);
      return;
    }
    if (file.size > MAX_LOCAL_CAPTURE_BYTES) {
      setError(new LocalizedError("offline:tooLargeLocal"));
      setBusy(false);
      return;
    }
    const controller = new AbortController();
    active.current = controller;
    setBusy(true);
    try {
      const typedFilters = {
        id: filters.id,
        direction: filters.direction as "all" | "rx" | "tx",
      };
      let index = indexRef.current;
      setReadingPage(
        !!index &&
          index.source === file &&
          JSON.stringify(index.filters) === JSON.stringify(typedFilters),
      );
      if (
        !index ||
        index.source !== file ||
        JSON.stringify(index.filters) !== JSON.stringify(typedFilters)
      ) {
        index = await indexCapture(
          file,
          controller.signal,
          typedFilters,
          (bytes) =>
            setProgress(Math.min(100, Math.floor((bytes / file.size) * 100))),
        );
        if (!controller.signal.aborted) indexRef.current = index;
      }
      const preview =
        nextOffset === 0
          ? index.summary.preview
          : await indexedCapturePage(index, nextOffset, controller.signal);
      const result = { ...index.summary, preview };
      if (!controller.signal.aborted) {
        setCapture(result);
        setOffset(nextOffset);
        setJumpPage(String(Math.floor(nextOffset / 100) + 1));
        setApplied(filters);
      }
    } catch (e) {
      if (!controller.signal.aborted) setError(e);
    } finally {
      if (!controller.signal.aborted) setBusy(false);
    }
  }
  return (
    <section
      className="offline-panel"
      aria-label={text("offlinePanel:offline_capture_viewer")}
    >
      <h2>{text("offlinePanel:offline_capture")}</h2>
      <p>
        {text(
          "offlinePanel:inspect_a_local_ndjson_recording_the_file_stays_in",
        )}
      </p>
      <label>
        {text("offlinePanel:local_capture_file")}{" "}
        <input
          type="file"
          accept=".ndjson,.jsonl"
          onChange={(e) => {
            indexRef.current = null;
            setCapture(null);
            const selected = e.target.files?.[0];
            setFile(selected ?? null);
            setIdFilter("");
            setDirection("all");
            void scan(selected);
            e.target.value = "";
          }}
        />
      </label>
      {busy && (
        <p role="status">
          {readingPage
            ? text("offlinePanel:reading_indexed_page")
            : text("offlinePanel:validating_file_value", {
                progress: progress,
              })}
        </p>
      )}
      {busy && (
        <button
          onClick={() => {
            active.current?.abort();
            setBusy(false);
          }}
        >
          {text("offlinePanel:cancel_inspection")}
        </button>
      )}
      {!!error && <p role="alert">{errorSummary(error)}</p>}
      {capture && (
        <>
          <p>
            {text("offlinePanel:file")}
            {filename}
          </p>
          <p>
            {text(
              "offlinePanel:integrity_whole_file_validation_finished_page_navigation_does_not",
            )}
          </p>
          <p data-testid="offline-summary">
            {capture.complete
              ? text("offlinePanel:complete_recording")
              : text("offlinePanel:incomplete_recording")}{" "}
            · {capture.frames}
            {text("offlinePanel:frames")}
            {capture.diagnostics}
            {text("offlinePanel:diagnostics")} {capture.dropped}
            {text("offlinePanel:known_dropped")}
            {capture.unknownLoss
              ? text("offlinePanel:additional_loss_unknown")
              : ""}
            {capture.truncated
              ? text("offlinePanel:partial_final_record_excluded")
              : ""}
          </p>
          <p>
            {text("offlinePanel:showing")}
            {capture.preview.length}
            {text("offlinePanel:events_up_to_100_per_page_received_frames_and")}
          </p>
          <label>
            {text("offlinePanel:filter_id_hex_contains")}
            <input
              value={idFilter}
              onChange={(e) => setIdFilter(e.target.value)}
              spellCheck={false}
            />
          </label>
          <label>
            {text("offlinePanel:direction")}
            <select
              value={direction}
              onChange={(e) => setDirection(e.target.value)}
            >
              <option value="all">{text("offlinePanel:all_events")}</option>
              <option value="rx">RX</option>
              <option value="tx">TX</option>
            </select>
          </label>
          <button
            disabled={busy}
            onClick={() =>
              void scan(file ?? undefined, 0, { id: idFilter, direction })
            }
          >
            {text("offlinePanel:apply_filters")}
          </button>
          <button
            disabled={busy}
            onClick={() => {
              setIdFilter("");
              setDirection("all");
              void scan(file ?? undefined, 0);
            }}
          >
            {text("offlinePanel:reset_filters")}
          </button>
          <p>
            {capture.matched}
            {text(
              "offlinePanel:matching_events_across_the_whole_recording_page",
            )}{" "}
            {Math.floor(offset / 100) + 1}.
          </p>
          {capture.matched === "0" && (
            <p role="status">
              {text(
                "offlinePanel:no_events_match_these_filters_reset_filters_to_inspect",
              )}
            </p>
          )}
          <label>
            {text("offlinePanel:time_display")}
            <select
              value={timeUnit}
              onChange={(event) => setTimeUnit(event.target.value)}
            >
              <option value="seconds">
                {text("offlinePanel:monotonic_seconds")}
              </option>
              <option value="ns">
                {text("offlinePanel:raw_monotonic_nanoseconds")}
              </option>
            </select>
          </label>
          <div>
            <button
              disabled={busy || offset === 0}
              onClick={() =>
                void scan(file ?? undefined, Math.max(0, offset - 100), applied)
              }
            >
              {text("offlinePanel:previous_page")}
            </button>
            <button
              disabled={busy || BigInt(offset + 100) >= BigInt(capture.matched)}
              onClick={() =>
                void scan(file ?? undefined, offset + 100, applied)
              }
            >
              {text("offlinePanel:next_page")}
            </button>
          </div>
          <form
            onSubmit={(event) => {
              event.preventDefault();
              const page = Number(jumpPage);
              if (
                Number.isSafeInteger(page) &&
                page >= 1 &&
                page <= Math.max(1, Math.ceil(Number(capture.matched) / 100))
              )
                void scan(file ?? undefined, (page - 1) * 100, applied);
            }}
          >
            <label>
              {text("offlinePanel:jump_to_page")}
              <input
                type="number"
                min="1"
                max={Math.max(1, Math.ceil(Number(capture.matched) / 100))}
                value={jumpPage}
                onChange={(event) => setJumpPage(event.target.value)}
              />
            </label>
            <button
              disabled={
                busy ||
                !Number.isInteger(Number(jumpPage)) ||
                Number(jumpPage) < 1 ||
                Number(jumpPage) >
                  Math.max(1, Math.ceil(Number(capture.matched) / 100))
              }
            >
              {text("offlinePanel:go_to_page")}
            </button>
          </form>
          <div className="offline-table">
            <table>
              <thead>
                <tr>
                  <th>{text("offlinePanel:sequence")}</th>
                  <th>
                    {timeUnit === "ns"
                      ? text("offlinePanel:monotonic_time_ns")
                      : text("offlinePanel:monotonic_time_s")}
                  </th>
                  <th>{text("offlinePanel:direction")}</th>
                  <th>{text("offlinePanel:id_diagnostic")}</th>
                  <th>{text("offlinePanel:data")}</th>
                  <th>{text("offlinePanel:metadata")}</th>
                </tr>
              </thead>
              <tbody>
                {visibleEvents.map((event) => (
                  <tr key={event.sequence}>
                    <td>{event.sequence}</td>
                    <td
                      title={text("offlinePanel:raw_monotonic_time_value_ns", {
                        host_monotonic_ns: event.host_monotonic_ns,
                      })}
                    >
                      {timeUnit === "ns"
                        ? event.host_monotonic_ns
                        : text("offlinePanel:value_value", {
                            n: BigInt(event.host_monotonic_ns) / 1000000000n,
                            value: (
                              BigInt(event.host_monotonic_ns) % 1000000000n
                            )
                              .toString()
                              .padStart(9, "0"),
                          })}
                    </td>
                    <td>
                      {event.type === "frame_received"
                        ? event.direction.toUpperCase()
                        : text("offlinePanel:diagnostic")}
                    </td>
                    <td>
                      {event.type === "frame_received"
                        ? event.frame.id.toString(16).toUpperCase()
                        : text("offlinePanel:error_classes_value", {
                            error_classes: event.error_classes,
                          })}
                    </td>
                    <td>
                      {event.type === "frame_received"
                        ? event.frame.data_hex
                        : event.raw_data_hex}
                    </td>
                    <td>
                      {event.timestamp_quality}
                      {text("offlinePanel:epoch")}
                      {event.timestamp_epoch} {text("offlinePanel:raw_us")}
                      {event.device_raw_timestamp_us ??
                        text("offlinePanel:unknown")}
                      {text("offlinePanel:device_us")}
                      {event.device_timestamp_us ??
                        text("offlinePanel:unknown")}
                      {event.type === "frame_received"
                        ? text(
                            "offlinePanel:value_value_brs_value_dlc_value_raw_dlc_value",
                            {
                              kind: event.frame.kind,
                              value: event.frame.extended ? "EXT" : "STD",
                              brs: event.frame.brs,
                              dlc: event.frame.dlc,
                              value2: event.raw_dlc ?? "unknown",
                              esi: event.frame.esi,
                            },
                          )
                        : text("offlinePanel:overflow_value", {
                            overflow: event.overflow,
                          })}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </>
      )}
    </section>
  );
}
