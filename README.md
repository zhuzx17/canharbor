# CANHarbor

> An open CAN/CAN FD platform for macOS, Linux and Windows, built in Rust.

CANHarbor provides a local Rust service, a `canharbor` CLI, and a browser workbench. The default service provides **two simulated channels on one virtual CAN bus**. Hardware access is opt-in through gs_usb, SLCAN, PEAK or Kvaser backends; no real-adapter compatibility has been verified. See [hardware access](docs/hardware-service.md).

## Start here

- **Use the workbench:** check [versioned downloads](https://github.com/soulteary/canharbor/releases). If no release is published, use the [Package workflow artifacts](https://github.com/soulteary/canharbor/actions/workflows/package.yml) or build below. Workflow downloads require GitHub access and expire.
- **Try without hardware:** start the default service and follow **First virtual receive / send walkthrough** in the connection panel.
- **Connect an adapter:** open **Connect a hardware adapter**; check the [backend prerequisites](docs/hardware-service.md) before restarting the service with hardware options.
- **Automate:** use the [CLI](docs/cli-status.md), [Python examples](python/README.md#complete-examples), or [Rust client](docs/rust-client.md).

## Run a packaged binary

The [packaging workflow](docs/distribution.md) produces archives with the browser workbench embedded. Packages target macOS ARM64 and Linux x86_64. Extract the matching archive and double-click `Start CANHarbor.command` on macOS, or run `./start-canharbor.sh` on Linux. You can also run `./canharbor serve --open`. Keep the service window open while using the workbench. Packages do not require Rust or Bun at runtime. No published release is claimed by this repository.

## Build and run

Requirements: the pinned Rust toolchain, Bun 1.4.2. macOS ARM64, Linux x86_64 and Windows 11 x64 (MSVC) are CI targets. See [Windows support](docs/windows.md) for driver prerequisites and scope.

```sh
bun install --cwd web --frozen-lockfile
bun run --cwd web build
cargo build -p canharbor-cli --features web-ui --locked
./target/debug/canharbor serve --open
```

The service runs in the foreground. Web assets are embedded in the executable; Node.js is not needed to run the built application. A build without `web-ui` can run `serve --no-web` for CLI-only use.

To open another paired browser, run `canharbor web open`. Pairing links expire after 60 seconds and can be used once. `web open --print` prints the link for manual opening; treat it as a temporary credential.

## Try the virtual bus

For a short demonstration after building the CLI:

```sh
python3 scripts/virtual-demo.py
```

The script starts an isolated virtual-only service, starts a listener, sends one
frame and shuts down its processes. Use `--binary PATH` to select another built
CLI. It requires Python 3 and does not require embedded Web assets.

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
- [Implementation status and limits](docs/status.md)
- [gs_usb protocol engine](docs/m1-gsusb.md)
- [nusb discovery and transport](docs/usb-transport.md)
- [Device actor ownership and queues](docs/device-actor.md)
- [Architecture and Rust modules](docs/technical-design.md)
- [HTTP and WebSocket contract](docs/api-design.md)
- [Web UI and local access](docs/web-ui.md)
- [Language selection / 中英文使用说明](docs/localization.md)
- [Milestones and acceptance criteria](docs/roadmap.md)
- [Licensing and source delivery](docs/licensing.md)
- [Local service decision](docs/adr/0001-local-service-and-web-ui.md)

The design documents describe planned behavior. For implemented capabilities and
validation limits, use the [status page](docs/status.md).

## Feature guides

- [Hardware selection](docs/hardware-service.md) and [device diagnostics](docs/device-diagnostics.md)
- [Channel configuration](docs/channel-configuration.md) and [transmission receipts](docs/transmission-receipts.md)
- [Periodic tasks: CLI](docs/periodic-cli.md), [Web](docs/periodic-web.md), [API](docs/periodic-api.md)
- [Recording](docs/capture-service.md) and [offline inspection](docs/offline-capture.md)
- [Bounded communication checks](docs/sequence-test.md)
- [Stress testing](docs/stress-testing.md)
- [Rust client](docs/rust-client.md), [C ABI](docs/c-abi.md), [Python client](python/README.md)

## License

CANHarbor code and documentation are licensed under **AGPL-3.0-or-later**. See [LICENSE](LICENSE). Third-party dependencies retain their own licenses. No upstream CAN driver implementation has been copied into the M0 backend.

AGPL permits commercial use, subject to its applicable obligations. Development builds expose their license and source repository through the About links and `/api/v1/legal`; release-specific source archives and distribution notices remain part of the release work.
