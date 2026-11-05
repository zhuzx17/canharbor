# CANHarbor Python client

The Python 3.10+ package uses the standard library `ctypes` interface to
CANHarbor's version 1 C ABI. It has no runtime Python dependencies and does not
bundle a native binary. Build and select a trusted `libcanharbor_ffi` explicitly:

```sh
cargo build -p canharbor-ffi
python -m pip install ./python
```

```python
from canharbor import Client

with Client("/absolute/private/runtime/dir",
            library="/absolute/path/libcanharbor_ffi.dylib") as client:
    print(client.devices())
```

`python/examples/discover.py` provides the same discovery operation from command
line arguments. `CanHarborError.status` retains the C ABI category;
`CanHarborError.response` retains API HTTP status and error detail when available.
Generation values remain decimal strings and are not converted to floating point.
The wrapper releases response handles in a `finally` block, closes its client
through a context manager or explicit `close()`, and has a best-effort finalizer.

Active operations require explicit `create(active=True)`, `acquire()` and
`start()` calls. Pass the returned generation to `renew`, `submit`, `stop` and
`release`. Callers supply and retain their own `request_id` for each logical
transmission. Reusing it is subject to the service's bounded deduplication
window; this is not a cross-restart exactly-once guarantee. There is no automatic
retry, lease renewal, background acquisition or reconnect. Stop and release
before closing: client close does not extend or implicitly release service leases.
A status-7 failure means the outcome may be unknown and must not be retried
blindly. Successful submission does not establish physical bus delivery.

The current wrapper exposes JSON control, not event streaming or binary capture
downloads. Those require separate C ABI interfaces. Tests run the Python wrapper
and example against the actual compiled shared library and a virtual service.
They cover separate identities, lease conflict, control, duplicate submission,
structured errors, nonfinite-JSON refusal, path validation and close. CI selects
Python 3.13 on Linux and macOS. No hardware, signing or notarization is verified.

The package's original source is AGPL-3.0-or-later. The full license is included
in `LICENSE`. Source and native ABI header are maintained in the CANHarbor
repository: https://github.com/soulteary/canharbor.

`client.receipt_by_request(session, request_id, generation=original_generation)`
reads an earlier result without acquiring a lease or submitting a frame. Generic
requests also accept `query={"key": "value"}`; keys and values must be strings.
Query support requires the additive ABI 1 `canharbor_request_with_query` symbol.
Older ABI 1 libraries remain usable for requests without query parameters.
A missing receipt can reflect expiry or restart and does not prove non-submission.

`client.download(capture_id, absolute_destination)` uses the additive capture
download extension. The native client streams up to 64 MiB and publishes the
file only when the declared length is received and synchronized. It refuses to
replace an existing file. The return value is `{"downloaded": True}`; recording
completeness is described by the capture's own final record. Earlier ABI 1
libraries without this extension still support ordinary JSON requests.

```python
with client.subscribe(session_id) as stream:
    message = stream.next(timeout_ms=1000)
    if message is not None:  # None means idle, not end-of-stream.
        print(message)
```

Subscriptions are read-only and do not renew the channel's control lease. The
initial message is `hello`; subsequent messages include events, gaps and faults.
They do not reconnect or resume tasks. Each stream allows one active reader;
`close()` can interrupt a waiting `next()`. Closing the parent client also stops
its streams. Handle terminal errors explicitly; a successful read is not a
promise that the stream remains connected. The native event-stream extension is
optional when loading an older ABI 1 library.

Periodic tasks use `start_periodic(session, generation, request_id=..., frame=...,
count=..., interval_ms=...)`, `periodic_tasks`, `periodic_task`, `cancel_periodic`
and read-only `periodic_by_request(..., generation=...)`. Call `renew` explicitly;
helpers never acquire control or retry. `TaskStartUnknown` preserves recovery
identifiers. Missing results do not prove nonexecution after expiry/restart.
