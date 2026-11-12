// SPDX-License-Identifier: AGPL-3.0-or-later
import i18n from "./index";
const locale = () => (i18n.resolvedLanguage === "zh-CN" ? "zh-CN" : "en");
const numbers = {
  en: new Intl.NumberFormat("en"),
  "zh-CN": new Intl.NumberFormat("zh-CN"),
};
export const formatNumber = (value: number | bigint) =>
  numbers[locale()].format(value);
export const formatTime = (value: Date) => value.toLocaleTimeString(locale());
export const formatDateTime = (value: Date) => value.toLocaleString(locale());
