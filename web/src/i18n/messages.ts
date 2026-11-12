// SPDX-License-Identifier: AGPL-3.0-or-later
import i18n, { resources } from "./index";
type Catalog = typeof resources.en;
export type MessageKey = {
  [N in keyof Catalog]: `${N}:${Extract<keyof Catalog[N], string>}`;
}[keyof Catalog];
export type Message = {
  key: MessageKey;
  values?: Readonly<Record<string, unknown>>;
};
export type UiMessage = string | Message;
export function uiMessage(
  key: MessageKey,
  values?: Readonly<Record<string, unknown>>,
): Message {
  return { key, values };
}
function interpolate(values?: Readonly<Record<string, unknown>>) {
  return Object.fromEntries(
    Object.entries(values ?? {}).map(([key, value]) => [
      key,
      value &&
      typeof value === "object" &&
      "key" in value &&
      typeof value.key === "string"
        ? displayMessage(value as Message)
        : value,
    ]),
  );
}
export function text(
  key: MessageKey,
  values?: Readonly<Record<string, unknown>>,
): string {
  const parameters = interpolate(values);
  if (!i18n.isInitialized) {
    const [namespace, name] = key.split(":");
    const fallback = (resources.en as Record<string, Record<string, string>>)[
      namespace
    ][name];
    return fallback.replace(/\{\{\s*(\w+)\s*\}\}/g, (match, name) =>
      parameters[name] === undefined ? match : String(parameters[name]),
    );
  }
  return i18n.t(key, parameters);
}
export function displayMessage(value: UiMessage): string {
  return typeof value === "string" ? value : text(value.key, value.values);
}
export class LocalizedError extends Error {
  constructor(
    readonly key: MessageKey,
    readonly values?: Readonly<Record<string, unknown>>,
  ) {
    super();
    this.name = "LocalizedError";
    Object.defineProperty(this, "message", {
      configurable: true,
      get: () => text(key, values),
    });
  }
}

export function stateMessage(value: string): UiMessage {
  return Object.hasOwn(resources.en.states, value)
    ? uiMessage(`states:${value}` as MessageKey)
    : value;
}
export const stateText = (value: string) => displayMessage(stateMessage(value));
