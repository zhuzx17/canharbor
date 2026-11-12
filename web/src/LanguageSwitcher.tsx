// SPDX-License-Identifier: AGPL-3.0-or-later
import { useRef, useState } from "react";
import type { Auth } from "./api";
import { api } from "./api";
import i18n, { useTranslation } from "./i18n";
import {
  initialPreference,
  preferenceKey,
  resolveLocale,
  type Preference,
} from "./i18n/locale";
export function LanguageSwitcher({ auth }: { auth: Auth | null }) {
  const { t } = useTranslation();
  const [preference, setPreference] = useState<Preference>(initialPreference);
  const [failed, setFailed] = useState(false);
  const [saving, setSaving] = useState(false);
  const changing = useRef(false);
  async function change(next: Preference) {
    if (changing.current) return;
    changing.current = true;
    setSaving(true);
    setFailed(false);
    const locale = resolveLocale(next, navigator.languages);
    try {
      await i18n.changeLanguage(locale);
      document.documentElement.lang = locale;
      setPreference(next);
      try {
        localStorage.setItem(preferenceKey, next);
      } catch {
        /* The service still saves the default. */
      }
      if (!auth) {
        setFailed(true);
        return;
      }
      await api(
        "ui_preferences",
        "/preferences",
        "PUT",
        { locale: next },
        auth,
      );
    } catch {
      setFailed(true);
    } finally {
      changing.current = false;
      setSaving(false);
    }
  }
  return (
    <div className="language-switcher">
      <label htmlFor="ui-language">{t("language")}</label>
      <select
        id="ui-language"
        disabled={saving}
        value={preference}
        onChange={(e) => void change(e.target.value as Preference)}
      >
        <option value="auto">{t("auto")}</option>
        <option value="zh-CN">简体中文</option>
        <option value="en">English</option>
      </select>
      {failed && <p role="status">{t("saveFailed")}</p>}
    </div>
  );
}
