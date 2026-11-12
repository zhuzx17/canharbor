import { nativeBinary } from "./native";
// SPDX-License-Identifier: AGPL-3.0-or-later
import { test, expect } from "./isolated-service";
import { execFileSync } from "node:child_process";
import { resolve } from "node:path";
const pairing = (runtime: string) =>
  execFileSync(
    nativeBinary,
    ["--runtime-dir", runtime, "web", "open", "--print"],
    { encoding: "utf8" },
  ).trim();
test("saved language works when browser storage is unavailable", async ({
  page,
  isolatedRuntime,
}) => {
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(e.message));
  await page.addInitScript(() =>
    Object.defineProperty(window, "localStorage", {
      get() {
        throw new DOMException("blocked", "SecurityError");
      },
    }),
  );
  await page.goto(pairing(isolatedRuntime));
  await expect(page.getByText("Browser paired", { exact: true })).toBeVisible();
  const saved = page.waitForResponse(
    (r) => r.url().endsWith("/preferences") && r.request().method() === "PUT",
  );
  await page.getByLabel("Language", { exact: true }).selectOption("zh-CN");
  expect((await saved).status()).toBe(200);
  await page.reload();
  await expect(page.getByText("浏览器已配对", { exact: true })).toBeVisible();
  await expect(page.getByLabel("语言", { exact: true })).toHaveValue("zh-CN");
  expect(errors).toEqual([]);
});
test("a failed save leaves language switching usable without operational mutations", async ({
  page,
  isolatedRuntime,
}) => {
  await page.goto(pairing(isolatedRuntime));
  await expect(page.getByText("Browser paired", { exact: true })).toBeVisible();
  const writes: string[] = [];
  page.on("request", (r) => {
    if (["POST", "PUT", "DELETE", "PATCH"].includes(r.method()))
      writes.push(new URL(r.url()).pathname);
  });
  await page.route("**/api/v1/preferences", (r) =>
    r.request().method() === "PUT" ? r.abort() : r.continue(),
  );
  await page.getByLabel("Language", { exact: true }).selectOption("zh-CN");
  await expect(
    page.getByText("语言已切换，但偏好未能保存。", { exact: true }),
  ).toBeVisible();
  await expect(page.getByLabel("语言", { exact: true })).toBeEnabled();
  await page.getByLabel("语言", { exact: true }).selectOption("en");
  await expect(
    page.getByText("Language changed, but the preference could not be saved.", {
      exact: true,
    }),
  ).toBeVisible();
  expect(writes).toEqual(["/api/v1/preferences", "/api/v1/preferences"]);
});
test("Auto follows the browser language and translated views fit a narrow viewport", async ({
  page,
  isolatedRuntime,
}) => {
  await page.addInitScript(() =>
    Object.defineProperty(navigator, "languages", { value: ["zh-CN", "en"] }),
  );
  const external: string[] = [];
  page.on("request", (r) => {
    if (new URL(r.url()).hostname !== "127.0.0.1") external.push(r.url());
  });
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto(pairing(isolatedRuntime));
  await expect(page.getByText("浏览器已配对", { exact: true })).toHaveText(
    "浏览器已配对",
  );
  await expect(page.getByLabel("语言", { exact: true })).toHaveValue("auto");
  for (const name of ["实时流量", "录制", "离线分析", "压力测试"]) {
    await page.getByRole("button", { name, exact: true }).click();
    expect(
      await page.evaluate(
        () => document.documentElement.scrollWidth <= window.innerWidth + 1,
      ),
      name,
    ).toBe(true);
  }
  await page.getByLabel("语言", { exact: true }).selectOption("en");
  await expect(page.locator("html")).toHaveAttribute("lang", "en");
  await expect(page.getByLabel("Language", { exact: true })).toBeEnabled();
  await page.getByLabel("Language", { exact: true }).selectOption("auto");
  await expect(page.locator("html")).toHaveAttribute("lang", "zh-CN");
  expect(external).toEqual([]);
});
