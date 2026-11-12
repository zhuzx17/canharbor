// SPDX-License-Identifier: AGPL-3.0-or-later
import { LocalizedError } from "./i18n/messages";
// Service captures and native/CLI download readers share a 64 MiB bound.
export const MAX_SERVICE_CAPTURE_BYTES = 64 * 1024 * 1024;
export async function recordingFile(
  id: string,
  signal: AbortSignal,
): Promise<File> {
  const response = await fetch(
    `/api/v1/captures/${encodeURIComponent(id)}/download`,
    {
      credentials: "same-origin",
      redirect: "error",
      cache: "no-store",
      signal,
    },
  );
  if (!response.ok)
    throw new LocalizedError(
      "recording:could_not_read_recording_http_value_refresh_recordings_and",
      { status: response.status },
    );
  if (!response.body)
    throw new LocalizedError("recording:recording_response_has_no_file_body");
  const declared = response.headers.get("content-length");
  if (
    declared !== null &&
    (!/^\d+$/.test(declared) ||
      BigInt(declared) > BigInt(MAX_SERVICE_CAPTURE_BYTES))
  ) {
    await response.body.cancel();
    throw new LocalizedError(
      "recording:recording_exceeds_the_64_mib_service_download_limit_open",
    );
  }
  const reader = response.body.getReader();
  const chunks: Uint8Array<ArrayBuffer>[] = [];
  let total = 0;
  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      total += value.length;
      if (total > MAX_SERVICE_CAPTURE_BYTES)
        throw new LocalizedError(
          "recording:recording_exceeds_the_64_mib_service_download_limit",
        );
      chunks.push(new Uint8Array(value));
    }
  } finally {
    await reader.cancel().catch(() => {});
    reader.releaseLock();
  }
  if (declared !== null && BigInt(total) !== BigInt(declared))
    throw new LocalizedError(
      "recording:recording_download_was_interrupted_view_it_again_to_read",
    );
  return new File(chunks, `${id}.ndjson`, { type: "application/x-ndjson" });
}
