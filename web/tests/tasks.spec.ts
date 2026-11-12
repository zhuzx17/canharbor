import { nativeBinary, nativeRuntime } from "./native";
// SPDX-License-Identifier: AGPL-3.0-or-later
import { test, expect, type Page } from "@playwright/test";
import { execFileSync } from "node:child_process";
import { resolve } from "node:path";
import { readTaskAttempts, recoverTask, validateTaskPlan } from "../src/tasks";
import type { PeriodicTask } from "../src/generated/api";
const runtime = nativeRuntime;
const cli = (...args: string[]) =>
  execFileSync(nativeBinary, ["--runtime-dir", runtime, ...args], {
    encoding: "utf8",
  }).trim();
async function ready(page: Page) {
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
  await expect(
    page.getByRole("button", { name: "Start periodic task", exact: true }),
  ).toBeEnabled();
}
async function closeSession(page: Page) {
  // Test cleanup uses the cookie owner's original generation. It never changes
  // the page's local control state or resumes transmission after reload.
  await page.evaluate(async () => {
    const authResponse = await fetch("/api/v1/auth/session", {
      cache: "no-store",
    });
    if (!authResponse.ok) return;
    const auth = await authResponse.json();
    const sessions = await (
      await fetch("/api/v1/sessions", { cache: "no-store" })
    ).json();
    for (const session of sessions.sessions) {
      if (session.channel_id !== "virtual:0") continue;
      let generation = session.generation;
      const headers: Record<string, string> = {
        "Content-Type": "application/json",
        "X-CANHarbor-CSRF": auth.csrf_token,
      };
      if (!session.controller) {
        const acquired = await (
          await fetch(`/api/v1/sessions/${session.id}/lease`, {
            method: "POST",
            headers,
            body: "{}",
          })
        ).json();
        generation = acquired.generation;
      }
      headers["X-CANHarbor-Lease-Generation"] = generation;
      const response = await fetch(`/api/v1/sessions/${session.id}`, {
        method: "DELETE",
        headers,
        body: "{}",
      });
      if (!response.ok) throw new Error(`cleanup HTTP ${response.status}`);
    }
    const logout = await fetch("/api/v1/auth/session", {
      method: "DELETE",
      headers: {
        "Content-Type": "application/json",
        "X-CANHarbor-CSRF": auth.csrf_token,
      },
      body: "{}",
    });
    if (!logout.ok) throw new Error(`logout HTTP ${logout.status}`);
  });
}
test.afterEach(async ({ page }) => {
  if (page.url().startsWith("http://127.0.0.1:32189")) await closeSession(page);
});
test("task bounds, recovery identifiers and ambiguous results are validated", () => {
  expect(() => validateTaskPlan(0, 10)).toThrow();
  expect(() => validateTaskPlan(6000, 100)).toThrow();
  expect(() => validateTaskPlan(2, 60000)).not.toThrow();
  expect(readTaskAttempts("x".repeat(16385))).toEqual([]);
  expect(readTaskAttempts(JSON.stringify(Array(17).fill({})))).toEqual([]);
  const attempt = {
    session: "session-1",
    request: "11111111-1111-4111-8111-111111111111",
    generation: "1",
  };
  expect(readTaskAttempts(JSON.stringify([attempt]))).toEqual([attempt]);
  expect(
    readTaskAttempts(JSON.stringify([{ ...attempt, generation: "01" }])),
  ).toEqual([]);
  expect(readTaskAttempts(JSON.stringify([{ ...attempt, frame: {} }]))).toEqual(
    [],
  );
  const task = {
    session_id: attempt.session,
    client_request_id: attempt.request,
    generation: "1",
  } as PeriodicTask;
  expect(() => recoverTask([task, task], attempt)).toThrow();
});
test("task creation, cancellation and renewal use the existing acquired lease", async ({
  page,
}) => {
  await ready(page);
  const panel = page.locator(".task-panel");
  await page
    .getByRole("spinbutton", { name: "Frame count", exact: true })
    .fill("2");
  await page
    .getByRole("spinbutton", { name: "Task interval", exact: true })
    .fill("16000");
  let renewals = 0;
  let starts = 0;
  page.on("request", (req) => {
    if (req.method() === "POST" && req.url().endsWith("/lease/renew"))
      renewals++;
    if (req.method() === "POST" && /\/sessions\/[^/]+\/tasks$/.test(req.url()))
      starts++;
  });
  await page
    .getByRole("button", { name: "Start periodic task", exact: true })
    .click();
  await expect(panel.locator(".task-result")).toHaveCount(1);
  await expect(panel.locator(".task-result")).toContainText(
    "State: completed",
    { timeout: 22000 },
  );
  await expect(panel.locator(".task-result")).toContainText("Submitted 2");
  expect(renewals).toBeGreaterThanOrEqual(2);
  await page
    .getByRole("spinbutton", { name: "Task interval", exact: true })
    .fill("60000");
  await page
    .getByRole("button", { name: "Start periodic task", exact: true })
    .click();
  await expect(panel.locator(".task-result")).toHaveCount(2);
  const cancel = panel
    .getByRole("button", { name: "Cancel task", exact: true })
    .all();
  for (const button of await cancel)
    if (await button.isEnabled()) {
      await button.click();
      break;
    }
  await expect(
    panel.locator(".task-result").filter({ hasText: "State: cancelled" }),
  ).toHaveCount(1);
  expect(starts).toBe(2);
  await closeSession(page);
});
test("lost admission response is recovered after reload without resubmission or restored control", async ({
  page,
}) => {
  await ready(page);
  const panel = page.locator(".task-panel");
  await page
    .getByRole("spinbutton", { name: "Frame count", exact: true })
    .fill("1");
  await page
    .getByRole("spinbutton", { name: "Task interval", exact: true })
    .fill("1000");
  let posts = 0;
  let task: PeriodicTask | undefined;
  await page.route("**/api/v1/sessions/*/tasks", async (route) => {
    if (route.request().method() !== "POST") return route.continue();
    posts++;
    const response = await route.fetch();
    expect(response.status()).toBe(202);
    task = await response.json();
    await route.abort("failed");
  });
  await page
    .getByRole("button", { name: "Start periodic task", exact: true })
    .click();
  await expect(panel.locator(".task-message")).toContainText("outcome unknown");
  await page.reload();
  await expect(
    page.getByRole("button", { name: "Start periodic task", exact: true }),
  ).toBeDisabled();
  await expect(
    panel.getByRole("button", { name: "Find task", exact: true }),
  ).toHaveCount(1);
  await panel.getByRole("button", { name: "Find task", exact: true }).click();
  await expect(panel.locator(".task-message")).toContainText(task!.id);
  expect(posts).toBe(1);
  const stored = await page.evaluate(() =>
    Object.entries(sessionStorage).filter(([k]) =>
      k.startsWith("canharbor:tasks:"),
    ),
  );
  expect(stored).toHaveLength(1);
  expect(Object.keys(JSON.parse(stored[0][1])[0]).sort()).toEqual([
    "generation",
    "request",
    "session",
  ]);
  await closeSession(page);
});
test("task read failures preserve renewal and recovery never infers a safe resend from absent results", async ({
  page,
}) => {
  await ready(page);
  const panel = page.locator(".task-panel");
  await page
    .getByRole("spinbutton", { name: "Frame count", exact: true })
    .fill("1");
  await page
    .getByRole("spinbutton", { name: "Task interval", exact: true })
    .fill("1000");
  let creates = 0;
  let renewals = 0;
  page.on("request", (r) => {
    if (r.method() === "POST" && r.url().endsWith("/lease/renew")) renewals++;
    if (r.method() === "POST" && /\/sessions\/[^/]+\/tasks$/.test(r.url()))
      creates++;
  });
  await page
    .getByRole("button", { name: "Start periodic task", exact: true })
    .click();
  await expect(panel.locator(".task-result")).toContainText("completed");
  await page.route("**/api/v1/tasks", (route) =>
    route.fulfill({ status: 503, json: {} }),
  );
  await expect(
    page.getByRole("alert", { name: "Operation recovery" }),
  ).toBeVisible();
  await expect(
    page.getByText("You have control", { exact: true }),
  ).toBeVisible();
  const before = renewals;
  await expect.poll(() => renewals, { timeout: 9000 }).toBeGreaterThan(before);
  await page.unroute("**/api/v1/tasks");
  await page.route("**/api/v1/tasks", (route) =>
    route.fulfill({ status: 200, json: { tasks: [] } }),
  );
  await panel.getByRole("button", { name: "Find task", exact: true }).click();
  await expect(panel.locator(".task-message")).toContainText(
    "does not prove nonexecution",
  );
  expect(creates).toBe(1);
  await page.unroute("**/api/v1/tasks");
  await closeSession(page);
});
test("stop/restart and service instance changes never replay a task or restore control", async ({
  page,
}) => {
  await ready(page);
  const panel = page.locator(".task-panel");
  let starts = 0;
  let renewals = 0;
  page.on("request", (r) => {
    if (r.method() === "POST" && /\/sessions\/[^/]+\/tasks$/.test(r.url()))
      starts++;
    if (r.method() === "POST" && r.url().endsWith("/lease/renew")) renewals++;
  });
  await page
    .getByRole("spinbutton", { name: "Frame count", exact: true })
    .fill("2");
  await page
    .getByRole("spinbutton", { name: "Task interval", exact: true })
    .fill("60000");
  await page
    .getByRole("button", { name: "Start periodic task", exact: true })
    .click();
  await expect(panel.locator(".task-result")).toHaveCount(1);
  await page.getByRole("button", { name: "Stop channel", exact: true }).click();
  await expect(panel.locator(".task-result")).toContainText("State: cancelled");
  await page
    .getByRole("button", { name: "Start channel", exact: true })
    .click();
  expect(starts).toBe(1);
  await page.route("**/api/v1/version", async (route) => {
    const response = await route.fetch();
    const version = await response.json();
    await route.fulfill({
      json: { ...version, server_instance_id: "new-test-instance" },
    });
  });
  await page.route("**/api/v1/tasks", (route) =>
    route.fulfill({ json: { tasks: [] } }),
  );
  await page.reload();
  await expect(
    page.getByRole("button", { name: "Start periodic task", exact: true }),
  ).toBeDisabled();
  await expect(
    panel.getByRole("button", { name: "Find task", exact: true }),
  ).toHaveCount(0);
  const stopped = renewals;
  await page.waitForTimeout(5500);
  expect(renewals).toBe(stopped);
  expect(starts).toBe(1);
  await closeSession(page);
});
test("a lost cancellation response is reconciled by reading the final state without retry", async ({
  page,
}) => {
  await ready(page);
  const panel = page.locator(".task-panel");
  await page
    .getByRole("spinbutton", { name: "Frame count", exact: true })
    .fill("2");
  await page
    .getByRole("spinbutton", { name: "Task interval", exact: true })
    .fill("60000");
  await page
    .getByRole("button", { name: "Start periodic task", exact: true })
    .click();
  await expect(panel.locator(".task-result")).toHaveCount(1);
  let cancels = 0;
  await page.route("**/api/v1/tasks/*/cancel", async (route) => {
    cancels++;
    const response = await route.fetch();
    expect(response.status()).toBe(200);
    await route.abort("failed");
  });
  await panel.getByRole("button", { name: "Cancel task", exact: true }).click();
  await expect(panel.locator(".task-message")).toContainText(
    "Cancellation response unavailable",
  );
  await expect(panel.locator(".task-result")).toContainText("State: cancelled");
  await expect(
    page.getByRole("button", { name: "Start periodic task", exact: true }),
  ).toBeDisabled();
  await page
    .getByRole("button", { name: "Refresh tasks", exact: true })
    .click();
  expect(cancels).toBe(1);
  await closeSession(page);
});
test("a slow acquisition response does not postpone the first browser renewal", async ({
  page,
}) => {
  let released = 0;
  let renewed = 0;
  page.on("request", (request) => {
    if (
      request.method() === "POST" &&
      request.url().endsWith("/lease/renew") &&
      !renewed
    )
      renewed = Date.now();
  });
  await page.route(
    "**/api/v1/sessions/*/lease",
    async (route) => {
      const response = await route.fetch();
      expect(response.status()).toBe(200);
      await new Promise((resolve) => setTimeout(resolve, 8000));
      released = Date.now();
      await route.fulfill({ response });
    },
    { times: 1 },
  );
  await ready(page);
  await expect.poll(() => renewed, { timeout: 3000 }).toBeGreaterThan(0);
  expect(renewed - released).toBeLessThan(4000);
  await closeSession(page);
});
