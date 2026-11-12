# gs_usb protocol engine

The transport-independent engine validates gs_usb records and manages device
state. Runtime hardware selection is documented in [hardware access](hardware-service.md);
[nusb transport](usb-transport.md) describes discovery and I/O. Protocol tests do
not establish real-adapter compatibility.

## Implemented boundary

`wire` decodes exactly one complete gs_usb record, using explicit little-endian
reads. It validates length, channel, identifier width, DLC, reserved fields, and
Classic/FD flag combinations. Classic data, RTR, and all CAN FD lengths are
supported. Transmit records are zero-padded and have no timestamp trailer.
Receive layouts are 20/76 bytes, or 24/80 bytes with a timestamp. A real transport
must supply complete firmware records using verified transfer boundaries; the
parser does not infer concatenation or splice arbitrary fragments.

Error frames retain class bits and all eight diagnostic bytes as a separate
payload. Overflow, ESI, raw DLC, echo ID and raw 32-bit microsecond timestamps are
retained. Timestamp zero is valid. No rollover extension, timestamp quality, or
bus acknowledgement is inferred. Classic DLC 9..15 is rejected unless the caller
explicitly selects the len8-DLC quirk; the device engine does not enable it.

`control` parses exact 12-byte device configuration and 40/72-byte timing
capabilities, retaining unknown feature bits. Timing validation checks segment,
SJW and prescaler limits before I/O. Callers may provide timing segments or use `timing::solve` for an exact bitrate
and preferred sample point. The solver returns the actual sample point and
rejects rates that cannot be represented exactly. Requests carry the channel in wValue and the USB interface in
wIndex. Only advertised listen-only, hardware timestamp and FD modes are enabled.
FD requires extended data-phase timing limits; no nominal-limit fallback occurs.

`device::Device<T>` owns one injectable transport for every channel. It negotiates
little-endian host format, reads configuration and per-channel capabilities, and
resets channels before exposing stopped state. It sends nominal/data timing
before start and provides explicit reset/shutdown. There is no background task,
implicit USB probing, automatic restart or retry. Transport implementations must
bound all I/O and report failed/short writes as errors.

TX has 32 shared pending slots. Echo IDs are checked against both channel and
frame before a slot is released. Duplicate or foreign echoes remain observable.
A write failure faults the device and retains pending IDs as outcome-unknown.
Stopping or resetting does not claim pending transmissions were cancelled.
Unresolved slots are retained until matching echoes arrive while running, or the
engine is discarded and a new device epoch is opened. A firmware echo is not a
claim of bus acknowledgement or delivery to an ECU.

## Evidence

Run `cargo test -p canharbor-gsusb --locked` and
`cargo clippy -p canharbor-gsusb --all-targets --locked -- -D warnings`.
The tests need no USB device, elevated USB permissions, network, or platform USB
library. Synthetic vectors cover a manually specified Classic record, complete
FD DLC combinations, RTR, timestamps, diagnostics, malformed records, control
responses, multi-channel request sequencing, mode rejection before I/O, echo
matching, bounded TX and transport errors. A deterministic bounded byte corpus
checks parser robustness; this is not coverage-guided fuzzing or hardware proof.

## Next integration steps

1. Implement descriptor-based, opt-in nusb discovery and a single device actor
   with bounded commands, receive queues, transfer cancellation and teardown.
2. Verify transfer boundaries, endpoints, firmware quirks, echo timing and
   timestamp behavior on identified Classic and FD devices.
3. Extend the core backend contract for real configuration/start/stop, diagnostic
   events, timestamp quality and unknown TX outcomes. Map these into API/CLI/UI
   before registering hardware; the M0 synchronous virtual trait cannot express
   all of these semantics.
4. Add hardware acceptance records, bitrate solving, timestamp epochs and error
   state interpretation. Recording and release/source packaging remain pending.

## Provenance

Protocol facts were checked against the MIT-licensed
[sorrowfeng/gsusb-canfd snapshot](https://github.com/sorrowfeng/gsusb-canfd/tree/6785cc82d66da93cb3ff5af66a6b3498f1118c66):
`src/gs_usb_protocol.hpp` (request numbers and bits), `src/device.cpp` (control
layouts), `src/frame.cpp` (wire layouts), and `include/canfd/bit_timing.hpp`
(timing fields). Rust code and vectors are independently written; no upstream
implementation was translated or vendored. Source hashes are recorded in `docs/gsusb-provenance.json`. The reference's MIT notice is retained
in `LICENSES/gsusb-canfd-MIT.txt` and `THIRD_PARTY_NOTICES`. No GPL-2.0-only Linux
implementation or firmware source was used as implementation input.
