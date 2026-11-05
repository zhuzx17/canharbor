# Periodic task CLI

Create a foreground task with explicit active mode:

```sh
canharbor task create --channel virtual:0 --id 123 --data 0102 \
  --count 100 --interval-ms 10 --active --request-id example-1
canharbor task list
canharbor task show TASK_UUID
canharbor task cancel TASK_UUID
canharbor task recover --session SESSION_ID --request example-1 --generation GENERATION
```

Create opens the selected channel, acquires control, starts it, and admits the
immutable frame plan. IDs are hexadecimal without a prefix. `--extended`, `--fd`,
`--brs`, `--data-bitrate` and `--rtr-dlc` select the corresponding frame/mode.
Bounds match the service: 1..6000 planned slots, interval 10..60000 ms, horizon
at most 60 seconds. Invalid plans and missing `--active` fail before opening.

The foreground process renews only its original lease every five seconds,
anchored before acquisition and maintained concurrently during startup/admission.
It
prints the task/recovery identifiers on stderr. It polls progress without
starting another task. Use show/list from another terminal for progress. On
completion it prints the final task JSON to stdout and attempts session cleanup.
Completed means the schedule ended; inspect skipped/submitted counts. Backend
submission does not establish physical delivery.

Ctrl+C requests cancellation, waits up to three seconds for its terminal report,
and closes the owned session. An interrupted admission, missing start response,
uncertain task result, lost observation/renewal, or unconfirmed cancellation
returns exit 7. No retry or control reacquisition occurs. Query show or recover;
missing results do not establish nonexecution after expiry/restart. Known task
failure/cancellation exits 6, argument errors 2, ambiguous recovery 5 and absent
retained matches 4. Ordinary API rejections retain their existing categories.

Read-only show/list/recover never renew a lease. Cancelling from another native
CLI is allowed by the same-user service policy. Exiting a reader does not cancel
a task. Killing the creator without cleanup stops its renewals; the original
lease expires and prevents later dispatch. Service restart never resumes tasks.

Hardware-free tests cover creation/query/recovery, active opt-in/bounds, external
and signal cancellation, explicit foreground renewal across the original lease
expiry, and a proxy that loses a committed start response while verifying exactly
one admission and read-only recovery. Physical adapters were not tested.
