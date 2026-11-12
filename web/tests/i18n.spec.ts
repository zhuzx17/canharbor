import { nativeBinary, temporaryRuntime } from "./native";
// SPDX-License-Identifier: AGPL-3.0-or-later
import { test, expect } from "./isolated-service";
import { execFileSync, spawn } from "node:child_process";
import { resolve } from "node:path";
import { mkdtempSync, rmSync } from "node:fs";
const binary = nativeBinary;
function pairing(runtime: string) {
  return execFileSync(
    binary,
    ["--runtime-dir", runtime, "web", "open", "--print"],
    { encoding: "utf8" },
  ).trim();
}
test("language changes preserve pairing and survive reload and a different service port", async ({
  page,
  isolatedRuntime,
}) => {
  let pairs = 0;
  page.on("request", (request) => {
    if (request.url().endsWith("/api/v1/auth/pair")) pairs++;
  });
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  await page.goto(pairing(isolatedRuntime));
  await expect(page.getByText("Browser paired", { exact: true })).toBeVisible();
  const saved = page.waitForResponse(
    (response) =>
      response.url().endsWith("/api/v1/preferences") &&
      response.request().method() === "PUT",
  );
  await page.getByLabel("Language", { exact: true }).selectOption("zh-CN");
  expect((await saved).status()).toBe(200);
  await expect(
    page.getByRole("heading", { name: "总线工作台", exact: true }),
  ).toBeVisible();
  await expect(page.locator("html")).toHaveAttribute("lang", "zh-CN");
  expect(pairs).toBe(1);
  await page.reload();
  await expect(
    page.getByRole("heading", { name: "总线工作台", exact: true }),
  ).toBeVisible();
  expect(pairs).toBe(1);
  const other = temporaryRuntime("ch-locale-port-");
  const service = spawn(
    binary,
    ["--runtime-dir", other, "serve", "--port", "0"],
    {
      stdio: "ignore",
      env: {
        ...process.env,
        CANHARBOR_CONFIG_DIR: `${isolatedRuntime}/preferences`,
      },
    },
  );
  const exited = new Promise((resolve) => service.once("exit", resolve));
  try {
    await expect
      .poll(() => {
        try {
          return !!pairing(other);
        } catch {
          return false;
        }
      })
      .toBe(true);
    const next = pairing(other);
    expect(new URL(next).port).not.toBe(new URL(page.url()).port);
    await page.goto(next);
    await expect(
      page.getByRole("heading", { name: "总线工作台", exact: true }),
    ).toBeVisible();
    await expect(page.getByLabel("语言", { exact: true })).toHaveValue("zh-CN");
    expect(errors).toEqual([]);
  } finally {
    service.kill("SIGTERM");
    await exited;
    rmSync(other, { recursive: true, force: true });
  }
});
