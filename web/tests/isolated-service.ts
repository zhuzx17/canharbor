import { nativeBinary, temporaryRuntime } from "./native";
// SPDX-License-Identifier: AGPL-3.0-or-later
import { test as base, expect } from "@playwright/test";
import { execFileSync, spawn } from "node:child_process";
import { resolve } from "node:path";
import { readdirSync, readFileSync, rmSync, statSync } from "node:fs";
export const test = base.extend<{ isolatedRuntime: string }>({
  // APIRequestContext resolves relative URLs against baseURL, not page.url().
  // Keep API assertions on the same service as the paired browser.
  baseURL: async ({ isolatedRuntime }, use) => {
    const pairing = execFileSync(
      nativeBinary,
      ["--runtime-dir", isolatedRuntime, "web", "open", "--print"],
      { encoding: "utf8" },
    ).trim();
    await use(new URL(pairing).origin);
  },
  isolatedRuntime: async ({}, use, testInfo) => {
    const runtime = temporaryRuntime("ch-ux-");
    const binary = nativeBinary;
    const service = spawn(
      binary,
      ["--runtime-dir", runtime, "serve", "--port", "0"],
      {
        stdio: ["ignore", "ignore", "pipe"],
        env: { ...process.env, CANHARBOR_CONFIG_DIR: `${runtime}/preferences` },
      },
    );
    let startupLog = "";
    service.stderr.on("data", (data) => {
      startupLog = (startupLog + data.toString()).slice(-8192);
    });
    const exited = new Promise((resolve) => service.once("exit", resolve));
    try {
      await expect
        .poll(() => {
          if (service.exitCode !== null)
            throw new Error(`Service exited before readiness: ${startupLog}`);
          try {
            execFileSync(binary, ["--runtime-dir", runtime, "devices"], {
              stdio: "ignore",
            });
            return true;
          } catch {
            return false;
          }
        })
        .toBe(true);
      await use(runtime);
    } finally {
      try {
        if (testInfo.status !== testInfo.expectedStatus) {
          await testInfo.attach("isolated-service-stderr", {
            body: startupLog,
            contentType: "text/plain",
          });
          // Only evidence metadata; never attach pairing/session credentials.
          const evidence = resolve(runtime, "captures", "stress");
          const inventory: unknown[] = [];
          try {
            for (const run of readdirSync(evidence).slice(0, 64)) {
              const directory = resolve(evidence, run);
              let names: string[];
              try {
                names = readdirSync(directory).slice(0, 1024);
              } catch (error) {
                inventory.push({ run, inspectionError: String(error) });
                continue;
              }
              for (const name of names) {
                try {
                  const path = resolve(directory, name);
                  const stat = statSync(path);
                  let checkpoint: unknown;
                  if (
                    ["checkpoint.json", "report.json"].includes(name) &&
                    stat.size <= 1024 * 1024
                  ) {
                    const report = JSON.parse(readFileSync(path, "utf8"));
                    checkpoint = {
                      state: report.state,
                      evidence: report.evidence,
                      errors: report.errors,
                    };
                  }
                  inventory.push({
                    run,
                    name,
                    bytes: stat.size,
                    modified: stat.mtimeMs,
                    checkpoint,
                  });
                } catch (error) {
                  inventory.push({ run, name, inspectionError: String(error) });
                }
              }
            }
          } catch (error) {
            inventory.push({ inspectionError: String(error) });
          }
          await testInfo.attach("isolated-evidence-inventory", {
            body: JSON.stringify(inventory, null, 2),
            contentType: "application/json",
          });
        }
      } finally {
        service.kill("SIGTERM");
        await exited;
        rmSync(runtime, { recursive: true, force: true });
      }
    }
  },
});

export { expect };
