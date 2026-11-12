# SPDX-License-Identifier: AGPL-3.0-or-later
"""Bounded monitor, single-send and periodic examples with explicit cleanup."""
import argparse
import json
import os
from pathlib import Path
import sys
import tempfile
import time
from uuid import uuid4

from canharbor import Client, CanHarborError, TaskStartUnknown


def run(args):
    request = str(uuid4())
    with Client(str(args.runtime_dir), library=str(args.library)) as client:
        features = client.native_features()
        print(json.dumps({"native_features": features}), file=sys.stderr)
        if args.operation == "monitor" and not features["event_stream"]:
            raise RuntimeError("This native library lacks event streaming; build a current canharbor-ffi library.")
        session = client.create(args.channel, active=args.active)["id"]
        generation = None
        try:
            generation = client.acquire(session)["generation"]
            renew_at = time.monotonic() + 5

            def renew_if_due():
                nonlocal renew_at
                if time.monotonic() >= renew_at:
                    # A failed renewal ends the example; no automatic retry.
                    client.renew(session, generation)
                    renew_at = time.monotonic() + 5

            print(json.dumps({"session": session, "generation": generation,
                              "request": request}), file=sys.stderr)
            frame = {"kind": "classic_data", "id": 0x123, "extended": False,
                     "brs": False, "data_hex": "01020304"}
            if args.operation == "monitor":
                with client.subscribe(session) as stream:
                    hello = None
                    hello_deadline = time.monotonic() + 5
                    while hello is None and time.monotonic() < hello_deadline:
                        renew_if_due()
                        hello = stream.next(timeout_ms=1000)
                    if not hello or hello.get("type") != "hello":
                        raise RuntimeError("Subscription was not confirmed; channel was not started.")
                    client.start(session, generation)
                    deadline = time.monotonic() + args.seconds
                    while time.monotonic() < deadline:
                        renew_if_due()
                        event = stream.next(timeout_ms=500)
                        if event is not None:
                            print(json.dumps(event))
                            if event.get("type") in {"session_fault", "resume_unavailable"}:
                                raise RuntimeError("Stream fault; inspect status before explicitly restarting.")
            elif args.operation == "send":
                client.start(session, generation)
                try:
                    print(json.dumps(client.submit(session, generation, request_id=request, frame=frame)))
                except CanHarborError as error:
                    if error.status == 7:
                        print("Transmission outcome unknown; keep the identifiers above and query receipt_by_request. Do not resubmit.", file=sys.stderr)
                    raise
            else:
                client.start(session, generation)
                try:
                    task = client.start_periodic(session, generation, request_id=request,
                                                 frame=frame, count=args.count,
                                                 interval_ms=args.interval_ms)
                except TaskStartUnknown:
                    if features["query"]:
                        # This lookup is read-only; an absent result is not proof of nonexecution.
                        print(json.dumps({"retained_result": client.periodic_by_request(
                            session, request, generation=generation)}), file=sys.stderr)
                    print("Task start outcome unknown. Do not create a replacement automatically.", file=sys.stderr)
                    raise
                deadline = time.monotonic() + 65
                while task["state"] in {"starting", "running", "cancelling"}:
                    if time.monotonic() >= deadline:
                        raise RuntimeError("Observation deadline exceeded; inspect the retained task result.")
                    renew_if_due()
                    time.sleep(0.1)
                    task = client.periodic_task(task["id"])
                print(json.dumps(task))
                if task["state"] != "completed" or task.get("failed", "0") != "0" or task.get("unknown", "0") != "0":
                    raise RuntimeError("Task did not complete without failures or unknown outcomes; inspect its report.")
        finally:
            # Closing a controlled session stops acquisition. It never repeats TX.
            primary_error = sys.exc_info()[0] is not None
            if generation is not None:
                try:
                    client.request("DELETE", f"/sessions/{session}", {}, generation=generation)
                except CanHarborError as cleanup:
                    print(f"Session cleanup failed: {cleanup}. Query session status; lease expiry remains a fallback.", file=sys.stderr)
                    if not primary_error:
                        raise


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("operation", choices=["monitor", "send", "periodic"])
    parser.add_argument("--library", required=True, type=Path, help="Absolute path to a trusted native library")
    runtime = (Path(os.environ.get("LOCALAPPDATA", tempfile.gettempdir())) / "CANHarbor" / "runtime"
               if os.name == "nt" else Path(tempfile.gettempdir()) / f"canharbor-{os.geteuid()}")
    parser.add_argument("--runtime-dir", type=Path, default=runtime)
    parser.add_argument("--channel", default="virtual:1")
    parser.add_argument("--active", action="store_true", help="Explicitly authorize active CAN operation")
    parser.add_argument("--seconds", type=int, default=10, help="Monitor duration, 1–60 seconds")
    parser.add_argument("--count", type=int, default=10)
    parser.add_argument("--interval-ms", type=int, default=100)
    args = parser.parse_args()
    if not 1 <= args.seconds <= 60:
        parser.error("--seconds must be 1–60")
    if args.operation != "monitor" and not args.active:
        parser.error("send and periodic require explicit --active")
    if not 1 <= args.count <= 6000 or not 10 <= args.interval_ms <= 60000 or (args.count - 1) * args.interval_ms > 60000:
        parser.error("periodic plan requires 1–6000 frames, 10–60000 ms interval and at most 60 seconds")
    run(args)


if __name__ == "__main__":
    main()
