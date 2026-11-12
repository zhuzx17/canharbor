import { nativeBinary } from "./native";
// SPDX-License-Identifier: AGPL-3.0-or-later
import { test, expect } from "./isolated-service";
import { execFileSync } from "node:child_process";
import { resolve } from "node:path";
import fixtures from "../../api/capture-fixtures.json" with { type: "json" };
const binary = nativeBinary;
test("recording drafts and periodic task results survive language changes without repeating admission", async ({
  page,
  isolatedRuntime,
}) => {
  const link = execFileSync(
    binary,
    ["--runtime-dir", isolatedRuntime, "web", "open", "--print"],
    { encoding: "utf8" },
  ).trim();
  await page.goto(link);
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
  const panel = page.locator(".task-panel");
  await panel.getByLabel("Frame count", { exact: true }).fill("1");
  let admissions = 0;
  page.on("request", (r) => {
    if (
      r.method() === "POST" &&
      /\/sessions\/[^/]+\/tasks$/.test(new URL(r.url()).pathname)
    )
      admissions++;
  });
  await panel
    .getByRole("button", { name: "Start periodic task", exact: true })
    .click();
  await expect(panel.locator(".task-result")).toContainText("State: completed");
  await page.getByRole("button", { name: "Recordings", exact: true }).click();
  await page.getByLabel("Recording name (optional)").fill("台架 run 01");
  await page.getByLabel("Language", { exact: true }).selectOption("zh-CN");
  await expect(page.getByLabel("录制名称（可选）")).toHaveValue("台架 run 01");
  await page.getByRole("button", { name: "实时流量", exact: true }).click();
  await expect(panel.locator(".task-result")).toContainText("已完成");
  await expect(panel.locator(".task-message")).toContainText("任务已受理");
  await page.getByLabel("语言", { exact: true }).selectOption("en");
  await expect(panel.locator(".task-message")).toContainText("Task admitted");
  expect(admissions).toBe(1);
});
test("offline rows, filters and validation errors translate without reloading the file", async ({
  page,
  isolatedRuntime,
}) => {
  const link = execFileSync(
    binary,
    ["--runtime-dir", isolatedRuntime, "web", "open", "--print"],
    { encoding: "utf8" },
  ).trim();
  await page.goto(link);
  await expect(page.getByText("Browser paired", { exact: true })).toBeVisible();
  await page
    .getByRole("button", { name: "Offline analysis", exact: true })
    .click();
  const viewer = page.locator(".offline-panel");
  const bytes = fixtures.valid[0].records
    .map((r) => JSON.stringify(r) + "\n")
    .join("");
  await viewer.getByLabel("Local capture file").setInputFiles({
    name: "双语.ndjson",
    mimeType: "application/x-ndjson",
    buffer: Buffer.from(bytes),
  });
  await expect(viewer.getByTestId("offline-summary")).toContainText(
    "Complete recording",
  );
  const rawCells = viewer.locator("tbody tr").first().locator("td");
  const cellsBefore = (await rawCells.allTextContents()).slice(0, 5);
  await viewer.getByLabel("Filter ID (hex, contains)").fill("ABC");
  await page.getByLabel("Language", { exact: true }).selectOption("zh-CN");
  await expect(viewer.getByTestId("offline-summary")).toContainText("完整录制");
  await expect(viewer.getByLabel("筛选 ID（十六进制，包含）")).toHaveValue(
    "ABC",
  );
  expect((await rawCells.allTextContents()).slice(0, 5)).toEqual(cellsBefore);
  await expect(viewer.locator("tbody")).toContainText("原始微秒");
  await viewer.getByLabel("本地录制文件").setInputFiles({
    name: "invalid.ndjson",
    mimeType: "application/x-ndjson",
    buffer: Buffer.from("{}\n"),
  });
  await expect(viewer.getByRole("alert")).toContainText("不支持的录制记录");
  await page.getByLabel("语言", { exact: true }).selectOption("en");
  await expect(viewer.getByRole("alert")).toContainText(
    "Unsupported capture record",
  );
});
