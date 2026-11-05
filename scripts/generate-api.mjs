// SPDX-License-Identifier: AGPL-3.0-or-later
import { spawnSync } from "node:child_process";
import { readFile, writeFile } from "node:fs/promises";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";
const root = fileURLToPath(new URL("../", import.meta.url));
const require = createRequire(new URL("../web/package.json", import.meta.url));
const { compile } = require("json-schema-to-typescript");
const prettier = require("prettier");
const check = process.argv.includes("--check");
const schemaFile = new URL("../api/schema.json", import.meta.url);
const typesFile = new URL("../web/src/generated/api.ts", import.meta.url);
const cargo = spawnSync(
  "cargo",
  [
    "run",
    "--quiet",
    "--locked",
    "-p",
    "canharbor-server",
    "--bin",
    "export-api",
    "--",
    ...(check ? ["--check"] : []),
    fileURLToPath(schemaFile),
  ],
  { cwd: root, stdio: "inherit" },
);
if (cargo.status !== 0) process.exit(cargo.status ?? 1);
const schema = JSON.parse(await readFile(schemaFile, "utf8"));
// TypeScript describes object shape; conditional CAN invariants are enforced
// by the original JSON Schema at runtime, not index signatures from allOf.
const typeSchema = structuredClone(schema);
for (const name of ["FrameInput", "Frame"])
  delete typeSchema.definitions[name].allOf;
const output = await prettier.format(
  await compile(typeSchema, "CanHarborContract", {
    bannerComment:
      "// SPDX-License-Identifier: AGPL-3.0-or-later\n// Generated from Rust wire DTOs. Run bun run --cwd web api:generate. Do not edit.",
    additionalProperties: false,
    unreachableDefinitions: true,
    unknownAny: true,
  }),
  { parser: "typescript" },
);
if (check) {
  if ((await readFile(typesFile, "utf8")) !== output)
    throw new Error(
      "Generated TypeScript is stale; run bun run --cwd web api:generate",
    );
} else await writeFile(typesFile, output);

// Compile validators during the build. Browsers must not compile schemas with
// new Function: the workbench intentionally keeps a CSP without unsafe-eval.
const Ajv = require("ajv");
const standaloneCode = require("ajv/dist/standalone").default;
const { build } = require("esbuild");
const ajv = new Ajv({
  allErrors: false,
  inlineRefs: false,
  strict: false,
  validateFormats: false,
  code: { source: true, esm: true, lines: true },
});
ajv.addSchema(schema, "canharbor");
const names = Object.keys(schema.properties).sort();
const refs = Object.fromEntries(
  names.map((name) => [name, `canharbor#/properties/${name}`]),
);
const bundled = await build({
  absWorkingDir: fileURLToPath(new URL("../web/", import.meta.url)),
  stdin: {
    contents: standaloneCode(ajv, refs),
    resolveDir: fileURLToPath(new URL("../web/", import.meta.url)),
    sourcefile: "api-validators.js",
  },
  bundle: true,
  write: false,
  format: "esm",
  platform: "browser",
  target: "es2022",
  legalComments: "inline",
});
const header =
  "// SPDX-License-Identifier: AGPL-3.0-or-later\n// Generated from Rust wire DTOs. Do not edit.\n";
const thirdPartyLicenses = await Promise.all(
  [
    ["ajv", "LICENSE"],
    ["esbuild", "LICENSE.md"],
  ].map(
    async ([name, file]) =>
      `/*! ${name} runtime helpers\n${await readFile(require.resolve(`${name}/${file}`), "utf8")}*/\n`,
  ),
);
const javascript = await prettier.format(
  header + thirdPartyLicenses.join("\n") + bundled.outputFiles[0].text,
  {
    parser: "babel",
  },
);
const declarations = await prettier.format(
  header +
    'import type { Contract } from "./api";\ninterface Validator<T> { (value: unknown): value is T; errors?: readonly { instancePath?: string; message?: string }[] | null; }\n' +
    names
      .map((name) => `export const ${name}: Validator<Contract["${name}"]>;`)
      .join("\n"),
  { parser: "typescript" },
);
for (const [file, output] of [
  ["api-validators.js", javascript],
  ["api-validators.d.ts", declarations],
]) {
  const url = new URL(`../web/src/generated/${file}`, import.meta.url);
  if (check) {
    if ((await readFile(url, "utf8")) !== output)
      throw new Error(`${file} is stale; run bun run --cwd web api:generate`);
  } else await writeFile(url, output);
}
