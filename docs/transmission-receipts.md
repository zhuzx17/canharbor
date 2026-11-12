# Submission receipts

The service reserves a bounded deduplication record before handing a frame to a
service-owned task. Cancelling the HTTP waiter does not cancel accounting or
allow the same logical request to execute again. The key is client identity,
session, lease generation and `client_request_id`. Frame comparison uses the
validated canonical frame, so hexadecimal case and an equivalent explicit DLC
do not create different payloads. Reusing a key for another frame returns 409.

POST currently waits for the backend submission result. The record is already
queryable while that wait is pending. Success returns 202 with `submitted` and
the backend's existing confirmation meaning. Virtual acceptance and native
submission remain distinct; neither is a claim about a target ECU. Failures
retain their HTTP error category. Once reserved, an error also includes a
`receipt`, `outcome_unknown`, and `X-CANHarbor-Receipt` header. A failed request
without a reserved record has no receipt. Repeating a failed logical request
returns its recorded failure rather than attempting another write.

`GET /api/v1/sessions/{session}/tx/{receipt}` reads `queued`, `submitted`,
`failed` or `outcome_unknown`. Browser queries are restricted to the creating
identity. Same-user UDS clients can inspect receipts administratively, including
those created by an earlier CLI
process; this matches the existing local capture boundary. No live lease is
needed to inspect an old result. The CLI provides:

```sh
canharbor receipt --session <session-id> --receipt <receipt-uuid>
```

Successful send output includes both session_id and receipt, so another CLI
process can inspect the record after sender cleanup closes the temporary session.

The cache holds at most 1024 pending/completed records. Completed outcomes stay
for five minutes after completion. Pending records are not evicted to make
space or to expire a deduplication window while the operation is still running.
A full cache rejects new requests. This is process-local accounting, not a
persistent exactly-once guarantee across restart or expiry. No automatic resend
follows a lost response, disconnect, ambiguous backend failure or task panic.

New logical requests share a per-client token bucket of 100/s with burst 20
across sessions. Duplicate keys do not consume credit. Owner buckets are also
bounded and expire after five idle minutes. Rejections use 429 and distinguish
`tx_rate_limited` from cache/resource exhaustion.

A prepared transmission is single-use and bound to its originating runtime and
current acquisition permit. Its child execution permit expires after one
second and remains subject to lease expiry, stop/restart and the terminal
shutdown fence. Lease renewal cannot extend that queue deadline. Checks happen
again immediately before backend I/O. A foreign operation already issued before
revocation may finish afterwards; software deadlines do not retract it.

Backend submission panics are caught around construction and polling of the
future. The runtime revokes control, faults the acquisition and reports an
unknown outcome. Native submission faults are conservatively unknown where
attempted I/O cannot be excluded; known validation, capacity and permit
rejections are failed without implying submission. Simulated deterministic
backend faults remain known failures. Receipt tasks also account for unexpected
executor termination rather than leaving a permanent queued result.

The Web UI displays an unknown-outcome message and receipt ID when supplied by
the API. It does not infer delivery from an echo, restart a task, or retry a
failed request. Per-request firmware-echo correlation is not provided by this
receipt layer; native success remains `backend_submitted`.

Tests cover HTTP-wait cancellation, duplicate execution refusal, canonical
frames, independent owner limits, cache retention/capacity, failed and unknown
outcomes, query ownership, expired execution permits, cross-runtime rejection,
stop/restart fencing and a panic after a possible write.

## Recovering a lost response

When the original response is lost before its receipt ID is available, use the
read-only request lookup:

```sh
canharbor receipt --session <session-id> --request <original-request-id> \
  --generation <original-generation>
```

Send failure diagnostics include this command. It calls
`GET /api/v1/sessions/{session}/tx?client_request_id=...&generation=...` and never
submits a frame. Same-user UDS administrative reads and browser owner isolation
use the same boundary as receipt-ID lookup. No control identity or lease is
restored. If a request ID matches more than one generation and the filter is
omitted, the API returns 409 `receipt_ambiguous`. It does not choose a record
arbitrarily. Query decoding failures use the normal JSON error envelope.

The Rust client encodes bounded query values separately from the validated path;
caller paths containing query syntax remain invalid. Request IDs use 1–128
Unicode scalar values, matching JSON Schema string length. A valid ID uses at
most 512 UTF-8 bytes. TX, capture and lookup validation share that rule. Tests
cover ASCII, CJK and astral limits and values containing URL delimiters.

Lookup exits 0 when a record is retrieved; inspect its `state` for the actual
submission outcome. A missing record can mean pending processing, expiry or a
service restart. It does not establish that no frame was submitted. Do not
resubmit based solely on 404. The C query extension and Python `receipt_by_request` helper use the same read-only lookup.

The recovery integration test forwards requests to a real virtual service,
discards only the committed TX response, and queries from a fresh CLI process
after sender cleanup. It verifies the original receipt and exactly one POST to
the transmit route.

## Browser recovery

The Web UI retains at most 16 recent request/session/generation identifiers in
per-tab storage, scoped to the browser identity and service instance. New entries
also retain a submission timestamp, channel ID when available, frame type,
identifier, ID width, byte count and BRS flag. This bounded summary contains no
frame payload or authorization token and does not restore control after a
reload. Older identifier-only entries remain queryable and explicitly show that
the time and frame summary are unavailable. Network errors, timeouts, malformed successful responses and incomplete
server failure metadata leave the result unknown. Explicit rejection statuses
and complete failure metadata remain distinguishable.

Each history entry has a **Check result** action that performs only a GET. A
missing receipt is not treated as proof of non-submission. Browser tests discard
a completed virtual submission's response, reload the page and retrieve its
original receipt without a second POST. JSON responses are bounded to 1 MiB,
requests have a 10-second timeout, and redirects are refused.


Unconfirmed (`pending` or `outcome_unknown`) entries block a new logical
send. First use **Check result** for each original request. The query never
transmits a frame; a missing record or a failed lookup leaves the outcome
unconfirmed. After attempting those queries, an explicit acknowledgment allows
one new logical send using the current frame and a new request ID. It does not
retry an earlier request and may duplicate a physical effect. The acknowledgment
is not persisted and resets after each new send or result query. If a query
resolves every outstanding result, no acknowledgment is needed. The normal
running-channel and explicit-control requirements still apply.
A returned `queued` receipt confirms service acceptance and does not require the
unknown-outcome acknowledgment. It still does not establish physical delivery.
