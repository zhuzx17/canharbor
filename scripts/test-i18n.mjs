// SPDX-License-Identifier: AGPL-3.0-or-later
import assert from "node:assert/strict";
import { isPreference, resolveLocale } from "../web/src/i18n/locale.ts";
assert.equal(resolveLocale("auto", ["zh-Hant-TW", "en-US"]), "zh-CN");
assert.equal(resolveLocale("auto", ["en-GB", "zh-CN"]), "en");
assert.equal(resolveLocale("auto", ["fr-FR"]), "en");
assert.equal(resolveLocale("en", ["zh-CN"]), "en");
assert.equal(resolveLocale("zh-CN", ["en-US"]), "zh-CN");
assert.equal(resolveLocale("auto", []), "en");
assert.equal(isPreference("../../en"), false);
assert.equal(isPreference(null), false);
assert.equal(isPreference("zh-CN"), true);
console.log("Locale selection and invalid preference checks passed.");
