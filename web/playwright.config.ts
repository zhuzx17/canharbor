// SPDX-License-Identifier: AGPL-3.0-or-later
import { defineConfig } from "@playwright/test";
import { nativeBinary, nativeRuntime } from "./tests/native";
export default defineConfig({
  testDir: "./tests",
  workers: 1,
  fullyParallel: false,
  timeout: 30000,
  use: {
    baseURL: "http://127.0.0.1:32189",
    headless: true,
    trace: "retain-on-failure",
    screenshot: "only-on-failure",
    browserName:
      process.env.CANHARBOR_BROWSER === "webkit" ? "webkit" : "chromium",
  },
  webServer: {
    command: `"${nativeBinary}" --runtime-dir "${nativeRuntime}" serve --port 32189`,
    url: "http://127.0.0.1:32189/api/v1/version",
    reuseExistingServer: false,
    timeout: 30000,
  },
});
