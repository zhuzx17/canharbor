# Browser periodic tasks

Open an active session, explicitly acquire control and start the channel. The
Periodic tasks panel uses the existing frame editor, slot count and interval to
admit an immutable service-owned plan. Bounds are 1..6000 slots, 10..60000 ms
interval and at most a 60-second scheduling horizon. The browser never schedules
CAN frame writes: it queries tasks and renews only the page's explicitly acquired
lease through the five-second control loop. Renewal is anchored to acquisition
and each renewal request rather than delayed responses; requests do not overlap.

API reads bypass the browser cache so recovery checks observe the running service.
The panel lists owned task state, session/generation/request identifiers,
submitted/skipped/failed/unknown counts and simulation status. Cancellation asks
the service to stop the original task; the displayed final report establishes
its outcome. Completed schedules may contain skipped slots. Backend submission
does not establish bus acknowledgement or application delivery.

Recovery identifiers are saved before admission, scoped to service instance and
browser identity, at most 16 attempts and 16 KiB. Storage contains only session,
generation and request ID: no frame, CSRF token, control state or start request.
A lost or malformed start response never triggers another POST. It clears local
control so renewal stops; already admitted work remains subject to its original
lease. Find task performs GET-only matching, rejects ambiguous results and never
acquires control. A missing retained result does not prove nonexecution after
retention expiry or restart.

Reload restores results and identifiers, not control or dispatch. A new service
instance does not restore old attempts. Stream loss, task polling failure or
renewal failure clears local control; reconnection and acquisition are explicit.
Stopping/restarting a session does not resume its earlier task. Closing a tab
stops its renewals; existing server-owned work is fenced when its original lease
expires. Cancellation failure is displayed as unconfirmed, not completed, and
never causes automatic restart. Other paired browser identities cannot inspect
or cancel these tasks under the server's owner policy.

Hardware-free browser tests include slow acquisition responses and cover bounds and stored-data validation, creation,
renewal across lease expiry, cancellation, committed-but-lost admission responses,
reload recovery, disconnect/no-renew behavior, missing results, stop/restart and
service-instance scoping. These tests run under Bun with Chromium and do not
constitute Safari or physical-adapter validation.
