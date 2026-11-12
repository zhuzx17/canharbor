import { nativeBinary } from "./native";
// SPDX-License-Identifier: AGPL-3.0-or-later
import { test, expect } from "./isolated-service";
import { execFileSync } from "node:child_process";
import { resolve } from "node:path";
const binary = nativeBinary;
test("language changes preserve a validated plan and an authorized virtual run", async ({
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
  await expect(page.getByText("Browser paired", { exact: true })).toBeVisible();
  await page
    .getByRole("button", { name: "Stress testing", exact: true })
    .click();
  const panel = page.locator(".stress-panel");
  await panel.getByLabel("Stress frame count", { exact: true }).fill("200");
  await panel.getByLabel("Stress target fps", { exact: true }).fill("20");
  let admitted: { plan_hash: string; plan: unknown } | undefined;
  let starts = 0,
    validations = 0;
  page.on("request", (r) => {
    if (r.method() === "POST") {
      if (new URL(r.url()).pathname === "/api/v1/stress/runs") {
        starts++;
        admitted = r.postDataJSON();
      }
      if (r.url().endsWith("/stress/validate")) validations++;
    }
  });
  let resolveValidation!: (value: { plan_hash: string; plan: unknown }) => void;
  const validationResult = new Promise<{ plan_hash: string; plan: unknown }>(
    (resolve) => {
      resolveValidation = resolve;
    },
  );
  // Capture the body before delivering it to the browser. Chromium's protocol
  // may no longer retain the resource body when waitForResponse is consumed.
  await page.route("**/api/v1/stress/validate", async (route) => {
    const response = await route.fetch();
    expect(response.status()).toBe(200);
    const value = await response.json();
    await route.fulfill({ response });
    resolveValidation(value);
  });
  await panel
    .getByRole("button", { name: "Validate stress plan", exact: true })
    .click();
  const validation = await validationResult;
  await expect(page.getByTestId("stress-validation")).toBeVisible();
  await panel
    .getByLabel("Authorize stress transmission", { exact: true })
    .check();
  await page.getByLabel("Language", { exact: true }).selectOption("zh-CN");
  await expect(page.getByTestId("stress-validation")).toBeVisible();
  await expect(
    panel.getByLabel("授权压力测试发送", { exact: true }),
  ).toBeChecked();
  await panel
    .getByRole("button", { name: "启动压力测试", exact: true })
    .click();
  await expect(panel.getByTestId("stress-report")).toBeVisible();
  const id = await panel
    .getByLabel("压力测试运行", { exact: true })
    .inputValue();
  const reportUrl = new URL(`/api/v1/stress/runs/${id}`, page.url()).href;
  expect(admitted?.plan_hash).toBe(validation.plan_hash);
  expect(admitted?.plan).toEqual(validation.plan);
  const before = await (await page.request.get(reportUrl)).json();
  expect(before.state).toBe("running");
  await page.getByLabel("语言", { exact: true }).selectOption("en");
  await expect(panel.getByLabel("Stress run", { exact: true })).toHaveValue(id);
  const after = await (await page.request.get(reportUrl)).json();
  expect(after.plan).toEqual(before.plan);
  expect(after.plan_hash).toBe(before.plan_hash);
  await expect
    .poll(
      async () => (await (await page.request.get(reportUrl)).json()).state,
      { timeout: 18000 },
    )
    .toBe("completed");
  await page.getByLabel("Language", { exact: true }).selectOption("zh-CN");
  await expect(panel.getByTestId("stress-report")).toContainText("已完成");
  expect({ starts, validations }).toEqual({ starts: 1, validations: 1 });
});
test("flow drafts and local validation errors follow language changes", async ({
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
  await expect(page.getByText("Browser paired", { exact: true })).toBeVisible();
  await page
    .getByRole("button", { name: "Stress testing", exact: true })
    .click();
  const panel = page.locator(".stress-panel");
  await panel.getByText("Advanced stress settings", { exact: true }).click();
  await panel
    .getByRole("button", { name: "Add reverse flow", exact: true })
    .click();
  const editor = panel.locator(".flow-editor");
  await editor.getByLabel("Flow name", { exact: true }).fill("反向 reverse");
  await editor.getByLabel("ID (hex)", { exact: true }).fill("ABC");
  await page.getByLabel("Language", { exact: true }).selectOption("zh-CN");
  await expect(editor.getByLabel("流名称", { exact: true })).toHaveValue(
    "反向 reverse",
  );
  await expect(
    editor.getByLabel("ID（十六进制）", { exact: true }),
  ).toHaveValue("ABC");
  await panel.getByText("高级 JSON", { exact: true }).click();
  await panel.getByLabel("压力测试附加流", { exact: true }).fill("{}");
  await expect(panel.getByRole("alert")).toContainText(
    "附加流必须是由对象组成的 JSON 数组",
  );
  await page.getByLabel("语言", { exact: true }).selectOption("en");
  await expect(panel.getByRole("alert")).toContainText(
    "Additional flows must be a JSON array of objects",
  );
});
