import { nativeBinary } from "./native";
// SPDX-License-Identifier: AGPL-3.0-or-later
import { test, expect } from "./isolated-service";
import { execFileSync } from "node:child_process";
import { resolve } from "node:path";
test("configuration drafts stay separate and releasing preserves the session", async ({
  page,
  isolatedRuntime,
}) => {
  const pairing = execFileSync(
    nativeBinary,
    ["--runtime-dir", isolatedRuntime, "web", "open", "--print"],
    { encoding: "utf8" },
  ).trim();
  await page.goto(pairing);
  const origin = new URL(page.url()).origin;
  await expect(page.getByText("Browser paired", { exact: true })).toBeVisible();
  await page.getByRole("button", { name: "Open channel", exact: true }).click();
  const selection = page.getByRole("combobox", {
    name: "Session",
    exact: true,
  });
  await expect(selection).not.toHaveValue("");
  const id = await selection.inputValue();
  const config = page.getByRole("region", { name: "Session configuration" });
  await expect(config).toContainText("Listen only");
  await page
    .getByRole("combobox", { name: "Mode", exact: true })
    .selectOption("active");
  await expect(config).toContainText("Unapplied changes");
  await expect(config).toContainText("Applied configuration: Listen only");
  await page
    .getByRole("button", { name: "Discard draft and load applied settings" })
    .click();
  await expect(
    page.getByRole("combobox", { name: "Mode", exact: true }),
  ).toHaveValue("listen_only");
  await page
    .getByRole("button", { name: "Acquire control", exact: true })
    .click();
  await page
    .getByRole("combobox", { name: "Mode", exact: true })
    .selectOption("active");
  await page.getByRole("button", { name: /Apply settings to/ }).click();
  await expect(config).toContainText("Applied configuration: Active");
  await expect(config).not.toContainText("Unapplied changes");
  await page
    .getByRole("button", { name: "Start channel", exact: true })
    .click();
  await page
    .getByRole("button", { name: "Stop and release control", exact: true })
    .click();
  await expect(page.getByText("Read only", { exact: true })).toBeVisible();
  await expect(selection).toHaveValue(id);
  await expect(selection).toBeEnabled();
  const response = await page.request.get(`${origin}/api/v1/sessions/${id}`);
  expect(response.ok()).toBe(true);
  const session = await response.json();
  expect(session.state).toBe("configured");
  expect(session.controller).toBeNull();
});

test("external configuration changes preserve the draft until explicit review", async ({
  page,
  isolatedRuntime,
}) => {
  const pairing = execFileSync(
    nativeBinary,
    ["--runtime-dir", isolatedRuntime, "web", "open", "--print"],
    { encoding: "utf8" },
  ).trim();
  await page.goto(pairing);
  const origin = new URL(page.url()).origin;
  await expect(page.getByText("Browser paired", { exact: true })).toBeVisible();
  await page.getByRole("button", { name: "Open channel", exact: true }).click();
  const selection = page.getByRole("combobox", {
    name: "Session",
    exact: true,
  });
  await expect(selection).not.toHaveValue("");
  const id = await selection.inputValue();
  await page
    .getByRole("combobox", { name: "Mode", exact: true })
    .selectOption("active");
  const auth = await (
    await page.request.get(`${origin}/api/v1/auth/session`)
  ).json();
  const owner = await (
    await page.request.post(`${origin}/api/v1/sessions/${id}/lease`, {
      data: {},
      headers: {
        "X-CANHarbor-CSRF": auth.csrf_token,
        Origin: new URL(page.url()).origin,
      },
    })
  ).json();
  const changed = await page.request.patch(
    `${origin}/api/v1/sessions/${id}/config`,
    {
      data: {
        mode: "listen_only",
        nominal_bitrate: 500000,
        fd: { enabled: true, data_bitrate: 2000000, iso: true },
      },
      headers: {
        "X-CANHarbor-CSRF": auth.csrf_token,
        Origin: new URL(page.url()).origin,
        "X-CANHarbor-Lease-Generation": owner.generation,
        "If-Match": JSON.stringify(owner.revision),
      },
    },
  );
  expect(changed.ok(), await changed.text()).toBe(true);
  await page.request.delete(`${origin}/api/v1/sessions/${id}/lease`, {
    data: {},
    headers: {
      "X-CANHarbor-CSRF": auth.csrf_token,
      Origin: new URL(page.url()).origin,
      "X-CANHarbor-Lease-Generation": owner.generation,
    },
  });
  const config = page.getByRole("region", { name: "Session configuration" });
  await expect(config).toContainText("server configuration changed");
  await expect(
    page.getByRole("combobox", { name: "Mode", exact: true }),
  ).toHaveValue("active");
  await expect(
    page.getByRole("button", { name: /Apply settings to/ }),
  ).toBeDisabled();
  await page
    .getByRole("button", { name: "Discard draft and load applied settings" })
    .click();
  await expect(
    page.getByRole("combobox", { name: "Mode", exact: true }),
  ).toHaveValue("listen_only");
  await expect(config).not.toContainText("server configuration changed");
  await expect(page.getByLabel("Enable CAN FD")).toBeChecked();
});
