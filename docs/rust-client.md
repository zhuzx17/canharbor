# Rust API and local client

`canharbor-api` owns the serializable wire DTOs and schema catalog. It depends
on core types but not the HTTP service or a USB backend. The service re-exports
the contract through `canharbor_server::dto` for compatibility; converting
service-owned capture state remains in that wrapper. TypeScript and validators
continue to be generated from the same schema with no wire format changes.

`canharbor-client` provides a same-user UDS HTTP client and re-exports the
contract as `api`. Each instance gets a distinct client ID. It has typed methods
for version, discovery, session creation, acquisition, start/stop and single
frame submission, plus a generic typed request method. Requests have a ten-second
limit, JSON responses have a one-MiB streaming bound, and capture downloads keep
the existing bounded streaming and atomic no-overwrite behavior. `ApiFailure`
retains HTTP status and structured service errors for callers to inspect.

```rust,no_run
use canharbor_client::LocalClient;
# async fn example() -> Result<(), Box<dyn std::error::Error>> {
let client = LocalClient::new("/tmp/canharbor-example".into())?;
let devices = client.devices().await?;
println!("{} devices", devices.devices.len());
# Ok(())
# }
```

The caller chooses the private service runtime directory. The client uses only
its UDS and disables proxies. Session IDs are opaque strings, not UUID values;
typed session methods reject path separators and malformed IDs. Creating a
session does not acquire control or start it. The library never retries a write,
renews a lease in the background or resumes a disconnected task. Applications
own acquisition, renewal, shutdown and uncertain-result policy. The CLI retains
its foreground monitoring and signal handling outside the library.

A local integration test exercises two independent client identities, typed
responses, exclusive acquisition, active transmission, idempotency and stop.
Existing service schema, authentication and capture tests validate the shared
contract. This crate is the Rust client foundation; the exported C ABI and
Python wrapper are separate work.

Request-ID receipt recovery uses `receipt_by_request(session, request, generation)`.
Its optional generation is a read filter. `request_with_query` accepts at most
16 pairs and 4 KiB of decoded query data, encodes them as URL values, and retains
the path validator. It performs no retry or implicit acquisition. A query can
read an old result after the corresponding session is closed.

## Event subscriptions

`LocalClient::subscribe(session, &api::Subscribe)` returns a pull-based
`EventStream`. `next()` yields the shared `StreamMessage` DTO, including the
initial greeting, gap records and terminal channel faults. A subscription does
not acquire, renew or restore control. There is no detached reader or unbounded
client queue; a slow consumer observes the server's gap accounting.

The client bounds both WebSocket frames and reassembled messages to 1 MiB,
event batches to 256 entries and outgoing buffering to 64 KiB. It checks the
protocol greeting and subsequent service/session epochs. Connection setup has a
ten-second limit. Cancelling a pending `next()` does not reconnect or submit
frames. `close()` has a two-second deadline; dropping the stream releases the
socket. The caller decides whether to open a new subscription after disconnect.

CLI monitoring uses the same implementation. Tests cover independent observer
identities, cancellation during an idle stream, subsequent event delivery,
explicit close, malformed responses and message size limits without hardware.

Cloning `LocalClient` shares its native identity and HTTP connection pool. Call
`LocalClient::new` to create an independent identity. The C stream worker uses
a clone of its parent client so opening subscriptions does not reset identity
or avoid service quotas.

## Periodic tasks

`start_periodic(session, generation, &PeriodicRequest)` uses the existing active
session and never acquires, renews or retries. `TaskStartUnknown` retains session,
generation and request ID if admission cannot be established. Use
`recover_periodic(session, request, Some(generation))` for read-only recovery;
None is not proof of nonexecution after retention expiry or service restart.
Omitting generation can return `AmbiguousTask`. `periodic_tasks`, `periodic_task`
and `cancel_periodic` provide typed inspection/cancellation. `renew` and `release`
are explicit caller operations. Dropping a client does not cancel an admitted
task or renew its lease. See [periodic-api.md](periodic-api.md) for bounds and
submission semantics.
