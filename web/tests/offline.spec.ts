import { nativeBinary, nativeRuntime } from "./native";
// SPDX-License-Identifier: AGPL-3.0-or-later
import { expect, test } from "@playwright/test";
import fixtures from "../../api/capture-fixtures.json" with { type: "json" };
import {
  inspectCapture,
  indexCapture,
  indexedCapturePage,
  MAX_CAPTURE_CHECKPOINTS,
} from "../src/offline";
import { execFileSync } from "node:child_process";
import { resolve } from "node:path";
function data(records: unknown[]): string {
  return records.map((record) => JSON.stringify(record) + "\n").join("");
}
function tiny(value: string): ReadableStream<Uint8Array> {
  const bytes = new TextEncoder().encode(value);
  let offset = 0;
  return new ReadableStream({
    pull(controller) {
      if (offset === bytes.length) controller.close();
      else {
        controller.enqueue(bytes.slice(offset, offset + 3));
        offset = Math.min(offset + 3, bytes.length);
      }
    },
  });
}
test("browser and Rust inspect the same complete and invalid capture corpus", async () => {
  for (const entry of fixtures.valid) {
    const result = await inspectCapture(tiny(data(entry.records)));
    expect(result.frames, entry.name).toBe(entry.frames);
    expect(result.diagnostics).toBe(entry.diagnostics);
    expect(result.complete).toBe(entry.complete);
    expect(result.preview[0]).toEqual(entry.records[1].event);
  }
  for (const entry of fixtures.invalid)
    await expect(
      inspectCapture(tiny(data(entry.records))),
      entry.name,
    ).rejects.toThrow();
});
test("offline preview and records are bounded and inspection can be cancelled", async () => {
  const base = fixtures.valid[0].records;
  const records: unknown[] = [base[0]];
  for (let i = 1; i <= 1000; i++)
    records.push({
      ...base[1],
      event: {
        ...base[1].event!,
        sequence: String(i),
        host_monotonic_ns: String(i),
      },
    });
  const result = await inspectCapture(
    new Blob([data(records) + '{"record":"event"']).stream(),
    undefined,
    5,
  );
  expect(result.events).toBe("1000");
  expect(result.preview).toHaveLength(5);
  expect(result.preview[0].sequence).toBe("996");
  expect(result.truncated).toBe(true);
  expect(result.complete).toBe(false);
  await expect(
    inspectCapture(new Blob(["x".repeat(8193)]).stream()),
  ).rejects.toThrow("8192");
  const cancelled = new AbortController();
  cancelled.abort();
  await expect(
    inspectCapture(new Blob([data(base)]).stream(), cancelled.signal),
  ).rejects.toThrow("cancelled");
});
test("offline pages search the whole recording and retain bounded results", async () => {
  const base = fixtures.valid[0].records;
  const records: unknown[] = [base[0]];
  for (let i = 1; i <= 1200; i++) {
    const original = base[1].event!;
    if (!("frame" in original)) throw new Error("missing frame");
    records.push({
      record: "event",
      event: {
        ...original,
        sequence: String(i),
        host_monotonic_ns: String(i),
        frame: { ...original.frame, id: i <= 200 ? 0x123 : 0x456 },
      },
    });
  }
  records.push({
    record: "footer",
    complete: true,
    events: "1200",
    dropped: "0",
    reason: null,
  });
  const text = data(records);
  const progress: number[] = [];
  const first = await inspectCapture(
    new Blob([text]).stream(),
    undefined,
    100,
    {
      offset: 0,
      id: "123",
      direction: "all",
      onProgress: (n) => progress.push(n),
    },
  );
  expect(first.matched).toBe("200");
  expect(first.events).toBe("1200");
  expect(first.preview).toHaveLength(100);
  expect(first.preview[0].sequence).toBe("1");
  expect(first.complete).toBe(true);
  expect(progress.at(-1)).toBe(new TextEncoder().encode(text).length);
  const second = await inspectCapture(
    new Blob([text]).stream(),
    undefined,
    100,
    { offset: 100, id: "123" },
  );
  expect(second.preview[0].sequence).toBe("101");
  expect(second.preview.at(-1)?.sequence).toBe("200");
  const cancelled = new AbortController();
  await expect(
    inspectCapture(new Blob([text]).stream(), cancelled.signal, 100, {
      offset: 0,
      onProgress: () => cancelled.abort(),
    }),
  ).rejects.toThrow("cancelled");
  await expect(
    inspectCapture(new Blob([text]).stream(), undefined, 100, { offset: -1 }),
  ).rejects.toThrow("offset");
});
test("sparse index is bounded and pages cannot bypass late corruption", async () => {
  const base = fixtures.valid[0].records;
  const original = base[1].event!;
  const lines = [JSON.stringify(base[0]) + "\n"];
  for (let i = 1; i <= 4200; i++)
    lines.push(
      JSON.stringify({
        record: "event",
        event: {
          ...original,
          sequence: String(i),
          host_monotonic_ns: String(i),
        },
      }) + "\n",
    );
  const footer =
    JSON.stringify({
      record: "footer",
      complete: true,
      events: "4200",
      dropped: "0",
    }) + "\n";
  const slices: number[] = [];
  class TrackedBlob extends Blob {
    override slice(start = 0, end?: number, contentType?: string) {
      slices.push(start);
      return super.slice(start, end, contentType);
    }
  }
  const blob = new TrackedBlob([...lines, footer]);
  const index = await indexCapture(blob);
  expect(index.checkpoints.length).toBeLessThanOrEqual(MAX_CAPTURE_CHECKPOINTS);
  expect(index.stride).toBeGreaterThan(16);
  expect(index.summary.complete).toBe(true);
  const page = await indexedCapturePage(index, 4100);
  expect(page).toHaveLength(100);
  expect(slices[0]).toBeGreaterThan(blob.size / 2);
  expect(page[0].sequence).toBe("4101");
  expect(page.at(-1)?.sequence).toBe("4200");
  expect(index.summary.events).toBe("4200");
  await expect(
    indexCapture(new Blob([...lines, '{"record":"invalid"}\n', footer])),
  ).rejects.toThrow();
  await expect(
    indexCapture(new Blob([...lines, footer, "invalid\n"])),
  ).rejects.toThrow();
  const cancelled = new AbortController();
  cancelled.abort();
  await expect(
    indexedCapturePage(index, 100, cancelled.signal),
  ).rejects.toThrow("cancelled");
});
test("local file viewing retains metadata and never posts to the API", async ({
  page,
}) => {
  const binary = nativeBinary;
  const runtime = nativeRuntime;
  const url = execFileSync(
    binary,
    ["--runtime-dir", runtime, "web", "open", "--print"],
    { encoding: "utf8" },
  ).trim();
  await page.goto(url);
  await expect(page.getByText("Browser paired", { exact: true })).toBeVisible();
  const mutations: string[] = [];
  page.on("request", (request) => {
    if (request.url().includes("/api/v1/") && request.method() !== "GET")
      mutations.push(request.url());
  });
  await page
    .getByRole("button", { name: "Offline analysis", exact: true })
    .click();
  const viewer = page.getByRole("region", { name: "Offline capture viewer" });
  await viewer.getByLabel("Local capture file").setInputFiles({
    name: "fixture.ndjson",
    mimeType: "application/x-ndjson",
    buffer: Buffer.from(data(fixtures.valid[0].records)),
  });
  await expect(viewer.getByTestId("offline-summary")).toContainText(
    "Complete recording · 1 frames",
  );
  await expect(
    viewer.getByRole("cell", { name: "9007199254740993", exact: true }),
  ).toBeVisible();
  await viewer
    .getByRole("combobox", { name: "Time display" })
    .selectOption("ns");
  await expect(
    viewer.getByRole("cell", { name: "18446744073709551615", exact: true }),
  ).toBeVisible();
  await expect(viewer.getByRole("cell", { name: /raw DLC 15/ })).toBeVisible();
  await expect(
    viewer.getByRole("cell", { name: /classic_data · STD · BRS false/ }),
  ).toBeVisible();
  const extended = structuredClone(fixtures.valid[0].records);
  const extendedEvent = extended[1].event;
  if (!extendedEvent || !("frame" in extendedEvent))
    throw new Error("missing frame fixture");
  extendedEvent.frame.extended = true;
  await viewer.getByLabel("Local capture file").setInputFiles({
    name: "extended.ndjson",
    mimeType: "application/x-ndjson",
    buffer: Buffer.from(data(extended)),
  });
  await expect(
    viewer.getByRole("cell", { name: /classic_data · EXT · BRS false/ }),
  ).toBeVisible();
  await viewer.getByLabel("Local capture file").setInputFiles({
    name: "fd.ndjson",
    mimeType: "application/x-ndjson",
    buffer: Buffer.from(data(fixtures.valid[3].records)),
  });
  await expect(
    viewer.getByRole("cell", { name: /fd_data · STD · BRS true/ }),
  ).toBeVisible();
  await expect(viewer.getByRole("cell", { name: /ESI true/ })).toBeVisible();
  const many: unknown[] = [fixtures.valid[0].records[0]];
  const original = fixtures.valid[0].records[1].event!;
  if (!("frame" in original)) throw new Error("missing frame");
  for (let i = 1; i <= 250; i++)
    many.push({
      record: "event",
      event: {
        ...original,
        sequence: String(i),
        host_monotonic_ns: String(i),
        frame: { ...original.frame, id: i <= 10 ? 0x123 : 0x456 },
      },
    });
  many.push({ record: "footer", complete: true, events: "250", dropped: "0" });
  await viewer.getByLabel("Local capture file").setInputFiles({
    name: "paged.ndjson",
    mimeType: "application/x-ndjson",
    buffer: Buffer.from(data(many)),
  });
  await expect(
    viewer.getByText("250 matching events across the whole recording. Page 1."),
  ).toBeVisible();
  await viewer.getByRole("button", { name: "Next page", exact: true }).click();
  await expect(
    viewer.getByText("250 matching events across the whole recording. Page 2."),
  ).toBeVisible();
  await viewer.getByLabel("Filter ID (hex, contains)").fill("123");
  await viewer
    .getByRole("button", { name: "Apply filters", exact: true })
    .click();
  await expect(
    viewer.getByText("10 matching events across the whole recording. Page 1."),
  ).toBeVisible();
  await expect(
    viewer.getByRole("button", { name: "Next page", exact: true }),
  ).toBeDisabled();
  await viewer
    .getByRole("button", { name: "Reset filters", exact: true })
    .click();
  await expect(
    viewer.getByText("250 matching events across the whole recording. Page 1."),
  ).toBeVisible();
  await viewer.getByLabel("Jump to page").fill("3");
  await viewer.getByRole("button", { name: "Go to page", exact: true }).click();
  await expect(
    viewer.getByText("250 matching events across the whole recording. Page 3."),
  ).toBeVisible();
  await viewer.getByLabel("Local capture file").setInputFiles({
    name: "bad.ndjson",
    mimeType: "application/x-ndjson",
    buffer: Buffer.from(data(fixtures.invalid[0].records)),
  });
  await expect(viewer.getByRole("alert")).toBeVisible();
  await expect(viewer.getByTestId("offline-summary")).toHaveCount(0);
  expect(mutations).toEqual([]);
  await page.getByRole("button", { name: "Live traffic", exact: true }).click();
  await expect(
    page.getByRole("button", { name: "Send once", exact: false }),
  ).toBeDisabled();
});
