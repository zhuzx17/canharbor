import { nativeBinary, nativeRuntime } from "./native";
// SPDX-License-Identifier: AGPL-3.0-or-later
import { test, expect } from "@playwright/test";
import { execFileSync } from "node:child_process";
import { resolve } from "node:path";
const runtime = nativeRuntime;
const cli = (...args: string[]) =>
  execFileSync(nativeBinary, ["--runtime-dir", runtime, ...args], {
    encoding: "utf8",
  }).trim();
test("recording names and source metadata survive browser refresh", async ({
  page,
}) => {
  await page.goto(cli("web", "open", "--print"));
  await expect(page.getByText("Browser paired", { exact: true })).toBeVisible();
  await page.getByRole("button", { name: "Open channel", exact: true }).click();
  await page
    .getByRole("button", { name: "Acquire control", exact: true })
    .click();
  await page
    .getByRole("button", { name: "Start channel", exact: true })
    .click();
  await expect(page.getByText("running", { exact: true })).toBeVisible();
  await page.getByRole("button", { name: "Recordings", exact: true }).click();
  const captures = page.getByRole("region", { name: "Captures", exact: true });
  await captures.getByLabel("Recording name (optional)").fill("Bench capture");
  await captures
    .getByRole("button", { name: "Start recording", exact: true })
    .click();
  await expect(
    captures.getByText("Bench capture", { exact: true }),
  ).toBeVisible();
  await captures
    .getByRole("button", { name: "Stop recording", exact: true })
    .click();
  await page.getByRole("button", { name: "Close", exact: true }).click();
  await page.reload();
  await page.getByRole("button", { name: "Recordings", exact: true }).click();
  await expect(
    captures.getByText("Bench capture", { exact: true }),
  ).toBeVisible();
  await expect(captures).toContainText("virtual:0");
  await expect(captures).toContainText("Created:");
  await captures
    .getByRole("button", { name: "Rename recording", exact: true })
    .click();
  await captures.getByLabel("New recording name").fill("Renamed bench");
  await captures
    .getByRole("button", { name: "Save recording name", exact: true })
    .click();
  await expect(
    captures.getByText("Renamed bench", { exact: true }),
  ).toBeVisible();
  await captures.getByLabel("Filter recordings by name").fill("not present");
  await expect(
    captures.getByText("Renamed bench", { exact: true }),
  ).toHaveCount(0);
  await captures.getByLabel("Filter recordings by name").fill("renamed");
  await captures
    .getByLabel("Filter recordings by channel")
    .selectOption("virtual:0");
  const downloaded = page.waitForEvent("download");
  await captures
    .getByRole("link", { name: "Download capture", exact: true })
    .click();
  const download = await downloaded;
  expect(download.suggestedFilename()).toMatch(
    /^Renamed bench_virtual_0_\d+_[a-f0-9-]+\.ndjson$/,
  );
  await page.reload();
  await page.getByRole("button", { name: "Recordings", exact: true }).click();
  await expect(
    captures.getByText("Renamed bench", { exact: true }),
  ).toBeVisible();
  page.once("dialog", async (dialog) => {
    expect(dialog.message()).toContain('"Renamed bench"');
    await dialog.accept();
  });
  await captures
    .getByRole("button", { name: "Delete recording", exact: true })
    .click();
  await expect(
    captures.getByText("Renamed bench", { exact: true }),
  ).toHaveCount(0);
  // Revoke the test browser identity and its remaining control lease.
  await page.evaluate(async () => {
    const auth = await (await fetch("/api/v1/auth/session")).json();
    await fetch("/api/v1/auth/session", {
      method: "DELETE",
      headers: {
        "Content-Type": "application/json",
        "X-CANHarbor-CSRF": auth.csrf_token,
      },
    });
  });
});

