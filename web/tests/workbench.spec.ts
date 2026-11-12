import { nativeBinary, nativeRuntime } from "./native";
// SPDX-License-Identifier: AGPL-3.0-or-later
import { test, expect } from "@playwright/test";
import { execFileSync, spawn } from "node:child_process";
import { resolve, join } from "node:path";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { validateFrame } from "../src/api";
const binary = nativeBinary;
const runtime = nativeRuntime;
const cli = (...args: string[]) =>
  execFileSync(binary, ["--runtime-dir", runtime, ...args], {
    encoding: "utf8",
  }).trim();
test("frame inputs preserve exact ID and CAN FD wire constraints", () => {
  expect(() => validateFrame("fd_data", "800", false, "", false, 0)).toThrow();
  expect(() =>
    validateFrame("fd_data", "123", false, "00".repeat(9), false, 0),
  ).toThrow();
  expect(
    validateFrame("fd_data", "1FFFFFFF", true, "00".repeat(12), true, 0).id,
  ).toBe(0x1fffffff);
});
test("browser receives CLI traffic and CLI receives browser traffic", async ({
  page,
  browser,
}) => {
  const pageErrors: string[] = [];
  page.on("pageerror", (error) => pageErrors.push(error.message));
  const pairing = cli("web", "open", "--print");
  await page.goto(pairing);
  await expect(page.getByText("Browser paired", { exact: true })).toBeVisible();
  await expect(page).toHaveURL("http://127.0.0.1:32189/");
  await page
    .getByRole("combobox", { name: "Channel", exact: true })
    .selectOption("virtual:1");
  await page.getByRole("button", { name: "Open channel", exact: true }).click();
  await page
    .getByRole("button", { name: "Acquire control", exact: true })
    .click();
  await page
    .getByRole("button", {
      name: /Apply settings to/,
      exact: true,
    })
    .click();
  await expect(
    page.getByRole("status", { name: "Operation result" }),
  ).toContainText("Configuration updated");
  await page
    .getByRole("button", { name: "Start channel", exact: true })
    .click();
  await expect(page.getByText("running", { exact: true })).toBeVisible();
  await expect(page.getByText("Live", { exact: true })).toBeVisible();
  await page.getByRole("button", { name: "Recordings", exact: true }).click();
  await page
    .getByRole("button", { name: "Start recording", exact: true })
    .click();
  await expect(
    page.getByRole("button", { name: "Stop recording", exact: true }),
  ).toBeVisible();
  const selectedSession = await page
    .getByRole("combobox", { name: "Session", exact: true })
    .inputValue();
  const nativeCapture = JSON.parse(
    cli(
      "capture",
      "start",
      "--session",
      selectedSession,
      "--request-id",
      "e2e-capture",
      "--duration",
      "60",
    ),
  );
  expect(
    JSON.parse(
      cli(
        "capture",
        "start",
        "--session",
        selectedSession,
        "--request-id",
        "e2e-capture",
      ),
    ).id,
  ).toBe(nativeCapture.id);
  expect(
    cli(
      "send",
      "--channel",
      "virtual:0",
      "--id",
      "321",
      "--data",
      "AABB",
      "--active",
    ),
  ).toContain("virtual_bus_accepted");
  await page.getByRole("button", { name: "Live traffic", exact: true }).click();
  await expect(
    page.getByRole("cell", { name: "321", exact: true }),
  ).toBeVisible();
  await expect(
    page.getByRole("cell", { name: "AA BB", exact: true }),
  ).toBeVisible();
  await page.getByLabel("Filter ID (hex)").fill("777");
  await expect(
    page.getByRole("cell", { name: "321", exact: true }),
  ).toHaveCount(0);
  await page.getByLabel("Filter ID (hex)").fill("");
  await page.getByRole("button", { name: "Recordings", exact: true }).click();
  await page
    .getByRole("button", { name: "Stop recording", exact: true })
    .click();
  const downloaded = page.waitForEvent("download");
  await page
    .getByRole("link", { name: "Download capture", exact: true })
    .click();
  const download = await downloaded;
  const browserRecords = readFileSync((await download.path())!, "utf8")
    .trim()
    .split("\n")
    .map((line) => JSON.parse(line));
  expect(browserRecords.at(-1).complete).toBe(true);
  expect(
    browserRecords.some(
      (record) => record.record === "event" && record.event.frame?.id === 0x321,
    ),
  ).toBe(true);
  page.once("dialog", (dialog) => dialog.accept());
  await page
    .getByRole("button", { name: "Delete recording", exact: true })
    .click();
  await expect(
    page.getByRole("link", { name: "Download capture", exact: true }),
  ).toHaveCount(0);
  await page.getByRole("button", { name: "Live traffic", exact: true }).click();
  expect(JSON.parse(cli("capture", "stop", nativeCapture.id)).complete).toBe(
    true,
  );
  expect(JSON.parse(cli("capture", "status", nativeCapture.id)).events).toBe(
    "1",
  );
  expect(
    JSON.parse(cli("capture", "list")).captures.some(
      (capture: { id: string }) => capture.id === nativeCapture.id,
    ),
  ).toBe(true);
  const directory = mkdtempSync(join(tmpdir(), "canharbor-download-"));
  try {
    const output = join(directory, "capture.ndjson");
    cli("capture", "download", nativeCapture.id, "--output", output);
    const original = readFileSync(output, "utf8");
    expect(JSON.parse(original.trim().split("\n").at(-1)!).complete).toBe(true);
    expect(() =>
      cli("capture", "download", nativeCapture.id, "--output", output),
    ).toThrow();
    expect(readFileSync(output, "utf8")).toBe(original);
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
  cli("capture", "delete", nativeCapture.id);
  await page
    .getByRole("button", { name: "Pause display", exact: true })
    .click();
  cli(
    "send",
    "--channel",
    "virtual:0",
    "--id",
    "322",
    "--data",
    "CC",
    "--active",
  );
  await expect(
    page.getByRole("cell", { name: "322", exact: true }),
  ).toHaveCount(0);
  await page
    .getByRole("button", { name: "Resume display", exact: true })
    .click();
  await page.getByRole("button", { name: "Close", exact: true }).click();

  const monitor = spawn(binary, [
    "--runtime-dir",
    runtime,
    "monitor",
    "--channel",
    "virtual:1",
    "--listen-only",
    "--count",
    "1",
  ]);
  let output = "";
  let stderr = "";
  monitor.stdout.on("data", (chunk) => (output += chunk));
  monitor.stderr.on("data", (chunk) => (stderr += chunk));
  const exited = new Promise<number | null>((resolve) =>
    monitor.on("exit", resolve),
  );
  try {
    await expect
      .poll(() =>
        JSON.parse(cli("session", "list")).sessions.some(
          (s: { channel_id: string; state: string }) =>
            s.channel_id === "virtual:1" && s.state === "running",
        ),
      )
      .toBe(true);
    await page
      .getByRole("combobox", { name: "Channel", exact: true })
      .selectOption("virtual:0");
    await page
      .getByRole("combobox", { name: "Mode", exact: true })
      .selectOption("active");
    await page
      .getByRole("button", { name: "Open channel", exact: true })
      .click();
    await page
      .getByRole("button", { name: "Acquire control", exact: true })
      .click();
    await page
      .getByRole("button", { name: "Start channel", exact: true })
      .click();
    await expect(page.getByText("Live", { exact: true })).toBeVisible();
    await page.getByLabel("Identifier").fill("456");
    await page
      .getByRole("textbox", { name: "Payload HEX BYTES", exact: true })
      .fill("DE AD BE EF");
    await page.getByRole("button", { name: "Send once", exact: false }).click();
    await expect(
      page.getByRole("status", { name: "Operation result" }),
    ).toContainText("Submitted to virtual bus");
    await expect.poll(() => output).toContain("DEADBEEF");
    expect(await exited, stderr).toBe(0);
    const received = JSON.parse(output.trim());
    expect(received.frame.id).toBe(0x456);
    expect(received.direction).toBe("rx");
  } finally {
    if (monitor.exitCode === null) monitor.kill("SIGINT");
  }
  // A second authenticated client may observe but cannot acquire the first client's lease.
  const context = await browser.newContext();
  const observer = await context.newPage();
  await observer.goto(cli("web", "open", "--print"));
  await expect(
    observer.getByText("Browser paired", { exact: true }),
  ).toBeVisible();
  await observer
    .getByRole("combobox", { name: "Session", exact: true })
    .selectOption(
      await page
        .getByRole("combobox", { name: "Session", exact: true })
        .inputValue(),
    );
  await expect(
    observer.getByRole("button", { name: "Acquire control", exact: true }),
  ).toBeDisabled();
  await context.close();
  await page.evaluate(() => window.scrollTo(0, 0));
  await page.screenshot({ path: "test-results/workbench.png", fullPage: true });
  await page.reload();
  await expect(page.getByText("Browser paired", { exact: true })).toBeVisible();
  await expect(
    page.getByRole("button", { name: "Send once", exact: false }),
  ).toBeDisabled();
  expect(pageErrors).toEqual([]);
});

test("lost browser TX response is recovered after reload without resubmission", async ({
  page,
}) => {
  await page.goto(cli("web", "open", "--print"));
  await expect(page.getByText("Browser paired", { exact: true })).toBeVisible();
  await page
    .getByRole("combobox", { name: "Channel", exact: true })
    .selectOption("virtual:1");
  await page
    .getByRole("combobox", { name: "Mode", exact: true })
    .selectOption("active");
  await page.getByRole("button", { name: "Open channel", exact: true }).click();
  await page
    .getByRole("button", { name: "Acquire control", exact: true })
    .click();
  await page
    .getByRole("button", { name: "Start channel", exact: true })
    .click();
  await expect(page.getByText("Live", { exact: true })).toBeVisible();
  let posts = 0;
  let original: { receipt: string; session_id: string } | undefined;
  await page.route("**/api/v1/sessions/*/tx", async (route) => {
    if (route.request().method() !== "POST") return route.continue();
    posts++;
    const response = await route.fetch();
    expect(response.status()).toBe(202);
    original = await response.json();
    await route.abort("failed");
  });
  await page.getByRole("button", { name: "Send once", exact: false }).click();
  await expect(page.getByRole("alert")).toContainText(
    "Transmission outcome unknown",
  );
  const history = page.getByRole("region", { name: "Submission history" });
  await expect(history).toContainText("outcome_unknown");
  const request = await history.locator("code").innerText();
  await page.reload();
  await expect(
    page.getByRole("button", { name: "Send once", exact: false }),
  ).toBeDisabled();
  await expect(history.locator("code")).toHaveText(request);
  // A missing record must not imply a safe resend.
  await page.route(
    "**/api/v1/sessions/*/tx?*",
    (route) => route.fulfill({ status: 404, json: {} }),
    { times: 1 },
  );
  await page
    .getByRole("button", { name: `Check result ${request}`, exact: true })
    .click();
  await expect(page.getByRole("alert")).toContainText(
    "does not prove that no frame was sent",
  );
  await page
    .getByRole("button", { name: `Check result ${request}`, exact: true })
    .click();
  await expect(
    page.getByRole("status", { name: "Operation result" }),
  ).toContainText(original!.receipt);
  await expect(history).toContainText("submitted");
  expect(posts).toBe(1);
  // History retains bounded summaries, not payload, authorization or active control.
  const stored = await page.evaluate(() =>
    Object.entries(sessionStorage).filter(([k]) =>
      k.startsWith("canharbor:receipts:"),
    ),
  );
  expect(stored).toHaveLength(1);
  const attempt = JSON.parse(stored[0][1])[0];
  expect(attempt).toMatchObject({
    session: expect.any(String),
    generation: expect.any(String),
    request: expect.any(String),
    state: "submitted",
  });
  expect(attempt.context).toMatchObject({
    kind: "classic_data",
    id: 0x123,
    bytes: 4,
  });
  for (const key of ["data_hex", "frame", "csrf_token", "auth", "lease"]) {
    expect(attempt).not.toHaveProperty(key);
    expect(attempt.context).not.toHaveProperty(key);
  }
});
