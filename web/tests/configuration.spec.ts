// SPDX-License-Identifier: AGPL-3.0-or-later
import { expect, test } from "@playwright/test";
import { sameConfiguration, describeConfiguration } from "../src/configuration";
test("configuration comparison distinguishes unapplied edits and ignores inactive FD rate", () => {
  const current = {
    mode: "listen_only" as const,
    nominalBitrate: 500000,
    fdEnabled: false,
    dataBitrate: 2000000,
  };
  expect(sameConfiguration(current, { ...current, dataBitrate: 4000000 })).toBe(
    true,
  );
  expect(sameConfiguration(current, { ...current, mode: "active" })).toBe(
    false,
  );
  expect(
    sameConfiguration(current, { ...current, nominalBitrate: 250000 }),
  ).toBe(false);
  expect(sameConfiguration(current, { ...current, fdEnabled: true })).toBe(
    false,
  );
  expect(describeConfiguration(current)).toContain("Listen only");
  expect(describeConfiguration(current)).toContain("Classic CAN");
});
