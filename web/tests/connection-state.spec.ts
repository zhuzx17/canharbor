import { nativeBinary } from "./native";
// SPDX-License-Identifier: AGPL-3.0-or-later
import type { WebSocketRoute } from "@playwright/test";
import { test, expect } from "./isolated-service";
import { execFileSync } from "node:child_process";
import { resolve } from "node:path";
test("read failures preserve control and status checks preserve the monitor", async ({
  page,
  isolatedRuntime,
}) => {
  const clockStart = Date.parse("2026-01-01T00:00:00Z");
  await page.clock.install({ time: clockStart });
  let pollingPaused = false;
  const pendingReads = new Set<import("@playwright/test").Request>();
  page.on("requestfinished", (request) => pendingReads.delete(request));
  page.on("requestfailed", (request) => pendingReads.delete(request));
  let socket: WebSocketRoute | undefined;
  let sockets = 0;
  let authRequests = 0;
  let renewals = 0;
  let failReads = false;
  let resumeReads = () => {};
  let releaseRead = () => {};
  page.on("request", (request) => {
    if (
      request.method() === "GET" &&
      new URL(request.url()).pathname === "/api/v1/sessions"
    )
      pendingReads.add(request);
    if (request.url().includes("/auth/")) authRequests++;
    if (request.url().includes("/lease/renew")) renewals++;
  });
  for (const endpoint of ["sessions", "tasks"]) {
    await page.route(`**/api/v1/${endpoint}`, (route) => {
      if (failReads && route.request().method() === "GET")
        return route.fulfill({
          status: 503,
          contentType: "application/json",
          body: "{}",
        });
      return route.continue();
    });
  }
  await page.routeWebSocket("**/stream", (route) => {
    socket = route;
    sockets++;
    route.connectToServer();
  });
  const pairing = execFileSync(
    nativeBinary,
    ["--runtime-dir", isolatedRuntime, "web", "open", "--print"],
    { encoding: "utf8" },
  ).trim();
  await page.goto(pairing);
  await expect(page.getByText("Browser paired", { exact: true })).toBeVisible();
  await page.getByRole("button", { name: "Open channel", exact: true }).click();
  await page
    .getByRole("button", { name: "Acquire control", exact: true })
    .click();
  try {
    await page
      .getByRole("button", { name: "Start channel", exact: true })
      .click();
    await expect(page.getByText("Live", { exact: true })).toBeVisible();
    socket!.send(
      JSON.stringify({
        type: "events",
        server_instance_id: "srv",
        session_epoch: "s1",
        events: [
          {
            type: "frame_received",
            sequence: "1",
            host_monotonic_ns: "1000000000",
            direction: "rx",
            timestamp_quality: "host_only",
            timestamp_epoch: "0",
            frame: {
              kind: "classic_data",
              id: 0x123,
              extended: false,
              brs: false,
              esi: false,
              data_hex: "",
              dlc: 0,
            },
          },
        ],
      }),
    );
    await expect(page.locator(".monitor tbody tr")).toHaveCount(1);
    await page
      .getByRole("button", { name: "Pause display", exact: true })
      .click();
    failReads = true;
    await expect(page.getByTestId("channel-status-certainty")).toContainText(
      "unknown",
    );
    await expect.poll(() => renewals, { timeout: 9000 }).toBeGreaterThan(0);
    await expect(
      page.getByText("You have control", { exact: true }),
    ).toBeVisible();
    const priorAuthRequests = authRequests;
    const priorSockets = sockets;
    failReads = false;
    await page
      .getByRole("button", {
        name: "Check connection and session status",
        exact: true,
      })
      .click();
    await expect(page.getByTestId("channel-status-certainty")).toContainText(
      "Last reported channel state: running",
    );
    expect(authRequests).toBe(priorAuthRequests);
    expect(sockets).toBe(priorSockets);
    await expect(
      page.getByRole("button", { name: "Resume display", exact: true }),
    ).toBeVisible();
    await expect(page.locator(".monitor tbody tr")).toHaveCount(1);
    // Hold a status read across stream loss. Its old successful response must
    // not overwrite the newer uncertainty (independent of polling timing).
    // Stop the interval and drain existing reads before installing the gate,
    // so a background poll cannot consume the explicit check's held response.
    // Stay in one clock domain: the Node host clock can lag the browser clock.
    // The one-minute virtual point is beyond this test's 30-second real deadline.
    await page.clock.pauseAt(clockStart + 60_000);
    pollingPaused = true;
    await expect.poll(() => pendingReads.size).toBe(0);
    let readStarted!: () => void;
    const held = new Promise<void>((resolve) => {
      releaseRead = resolve;
    });
    const started = new Promise<void>((resolve) => {
      readStarted = resolve;
    });
    const resumed = new Promise<void>((resolve) => {
      resumeReads = resolve;
    });
    let holdNext = true;
    const holdRead = async (route: import("@playwright/test").Route) => {
      if (route.request().method() !== "GET") return route.fallback();
      if (!holdNext) {
        await resumed;
        return route.fallback();
      }
      holdNext = false;
      const response = await route.fetch();
      expect(response.status()).toBe(200);
      readStarted();
      await held;
      await route.fulfill({ response });
    };
    await page.route("**/api/v1/sessions", holdRead);
    // Dispatch directly so actionability waiting cannot let the held read hit
    // its deadline if polling collapses the details panel in the meantime.
    const check = page.getByRole("button", {
      name: "Check final status",
      exact: true,
      includeHidden: true,
    });
    await check.dispatchEvent("click");
    await started;
    failReads = true;
    await socket!.close();
    await expect(
      page.getByRole("button", { name: "Reconnect stream", exact: true }),
    ).toBeEnabled();
    releaseRead();
    await expect(check).toBeEnabled();
    await expect(page.getByTestId("channel-status-certainty")).toContainText(
      "Current channel state unknown",
    );
    await expect(
      page.getByText("You have control", { exact: true }),
    ).toBeVisible();
    failReads = false;
    resumeReads();
    await page.unroute("**/api/v1/sessions", holdRead);
    await page.clock.resume();
    pollingPaused = false;
    await page
      .getByRole("button", { name: "Reconnect stream", exact: true })
      .click();
    await expect(page.getByTestId("stream-segments")).toBeVisible();
    await expect(page.getByText("Live", { exact: true })).toBeVisible();
    await expect(page.locator(".monitor tbody tr")).toHaveCount(1);
    await expect(
      page.getByRole("button", { name: "Resume display", exact: true }),
    ).toBeVisible();
  } finally {
    releaseRead();
    resumeReads();
    if (pollingPaused) await page.clock.resume();
    failReads = false;
    const close = page.getByRole("button", { name: "Close", exact: true });
    if (await close.isEnabled()) await close.click();
  }
});
