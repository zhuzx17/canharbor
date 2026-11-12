import { nativeBinary } from "./native";
// SPDX-License-Identifier: AGPL-3.0-or-later
import { test, expect } from "./isolated-service";
import { execFileSync } from "node:child_process";
import { resolve } from "node:path";
const binary = nativeBinary;
test("navigation preserves view history and offline work is independent of channel setup", async ({
  page,
  isolatedRuntime,
}) => {
  await page.goto(
    execFileSync(
      binary,
      ["--runtime-dir", isolatedRuntime, "web", "open", "--print"],
      { encoding: "utf8" },
    ).trim(),
  );
  await page
    .getByRole("button", { name: "Offline analysis", exact: true })
    .click();
  await expect(
    page.getByRole("heading", { name: "Offline capture", exact: true }),
  ).toBeVisible();
  await expect(page.locator("#devices")).toBeHidden();
  const heading = await page
    .getByRole("heading", { name: "Offline capture", exact: true })
    .boundingBox();
  expect(heading!.y).toBeLessThan(400);
  await page.getByRole("link", { name: "Workspace 01" }).click();
  await expect(
    page.getByRole("heading", { name: "Frame monitor" }),
  ).toBeVisible();
  await expect(
    page.getByRole("heading", { name: "Offline capture", exact: true }),
  ).toBeHidden();
  await page.goBack();
  await expect(
    page.getByRole("heading", { name: "Offline capture", exact: true }),
  ).toBeVisible();
  await page.reload();
  await expect(
    page.getByRole("button", { name: "Offline analysis", exact: true }),
  ).toHaveAttribute("aria-pressed", "true");
  await page.setViewportSize({ width: 390, height: 844 });
  expect(
    (await page
      .getByRole("heading", { name: "Offline capture", exact: true })
      .boundingBox())!.y,
  ).toBeLessThan(600);
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= innerWidth,
    ),
  ).toBe(true);
  await page.getByRole("button", { name: "Live traffic", exact: true }).focus();
  await page.keyboard.press("Enter");
  await expect(
    page.getByRole("button", { name: "Live traffic", exact: true }),
  ).toHaveAttribute("aria-pressed", "true");
});

test("By ID retains a rare frame after the recent-frame window evicts it", async ({
  page,
  isolatedRuntime,
}) => {
  let socket: import("@playwright/test").WebSocketRoute | undefined;
  await page.routeWebSocket("**/stream", (route) => {
    socket = route;
    route.connectToServer();
  });
  await page.goto(
    execFileSync(
      binary,
      ["--runtime-dir", isolatedRuntime, "web", "open", "--print"],
      { encoding: "utf8" },
    ).trim(),
  );
  await page.getByRole("button", { name: "Open channel", exact: true }).click();
  await expect(page.getByText("Live", { exact: true })).toBeVisible();
  socket!.send(
    JSON.stringify({
      type: "events",
      server_instance_id: "review",
      session_epoch: "review",
      events: Array.from({ length: 601 }, (_, i) => ({
        type: "frame_received",
        sequence: String(i + 1),
        host_monotonic_ns: String((i + 1) * 1000000),
        direction: "rx",
        timestamp_quality: "host_only",
        timestamp_epoch: "0",
        frame: {
          kind: "classic_data",
          id: i === 0 ? 0x123 : 0x456,
          extended: false,
          brs: false,
          esi: false,
          data_hex: "0102",
          dlc: 2,
        },
      })),
    }),
  );
  await expect(page.locator(".monitor tbody tr")).toHaveCount(500);
  await expect(page.locator(".display-loss")).toContainText(
    "Display evicted: 101 frames",
  );
  await page
    .getByRole("textbox", { name: "Filter ID (hex)", exact: true })
    .fill("123");
  await expect(
    page.getByText("No frames match the current filters."),
  ).toBeVisible();
  await page
    .getByRole("combobox", { name: "Monitor view" })
    .selectOption("ids");
  await expect(page.locator(".aggregate-table tbody tr")).toHaveCount(1);
  await expect(page.locator(".aggregate-table tbody tr")).toContainText("123");
  await page
    .getByRole("textbox", { name: "Filter ID (hex)", exact: true })
    .fill("xyz");
  await expect(page.getByRole("alert")).toContainText("hexadecimal digits");
});

