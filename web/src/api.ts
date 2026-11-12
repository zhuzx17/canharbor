// SPDX-License-Identifier: AGPL-3.0-or-later
import { decode, type ContractName, type Payload } from "./contract";
import type { Auth, ErrorDetail, FrameInput } from "./generated/api";
import { LocalizedError } from "./i18n/messages";
export type {
  Auth,
  BusEvent,
  Device,
  Frame,
  FrameInput,
  Session,
} from "./generated/api";
export class ApiFailure extends Error {
  readonly unknown: boolean;
  constructor(
    readonly status: number,
    readonly detail?: ErrorDetail,
    explicitUnknown?: boolean,
  ) {
    super(detail?.message ?? `HTTP ${status} from local service`);
    this.unknown =
      explicitUnknown === true ||
      (![
        400, 401, 403, 404, 405, 409, 412, 413, 414, 415, 422, 428, 429, 431,
      ].includes(status) &&
        !(detail && explicitUnknown === false));
  }
}
export async function boundedJson(response: Response): Promise<unknown> {
  if (!response.body)
    throw new LocalizedError("validation:missing_api_response_body");
  const reader = response.body.getReader();
  const chunks: Uint8Array[] = [];
  let size = 0;
  try {
    while (true) {
      const { value, done } = await reader.read();
      if (done) break;
      size += value.length;
      if (size > 1024 * 1024)
        throw new LocalizedError("validation:api_response_exceeds_1_mib");
      chunks.push(value);
    }
  } finally {
    await reader.cancel().catch(() => {});
    reader.releaseLock();
  }
  const bytes = new Uint8Array(size);
  let offset = 0;
  for (const chunk of chunks) {
    bytes.set(chunk, offset);
    offset += chunk.length;
  }
  return JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(bytes));
}
export async function api<K extends ContractName | "empty">(
  name: K,
  path: string,
  method = "GET",
  body?: unknown,
  auth?: Auth,
  generation?: string,
  revision?: string,
): Promise<
  K extends "empty" ? void : K extends ContractName ? Payload<K> : never
> {
  const headers: Record<string, string> = {
    "Content-Type": "application/json",
  };
  if (auth?.csrf_token) headers["X-CANHarbor-CSRF"] = auth.csrf_token;
  if (generation) headers["X-CANHarbor-Lease-Generation"] = generation;
  if (revision) headers["If-Match"] = JSON.stringify(revision);
  const response = await fetch(`/api/v1${path}`, {
    method,
    // Read-only polling may overlap a reload. Let the browser finish it rather
    // than turning document teardown into a WebKit access-control rejection.
    keepalive: method === "GET",
    signal: AbortSignal.timeout(10_000),
    redirect: "error",
    cache: "no-store",
    headers,
    credentials: "same-origin",
    body: method === "GET" ? undefined : JSON.stringify(body ?? {}),
  });
  type Result = K extends "empty"
    ? void
    : K extends ContractName
      ? Payload<K>
      : never;
  if (response.status === 204) {
    if (name !== "empty")
      throw new LocalizedError("validation:unexpected_empty_api_response");
    return undefined as Result;
  }
  let data: unknown;
  try {
    data = await boundedJson(response);
  } catch (error) {
    if (!response.ok) throw new ApiFailure(response.status);
    throw error;
  }
  if (!response.ok) {
    let detail: ErrorDetail | undefined;
    let explicitUnknown: boolean | undefined;
    if (
      data &&
      typeof data === "object" &&
      "error" in data &&
      data.error &&
      typeof data.error === "object" &&
      "outcome_unknown" in data.error &&
      typeof data.error.outcome_unknown === "boolean"
    )
      explicitUnknown = data.error.outcome_unknown;
    try {
      detail = decode("error", data).error;
    } catch {
      /* Preserve status for malformed errors. */
    }
    throw new ApiFailure(response.status, detail, explicitUnknown);
  }
  if (name === "empty")
    throw new LocalizedError("validation:unexpected_api_response_body");
  return decode(name as ContractName, data) as Result;
}
export function validateFrame(
  kind: FrameInput["kind"],
  idText: string,
  extended: boolean,
  dataText: string,
  brs: boolean,
  rtrDlc: number,
): FrameInput {
  if (!/^[0-9a-fA-F]+$/.test(idText))
    throw new LocalizedError(
      "validation:enter_a_hexadecimal_id_without_a_prefix",
    );
  const id = Number.parseInt(idText, 16);
  if (!Number.isSafeInteger(id) || id > (extended ? 0x1fffffff : 0x7ff))
    throw new LocalizedError("validation:id_exceeds_the_selected_width");
  const data_hex = dataText.replace(/\s/g, "").toUpperCase();
  if (!/^(?:[0-9A-F]{2})*$/.test(data_hex))
    throw new LocalizedError(
      "validation:data_must_contain_complete_hexadecimal_bytes",
    );
  const length = data_hex.length / 2;
  if (kind === "classic_remote") {
    if (
      data_hex ||
      brs ||
      !Number.isInteger(rtrDlc) ||
      rtrDlc < 0 ||
      rtrDlc > 8
    )
      throw new LocalizedError(
        "validation:rtr_requires_an_empty_payload_and_dlc_0_8",
      );
    return { kind, id, extended, brs: false, data_hex: "", dlc: rtrDlc };
  }
  if (kind === "classic_data" && (length > 8 || brs))
    throw new LocalizedError(
      "validation:classic_can_allows_up_to_8_bytes_and_no",
    );
  if (
    kind === "fd_data" &&
    ![0, 1, 2, 3, 4, 5, 6, 7, 8, 12, 16, 20, 24, 32, 48, 64].includes(length)
  )
    throw new LocalizedError(
      "validation:use_an_exact_can_fd_wire_length_up_to",
    );
  return { kind, id, extended, brs, data_hex };
}
