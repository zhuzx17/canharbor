// SPDX-License-Identifier: AGPL-3.0-or-later
import { useEffect, useState } from "react";
import type { StressFlow } from "./generated/api";
import { useTranslation } from "./i18n";
import { text } from "./i18n/messages";
export function FlowEditor({
  flow,
  channels,
  onChange,
  onRemove,
}: {
  flow: StressFlow;
  channels: { id: string; label: string }[];
  onChange: (value: StressFlow) => void;
  onRemove: () => void;
}) {
  const [identifier, setIdentifier] = useState(
    Number.isInteger(flow.can_id) && flow.can_id >= 0
      ? flow.can_id.toString(16).toUpperCase()
      : "",
  );
  useEffect(() => {
    if (flow.can_id >= 0) setIdentifier(flow.can_id.toString(16).toUpperCase());
  }, [flow.can_id]);
  useTranslation();
  return (
    <fieldset
      aria-label={text("flow:flow_value", {
        value: flow.name || text("flow:unnamed_flow"),
      })}
    >
      <legend>{flow.name || text("flow:unnamed_flow")}</legend>
      <div className="flow-editor">
        <label>
          {text("flow:flow_name")}
          <input
            maxLength={32}
            value={flow.name ?? ""}
            onChange={(e) => onChange({ ...flow, name: e.target.value })}
          />
        </label>
        <label>
          {text("flow:sender")}
          <select
            value={flow.sender ?? ""}
            onChange={(e) =>
              onChange({
                ...flow,
                sender: e.target.value,
                receivers: (flow.receivers ?? []).filter(
                  (id) => id !== e.target.value,
                ),
              })
            }
          >
            {channels.map((c) => (
              <option key={c.id} value={c.id}>
                {c.label}
              </option>
            ))}
          </select>
        </label>
        <label>
          {text("flow:id_hex")}
          <input
            value={identifier}
            aria-invalid={!/^[0-9a-f]+$/i.test(identifier)}
            onChange={(e) => {
              setIdentifier(e.target.value);
              onChange({
                ...flow,
                can_id: /^[0-9a-f]+$/i.test(e.target.value)
                  ? parseInt(e.target.value, 16)
                  : -1,
              });
            }}
          />
        </label>
        <label>
          {text("flow:identifier_width")}
          <select
            value={flow.extended ? "29" : "11"}
            onChange={(e) =>
              onChange({ ...flow, extended: e.target.value === "29" })
            }
          >
            <option value="11">{text("flow:11_bit")}</option>
            <option value="29">{text("flow:29_bit")}</option>
          </select>
        </label>
        {!flow.stages?.length && (
          <>
            <label>
              {text("flow:frame_count")}
              <input
                type="number"
                min={1}
                value={flow.count ?? 0}
                onChange={(e) =>
                  onChange({ ...flow, count: Number(e.target.value) })
                }
              />
            </label>
            <label>
              {text("flow:rate_frames_s")}
              <input
                type="number"
                min={1}
                value={flow.fps ?? 0}
                onChange={(e) =>
                  onChange({ ...flow, fps: Number(e.target.value) })
                }
              />
            </label>
          </>
        )}
      </div>
      <p>{text("flow:expected_receivers")}</p>
      {channels
        .filter((c) => c.id !== flow.sender)
        .map((c) => (
          <label key={c.id} className="stress-check">
            <input
              type="checkbox"
              checked={(flow.receivers ?? []).includes(c.id)}
              onChange={(e) =>
                onChange({
                  ...flow,
                  receivers: e.target.checked
                    ? [...(flow.receivers ?? []), c.id]
                    : (flow.receivers ?? []).filter((id) => id !== c.id),
                })
              }
            />
            {c.label}
          </label>
        ))}
      {!!flow.stages?.length && (
        <p>
          {text("flow:this_flow_uses")}
          {flow.stages.length}
          {text("flow:stages_edit_their_timing_in_advanced_json")}
        </p>
      )}
      <button onClick={onRemove}>{text("flow:remove_flow")}</button>
    </fieldset>
  );
}
