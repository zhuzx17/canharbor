# Offline capture inspection

`canharbor capture inspect capture.ndjson --limit 100` reads a local recording
without contacting the service, acquiring control or opening a device. It
returns a JSON summary and the last requested events, limited to 500.

The reader scans one bounded line at a time: 8192 bytes per record and 64 MiB
per file. It validates record order, schema version, counters, footer counts,
frame invariants, raw DLC and ESI metadata, diagnostic bytes, event sequence and
monotonic timestamp order. Partial final records are excluded; complete valid
prefixes remain inspectable as incomplete. Malformed complete records are
rejected. No incomplete recording is silently labeled complete.

Preview entries retain received/transmitted direction, raw DLC, timestamp
quality and epoch, hardware counters, ESI and diagnostics. Diagnostics are
counted separately from frames. Inspection does not turn observed traffic into
a transmit plan or discard metadata to make a frame sendable. Active replay
requires a separate explicit operation and policy.

The shared synthetic capture corpus covers Classic raw DLC, CAN FD BRS/ESI,
64-bit precision, diagnostics, recoverable prefixes and invalid frames. Tests
also use a three-byte reader, verify bounded previews over 1000 events, and
invoke the CLI with no service running. Hardware was not used.

The Web UI provides a local file viewer for recordings up to 1 GiB. Its initial
scan validates every record before publishing either an integrity summary or a
page index. A malformed late record or invalid footer rejects the scan even if
the first page itself is valid. Valid incomplete prefixes retain an explicit
incomplete status.

The index stores at most 256 byte-offset checkpoints, initially one per 16
matching events. When full, the reader doubles checkpoint spacing and compacts
the index. Pages hold at most 100 events; record buffers remain limited to 8192
bytes. Previous/next and page-number jumps read from the nearest checkpoint in
the immutable local file instead of revalidating from byte zero. Page reads do
not change the separately displayed whole-file integrity result. Sparse matches
or widely spaced checkpoints can still require a longer forward scan.

ID substring and RX/TX filters cover the whole recording. Applying different
filters performs a new full scan and builds a replacement bounded index; Reset
filters restores all events and page one. The CLI retains its independent
64 MiB limit and last-event preview behavior.

Scanning reports byte progress and can be cancelled. Files remain local and
are never uploaded. Loading, filtering, paging or cancelling does not acquire
control or issue API mutations. Rendering uses text nodes and preserves 64-bit
values as decimal strings.
