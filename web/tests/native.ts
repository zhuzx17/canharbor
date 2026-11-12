// SPDX-License-Identifier: AGPL-3.0-or-later
import { randomUUID } from "node:crypto";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
export const nativeBinary = resolve(
  `../target/debug/canharbor${process.platform === "win32" ? ".exe" : ""}`,
);
export const nativeRuntime =
  process.env.CANHARBOR_TEST_RUNTIME ??
  join(
    process.platform === "win32" ? tmpdir() : "/tmp",
    "canharbor-playwright",
  );
export function temporaryRuntime(prefix: string): string {
  // Let the service create this path with its private ACL. Node mkdir's
  // POSIX mode cannot create the required protected Windows DACL.
  return join(
    process.platform === "win32" ? tmpdir() : "/tmp",
    `${prefix}${randomUUID()}`,
  );
}
