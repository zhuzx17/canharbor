# CANHarbor

> An open CAN/CAN FD platform for macOS, built in Rust.

CANHarbor provides a local Rust service, a `canharbor` CLI, and a browser workbench. The current M0 implementation supports **two simulated channels on one virtual CAN bus**. USB access is opt-in through the gs_usb backend; no real-adapter compatibility has been verified. See [hardware access](docs/hardware-service.md).

## Build and run

Requirements: the pinned Rust toolchain, Bun 1.4.2. macOS is the target platform; the Unix transport also builds on Linux.

```sh
bun install --cwd web --frozen-lockfile
bun run --cwd web build
cargo build -p canharbor-cli --features web-ui --locked
./target/debug/canharbor serve --open
```

The service runs in the foreground. Web assets are embedded in the executable; Node.js is not needed to run the built application. A build without `web-ui` can run `serve --no-web` for CLI-only use.

To open another paired browser, run `canharbor web open`. Pairing links expire after 60 seconds and can be used once. `web open --print` prints the link for manual opening; treat it as a temporary credential.

## Try the virtual bus

With the service running, start a listener in a second terminal:

```sh
./target/debug/canharbor devices
./target/debug/canharbor monitor --channel virtual:1 --listen-only
```

Send from a third terminal, or open `virtual:0` in active mode in the Web UI, acquire control, and start the channel:

```sh
./target/debug/canharbor send --channel virtual:0 --id 123 --data 01020304 --active
```

CLI identifiers are hexadecimal without a prefix. CAN FD accepts exact wire lengths; add `--fd --brs` to the send command. The simulated bus has a fixed nominal bitrate of 500 kbit/s and FD data bitrate of 2 Mbit/s. No background traffic is generated.

The CLI and browser share channel reservations. An observer cannot change another client's session. Closing a controlling client stops lease renewal; after 15 seconds, its channel stops. Refreshing the browser does not restore control or transmission. “Pause display” only pauses the table.

## Development checks

```sh
bun run --cwd web api:check
bun run --cwd web build
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
cargo build -p canharbor-cli --features web-ui --locked
(cd web && bun x --bun playwright install chromium && bun run test:e2e)
```

The browser test starts its own local service, then checks CLI-to-browser and browser-to-CLI delivery. It also checks that a second browser cannot acquire an occupied control lease and that refresh does not resume sending.

## Design and status

- [Generated API contract and fault injection](docs/api-schema.md)
- [M0 implementation status and limits](docs/m0-status.md)
- [M1 gs_usb protocol increment](docs/m1-gsusb.md)
- [nusb discovery and transport](docs/usb-transport.md)
- [Device actor ownership and queues](docs/device-actor.md)
- [Architecture and Rust modules](docs/technical-design.md)
- [HTTP and WebSocket contract](docs/api-design.md)
- [Web UI and local access](docs/web-ui.md)
- [Milestones and acceptance criteria](docs/roadmap.md)
- [Licensing and source delivery](docs/licensing.md)
- [Local service decision](docs/adr/0001-local-service-and-web-ui.md)

The design documents describe the planned platform. They are not a claim that all endpoints or backends are implemented. gs_usb has a transport-independent protocol engine with simulated tests, with explicit nusb discovery/transport and opt-in runtime registration; recording, replay, C/Python bindings, and hardware validation remain pending. CANHarbor does not provide the Linux SocketCAN kernel ABI or hard real-time scheduling.

## License

CANHarbor code and documentation are licensed under **AGPL-3.0-or-later**. See [LICENSE](LICENSE). Third-party dependencies retain their own licenses. No upstream CAN driver implementation has been copied into the M0 backend.

AGPL permits commercial use, subject to its applicable obligations. Development builds expose their license and source repository through the About links and `/api/v1/legal`; release-specific source archives and distribution notices remain part of the release work.

Device inventory reports: [device-diagnostics.md](docs/device-diagnostics.md).

Bounded communication checks: [sequence-test.md](docs/sequence-test.md).

Service-owned periodic tasks: [periodic-api.md](docs/periodic-api.md).

Periodic task controls: [CLI](docs/periodic-cli.md), [Web](docs/periodic-web.md), [Rust SDK](docs/rust-client.md).
