import { nativeBinary, nativeRuntime } from "./native";
// SPDX-License-Identifier: AGPL-3.0-or-later
import { expect, test } from "@playwright/test";
import { execFileSync } from "node:child_process";
import { resolve } from "node:path";
import { readAttempts } from "../src/TransmissionActions";
const runtime = nativeRuntime;
const cli = (...args: string[]) =>
  execFileSync(nativeBinary, ["--runtime-dir", runtime, ...args], {
    encoding: "utf8",
  }).trim();
const legacy = {
  session: "legacy-session",
  generation: "1",
  request: "00000000-0000-4000-8000-000000000001",
  state: "outcome_unknown",
};

test("submission history accepts legacy identifiers and discards invalid optional summaries", () => {
  expect(readAttempts(JSON.stringify([legacy]))).toEqual([legacy]);
  const context = {
    at: "2026-11-09T01:02:03.000Z",
    channel: "virtual:0",
    kind: "classic_data",
    id: 0x123,
    extended: false,
    bytes: 4,
    brs: false,
  };
  expect(readAttempts(JSON.stringify([{ ...legacy, context }]))).toEqual([
    { ...legacy, context },
  ]);
  expect(
    readAttempts(
      JSON.stringify([
        { ...legacy, context: { ...context, data_hex: "01020304" } },
      ]),
    ),
  ).toEqual([legacy]);
  expect(
    readAttempts(
      JSON.stringify([
        { ...legacy, context: { ...context, channel: "x".repeat(129) } },
      ]),
    ),
  ).toEqual([legacy]);
  expect(readAttempts(JSON.stringify([{ ...legacy, auth: "secret" }]))).toEqual(
    [],
  );
  expect(
    readAttempts(JSON.stringify(Array.from({ length: 17 }, () => legacy))),
  ).toEqual([]);
  expect(readAttempts(" ".repeat(16385))).toEqual([]);
});

test("an unknown send requires querying the original and acknowledging a distinct new request", async ({
  page,
}) => {
  await page.goto(cli("web", "open", "--print"));
  await expect(page.getByText("Browser paired", { exact: true })).toBeVisible();
  await page
    .getByRole("combobox", { name: "Channel", exact: true })
    .selectOption("virtual:0");
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
  const requests: { client_request_id: string; frame: { id: number } }[] = [];
  let originalReceipt: string | undefined;
  await page.route("**/api/v1/sessions/*/tx", async (route) => {
    if (route.request().method() !== "POST") return route.continue();
    requests.push(route.request().postDataJSON());
    if (requests.length === 1) {
      const response = await route.fetch();
      expect(response.status()).toBe(202);
      originalReceipt = (await response.json()).receipt;
      await route.abort("failed");
    } else await route.continue();
  });
  const send = page.getByRole("button", { name: "Send once", exact: false });
  await send.click();
  const history = page.getByRole("region", { name: "Submission history" });
  await expect(history).toContainText("outcome_unknown");
  await expect(history).toContainText("virtual:0");
  await expect(history).toContainText("ID 123 (11-bit) · 4 bytes");
  await expect(history.locator("time")).toHaveAttribute("datetime", /^\d{4}-/);
  const unknown = page.getByRole("complementary", {
    name: "Unconfirmed transmissions",
  });
  const acknowledgment = unknown.getByRole("checkbox");
  await expect(send).toBeDisabled();
  await expect(acknowledgment).toBeDisabled();
  await page.route(
    "**/api/v1/sessions/*/tx?*",
    (route) => route.fulfill({ status: 404, json: {} }),
    { times: 1 },
  );
  await history
    .getByRole("button", {
      name: `Check result ${requests[0].client_request_id}`,
      exact: true,
    })
    .click();
  await expect(page.getByRole("alert")).toContainText(
    "does not prove that no frame was sent",
  );
  expect(requests).toHaveLength(1);
  await expect(acknowledgment).toBeEnabled();
  await expect(send).toBeDisabled();
  await acknowledgment.check();
  await page.getByLabel("Identifier", { exact: false }).fill("124");
  await send.click();
  await expect(history).toContainText("submitted");
  expect(requests).toHaveLength(2);
  expect(requests[1].client_request_id).not.toBe(requests[0].client_request_id);
  expect(requests.map((r) => r.frame.id)).toEqual([0x123, 0x124]);
  await expect(send).toBeDisabled();
  await history
    .getByRole("button", {
      name: `Check result ${requests[0].client_request_id}`,
      exact: true,
    })
    .click();
  await expect(
    page.getByRole("status", { name: "Operation result" }),
  ).toContainText(originalReceipt!);
  await expect(unknown).toHaveCount(0);
  const persisted = await page.evaluate(() =>
    Object.entries(sessionStorage).find(([key]) =>
      key.startsWith("canharbor:receipts:"),
    ),
  );
  expect(persisted).toBeDefined();
  const entries = JSON.parse(persisted![1]);
  expect(
    entries.every(
      (entry: { context: object }) =>
        Object.keys(entry.context).sort().join() ===
        "at,brs,bytes,channel,extended,id,kind",
    ),
  ).toBe(true);
  expect(persisted![1]).not.toMatch(/data_hex|csrf|payload|auth/);
  // A legacy unknown entry remains queryable after refresh. No control is restored.
  await page.evaluate(
    ({ key, attempts, request }) => {
      const original = attempts.find(
        (a: { request: string }) => a.request === request,
      );
      const { context: _context, ...identifiers } = original;
      sessionStorage.setItem(
        key,
        JSON.stringify([{ ...identifiers, state: "outcome_unknown" }]),
      );
    },
    {
      key: persisted![0],
      attempts: entries,
      request: requests[0].client_request_id,
    },
  );
  await page.reload();
  await expect(history).toContainText("time and frame summary unavailable");
  await expect(send).toBeDisabled();
  await history
    .getByRole("button", {
      name: `Check result ${requests[0].client_request_id}`,
      exact: true,
    })
    .click();
  await expect(history).toContainText("submitted");
  expect(requests).toHaveLength(2);
});

test.afterEach(async ({ page }) => {
  if (!page.url().startsWith("http://127.0.0.1:32189")) return;
  // Stop the session explicitly with the original cookie owner's generation.
  await page.evaluate(async () => {
    const response = await fetch("/api/v1/auth/session");
    if (!response.ok) return;
    const auth = await response.json();
    const list = await (await fetch("/api/v1/sessions")).json();
    for (const session of list.sessions) {
      if (session.channel_id !== "virtual:0") continue;
      const response = await fetch(`/api/v1/sessions/${session.id}`, {
        method: "DELETE",
        headers: {
          "Content-Type": "application/json",
          "X-CANHarbor-CSRF": auth.csrf_token,
          "X-CANHarbor-Lease-Generation": session.generation,
        },
        body: "{}",
      });
      if (!response.ok) throw new Error(`cleanup failed: ${response.status}`);
    }
    await fetch("/api/v1/auth/session", {
      method: "DELETE",
      headers: {
        "Content-Type": "application/json",
        "X-CANHarbor-CSRF": auth.csrf_token,
      },
      body: "{}",
    });
  });
});