test("stopped recordings open directly in offline analysis without restarting the channel", async ({
  page,
  isolatedRuntime,
}) => {
  await page.goto(
    execFileSync(
      binary,
      ["--runtime-dir", isolatedRuntime, "web", "open", "--print"],
      { encoding: "utf8" },
    ).trim(),
  );
  await page.getByRole("button", { name: "Open channel", exact: true }).click();
  await page
    .getByRole("button", { name: "Acquire control", exact: true })
    .click();
  await page
    .getByRole("button", { name: "Start channel", exact: true })
    .click();
  await page.getByRole("button", { name: "Recordings", exact: true }).click();
  await page
    .getByRole("button", { name: "Start recording", exact: true })
    .click();
  await expect(
    page.getByRole("button", { name: "Stop recording", exact: true }),
  ).toBeVisible();
  execFileSync(binary, [
    "--runtime-dir",
    isolatedRuntime,
    "send",
    "--channel",
    "virtual:1",
    "--id",
    "123",
    "--data",
    "01020304",
    "--active",
  ]);
  await page
    .getByRole("button", { name: "Stop recording", exact: true })
    .click();
  await page
    .getByRole("button", { name: "Analyze recording", exact: true })
    .click();
  await expect(page.getByTestId("offline-summary")).toContainText(
    "Complete recording",
  );
  await expect(page.locator(".offline-table tbody tr")).toHaveCount(1);
  await expect(page.locator(".offline-table tbody tr")).toContainText(
    "01020304",
  );
  await page.getByRole("combobox", { name: "Time display" }).selectOption("ns");
  await expect(page.locator(".offline-table thead")).toContainText(
    "Monotonic time (ns)",
  );
  const liveChannel = page.getByRole("complementary", {
    name: "Active channel summary",
  });
  await expect(liveChannel).toContainText("running");
  await liveChannel
    .getByRole("button", { name: "Stop and release live channel" })
    .click();
  await expect(liveChannel).toContainText("Read only");
  await expect(
    page.getByRole("heading", { name: "Offline capture", exact: true }),
  ).toBeVisible();
});

test("hardware guidance and capability checks are read-only", async ({
  page,
  isolatedRuntime,
}) => {
  const mutations: string[] = [];
  page.on("request", (request) => {
    if (request.method() !== "GET" && !request.url().endsWith("/auth/pair"))
      mutations.push(request.url());
  });
  await page.route("**/api/v1/devices", async (route) => {
    const response = await route.fetch();
    const value = await response.json();
    for (const device of value.devices) device.simulated = false;
    await route.fulfill({ response, json: value });
  });
  await page.route("**/api/v1/channels/*/capabilities", async (route) => {
    const response = await route.fetch();
    const value = await response.json();
    await route.fulfill({
      response,
      json: { ...value, simulated: false, fixed_nominal_bitrates: [250000] },
    });
  });
  await page.goto(
    execFileSync(
      binary,
      ["--runtime-dir", isolatedRuntime, "web", "open", "--print"],
      { encoding: "utf8" },
    ).trim(),
  );
  await page.getByText("Connect a hardware adapter", { exact: true }).click();
  await expect(page.locator(".hardware-guide")).toContainText("usb-list");
  await page
    .getByRole("combobox", { name: "Adapter backend" })
    .selectOption("slcan");
  await expect(page.locator(".hardware-guide")).toContainText("serial-baud");
  await expect(page.getByRole("alert")).toContainText(
    "nominal bitrate advertised",
  );
  await expect(
    page.getByRole("button", { name: "Open channel", exact: true }),
  ).toBeDisabled();
  await page
    .getByRole("spinbutton", { name: "Nominal bitrate", exact: true })
    .fill("250000");
  await expect(
    page.getByRole("button", { name: "Open channel", exact: true }),
  ).toBeEnabled();
  expect(mutations).toEqual([]);
});
