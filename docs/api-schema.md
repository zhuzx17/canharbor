# API contract generation

The implemented M0 payloads are defined in `crates/canharbor-server/src/dto.rs`. Handlers serialize these DTOs directly. Request extraction uses the same types, including tagged frame kinds and enums. The contract catalog is a list of payload definitions, not an HTTP response or an OpenAPI document.

## Generate and check

```sh
bun install --cwd web --frozen-lockfile
bun run --cwd web api:generate
bun run --cwd web api:check
```

The generator runs the Rust `export-api` binary without the `web-ui` feature, writes [JSON Schema draft-07](../api/schema.json), and derives [TypeScript types](../web/src/generated/api.ts) using the locked Node toolchain dependencies. Node is not needed to generate JSON Schema directly:

```sh
cargo run --locked -p canharbor-server --bin export-api -- api/schema.json
cargo run --locked -p canharbor-server --bin export-api -- --check api/schema.json
```

The same command compiles standalone validation functions and declarations in `web/src/generated/`. Browsers do not import Ajv or compile schemas, so runtime checks work under the existing CSP without unsafe-eval. Generated runtime helpers retain their upstream MIT notices inline.

Commit changes to all generated files with the DTO change. `api:check` compares all artifacts without rewriting them; CI runs it before the frontend build. The Rust schema snapshot test independently compares the committed schema with current DTO output. Formatting alone cannot update a stale contract.

## Validation boundaries

The frontend imports generated request/response types. Named API decoders infer response types from the generated contract; there are no separately maintained domain interfaces in `web/src/api.ts`. Build-generated validators validate HTTP JSON and WebSocket messages before the UI consumes them. Stream variants are handled exhaustively; unknown variants, missing fields, invalid counters and invalid frame shapes close the stream rather than being cast to a trusted event.

Request objects reject unknown fields. Responses allow additive fields for minor-version compatibility; a new stream discriminator still requires an updated consumer. Empty HTTP responses have no JSON payload to validate. Routing, headers, cookies, leases and status-code semantics remain specified in [api-design.md](api-design.md); the JSON Schema catalog is not a generated HTTP client or an OpenAPI routing specification.

Frame constraints include ID widths, exact CAN FD wire lengths, Classic/RTR/BRS restrictions and DLC/payload agreement. These conditional rules are attached to the Rust DTO's schema transform. Generated TypeScript describes shape, not every conditional invariant; the original schema enforces them at runtime and the Rust core remains the final semantic boundary.

Counters are canonical decimal u64 strings, including values above JavaScript's exact integer range. Rust deserialization and the generated regex enforce the same upper bound. Frontend calculations use BigInt. Nullable response fields are required and explicitly nullable; optional request fields remain optional.

[Shared fixtures](../api/fixtures.json) are tested against Rust deserialization/core validation and both JSON Schema validators. HTTP tests validate actual handler responses. A Unix-socket WebSocket test validates actual `hello` and fault messages emitted by the service, including closing behavior.

## Fault injection

`canharbor-virtual::VirtualBus::inject` provides deterministic harness controls for disconnect, bus-off, receive overflow and failure of the next valid transmission. These controls are not exposed through HTTP or the Web UI. `clear_faults` restores backend health only.

A backend fault stops the affected session, revokes its lease, retains diagnostic state, and closes its old event channel. Stream clients receive `session_fault` before the close handshake when the transport is writable. A failed transport cannot guarantee delivery of a final message; clients must treat closure itself as a discontinuity.

Recovery requires a fresh control lease and explicit start. Old generations and old subscriptions remain invalid. Backend generations also fence fault notifications and frames queued before explicit recovery. A one-shot TX failure emits no frame and faults only the sending session; persistent bus faults affect both simulated channels. Buffer-overflow tests also exhaust the actual bounded backend queue without a scheduling delay. The model does not simulate electrical arbitration, device firmware or physical CAN timing.
