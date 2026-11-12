// SPDX-License-Identifier: AGPL-3.0-or-later
import i18n from "i18next";
import { initReactI18next } from "react-i18next";
import { initialPreference, resolveLocale } from "./locale";
import capture_en from "./locales/en/capture.json";
import capture_zh from "./locales/zh-CN/capture.json";
import common_en from "./locales/en/common.json";
import common_zh from "./locales/zh-CN/common.json";
import configuration_en from "./locales/en/configuration.json";
import configuration_zh from "./locales/zh-CN/configuration.json";
import errorRecovery_en from "./locales/en/errorRecovery.json";
import errorRecovery_zh from "./locales/zh-CN/errorRecovery.json";
import errors_en from "./locales/en/errors.json";
import errors_zh from "./locales/zh-CN/errors.json";
import flow_en from "./locales/en/flow.json";
import flow_zh from "./locales/zh-CN/flow.json";
import hardware_en from "./locales/en/hardware.json";
import hardware_zh from "./locales/zh-CN/hardware.json";
import main_en from "./locales/en/main.json";
import main_zh from "./locales/zh-CN/main.json";
import offline_en from "./locales/en/offline.json";
import offline_zh from "./locales/zh-CN/offline.json";
import offlinePanel_en from "./locales/en/offlinePanel.json";
import offlinePanel_zh from "./locales/zh-CN/offlinePanel.json";
import recording_en from "./locales/en/recording.json";
import recording_zh from "./locales/zh-CN/recording.json";
import recovery_en from "./locales/en/recovery.json";
import recovery_zh from "./locales/zh-CN/recovery.json";
import states_en from "./locales/en/states.json";
import states_zh from "./locales/zh-CN/states.json";
import stress_en from "./locales/en/stress.json";
import stress_zh from "./locales/zh-CN/stress.json";
import taskPanel_en from "./locales/en/taskPanel.json";
import taskPanel_zh from "./locales/zh-CN/taskPanel.json";
import tasks_en from "./locales/en/tasks.json";
import tasks_zh from "./locales/zh-CN/tasks.json";
import transmission_en from "./locales/en/transmission.json";
import transmission_zh from "./locales/zh-CN/transmission.json";
import validation_en from "./locales/en/validation.json";
import validation_zh from "./locales/zh-CN/validation.json";
export const resources = {
  en: {
    capture: capture_en,
    common: common_en,
    configuration: configuration_en,
    errorRecovery: errorRecovery_en,
    errors: errors_en,
    flow: flow_en,
    hardware: hardware_en,
    main: main_en,
    offline: offline_en,
    offlinePanel: offlinePanel_en,
    recording: recording_en,
    recovery: recovery_en,
    states: states_en,
    stress: stress_en,
    taskPanel: taskPanel_en,
    tasks: tasks_en,
    transmission: transmission_en,
    validation: validation_en,
  },
  "zh-CN": {
    capture: capture_zh,
    common: common_zh,
    configuration: configuration_zh,
    errorRecovery: errorRecovery_zh,
    errors: errors_zh,
    flow: flow_zh,
    hardware: hardware_zh,
    main: main_zh,
    offline: offline_zh,
    offlinePanel: offlinePanel_zh,
    recording: recording_zh,
    recovery: recovery_zh,
    states: states_zh,
    stress: stress_zh,
    taskPanel: taskPanel_zh,
    tasks: tasks_zh,
    transmission: transmission_zh,
    validation: validation_zh,
  },
} as const;
export async function initializeLanguage() {
  const lng = resolveLocale(initialPreference(), navigator.languages);
  await i18n.use(initReactI18next).init({
    resources,
    lng,
    supportedLngs: ["en", "zh-CN"],
    fallbackLng: "en",
    defaultNS: "common",
    interpolation: { escapeValue: false },
    react: { useSuspense: false },
  });
  document.documentElement.lang = lng;
}
export { useTranslation } from "react-i18next";
export default i18n;
