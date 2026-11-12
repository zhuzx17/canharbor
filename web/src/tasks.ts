// SPDX-License-Identifier: AGPL-3.0-or-later
import { decode } from "./contract";
import type { PeriodicTask } from "./generated/api";
import { LocalizedError } from "./i18n/messages";
export type TaskAttempt = {
  session: string;
  request: string;
  generation: string;
};
export function readTaskAttempts(raw: string | null): TaskAttempt[] {
  if (!raw || raw.length > 16384) return [];
  try {
    const values: unknown = JSON.parse(raw);
    if (!Array.isArray(values) || values.length > 16) return [];
    return values.filter((v): v is TaskAttempt => {
      if (
        !v ||
        typeof v !== "object" ||
        Object.keys(v).sort().join() !== "generation,request,session" ||
        typeof v.session !== "string" ||
        !/^[A-Za-z0-9_-]{1,128}$/.test(v.session) ||
        typeof v.request !== "string" ||
        !/^[a-f0-9-]{36}$/.test(v.request)
      )
        return false;
      try {
        decode("counter", v.generation);
        return true;
      } catch {
        return false;
      }
    });
  } catch {
    return [];
  }
}
export function recoverTask(
  tasks: PeriodicTask[],
  attempt: TaskAttempt,
): PeriodicTask | undefined {
  const matches = tasks.filter(
    (t) =>
      t.session_id === attempt.session &&
      t.generation === attempt.generation &&
      t.client_request_id === attempt.request,
  );
  if (matches.length > 1)
    throw new LocalizedError("tasks:multiple_tasks_match_no_task_was_selected");
  return matches[0];
}
export function validateTaskPlan(count: number, interval: number) {
  if (
    !Number.isInteger(count) ||
    !Number.isInteger(interval) ||
    count < 1 ||
    count > 6000 ||
    interval < 10 ||
    interval > 60000 ||
    (count - 1) * interval > 60000
  )
    throw new LocalizedError("tasks:use_1_6000_slots_an_interval_of_10_60000");
}
export function taskTerminal(task: PeriodicTask) {
  return !["starting", "running", "cancelling"].includes(task.state);
}
