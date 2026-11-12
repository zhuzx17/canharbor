// SPDX-License-Identifier: AGPL-3.0-or-later
export type Locale = "en" | "zh-CN";
export type Preference = import("../generated/api").UiLocale;
export const preferenceKey = "canharbor.locale.v1";
export function isPreference(value: unknown): value is Preference {
  return value === "auto" || value === "en" || value === "zh-CN";
}
export function resolveLocale(
  preference: Preference,
  languages: readonly string[],
): Locale {
  if (preference !== "auto") return preference;
  for (const language of languages) {
    if (/^zh(?:-|$)/i.test(language)) return "zh-CN";
    if (/^en(?:-|$)/i.test(language)) return "en";
  }
  return "en";
}
export function initialPreference(): Preference {
  try {
    const saved = localStorage.getItem(preferenceKey);
    if (isPreference(saved)) return saved;
  } catch {
    /* Storage can be unavailable. */
  }
  const saved = document.documentElement.dataset.uiLocale;
  return isPreference(saved) ? saved : "auto";
}
