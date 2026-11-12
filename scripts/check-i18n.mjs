// SPDX-License-Identifier: AGPL-3.0-or-later
import { readFileSync, readdirSync } from "node:fs";
import { resolve } from "node:path";
import assert from "node:assert/strict";
const root = resolve(import.meta.dirname, "../web/src/i18n/locales");
const namespaces = readdirSync(`${root}/en`)
  .filter((f) => f.endsWith(".json"))
  .sort();
assert.deepEqual(readdirSync(`${root}/zh-CN`).sort(), namespaces);
function leaves(object, prefix = "") {
  return Object.entries(object).flatMap(([key, value]) => {
    const name = prefix ? `${prefix}.${key}` : key;
    if (typeof value === "object" && value !== null && !Array.isArray(value))
      return leaves(value, name);
    assert.equal(typeof value, "string", `Non-string message: ${name}`);
    assert.ok(value.trim(), `Empty message: ${name}`);
    return [[name, value]];
  });
}
const variables = (text) =>
  [
    ...new Set(
      [...text.matchAll(/\{\{\s*([\w]+)(?:\s*,[^}]*)?\s*\}\}/g)].map(
        (m) => m[1],
      ),
    ),
  ].sort();
let count = 0;
for (const namespace of namespaces) {
  const en = new Map(
    leaves(JSON.parse(readFileSync(`${root}/en/${namespace}`, "utf8"))),
  );
  const zh = new Map(
    leaves(JSON.parse(readFileSync(`${root}/zh-CN/${namespace}`, "utf8"))),
  );
  assert.deepEqual(
    [...en.keys()].sort(),
    [...zh.keys()].sort(),
    `Message keys differ: ${namespace}`,
  );
  for (const [key, value] of en)
    assert.deepEqual(
      variables(value),
      variables(zh.get(key)),
      `Interpolation differs: ${namespace}:${key}`,
    );
  count += en.size;
}
console.log(
  `Checked ${count} messages across ${namespaces.length} namespaces in both languages.`,
);
