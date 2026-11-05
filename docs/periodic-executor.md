# Periodic execution foundation

`canharbor-tasks` provides Rust-owned periodic execution over an existing active
session. The service task routes are documented in [periodic-api.md](periodic-api.md);
CLI, SDK and Web controls use that service API and never schedule backend
transmissions directly.

A plan contains one immutable frame, 1–6000 scheduled attempts, an interval of
10 ms–60 s, and a scheduling horizon of at most 60 seconds. There are at most
16 execution slots per executor. It does not acquire or renew a lease. Callers
supply the same owner rate limiter used for manual submissions; exhausted credit
terminates a plan without retrying a frame.

Each plan binds to the original acquisition epoch. Stop/restart, lease expiry,
shutdown or explicit cancellation prevents later submissions; restarting the
session does not resume the old plan. Every frame receives a fresh, short-lived
prepared execution permit. Cancelling its child authorization does not revoke
the controlling lease. Waiting to prepare a frame is interruptible, even when
an unrelated backend operation holds the runtime lock. After authorization obtains
the lock, runtime expiry and fault cleanup run in an independently owned task.
Cancelling the preparation waiter cannot abandon a queued hardware stop or leave
an acquisition unfinished. Preparation never submits the frame itself. No
background preparation is queued before the lock is obtained.

Accepted executions are owned by the service task registry independently of
HTTP response waiters. Startup and per-frame preparation can both be cancelled.

Scheduling uses a monotonic host clock. Overrun slots are counted and skipped to
a future deadline, without a catch-up burst. Reports count attempts, submitted,
failed, unknown and skipped frames, plus measured host dispatch lateness. These
measurements do not claim physical bus timing or delivery confirmation.

A submission has a one-second observation deadline. If it has not completed,
its authorization is revoked and the plan terminates with an unknown outcome;
the executor never retries it. The outstanding operation remains owned and keeps
its execution slot until it exits. Non-preemptible native calls therefore
cannot create an unlimited number of replacement workers by timing out. An
already-issued operation cannot be retracted. Dropping the task handle requests
cancellation; dropping a borrowed result waiter alone does not.

Virtual/queued-backend tests cover shared credit, resource limits, acquisition
fencing, cancellation before write, retained unknown outcomes and missed-slot
skipping. Hardware and real-time performance acceptance remain deferred.
