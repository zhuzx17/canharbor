# Implementation status

This page describes the current source tree. Design documents describe intended
behavior and may include work that has not been implemented.

## Available features

- Local Rust service with same-user Unix transport and a paired loopback Web UI.
- Two virtual channels on one simulated bus; no generated background traffic.
- Classic data, Classic RTR and CAN FD frame validation.
- Channel configuration, capability queries, exclusive sessions, expiring control
  leases and generation fencing. Refresh and reconnect do not resume sending.
- CLI and browser monitoring, one-shot transmission, request receipts and bounded
  periodic tasks.
- Service-owned recordings, downloads and a bounded browser offline viewer.
- Rust client, C ABI and Python wrapper, including optional native streaming and
  capture download extensions.
- Stress plans, multi-flow execution, explicit criteria, persisted evidence,
  read-only recovery, FD/RTT, adaptive plans and multi-host coordination.
- Opt-in gs_usb, SLCAN, PEAK and Kvaser backends. Native vendor backends require
  explicit library and channel selection; discovery is not compatibility proof.

See the [README](../README.md) for build instructions and feature documentation.

## Validation boundaries

Virtual CAN models connectivity, not physical arbitration, ACK behavior, wiring,
USB reliability or electrical faults. Driver submission and local TX echo do not
prove delivery to another physical device. Hardware compatibility remains
unverified; software tests use virtual devices and simulated native libraries.

Browser history and event queues are bounded. A gap indicates missing stream
observations. Display pause affects the table, not acquisition or recording.
Fault recovery requires explicit control and start; service restart never
restores transmission.

Stress evidence, receiver completeness and achieved workload are separate facts.
The checked-in [benchmark records](benchmarks/) include failed runs; stable
throughput still requires repeated validation on a controlled host. Virtual-clock
long runs do not establish 24–72 hours of real elapsed-time reliability. Physical
FD/BRS, external SSH/network failures, USB unplug/sleep behavior and independent
analyzer measurements require separate hardware validation.

## Distribution limits

The repository does not include release binaries. The Package workflow can build
unsigned archives with launchers, matching source and checksums, and prepare a
draft for an existing version tag. Signing/notarization, supported macOS-version
validation, Safari acceptance and publication remain release work. CI includes
focused WebKit checks; these do not certify Safari. The service is local-only; remote hosting and LAN access are not
supported. CANHarbor does not implement the Linux SocketCAN kernel ABI or promise
hard real-time scheduling.
