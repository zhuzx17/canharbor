// SPDX-License-Identifier: AGPL-3.0-or-later
import { useEffect, useRef, useState } from "react";
import { api, ApiFailure, type Auth, type FrameInput } from "./api";
import type { PeriodicTask } from "./generated/api";
import { useTranslation } from "./i18n";
import {
  displayMessage,
  stateMessage,
  stateText,
  text,
  uiMessage,
  type UiMessage,
} from "./i18n/messages";
import {
  readTaskAttempts,
  recoverTask,
  taskTerminal,
  validateTaskPlan,
  type TaskAttempt,
} from "./tasks";
export function TaskPanel(props: {
  auth: Auth | null;
  session: string;
  generation?: string;
  disabled: boolean;
  frame: () => FrameInput;
  onError: (message: unknown) => void;
  onControlLost: () => void;
}) {
  useTranslation();
  const { auth } = props;
  const [count, setCount] = useState(10);
  const [interval, setIntervalValue] = useState(100);
  const [tasks, setTasks] = useState<PeriodicTask[]>([]);
  const [loadedClient, setLoadedClient] = useState<string | null>(null);
  const [scope, setScope] = useState<{
    key: string;
    attempts: TaskAttempt[];
  } | null>(null);
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<UiMessage>("");
  const alive = useRef(true);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);
  async function refresh() {
    try {
      const result = await api("periodic_tasks", "/tasks");
      if (alive.current) setTasks(result.tasks);
      return result.tasks;
    } catch (e) {
      if (e instanceof ApiFailure && [401, 403].includes(e.status))
        props.onControlLost();
      throw e;
    }
  }
  useEffect(() => {
    let active = true;
    let timer: ReturnType<typeof setTimeout> | undefined;
    setScope(null);
    setTasks([]);
    if (!auth) return;
    const tick = async () => {
      try {
        const result = await api("periodic_tasks", "/tasks");
        if (active) {
          setTasks(result.tasks);
          setLoadedClient(auth.client_id);
        }
      } catch (e) {
        if (active) {
          props.onError(e);
          if (e instanceof ApiFailure && [401, 403].includes(e.status))
            props.onControlLost();
        }
      }
      if (active) timer = setTimeout(tick, 1000);
    };
    void (async () => {
      try {
        const version = await api("version", "/version");
        if (!active) return;
        const key = `canharbor:tasks:${version.server_instance_id}:${auth.client_id}`;
        let attempts: TaskAttempt[] = [];
        try {
          attempts = readTaskAttempts(sessionStorage.getItem(key));
        } catch {
          /* storage can be unavailable */
        }
        setScope({ key, attempts });
        await tick();
      } catch (e) {
        if (active) props.onError(e);
      }
    })();
    return () => {
      active = false;
      clearTimeout(timer);
    };
  }, [auth]);
  async function start() {
    if (!auth || !scope || !props.generation || props.disabled || busy) return;
    let attempt: TaskAttempt | undefined;
    setBusy(true);
    props.onError("");
    setMessage("");
    try {
      validateTaskPlan(count, interval);
      const frame = props.frame();
      attempt = {
        session: props.session,
        generation: props.generation,
        request: crypto.randomUUID(),
      };
      const attempts = [attempt, ...scope.attempts].slice(0, 16);
      // Save only recovery identifiers, before admission. Never restore a lease,
      // frame, CSRF token, create request, or a timer that transmits frames.
      setScope({ ...scope, attempts });
      try {
        sessionStorage.setItem(scope.key, JSON.stringify(attempts));
      } catch {
        /* in-memory recovery still works */
      }
      const task = await api(
        "periodic_task",
        `/sessions/${attempt.session}/tasks`,
        "POST",
        {
          client_request_id: attempt.request,
          frame,
          count,
          interval_ms: interval,
        },
        auth,
        attempt.generation,
      );
      if (alive.current) {
        setTasks((old) => [task, ...old.filter((t) => t.id !== task.id)]);
        setMessage(
          uiMessage(
            "taskPanel:task_admitted_dispatch_runs_in_the_service_this_page",
          ),
        );
      }
    } catch (e) {
      if (alive.current) {
        if (attempt && (!(e instanceof ApiFailure) || e.unknown)) {
          setMessage(
            uiMessage(
              "taskPanel:task_start_outcome_unknown_find_the_original_task_below",
            ),
          );
          props.onControlLost();
        }
        props.onError(e);
      }
    } finally {
      if (alive.current) setBusy(false);
    }
  }
  async function recover(attempt: TaskAttempt) {
    if (busy) return;
    setBusy(true);
    try {
      const found = recoverTask(await refresh(), attempt);
      if (alive.current)
        setMessage(
          found
            ? uiMessage("taskPanel:recovered", {
                id: found.id,
                state: stateMessage(found.state),
              })
            : uiMessage("taskPanel:notFound"),
        );
    } catch (e) {
      if (alive.current) props.onError(e);
    } finally {
      if (alive.current) setBusy(false);
    }
  }
  async function cancel(task: PeriodicTask) {
    if (!auth || busy) return;
    setBusy(true);
    try {
      const result = await api(
        "periodic_task",
        `/tasks/${task.id}/cancel`,
        "POST",
        {},
        auth,
      );
      if (alive.current) {
        setTasks((old) => old.map((t) => (t.id === result.id ? result : t)));
        setMessage(
          uiMessage(
            "taskPanel:cancellation_requested_inspect_the_final_task_result_issued_frames",
          ),
        );
      }
    } catch (e) {
      if (alive.current) {
        props.onError(e);
        setMessage(
          uiMessage(
            "taskPanel:cancellation_response_unavailable_refresh_the_task_result_cancellation_is",
          ),
        );
        props.onControlLost();
      }
    } finally {
      if (alive.current) setBusy(false);
    }
  }
  let planError = "";
  try {
    validateTaskPlan(count, interval);
  } catch (error) {
    planError = error instanceof Error ? error.message : String(error);
  }
  return (
    <section
      className="task-panel"
      aria-label={text("taskPanel:periodic_tasks")}
      aria-busy={!!auth && loadedClient !== auth.client_id}
    >
      <h3>{text("taskPanel:periodic_tasks")}</h3>
      <p>
        {text(
          "taskPanel:uses_the_frame_editor_above_requires_a_running_active",
        )}
      </p>
      <div className="button-row">
        <label>
          {text("taskPanel:frame_count")}
          <input
            aria-label={text("taskPanel:frame_count")}
            type="number"
            min={1}
            max={6000}
            value={count}
            onChange={(e) => setCount(Number(e.target.value))}
          />
        </label>
        <label>
          {text("taskPanel:task_interval_ms")}
          <input
            aria-label={text("taskPanel:task_interval")}
            type="number"
            min={10}
            max={60000}
            value={interval}
            onChange={(e) => setIntervalValue(Number(e.target.value))}
          />
        </label>
      </div>
      {planError ? (
        <p role="alert">{planError}</p>
      ) : (
        <p>
          {text("taskPanel:plan")}
          {count}
          {text("taskPanel:frames")}
          {interval}
          {text("taskPanel:ms_apart_last_dispatch_after")}{" "}
          {(((count - 1) * interval) / 1000).toFixed(2)}
          {text("taskPanel:seconds_skipped_slots_are_not_retried")}
        </p>
      )}
      <button
        disabled={!scope || !auth || props.disabled || busy || !!planError}
        onClick={() => void start()}
      >
        {text("taskPanel:start_periodic_task")}
      </button>
      <button
        disabled={!auth || busy}
        onClick={() => void refresh().catch((e) => props.onError(e))}
      >
        {text("taskPanel:refresh_tasks")}
      </button>
      <p className="task-message" aria-live="polite">
        {displayMessage(message)}
      </p>
      <p>
        {text(
          "taskPanel:reloading_restores_results_only_control_must_be_acquired_explicitly",
        )}
      </p>
      {tasks.map((task) => (
        <article
          key={task.id}
          data-testid={`task-${task.id}`}
          className="task-result"
        >
          <strong>
            {text("taskPanel:task")}
            {task.id}
          </strong>
          <div>
            {text("taskPanel:session")}
            {task.session_id}
            {text("taskPanel:generation")}
            {task.generation}
          </div>
          <div>
            {text("taskPanel:request")}
            {task.client_request_id}
          </div>
          <div>
            {text("taskPanel:state")}
            {stateText(task.state)} ·{" "}
            {task.simulated
              ? text("taskPanel:simulation")
              : text("taskPanel:hardware_unverified")}
          </div>
          <div>
            {text("taskPanel:submitted")}
            {task.submitted}
            {text("taskPanel:planned")}
            {task.planned}
            {text("taskPanel:skipped")} {task.skipped}
            {text("taskPanel:failed")}
            {task.failed}
            {text("taskPanel:unknown")}
            {task.unknown}
          </div>
          {task.reason && <div>{task.reason}</div>}
          <button
            disabled={busy || taskTerminal(task)}
            onClick={() => void cancel(task)}
          >
            {text("taskPanel:cancel_task")}
          </button>
        </article>
      ))}
      {scope?.attempts.map((attempt) => (
        <div
          key={`${attempt.session}:${attempt.generation}:${attempt.request}`}
          className="task-attempt"
        >
          <code>{attempt.request}</code>
          <button
            disabled={busy || !auth}
            onClick={() => void recover(attempt)}
          >
            {text("taskPanel:find_task")}
          </button>
        </div>
      ))}
    </section>
  );
}
