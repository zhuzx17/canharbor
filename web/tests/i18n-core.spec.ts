import { nativeBinary } from "./native";
// SPDX-License-Identifier: AGPL-3.0-or-later
import { test, expect } from "./isolated-service";
import { execFileSync } from "node:child_process";
import { resolve } from "node:path";
const binary = nativeBinary;
const cli = (runtime: string, ...args: string[]) =>
  execFileSync(binary, ["--runtime-dir", runtime, ...args], {
    encoding: "utf8",
  }).trim();
test("switching languages preserves an active session, stream and frame draft without transmitting", async ({
  page,
  isolatedRuntime,
}) => {
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  let streams = 0,
    transmissions = 0,
    pairs = 0;
  page.on("websocket", () => streams++);
  page.on("request", (request) => {
    if (
      request.method() === "POST" &&
      /\/tx$/.test(new URL(request.url()).pathname)
    )
      transmissions++;
    if (request.url().endsWith("/auth/pair")) pairs++;
  });
  await page.goto(cli(isolatedRuntime, "web", "open", "--print"));
  await expect(page.getByText("Browser paired", { exact: true })).toBeVisible();
  await page
    .getByRole("combobox", { name: "Channel", exact: true })
    .selectOption("virtual:1");
  await page.getByRole("button", { name: "Open channel", exact: true }).click();
  await page
    .getByRole("button", { name: "Acquire control", exact: true })
    .click();
  await page
    .getByRole("button", { name: "Start channel", exact: true })
    .click();
  await expect(page.getByText("Live", { exact: true })).toBeVisible();
  const session = await page
    .getByRole("combobox", { name: "Session", exact: true })
    .inputValue();
  const url = new URL("/api/v1/sessions", page.url()).href;
  const before = await (await page.request.get(url)).json();
  cli(
    isolatedRuntime,
    "send",
    "--channel",
    "virtual:0",
    "--id",
    "5A1",
    "--data",
    "CAFE",
    "--active",
  );
  await expect(
    page.getByRole("cell", { name: "5A1", exact: true }),
  ).toBeVisible();
  await page.getByLabel("Identifier").fill("BAD!");
  await expect(
    page.getByText("Enter a hexadecimal ID without a prefix.", { exact: true }),
  ).toBeVisible();
  await page.getByLabel("Language", { exact: true }).selectOption("zh-CN");
  await expect(page.getByText("浏览器已配对", { exact: true })).toBeVisible();
  await expect(page.getByText("实时接收", { exact: true })).toBeVisible();
  await expect(
    page.getByText("请输入不带前缀的十六进制 ID。", { exact: true }),
  ).toBeVisible();
  await expect(page.getByLabel("标识符")).toHaveValue("BAD!");
  await expect(
    page.getByRole("combobox", { name: "会话", exact: true }),
  ).toHaveValue(session);
  await expect(
    page.getByRole("cell", { name: "5A1", exact: true }),
  ).toBeVisible();
  cli(
    isolatedRuntime,
    "send",
    "--channel",
    "virtual:0",
    "--id",
    "5A2",
    "--data",
    "BABE",
    "--active",
  );
  await expect(
    page.getByRole("cell", { name: "5A2", exact: true }),
  ).toBeVisible();
  const after = await (await page.request.get(url)).json();
  expect(
    after.sessions.find((s: { id: string }) => s.id === session).config,
  ).toEqual(
    before.sessions.find((s: { id: string }) => s.id === session).config,
  );
  await expect(page.getByLabel("语言", { exact: true })).toBeEnabled();
  await page.getByLabel("语言", { exact: true }).selectOption("en");
  await expect(
    page.getByText("Enter a hexadecimal ID without a prefix.", { exact: true }),
  ).toBeVisible();
  expect({ streams, transmissions, pairs }).toEqual({
    streams: 1,
    transmissions: 0,
    pairs: 1,
  });
  expect(errors).toEqual([]);
});

test("an unpaired browser can translate recovery guidance without redeeming its pairing link again", async ({
  page,
  isolatedRuntime,
}) => {
  const mutations: string[] = [];
  page.on("request", (request) => {
    if (
      request.method() !== "GET" &&
      new URL(request.url()).pathname.startsWith("/api/v1/")
    )
      mutations.push(new URL(request.url()).pathname);
  });
  const origin = new URL(cli(isolatedRuntime, "web", "open", "--print")).origin;
  await page.goto(`${origin}/#pair=expired-fixture`);
  await expect(
    page.getByRole("alert", { name: "Operation recovery" }),
  ).toContainText("fresh pairing link");
  await page.getByLabel("Language", { exact: true }).selectOption("zh-CN");
  await expect(page.getByRole("alert", { name: "操作恢复" })).toContainText(
    "新的配对链接",
  );
  await expect(
    page.getByRole("button", { name: "检查连接与会话状态", exact: true }),
  ).toBeVisible();
  await expect(
    page.getByText("语言已切换，但偏好未能保存。", { exact: true }),
  ).toBeVisible();
  expect(mutations).toEqual(["/api/v1/auth/pair"]);
});

test("session status and mode labels translate without changing protocol values", async ({
  page,
  isolatedRuntime,
}) => {
  await page.goto(cli(isolatedRuntime, "web", "open", "--print"));
  await expect(page.getByText("Browser paired", { exact: true })).toBeVisible();
  await page.getByRole("button", { name: "Open channel", exact: true }).click();
  await expect(page.locator(".session-state")).toHaveText("configured");
  const selection = page.getByRole("combobox", {
    name: "Session",
    exact: true,
  });
  const id = await selection.inputValue();
  await expect(selection.locator(`option[value="${id}"]`)).toContainText(
    "Listen only",
  );
  await page.getByLabel("Language", { exact: true }).selectOption("zh-CN");
  await expect(page.locator(".session-state")).toHaveText("已配置");
  await expect(
    page
      .getByRole("combobox", { name: "会话", exact: true })
      .locator(`option[value="${id}"]`),
  ).toContainText("只听模式");
  await page.getByText("连接与控制详情", { exact: true }).click();
  await expect(page.getByTestId("channel-status-certainty")).toContainText(
    "已配置",
  );
  await page.getByRole("button", { name: "离线分析", exact: true }).click();
  await expect(
    page.getByRole("complementary", { name: "当前通道摘要" }),
  ).toContainText("已配置");
  const sessions = await (
    await page.request.get(new URL("/api/v1/sessions", page.url()).href)
  ).json();
  const session = sessions.sessions.find((s: { id: string }) => s.id === id);
  expect(session.state).toBe("configured");
  expect(session.mode).toBe("listen_only");
  await page.getByLabel("语言", { exact: true }).selectOption("en");
  await expect(
    page.getByRole("complementary", { name: "Active channel summary" }),
  ).toContainText("configured");
});
