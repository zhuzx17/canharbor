// SPDX-License-Identifier: AGPL-3.0-or-later
import { expect, test } from "@playwright/test";
import { ApiFailure } from "../src/api";
import { recoveryAdvice } from "../src/recovery";

test("recovery advice distinguishes authentication, conflicts, validation and uncertain responses", () => {
  expect(recoveryAdvice(new ApiFailure(401)).next).toContain(
    "fresh pairing link",
  );
  expect(recoveryAdvice(new ApiFailure(403)).next).toContain("work once");
  for (const status of [409, 412, 428])
    expect(recoveryAdvice(new ApiFailure(status)).next).toContain(
      "Review the selected session",
    );
  expect(recoveryAdvice(new ApiFailure(422)).check).toBe(false);
  expect(recoveryAdvice(new TypeError("Failed to fetch")).next).toContain(
    "outcome unknown",
  );
  expect(recoveryAdvice(new ApiFailure(503)).next).toContain(
    "submission history",
  );
});

test("expired pairing offers a read-only check without redeeming the link again", async ({
  page,
}) => {
  const mutations: string[] = [];
  page.on("request", (request) => {
    if (request.url().includes("/api/v1/") && request.method() !== "GET")
      mutations.push(new URL(request.url()).pathname);
  });
  await page.goto("/#pair=expired-fixture");
  const recovery = page.getByRole("alert", { name: "Operation recovery" });
  await expect(recovery).toContainText("fresh pairing link");
  await recovery
    .getByRole("button", { name: "Check connection and session status" })
    .click();
  await expect(recovery).toContainText("fresh pairing link");
  expect(mutations).toEqual(["/api/v1/auth/pair"]);
  await recovery.getByRole("button", { name: "Dismiss error" }).click();
  await expect(recovery).toHaveCount(0);
});

test("service connection failures offer a status check without control operations", async ({
  page,
}) => {
  let reads = 0;
  await page.route("**/api/v1/auth/session", async (route) => {
    reads++;
    await route.abort("failed");
  });
  const mutations: string[] = [];
  page.on("request", (request) => {
    if (request.url().includes("/api/v1/") && request.method() !== "GET")
      mutations.push(request.method());
  });
  await page.goto("/");
  const recovery = page.getByRole("alert", { name: "Operation recovery" });
  await expect(recovery).toContainText("local service is running");
  await recovery
    .getByRole("button", { name: "Check connection and session status" })
    .click();
  await expect.poll(() => reads).toBe(2);
  await expect(recovery).toContainText("outcome unknown");
  expect(mutations).toEqual([]);
});

test("recovery guidance uses resource context and preserves access failure distinctions", () => {
  expect(recoveryAdvice(new ApiFailure(422), "recording").next).toContain(
    "recording name",
  );
  expect(recoveryAdvice(new ApiFailure(422), "stress").next).toContain(
    "test plan",
  );
  expect(recoveryAdvice(new ApiFailure(403)).next).toContain(
    "request-verification failure",
  );
  expect(recoveryAdvice(new ApiFailure(404)).next).toContain("does not prove");
  const expired = new ApiFailure(409, {
    code: "lease_expired",
    message: "expired",
    retryable: false,
    request_id: "r",
    receipt: null,
    outcome_unknown: false,
  });
  expect(recoveryAdvice(expired).next).toContain("explicitly acquire control");
});
