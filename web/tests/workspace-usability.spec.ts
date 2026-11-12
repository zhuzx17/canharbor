import { nativeBinary, temporaryRuntime } from "./native";
// SPDX-License-Identifier: AGPL-3.0-or-later
import { test as base, expect, type WebSocketRoute } from "@playwright/test";
import { execFileSync, spawn } from "node:child_process";
import { resolve } from "node:path";
import { mkdtempSync, rmSync } from "node:fs";
const test = base.extend<{ isolatedRuntime: string }>({
  isolatedRuntime: async ({}, use) => {
    const runtime = temporaryRuntime("ch-ux-");
    const binary = nativeBinary;
    const service = spawn(
      binary,
      ["--runtime-dir", runtime, "serve", "--port", "0"],
      { stdio: "ignore" },
    );
    const exited = new Promise((resolve) => service.once("exit", resolve));
    try {
      await expect
        .poll(() => {
          try {
            execFileSync(binary, ["--runtime-dir", runtime, "devices"], {
              stdio: "ignore",
            });
            return true;
          } catch {
            return false;
          }
        })
        .toBe(true);
      await use(runtime);
    } finally {
      service.kill("SIGTERM");
      await exited;
      rmSync(runtime, { recursive: true, force: true });
    }
  },
});
test("workbench preserves drafts without restoring control and keeps monitor time stable", async ({
  page,
  isolatedRuntime,
}) => {
  let socket: WebSocketRoute | undefined;
  await page.routeWebSocket("**/stream", (route) => {
    socket = route;
    route.connectToServer();
  });
  const pairing = execFileSync(
    nativeBinary,
    ["--runtime-dir", isolatedRuntime, "web", "open", "--print"],
    { encoding: "utf8" },
  ).trim();
  await page.goto(pairing);
  await expect(page.getByText("Browser paired", { exact: true })).toBeVisible();
  await page
    .getByRole("combobox", { name: "Mode", exact: true })
    .selectOption("active");
  await page.getByRole("button", { name: "Open channel", exact: true }).click();
  await expect(
    page.getByRole("combobox", { name: "Session", exact: true }),
  ).not.toHaveValue("");
  const session = await page
    .getByRole("combobox", { name: "Session", exact: true })
    .inputValue();
  await page.getByRole("textbox", { name: "Identifier" }).fill("456");
  await page.reload();
  await expect(
    page.getByRole("combobox", { name: "Session", exact: true }),
  ).toHaveValue(session);
  await expect(
    page.getByRole("combobox", { name: "Mode", exact: true }),
  ).toHaveValue("active");
  await expect(page.getByRole("textbox", { name: "Identifier" })).toHaveValue(
    "456",
  );
  await expect(page.getByRole("button", { name: "Send once" })).toBeDisabled();
  await page
    .getByRole("button", { name: "Acquire control", exact: true })
    .click();
  try {
    await expect(
      page.getByRole("combobox", { name: "Session", exact: true }),
    ).toBeDisabled();
    await expect(
      page.getByRole("combobox", { name: "Channel", exact: true }),
    ).toBeDisabled();
    await page
      .getByRole("button", { name: "Start channel", exact: true })
      .click();
    await expect(page.getByText("Live", { exact: true })).toBeVisible();
    await page
      .getByRole("combobox", { name: "Frame type", exact: true })
      .selectOption("fd_data");
    await expect(
      page.getByText(
        "Enable CAN FD in the selected session before sending an FD frame.",
        { exact: true },
      ),
    ).toBeVisible();
    await expect(
      page.getByRole("button", { name: "Send once" }),
    ).toBeDisabled();
    const emit = (start: number, count: number) =>
      socket!.send(
        JSON.stringify({
          type: "events",
          server_instance_id: "srv",
          session_epoch: "s1",
          events: Array.from({ length: count }, (_, offset) => ({
            type: "frame_received",
            sequence: String(start + offset),
            host_monotonic_ns: String(BigInt(start + offset) * 1000000000n),
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
          })),
        }),
      );
    emit(1, 1);
    await expect(page.locator(".monitor tbody tr")).toHaveCount(1);
    emit(2, 500);
    await expect(page.locator(".monitor tbody tr")).toHaveCount(500);
    await expect(
      page.locator(".monitor tbody tr").last().locator("td").nth(1),
    ).toHaveText("500.000");
    await page.getByRole("textbox", { name: "Filter ID (hex)" }).fill("777");
    await expect(
      page.getByText("No frames match the current filters."),
    ).toBeVisible();
    await page
      .getByRole("button", { name: "Clear filters", exact: true })
      .click();
    await expect(page.locator(".monitor tbody tr")).toHaveCount(500);
    await page
      .getByRole("button", { name: "Pause display", exact: true })
      .click();
    await page.getByRole("button", { name: "Close", exact: true }).click();
    await expect(
      page.getByRole("button", { name: "Pause display", exact: true }),
    ).toBeVisible();
    const stored = await page.evaluate(() =>
      JSON.parse(localStorage.getItem("canharbor.workbench.v1") ?? "{}"),
    );
    expect(stored).not.toHaveProperty("lease");
    expect(stored).not.toHaveProperty("auth");
  } finally {
    const close = page.getByRole("button", { name: "Close", exact: true });
    if (await close.isEnabled()) await close.click();
  }
});
