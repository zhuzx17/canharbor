// SPDX-License-Identifier: AGPL-3.0-or-later
import { formatNumber } from "./i18n/format";
import { stateText, text } from "./i18n/messages";
import type { Session } from "./api";
export interface EditableConfiguration {
  mode: "active" | "listen_only";
  nominalBitrate: number;
  fdEnabled: boolean;
  dataBitrate: number;
}
export function sessionConfiguration(session: Session): EditableConfiguration {
  return {
    mode: session.config.mode,
    nominalBitrate: session.config.nominal_bitrate,
    fdEnabled: session.config.data_bitrate !== null,
    dataBitrate: session.config.data_bitrate ?? 2000000,
  };
}
export function sameConfiguration(
  a: EditableConfiguration,
  b: EditableConfiguration,
) {
  return (
    a.mode === b.mode &&
    a.nominalBitrate === b.nominalBitrate &&
    a.fdEnabled === b.fdEnabled &&
    (!a.fdEnabled || a.dataBitrate === b.dataBitrate)
  );
}
export function describeConfiguration(value: EditableConfiguration) {
  return text("configuration:summary", {
    mode: stateText(value.mode),
    nominal: formatNumber(value.nominalBitrate),
    protocol: value.fdEnabled
      ? text("configuration:fd", { rate: formatNumber(value.dataBitrate) })
      : text("configuration:classic"),
  });
}
