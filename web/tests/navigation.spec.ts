import { nativeBinary } from "./native";
// SPDX-License-Identifier: AGPL-3.0-or-later
import { test, expect } from "./isolated-service";
import { execFileSync } from "node:child_process";
import { resolve } from "node:path";
test("reload preserves pending read-only polling without page errors", async ({
  page,
  isolatedRuntime,
}) => {
  const pageErrors: string[] = [];
  page.on("pageerror", (error) => pageErrors.push(error.message));
  await page.addInitScript(() => {
    const fetch = window.fetch;
    window.fetch = (input, init) => {
      if (String(input).endsWith("/api/v1/tasks")) {
        sessionStorage.setItem("task-poll-keepalive", String(init?.keepalive));
      }
      return fetch(input, init);
    };
  });
  await page.goto(
    execFileSync(
      nativeBinary,
      ["--runtime-dir", isolatedRuntime, "web", "open", "--print"],
      { encoding: "utf8" },
    ).trim(),
  );
  await expect(page.getByText("Browser paired", { exact: true })).toBeVisible();
  for (let attempt = 0; attempt < 3; attempt++) {
    await page.evaluate(() => sessionStorage.removeItem("task-poll-keepalive"));
    let intercepted!: () => void;
    const pending = new Promise<void>((resolve) => (intercepted = resolve));
    let release!: () => void;
    const gate = new Promise<void>((resolve) => (release = resolve));
    let held = false;
    const holdPoll = async (route: import("@playwright/test").Route) => {
      if (held) return route.continue();
      held = true;
      intercepted();
      await gate;
      await route.abort().catch(() => {}); // The old document may already be gone.
    };
    await page.route("**/api/v1/tasks", holdPoll);
    try {
      // Force the race instead of hoping a fast local request overlaps reload.
      await pending;
      await page.reload();
      await expect(
        page.getByText("Browser paired", { exact: true }),
      ).toBeVisible();
      expect(
        await page.evaluate(() =>
          sessionStorage.getItem("task-poll-keepalive"),
        ),
      ).toBe("true");
      const response = await page.waitForResponse((response) =>
        response.url().endsWith("/api/v1/tasks"),
      );
      expect(response.status()).toBe(200);
      expect(pageErrors).toEqual([]);
    } finally {
      release();
      await page.unroute("**/api/v1/tasks", holdPoll);
    }
  }
});

test("stopping a pending navigation leaves the workbench able to poll", async ({
  page,
  isolatedRuntime,
  browserName,
}) => {
  const pageErrors: string[] = [];
  page.on("pageerror", (error) => pageErrors.push(error.message));
  await page.goto(
    execFileSync(
      nativeBinary,
      ["--runtime-dir", isolatedRuntime, "web", "open", "--print"],
      { encoding: "utf8" },
    ).trim(),
  );
  await expect(page.getByText("Browser paired", { exact: true })).toBeVisible();
  await expect(
    page.getByRole("region", { name: "Periodic tasks", exact: true }),
  ).toHaveAttribute("aria-busy", "false");
  let intercepted!: () => void;
  const pending = new Promise<void>((resolve) => (intercepted = resolve));
  let release!: () => void;
  const gate = new Promise<void>((resolve) => (release = resolve));
  await page.route("**/pending-navigation", async (route) => {
    intercepted();
    await gate;
    await route.abort().catch(() => {});
  });
  try {
    // Leave the current document active while the next document is loading.
    await page.evaluate(() => {
      (window as Window & { departureStarted?: boolean }).departureStarted =
        false;
      window.addEventListener(
        "beforeunload",
        () => {
          (window as Window & { departureStarted?: boolean }).departureStarted =
            true;
        },
        { once: true },
      );
      location.href = "/pending-navigation";
    });
    await pending;
    if (browserName === "chromium") {
      // evaluate waits for a pending Chromium navigation; use the browser's
      // Stop command before reading the surviving document again.
      const session = await page.context().newCDPSession(page);
      await session.send("Page.stopLoading");
      await session.detach();
    } else {
      await page.evaluate(() => window.stop());
    }
    expect(
      await page.evaluate(
        () =>
          (window as Window & { departureStarted?: boolean }).departureStarted,
      ),
    ).toBe(true);
    const response = await page.waitForResponse((response) =>
      response.url().endsWith("/api/v1/tasks"),
    );
    expect(response.status()).toBe(200);
    await expect(
      page.getByText("Browser paired", { exact: true }),
    ).toBeVisible();
    expect(pageErrors).toEqual([]);
  } finally {
    release();
  }
});
