# SLCAN implementation

The serial backend has two explicitly selected firmware profiles. No ports are
automatically probed and no firmware dialect is inferred from a version string.
Both profiles support standard/extended Classic CAN data and RTR frames.

| Profile | FD | Configuration | Serial command confirmation |
| --- | --- | --- | --- |
| `lawicel` (default) | No | `C`, `Sx`, `L` or `O` | ACK/NACK required |
| `canable-fd` | Up to 64 bytes, with/without BRS | `C`, `Sx`, optional `Y2`/`Y5`, `M0`/`M1`, `O` | Complete host write only; firmware has no ACK/NACK |

Protocol facts are referenced from the [LAWICEL CAN232 3.0B manual](https://www.canusb.com/files/can232_v3.pdf)
and the [CANable 2.0 firmware command documentation](https://github.com/normaldotcom/canable2-fw#supported-commands).
This is an independent implementation; upstream implementation code, firmware,
manual text and vendor binaries are not included.

## Start the service

Build with the independent `slcan` feature, then select a tty and UART baud rate:

```sh
cargo build -p canharbor-cli --features slcan
canharbor serve --slcan /dev/cu.EXPLICIT_DEVICE --serial-baud 115200
# LAWICEL firmware configured for single-frame polling:
canharbor serve --slcan /dev/cu.EXPLICIT_DEVICE --serial-baud 57600 --slcan-polling
# CANable FD-compatible firmware (streaming only):
canharbor serve --slcan /dev/cu.EXPLICIT_DEVICE --serial-baud 115200 --slcan-profile canable-fd
```

The `canable-fd` profile advertises FD, data rates 2/5 Mbit/s, listen-only, and
host receipt timestamps. Nominal rates are 10/20/50/100/125/250/500/750/1000 kbit/s
and 83.3 kbit/s (`83300`). `S7` means 750 kbit/s for this profile; it remains
800 kbit/s for LAWICEL. CANable does not use LAWICEL `L`, `P`, or periodic `F`
queries. `--slcan-polling` is rejected with this profile before opening the tty.
The selected profile is a compatibility declaration, not verified firmware identity.

For example, open an FD session with nominal 1 Mbit/s and configured data 5 Mbit/s:

```sh
canharbor session open --channel slcan:DEVICE_EPOCH:0 --bitrate 1000000 --fd --data-bitrate 5000000 --listen-only
```

Use the channel ID returned by `canharbor devices --json`. The regular CLI, Web
and API controls use the same backend. Per-frame BRS is independent of the
configured data bitrate: a frame with BRS disabled uses the nominal rate throughout.
Without FD enabled in the session, only Classic frames can be submitted.

## Codec and engine

Default LAWICEL parsing remains strict and rejects FD extensions. CANable adds
`d`/`D` (standard/extended FD without BRS) and `b`/`B` (FD with BRS). The hexadecimal
DLC maps to 0–8, 12, 16, 20, 24, 32, 48 and 64 bytes. Invalid identifiers, lengths,
hex and extra suffixes are rejected. Line bounds excluding CR are 32 bytes for
LAWICEL and 138 for CANable FD. Oversized records are discarded through CR; BEL
reports rejection. CANable firmware version text is accepted only during the
explicit `V` handshake and retained separately from numeric LAWICEL versions.

`Serial::open` requests tty exclusivity and a cooperative file lock, selects raw
8N1, and restores termios on drop. One worker owns the transport, with 32 queued
requests, a bounded 256-event publication ring and bounded command/RX queues.
Start and transmit check the runtime lease and cancellation before each mutating
write. Cancelled queued requests are skipped. No EEPROM or automatic-retry
configuration commands are sent.

LAWICEL replies have a 250 ms deadline. Missing replies, short writes and I/O
failures poison the engine and require explicit reopen. A partial transmit is
never retried or completed by a cleanup CR, which could transmit an unintended
frame. A poisoned engine cannot promise controller closure.

CANable commands do not wait for nonexistent replies. Configuration, listen-only
and close results mean only that their complete commands were written to the
host tty; the firmware cannot confirm that it applied them. Each submitted TX
remains in `unknown_transmissions`, including after a remote RX response or stop.
Receipts report `backend_submitted`; no TX echo or bus ACK is fabricated. Service
shutdown after any such TX therefore exits **7**, retaining the unconfirmed count.
This is expected uncertainty, not a missing-reply timeout. Short writes still
poison the transport and prevent cleanup from completing a partial TX command.

LAWICEL raw timestamp milliseconds remain unmapped modulo 60000. CANable has no
device timestamps in this dialect. LAWICEL status flags are retained as raw
diagnostics and overflow faults acquisition. CANable has no supported status
query here, so missing diagnostics must not be interpreted as zero bus errors.

## Validation

Tests cover strict LAWICEL behavior, both FD ID widths and BRS values at every DLC,
fragmented 64-byte records, bounded malformed input, the observed 32-byte RX
record, string versions, command order, listen-only, lease revocation, invalid
rates, partial TX, no synthetic echo and unknown counts. Pseudo-terminal tests
exercise both profiles through real service discovery, monitoring and submission.
Firmware variants still require individual hardware validation; a `custom-build`
version cannot establish its source or revision.

### Hardware smoke test

On 2026-11-05, a USB `16d0:117e` adapter reporting `custom-build` was tested on
Apple Silicon macOS through the official Python client, FFI, local service and
this backend. With UART 115200 8N1, nominal 1 Mbit/s, configured data 5 Mbit/s
and BRS disabled, standard IDs `001` and `002` each received one submission with
the 32-byte payload `010019AA00000000AAAAAAAAAAAAAAAAAAAA00AAAAAA00000000000000000000`.
After the `002` submission, one independent FD RX arrived on standard ID `402`:

```text
0100199800000000979299149DABADB3A89F00ABA7AE05050517000000000000
```

The recording completed with one RX event and zero reported drops. The session
remained healthy, stop/close commands were submitted, and service shutdown
retained two unconfirmed transmissions with exit code 7 and no cleanup failure.
No independent response to `001` was observed. This validates this device's
32-byte, standard-ID, no-BRS path; actual 5 Mbit/s bus traffic, BRS, extended IDs,
64-byte frames and listen-only behavior were not tested on hardware. With BRS
disabled these frames used the nominal 1 Mbit/s throughout. Firmware command
acceptance and bus error counters remain unavailable.
