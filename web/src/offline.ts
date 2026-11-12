// SPDX-License-Identifier: AGPL-3.0-or-later
import { decode } from "./contract";
import type { BusEvent } from "./generated/api";
import { LocalizedError } from "./i18n/messages";
const MAX_BYTES = 64 * 1024 * 1024;
export const MAX_LOCAL_CAPTURE_BYTES = 1024 * 1024 * 1024;
export interface CapturePageOptions {
  offset: number;
  id?: string;
  direction?: "all" | "rx" | "tx";
  onProgress?: (bytes: number) => void;
  onMatch?: (event: BusEvent, byteOffset: number, ordinal: number) => void;
}
const REASONS = [
  "queue_full",
  "source_gap",
  "size_limit",
  "io_error",
  "disk_full",
  "owner_dropped",
  "session_stopped",
  "invalid_event",
  "duration_limit",
];
export interface OfflineCapture {
  complete: boolean;
  truncated: boolean;
  events: string;
  frames: string;
  diagnostics: string;
  dropped: string;
  unknownLoss: boolean;
  preview: BusEvent[];
  matched: string;
}
function counter(value: unknown): bigint {
  return BigInt(decode("counter", value));
}
function reason(value: unknown): boolean {
  return typeof value === "string" && REASONS.includes(value);
}
function object(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value))
    throw new LocalizedError("offline:invalid_capture_record");
  return value as Record<string, unknown>;
}
/** Inspect local bytes only. Bounded lines/preview; no API, lease or device access. */
export async function inspectCapture(
  source: ReadableStream<Uint8Array>,
  signal?: AbortSignal,
  limit = 500,
  page?: CapturePageOptions,
): Promise<OfflineCapture> {
  if (!Number.isInteger(limit) || limit < 0 || limit > 500)
    throw new LocalizedError("offline:preview_limit_exceeds_500");
  if (page && (!Number.isSafeInteger(page.offset) || page.offset < 0))
    throw new LocalizedError("offline:invalid_page_offset");
  const id = page?.id?.trim().replace(/^0x/i, "").toUpperCase() ?? "";
  if (id && !/^[0-9A-F]+$/.test(id))
    throw new LocalizedError("offline:enter_a_hexadecimal_id_filter");
  let matched = 0;
  let consumedBytes = 0;
  let lineStart = 0;
  let header = false,
    footer = false,
    complete = false,
    gaps = false,
    unknownLoss = false;
  let events = 0n,
    frames = 0n,
    diagnostics = 0n,
    dropped = 0n;
  let previous: [bigint, bigint] | undefined;
  const preview: BusEvent[] = [];
  function consume(bytes: number[]) {
    if (footer)
      throw new LocalizedError("offline:content_after_capture_footer");
    const record = object(
      JSON.parse(
        new TextDecoder("utf-8", { fatal: true }).decode(new Uint8Array(bytes)),
      ),
    );
    switch (record.record) {
      case "header":
        if (header || record.schema_version !== 1)
          throw new LocalizedError("offline:capture_schema_or_record_order");
        for (const key of [
          "capture_id",
          "owner",
          "server_instance_id",
          "session_epoch",
          "created_unix_ms",
        ])
          if (typeof record[key] !== "string")
            throw new LocalizedError("offline:invalid_capture_header");
        if (!("config" in record))
          throw new LocalizedError("offline:missing_capture_configuration");
        counter(record.created_unix_ms);
        header = true;
        break;
      case "event": {
        if (!header) throw new LocalizedError("offline:missing_capture_header");
        const message = decode("stream_message", {
          type: "events",
          server_instance_id: "offline",
          session_epoch: "offline",
          events: [record.event],
        });
        if (message.type !== "events")
          throw new LocalizedError("offline:invalid_captured_event");
        const event = message.events[0];
        if (event.type === "frame_received") {
          const frame = event.frame;
          decode("frame_input", {
            kind: frame.kind,
            id: frame.id,
            extended: frame.extended,
            brs: frame.brs,
            data_hex: frame.data_hex,
            dlc: frame.dlc,
          });
          if (
            (event.raw_dlc != null &&
              (!Number.isInteger(event.raw_dlc) ||
                event.raw_dlc < 0 ||
                event.raw_dlc > 15 ||
                (frame.kind === "fd_data"
                  ? event.raw_dlc
                  : Math.min(event.raw_dlc, 8)) !== frame.dlc)) ||
            (frame.esi && frame.kind !== "fd_data")
          )
            throw new LocalizedError("offline:invalid_captured_frame_metadata");
          frames++;
        } else {
          if (
            event.raw_data_hex.length > 128 ||
            !/^(?:[0-9a-fA-F]{2})*$/.test(event.raw_data_hex)
          )
            throw new LocalizedError("offline:invalid_diagnostic_bytes");
          diagnostics++;
        }
        const sequence = counter(event.sequence),
          time = counter(event.host_monotonic_ns);
        if (previous && (sequence <= previous[0] || time < previous[1]))
          throw new LocalizedError(
            "offline:capture_sequence_or_time_moved_backwards",
          );
        previous = [sequence, time];
        events++;
        const matches =
          !page ||
          (event.type === "frame_received"
            ? (!page.direction ||
                page.direction === "all" ||
                event.direction === page.direction) &&
              event.frame.id.toString(16).toUpperCase().includes(id)
            : (!page.direction || page.direction === "all") && !id);
        if (matches) {
          page?.onMatch?.(event, lineStart, matched);
          if (limit) {
            if (page) {
              if (matched >= page.offset && preview.length < limit)
                preview.push(event);
            } else {
              if (preview.length === limit) preview.shift();
              preview.push(event);
            }
          }
          matched++;
        }
        break;
      }
      case "gap":
        if (!header || !reason(record.cause))
          throw new LocalizedError("offline:invalid_capture_gap");
        gaps = true;
        if (record.count == null) unknownLoss = true;
        else {
          dropped += counter(record.count);
          if (dropped > 18446744073709551615n)
            throw new LocalizedError("offline:gap_count_overflow");
        }
        break;
      case "footer": {
        if (
          !header ||
          typeof record.complete !== "boolean" ||
          (record.reason != null && !reason(record.reason))
        )
          throw new LocalizedError("offline:invalid_capture_footer");
        const count = counter(record.dropped);
        if (
          counter(record.events) !== events ||
          (unknownLoss ? count < dropped : count !== dropped)
        )
          throw new LocalizedError("offline:capture_footer_count_mismatch");
        if (record.complete && (record.reason != null || gaps || count > 0n))
          throw new LocalizedError("offline:inconsistent_capture_footer");
        dropped = count;
        complete = record.complete;
        footer = true;
        break;
      }
      default:
        throw new LocalizedError("offline:unsupported_capture_record");
    }
  }
  const reader = source.getReader();
  const cancel = () => {
    void reader.cancel().catch(() => {});
  };
  signal?.addEventListener("abort", cancel, { once: true });
  let bytes = 0,
    records = 0;
  let line: number[] = [];
  try {
    if (signal?.aborted)
      throw new LocalizedError("offline:inspection_cancelled");
    while (true) {
      if (signal?.aborted)
        throw new LocalizedError("offline:inspection_cancelled");
      const { value, done } = await reader.read();
      if (done) break;
      bytes += value.length;
      if (bytes > (page ? MAX_LOCAL_CAPTURE_BYTES : MAX_BYTES))
        throw new LocalizedError(
          page ? "offline:tooLargeLocal" : "offline:tooLargeService",
        );
      page?.onProgress?.(bytes);
      for (const byte of value) {
        if (footer)
          throw new LocalizedError("offline:content_after_capture_footer");
        consumedBytes++;
        line.push(byte);
        if (line.length > 8192)
          throw new LocalizedError("offline:capture_record_exceeds_8192_bytes");
        if (byte === 10) {
          consume(line);
          lineStart = consumedBytes;
          line = [];
          if (++records % 128 === 0) {
            await new Promise((resolve) => setTimeout(resolve, 0));
            if (signal?.aborted)
              throw new LocalizedError("offline:inspection_cancelled");
          }
        }
      }
    }
    if (signal?.aborted)
      throw new LocalizedError("offline:inspection_cancelled");
    if (!header) throw new LocalizedError("offline:missing_capture_header");
    return {
      complete: complete && !line.length,
      truncated: !!line.length,
      events: String(events),
      frames: String(frames),
      diagnostics: String(diagnostics),
      dropped: String(dropped),
      unknownLoss,
      preview,
      matched: String(matched),
    };
  } finally {
    signal?.removeEventListener("abort", cancel);
    await reader.cancel().catch(() => {});
    reader.releaseLock();
  }
}