test("rename conflicts preserve drafts and require an explicit new comparison", async ({
  page,
  context,
}) => {
  await page.goto(cli("web", "open", "--print"));
  await expect(page.getByText("Browser paired", { exact: true })).toBeVisible();
  await page.getByRole("button", { name: "Open channel", exact: true }).click();
  await page
    .getByRole("button", { name: "Acquire control", exact: true })
    .click();
  await page
    .getByRole("button", { name: "Start channel", exact: true })
    .click();
  await expect(page.getByText("running", { exact: true })).toBeVisible();
  await page.getByRole("button", { name: "Recordings", exact: true }).click();
  const captures = page.getByRole("region", { name: "Captures", exact: true });
  await captures.getByLabel("Recording name (optional)").fill("Initial name");
  await captures
    .getByRole("button", { name: "Start recording", exact: true })
    .click();
  await captures
    .getByRole("button", { name: "Stop recording", exact: true })
    .click();
  await page.getByRole("button", { name: "Close", exact: true }).click();
  const row = captures.locator("[data-capture-id]");
  const id = (await row.getAttribute("data-capture-id"))!;
  const other = await context.newPage();
  await other.goto(page.url());
  await expect(
    other.getByText("Browser paired", { exact: true }),
  ).toBeVisible();
  async function changeElsewhere(expected: string, name: string) {
    return other.evaluate(
      async ({ id, expected, name }) => {
        const auth = await (await fetch("/api/v1/auth/session")).json();
        return (
          await fetch(`/api/v1/captures/${id}`, {
            method: "PATCH",
            headers: {
              "Content-Type": "application/json",
              "X-CANHarbor-CSRF": auth.csrf_token,
            },
            body: JSON.stringify({ expected_name: expected, name }),
          })
        ).status;
      },
      { id, expected, name },
    );
  }
  let writes = 0;
  page.on("request", (request) => {
    if (
      request.method() === "PATCH" &&
      request.url().endsWith(`/captures/${id}`)
    )
      writes++;
  });
  await row
    .getByRole("button", { name: "Rename recording", exact: true })
    .click();
  await row.getByLabel("New recording name").fill("My draft");
  expect(await changeElsewhere("Initial name", "Other client")).toBe(200);
  await row
    .getByRole("button", { name: "Save recording name", exact: true })
    .click();
  const conflict = row.getByLabel("Rename conflict");
  await expect(conflict).toContainText("Current server name: Other client");
  await expect(row.getByLabel("New recording name")).toHaveValue("My draft");
  await expect(
    row.getByRole("button", { name: "Save recording name", exact: true }),
  ).toBeDisabled();
  expect(writes).toBe(1);
  expect(await changeElsewhere("Other client", "Third name")).toBe(200);
  await captures
    .getByRole("button", { name: "Refresh recordings", exact: true })
    .click();
  await expect(conflict).toContainText("Current server name: Third name");
  await expect(row.getByLabel("New recording name")).toHaveValue("My draft");
  expect(writes).toBe(1);
  await conflict
    .getByRole("button", {
      name: "Keep draft against current name",
      exact: true,
    })
    .click();
  expect(writes).toBe(1);
  await row
    .getByRole("button", { name: "Save recording name", exact: true })
    .click();
  await expect(row.getByText("My draft", { exact: true })).toBeVisible();
  expect(writes).toBe(2);
  await row
    .getByRole("button", { name: "Rename recording", exact: true })
    .click();
  await row.getByLabel("New recording name").fill("Discard draft");
  expect(await changeElsewhere("My draft", "Last name")).toBe(200);
  await row
    .getByRole("button", { name: "Save recording name", exact: true })
    .click();
  await expect(conflict).toContainText("Current server name: Last name");
  await conflict
    .getByRole("button", { name: "Load current name", exact: true })
    .click();
  await expect(row.getByLabel("New recording name")).toHaveValue("Last name");
  expect(writes).toBe(3);
  await row.getByRole("button", { name: "Cancel rename", exact: true }).click();
  page.once("dialog", (dialog) => dialog.accept());
  await row
    .getByRole("button", { name: "Delete recording", exact: true })
    .click();
  await expect(row).toHaveCount(0);
  await other.close();
});
