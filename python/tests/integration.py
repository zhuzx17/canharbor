# SPDX-License-Identifier: AGPL-3.0-or-later
"""Executed by the Rust ABI integration test against a virtual service."""
import sys
from canharbor import Client, CanHarborError, TaskStartUnknown

runtime, library = sys.argv[1:]
with Client(runtime, library=library) as first, Client(runtime, library=library) as other:
    assert first.devices()["devices"]
    session = first.create("virtual:0", active=True)["id"]
    acquired = first.acquire(session)
    generation = acquired["generation"]
    assert isinstance(generation, str)
    try:
        other.acquire(session)
        raise AssertionError("second client acquired an occupied lease")
    except CanHarborError as error:
        assert error.status == 5
        assert error.response["http_status"] == 409
    first.start(session, generation)
    capture = first.request("POST", "/captures", {"session_id": session, "client_request_id": "py-record", "max_duration_seconds": 60})["id"]
    stream = other.subscribe(session)
    assert stream.next()["type"] == "hello"
    assert stream.next(timeout_ms=10) is None
    frame = {"kind": "classic_data", "id": 291, "data_hex": "0102"}
    one = first.submit(session, generation, request_id="python-once", frame=frame)
    two = first.submit(session, generation, request_id="python-once", frame=frame)
    assert one["receipt"] == two["receipt"]
    assert one["confirmation"] == "virtual_bus_accepted"
    events = stream.next()["events"]
    assert any(event.get("frame", {}).get("id") == 291 for event in events)
    from concurrent.futures import ThreadPoolExecutor
    import time
    def interrupted_read():
        try:
            stream.next(timeout_ms=1000)
            raise AssertionError("closed subscription returned data")
        except CanHarborError as error:
            assert error.status == 4
    with ThreadPoolExecutor(max_workers=1) as pool:
        waiting = pool.submit(interrupted_read)
        time.sleep(0.02)
        stream.close()
        waiting.result(timeout=3)
    stream.close()
    assert other.receipt_by_request(session, "python-once", generation=generation)["receipt"] == one["receipt"]
    odd_id = "query + /?&= 中文 🚀"
    odd = first.submit(session, generation, request_id=odd_id, frame=frame)
    assert other.receipt_by_request(session, odd_id)["receipt"] == odd["receipt"]
    for query in [{str(i): "" for i in range(17)}, {"k": "x" * 4096}]:
        try:
            first.request("GET", "/version", query=query)
            raise AssertionError("oversized query accepted")
        except CanHarborError as error:
            assert error.status == 2
    periodic = first.start_periodic(session, generation, request_id="python-periodic", frame=frame, count=1, interval_ms=1000)
    assert other.periodic_by_request(session, "python-periodic", generation=generation)["id"] == periodic["id"]
    deadline = time.monotonic() + 3
    while True:
        task = other.periodic_task(periodic["id"])
        if task["state"] == "completed":
            assert task["submitted"] == "1"
            break
        assert time.monotonic() < deadline
        time.sleep(0.01)
    long_task = first.start_periodic(session, generation, request_id="python-cancel", frame=frame, count=2, interval_ms=60000)
    other.cancel_periodic(long_task["id"])
    assert other.periodic_by_request(session, "absent", generation=generation) is None
    from unittest.mock import patch
    with patch.object(first, "request", side_effect=CanHarborError(7, {"error": "lost response"})) as calls:
        try:
            first.start_periodic(session, generation, request_id="lost-start", frame=frame, count=1, interval_ms=10)
            raise AssertionError("uncertain admission accepted")
        except TaskStartUnknown as error:
            assert (error.session, error.generation, error.request_id) == (session, generation, "lost-start")
        assert calls.call_count == 1
    with patch.object(other, "periodic_tasks", return_value={"tasks": [
            {"session_id": session, "client_request_id": "ambiguous", "generation": "1"},
            {"session_id": session, "client_request_id": "ambiguous", "generation": "2"}]}):
        try:
            other.periodic_by_request(session, "ambiguous")
            raise AssertionError("ambiguous task was selected")
        except ValueError:
            pass
        assert other.periodic_by_request(session, "ambiguous", generation="2")["generation"] == "2"
    first.renew(session, generation)
    recorded = first.request("POST", f"/captures/{capture}/stop", {})
    assert recorded["complete"]
    from tempfile import TemporaryDirectory
    from pathlib import Path
    import json
    with TemporaryDirectory(prefix="canharbor-python-download-") as temporary:
        destination = str(Path(temporary) / "capture.ndjson")
        assert other.download(capture, destination) == {"downloaded": True}
        data = Path(destination).read_bytes()
        assert json.loads(data.splitlines()[-1])["complete"]
        assert any(json.loads(line).get("record") == "event" for line in data.splitlines())
        try:
            other.download(capture, destination)
            raise AssertionError("existing download replaced")
        except CanHarborError as error:
            assert error.status == 5
        assert Path(destination).read_bytes() == data
        assert not list(Path(temporary).glob(".canharbor-*.part"))
    first.request("DELETE", f"/captures/{capture}")
    first.stop(session, generation)
    first.release(session, generation)
    parent_stream = other.subscribe(session)
    assert parent_stream.next()["type"] == "hello"
    other.close()
    try:
        parent_stream.next(timeout_ms=10)
        raise AssertionError("parent close left a stream usable")
    except CanHarborError as error:
        assert error.status == 4
    parent_stream.close()
    deadline = time.monotonic() + 3
    while first.request("GET", "/diagnostics")["ws_slots_available"] != 16:
        assert time.monotonic() < deadline, "closed streams retained service slots"
        time.sleep(0.01)
    bounded_streams = [first.subscribe(session) for _ in range(16)]
    try:
        first.subscribe(session)
        raise AssertionError("stream capacity exceeded")
    except CanHarborError as error:
        assert error.status == 8
    finally:
        for bounded in bounded_streams:
            bounded.close()
    try:
        first.acquire("../bad")
        raise AssertionError("invalid path accepted")
    except ValueError:
        pass
    try:
        first.request("POST", "/sessions", {"invalid": float("nan")})
        raise AssertionError("nonfinite JSON accepted")
    except ValueError:
        pass
first.close()
try:
    first.devices()
    raise AssertionError("closed client remained usable")
except CanHarborError as error:
    assert error.status == 4
print("python virtual control passed")
