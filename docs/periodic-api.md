# Periodic task API

The authenticated local service exposes bounded periodic execution over an
already running active session. It does not create a session, start a stopped
controller, acquire/renew control, or reconnect a device. CLI controls are
documented in [periodic-cli.md](periodic-cli.md), browser
controls in [periodic-web.md](periodic-web.md), and SDK helpers in
[rust-client.md](rust-client.md) and the Python package README.

## Routes

- `POST /api/v1/sessions/{session_id}/tasks`: admit a plan, returning HTTP 202.
  Supply the current `X-Canharbor-Lease-Generation` and normal authentication.
- `GET /api/v1/tasks`: list retained tasks without changing control or sending.
- `GET /api/v1/tasks/{task_id}`: read a retained task.
- `POST /api/v1/tasks/{task_id}/cancel`: request idempotent cancellation. A
  cancelling response does not retract frames that were already issued.

Browser requests use pairing/cookie authentication, Origin checks and CSRF for
mutations. Browser task reads/cancellation are owner-scoped; another browser
receives 404 for an unowned task. Logout requests cancellation before revoking
that browser's runtime control. Same-user native clients may inspect and cancel
all retained tasks for recovery; this does not grant the ability to start a task
under another identity's lease.

A start request has this shape (ordinary frame validation applies):

```json
{
  "client_request_id": "periodic-example-1",
  "frame": { "kind": "classic_data", "id": 291, "data_hex": "0102" },
  "interval_ms": 10,
  "count": 100
}
```

Counts are 1..6000, intervals 10..60000 ms, and `(count - 1) * interval_ms` must
not exceed 60000 ms. These are scheduling bounds, not a lease extension. A caller
must explicitly renew its existing lease if needed. Tasks never renew or
reacquire it. Stop/restart binds a new acquisition epoch; an earlier task cannot
resume in it. Registration and startup can be cancelled while waiting for the
runtime lock. Once authorization begins expiry/fault cleanup, that cleanup is
owned independently of the cancelled waiter.

## Results and recovery

Responses use the generated `PeriodicTask` schema: `starting`, `running`,
`cancelling`, `completed`, `cancelled`, `failed`, or `outcome_unknown`. Counters
and lease generations are decimal strings. `simulated` identifies virtual
execution; submitted counts mean backend submission, not physical bus ACK or
peer application delivery. Dispatch lateness is host timing, not wire timing.
Completed plans may contain skipped slots; compare all counters rather than
interpreting the state alone as a guarantee that every planned frame was sent.
Cancelled/failed plans can stop before all planned slots are attempted or skipped.

Admission is atomic and idempotent within owner, session, lease generation and
client request ID. Equivalent normalized frames share the same plan identity.
Changing its frame, count or interval returns 409. Concurrent duplicates do not
start another executor. A retained duplicate returns its original task even if
control has since been released; that operation performs no new transmission.

If the start response is lost, use GET to list tasks and match the session,
generation and client request ID, then GET the task ID. Do not create a new
request ID to retry transmission. A missing result is not proof that no frame
was sent: terminal records expire and results are not persisted across service
restarts. The service instance changes on restart.

The registry retains at most 128 tasks, with at most 16 nonterminal tasks globally
and four per owner. Terminal records remain for five minutes. Full capacity
rejects admission with 429 instead of evicting retained results. The executor
also has 16 slots: a timed-out native submission keeps its slot until the owned
operation exits, so repeatedly timing out cannot create unlimited USB workers.
Manual and periodic submissions share the same per-owner TX bucket (100/s,
burst 20). Exhausted credit terminates a plan; there is no automatic retry.

Cancellation revokes the current child execution permit, not the parent lease.
A submission that exceeds its one-second observation deadline is marked unknown
and is never retried. Later native completion does not rewrite that final result.
HTTP response/waiter lifetimes do not own accepted executions. An API cancel,
lease expiry, original acquisition ending, logout, or service shutdown stops
later submissions; closing a browser tab alone does not immediately cancel a
server-owned task. Without renewal its existing lease expires normally.

## Shutdown and evidence

Shutdown fences new device mutations synchronously, requests task cancellation,
and starts owned runtime cleanup before awaiting task accounting. Dropping the
shutdown waiter cannot abandon that started cleanup. Bare service drop fences
and cancels tasks; explicit async shutdown is still required for a complete
cleanup report. Task accounting waits at most two seconds; native operations
remain subject to their backend's own teardown contract.

`ServiceCloseSummary.unknown_tasks` counts uncertain terminal task results over
the service lifetime plus still-pending tasks at its accounting deadline. That
uncertainty survives task result expiry. It is separate from backend
`unknown_transmissions`; adding those numbers would double-count some operations.
The CLI reports both categories and exits 7 if either is nonzero. A clean task
count does not hide backend cleanup failures.

Hardware-free tests cover generated schema responses, canonical/concurrent
idempotency, lost response recovery, retention/resource limits, shared rate
credit, browser ownership/CSRF/logout, startup/per-frame cancellation, stale
epochs, queued I/O fencing, unknown outcomes and cancellation-safe cleanup.
Hardware, real-time scheduling, sleep/wake and adapter performance acceptance
remain deferred.
