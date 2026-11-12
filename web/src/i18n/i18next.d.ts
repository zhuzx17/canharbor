// SPDX-License-Identifier: AGPL-3.0-or-later
import "i18next";
import type { resources } from "./index";
declare module "i18next" {
  interface CustomTypeOptions {
    defaultNS: "common";
    resources: (typeof resources)["en"];
    strictKeyChecks: true;
  }
}