export const MAX_CAPTURE_CHECKPOINTS = 256;
interface CaptureCheckpoint {
  byteOffset: number;
  ordinal: number;
  sequence: string;
}
export interface CaptureIndex {
  source: Blob;
  summary: OfflineCapture;
  filters: { id: string; direction: "all" | "rx" | "tx" };
  checkpoints: CaptureCheckpoint[];
  stride: number;
}
/** Build only after validating all records, including the final footer. */
export async function indexCapture(
  source: Blob,
  signal?: AbortSignal,
  filters: CaptureIndex["filters"] = { id: "", direction: "all" },
  onProgress?: (bytes: number) => void,
): Promise<CaptureIndex> {
  let stride = 16;
  let checkpoints: CaptureCheckpoint[] = [];
  const summary = await inspectCapture(source.stream(), signal, 100, {
    offset: 0,
    ...filters,
    onProgress,
    onMatch(event, byteOffset, ordinal) {
      if (ordinal % stride) return;
      if (checkpoints.length === MAX_CAPTURE_CHECKPOINTS) {
        stride *= 2;
        checkpoints = checkpoints.filter(
          (point) => point.ordinal % stride === 0,
        );
      }
      if (ordinal % stride === 0)
        checkpoints.push({ byteOffset, ordinal, sequence: event.sequence });
    },
  });
  return { source, summary, filters, checkpoints, stride };
}
/** Read a bounded page from a trusted, immutable Blob already validated by indexCapture. */
export async function indexedCapturePage(
  index: CaptureIndex,
  offset: number,
  signal?: AbortSignal,
  limit = 100,
): Promise<BusEvent[]> {
  if (
    !Number.isSafeInteger(offset) ||
    offset < 0 ||
    !Number.isInteger(limit) ||
    limit < 1 ||
    limit > 500
  )
    throw new LocalizedError("offline:invalid_page_bounds");
  if (offset >= Number(index.summary.matched)) return [];
  let point = index.checkpoints[0];
  for (const candidate of index.checkpoints) {
    if (candidate.ordinal > offset) break;
    point = candidate;
  }
  const result: BusEvent[] = [];
  const reader = index.source.slice(point.byteOffset).stream().getReader();
  const cancel = () => {
    void reader.cancel().catch(() => {});
  };
  signal?.addEventListener("abort", cancel, { once: true });
  let line: number[] = [],
    ordinal = point.ordinal,
    records = 0;
  try {
    while (result.length < limit) {
      if (signal?.aborted)
        throw new LocalizedError("offline:inspection_cancelled");
      const chunk = await reader.read();
      if (chunk.done) break;
      for (const byte of chunk.value) {
        line.push(byte);
        if (line.length > 8192)
          throw new LocalizedError("offline:capture_record_exceeds_8192_bytes");
        if (byte !== 10) continue;
        const record = object(
          JSON.parse(
            new TextDecoder("utf-8", { fatal: true }).decode(
              new Uint8Array(line),
            ),
          ),
        );
        line = [];
        if (record.record === "event") {
          const message = decode("stream_message", {
            type: "events",
            server_instance_id: "offline",
            session_epoch: "offline",
            events: [record.event],
          });
          if (message.type !== "events")
            throw new LocalizedError("offline:invalid_captured_event");
          const event = message.events[0];
          const id = index.filters.id.trim().replace(/^0x/i, "").toUpperCase();
          const matches =
            event.type === "frame_received"
              ? (index.filters.direction === "all" ||
                  index.filters.direction === event.direction) &&
                event.frame.id.toString(16).toUpperCase().includes(id)
              : index.filters.direction === "all" && !id;
          if (matches) {
            if (ordinal++ >= offset) result.push(event);
            if (result.length === limit) break;
          }
        }
        if (++records % 128 === 0) {
          await new Promise((resolve) => setTimeout(resolve, 0));
        }
      }
    }
    if (signal?.aborted)
      throw new LocalizedError("offline:inspection_cancelled");
    return result;
  } finally {
    signal?.removeEventListener("abort", cancel);
    await reader.cancel().catch(() => {});
    reader.releaseLock();
  }
}
