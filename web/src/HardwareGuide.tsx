// SPDX-License-Identifier: AGPL-3.0-or-later
import { useState } from "react";
import { useTranslation } from "./i18n";
import { text } from "./i18n/messages";
export function HardwareGuide() {
  useTranslation();
  const [backend, setBackend] = useState("gsusb");
  return (
    <details className="hardware-guide">
      <summary>{text("hardware:connect_a_hardware_adapter")}</summary>
      <p>
        {text(
          "hardware:the_channel_list_shows_adapters_registered_by_this_service",
        )}
      </p>
      <label>
        {text("hardware:adapter_backend")}
        <select value={backend} onChange={(e) => setBackend(e.target.value)}>
          <option value="gsusb">{text("hardware:gs_usb_usb")}</option>
          <option value="slcan">{text("hardware:slcan_serial")}</option>
          <option value="peak">PEAK</option>
          <option value="kvaser">Kvaser</option>
        </select>
      </label>
      {backend === "gsusb" ? (
        <>
          <p>{text("hardware:1_find_your_adapter_s_vendor_product_id_then")}</p>
          <pre>canharbor usb-list --vid-pid VENDOR:PRODUCT</pre>
          <p>
            {text(
              "hardware:2_use_the_reported_index_interface_and_alternate_setting",
            )}
          </p>
          <pre>
            canharbor serve --open --usb VENDOR:PRODUCT --usb-index INDEX
            --usb-interface INTERFACE --usb-alternate ALTERNATE
          </pre>
        </>
      ) : backend === "slcan" ? (
        <>
          <p>
            {text("hardware:find_the_serial_device_path_and_firmware_s_serial")}
          </p>
          <pre>
            canharbor serve --open --slcan /dev/cu.YOUR_ADAPTER --serial-baud
            115200 --slcan-profile lawicel
          </pre>
          <p>
            {text(
              "hardware:for_canable_fd_firmware_use_slcan_profile_canable_fd",
            )}
          </p>
        </>
      ) : (
        <>
          <p>
            {text(
              "hardware:install_the_vendor_s_compatible_native_library_for_this",
            )}
            {backend}
            {text("hardware:backend")}
          </p>
          <pre>{`canharbor ${backend}-list --library /absolute/path/to/vendor-library\ncanharbor serve --open --${backend}-library /absolute/path/to/vendor-library --${backend}-channel HANDLE`}</pre>
        </>
      )}
      <p>
        {text(
          "hardware:replace_the_uppercase_placeholders_with_your_device_values_stop",
        )}
      </p>
      <ul>
        <li>
          {text("hardware:not_listed_by_discovery_check_cable_power_device_id")}
        </li>
        <li>
          {text(
            "hardware:discovered_but_no_channel_here_check_service_startup_options",
          )}
        </li>
        <li>
          {text(
            "hardware:channel_listed_but_cannot_open_inspect_firmware_capability_and",
          )}
        </li>
      </ul>
      <p>
        {text("hardware:read_only_diagnostic_export")}{" "}
        <code>canharbor doctor --devices &gt; report.json</code>
        {text("hardware:review_device_labels_before_sharing")}
      </p>
    </details>
  );
}
