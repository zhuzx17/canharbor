# SPDX-License-Identifier: AGPL-3.0-or-later
"""Explicit local-service control. No automatic lease renewal or TX retries."""
from __future__ import annotations

import ctypes as C
import json
from pathlib import Path
from threading import Lock
from weakref import finalize
from typing import Any, Optional

__all__ = ["Client", "EventStream", "CanHarborError", "TaskStartUnknown"]


class CanHarborError(RuntimeError):
    def __init__(self, status: int, response: Any = None):
        self.status = status
        self.response = response
        super().__init__(f"CANHarbor status {status}: {response}")


class TaskStartUnknown(CanHarborError):
    """Recover using periodic_by_request; never automatically repeat admission."""
    def __init__(self, session: str, generation: str, request_id: str, response: Any):
        self.session, self.generation, self.request_id = session, generation, request_id
        super().__init__(7, response)


def _library(path: str):
    selected = Path(path)
    if not selected.is_absolute() or not selected.is_file():
        raise ValueError("library must be an explicit absolute file path")
    lib = C.CDLL(str(selected.resolve(strict=True)))
    byte = C.c_void_p
    size = C.c_size_t
    handle = C.c_uint64
    signatures = {
        "canharbor_abi_version": ([], C.c_uint32),
        "canharbor_client_open": ([byte, size, C.POINTER(handle)], C.c_int32),
        "canharbor_client_close": ([handle], C.c_int32),
        "canharbor_request": ([handle, byte, size, byte, size, byte, size,
                               byte, size, C.POINTER(handle)], C.c_int32),
        "canharbor_request_with_query": ([handle, byte, size, byte, size, byte, size,
                                          byte, size, byte, size, C.POINTER(handle)], C.c_int32),
        "canharbor_capture_download": ([handle, byte, size, byte, size, C.POINTER(handle)], C.c_int32),
        "canharbor_stream_open": ([handle, byte, size, byte, size, C.POINTER(handle)], C.c_int32),
        "canharbor_stream_next": ([handle, C.c_uint32, C.POINTER(handle)], C.c_int32),
        "canharbor_stream_close": ([handle], C.c_int32),
        "canharbor_response_status": ([handle], C.c_int32),
        "canharbor_response_copy": ([handle, byte, size, C.POINTER(size)], C.c_int32),
        "canharbor_response_close": ([handle], C.c_int32),
    }
    for name, (args, result) in signatures.items():
        if (name in {"canharbor_request_with_query", "canharbor_capture_download"} or name.startswith("canharbor_stream_")) and not hasattr(lib, name):
            continue  # Original ABI 1 libraries still support non-query requests.
        function = getattr(lib, name)
        function.argtypes = args
        function.restype = result
    if lib.canharbor_abi_version() != 1:
        raise ValueError("unsupported CANHarbor C ABI major")
    return lib


class Client:
    """One native identity. Callers own explicit acquisition, renewal and stop.

    Loading the supplied library executes native code. Use a trusted CANHarbor
    build. Closing releases the client transport; service leases retain their
    normal TTL unless explicitly released first.
    """
    def __init__(self, runtime_dir: str, *, library: str):
        directory = Path(runtime_dir)
        if not directory.is_absolute():
            raise ValueError("runtime directory must be absolute")
        self._lock = Lock()
        self._handle = 0
        self._lib = _library(library)
        raw = str(directory).encode("utf-8")
        handle = C.c_uint64()
        status = self._lib.canharbor_client_open(raw, len(raw), C.byref(handle))
        if status:
            raise CanHarborError(status)
        self._handle = handle.value
        self._finalizer = finalize(self, self._lib.canharbor_client_close, handle.value)

    def close(self):
        with self._lock:
            handle, self._handle = self._handle, 0
        if handle:
            self._finalizer.detach()
            status = self._lib.canharbor_client_close(handle)
            if status:
                raise CanHarborError(status)

    def __enter__(self):
        return self

    def __exit__(self, *_):
        self.close()

    def request(self, method: str, path: str, body: Any = None, *,
                generation: Optional[str] = None, query: Optional[dict[str, str]] = None) -> Any:
        with self._lock:
            handle = self._handle
        if not handle:
            raise CanHarborError(4, {"error": "client is closed"})
        if generation is not None and not isinstance(generation, str):
            raise TypeError("generation must be a decimal string")
        method_bytes = method.encode("utf-8")
        path_bytes = path.encode("utf-8")
        body_bytes = json.dumps(body, allow_nan=False, separators=(",", ":")).encode("utf-8")
        generation_bytes = (generation or "").encode("utf-8")
        response = C.c_uint64()
        if query is None:
            status = self._lib.canharbor_request(
                handle, method_bytes, len(method_bytes), path_bytes, len(path_bytes),
                body_bytes, len(body_bytes), generation_bytes, len(generation_bytes),
                C.byref(response))
        else:
            if not isinstance(query, dict) or any(not isinstance(k, str) or not isinstance(v, str) for k, v in query.items()):
                raise TypeError("query must map strings to strings")
            if not hasattr(self._lib, "canharbor_request_with_query"):
                raise CanHarborError(2, {"error": "native library lacks the query extension"})
            query_bytes = json.dumps(list(query.items()), ensure_ascii=False, separators=(",", ":")).encode("utf-8")
            status = self._lib.canharbor_request_with_query(
                handle, method_bytes, len(method_bytes), path_bytes, len(path_bytes),
                body_bytes, len(body_bytes), generation_bytes, len(generation_bytes),
                query_bytes, len(query_bytes), C.byref(response))
        if status:
            raise CanHarborError(status)
        return self._response(response.value)

    def _response(self, response: int):
        try:
            size = C.c_size_t()
            status = self._lib.canharbor_response_copy(response, None, 0, C.byref(size))
            if status != 8 or not 0 < size.value <= 2 * 1024 * 1024:
                raise CanHarborError(status or 8, {"error": "invalid response size"})
            buffer = C.create_string_buffer(size.value)
            status = self._lib.canharbor_response_copy(
                response, buffer, len(buffer), C.byref(size))
            if status:
                raise CanHarborError(status)
            value = json.loads(buffer.raw[:size.value])
            status = self._lib.canharbor_response_status(response)
            if status:
                raise CanHarborError(status, value)
            return value
        finally:
            self._lib.canharbor_response_close(response)

    @staticmethod
    def _session(session: str) -> str:
        if (not isinstance(session, str) or not session or len(session) > 128
                or not session.isascii()
                or not all(c.isalnum() or c in "_-" for c in session)):
            raise ValueError("invalid opaque session ID")
        return f"/sessions/{session}"

    def devices(self):
        return self.request("GET", "/devices")

    def native_features(self):
        """Inspect local ABI extensions without contacting or opening a device."""
        return {
            "abi_major": self._lib.canharbor_abi_version(),
            "query": hasattr(self._lib, "canharbor_request_with_query"),
            "capture_download": hasattr(self._lib, "canharbor_capture_download"),
            "event_stream": all(hasattr(self._lib, name) for name in (
                "canharbor_stream_open", "canharbor_stream_next", "canharbor_stream_close")),
        }

    def create(self, channel: str, *, bitrate: int = 500000,
               active: bool = False, data_bitrate: Optional[int] = None):
        config = {"mode": "active" if active else "listen_only", "nominal_bitrate": bitrate}
        if data_bitrate is not None:
            config["fd"] = {"enabled": True, "data_bitrate": data_bitrate, "iso": True}
        return self.request("POST", "/sessions", {"channel_id": channel, "config": config})

    def acquire(self, session: str):
        return self.request("POST", self._session(session) + "/lease", {})

    def renew(self, session: str, generation: str):
        return self.request("POST", self._session(session) + "/lease/renew", {}, generation=generation)

    def release(self, session: str, generation: str):
        return self.request("DELETE", self._session(session) + "/lease", {}, generation=generation)

    def start(self, session: str, generation: str):
        return self.request("POST", self._session(session) + "/start", {}, generation=generation)

    def stop(self, session: str, generation: str):
        return self.request("POST", self._session(session) + "/stop", {}, generation=generation)

    def submit(self, session: str, generation: str, *, request_id: str, frame: dict):
        return self.request("POST", self._session(session) + "/tx",
                            {"client_request_id": request_id, "frame": frame}, generation=generation)

    def receipt_by_request(self, session: str, request_id: str, *, generation: Optional[str] = None):
        """Read an earlier result without acquiring control or resubmitting."""
        if not isinstance(request_id, str) or not 1 <= len(request_id) <= 128:
            raise ValueError("request ID must contain 1–128 Unicode characters")
        query = {"client_request_id": request_id}
        if generation is not None:
            if (not isinstance(generation, str) or not generation.isascii() or not generation.isdecimal()
                    or (len(generation) > 1 and generation[0] == "0") or len(generation) > 20
                    or int(generation) > 18446744073709551615):
                raise ValueError("generation must be a canonical u64 string")
            query["generation"] = generation
        return self.request("GET", self._session(session) + "/tx", query=query)

    def start_periodic(self, session: str, generation: str, *, request_id: str,
                       frame: dict, count: int, interval_ms: int):
        """Caller controls renewal. A failed response must never cause a retry."""
        if not isinstance(request_id, str) or not 1 <= len(request_id) <= 128:
            raise ValueError("request ID must contain 1–128 Unicode characters")
        if type(count) is not int or type(interval_ms) is not int or not 1 <= count <= 6000 or not 10 <= interval_ms <= 60000 or (count - 1) * interval_ms > 60000:
            raise ValueError("periodic plan exceeds bounds")
        if (not isinstance(generation, str) or not generation.isascii() or not generation.isdecimal()
                or len(generation) > 20 or str(int(generation)) != generation or int(generation) > 18446744073709551615):
            raise ValueError("generation must be a canonical u64 string")
        path = self._session(session) + "/tasks"
        try:
            return self.request("POST", path, {"client_request_id": request_id, "frame": frame,
                                "count": count, "interval_ms": interval_ms}, generation=generation)
        except CanHarborError as error:
            if error.status == 7:
                raise TaskStartUnknown(session, generation, request_id, error.response) from error
            raise

    def periodic_tasks(self):
        return self.request("GET", "/tasks")

    def periodic_task(self, task: str):
        from uuid import UUID
        return self.request("GET", f"/tasks/{UUID(task)}")

    def cancel_periodic(self, task: str):
        from uuid import UUID
        return self.request("POST", f"/tasks/{UUID(task)}/cancel", {})

    def periodic_by_request(self, session: str, request_id: str, *, generation: Optional[str] = None):
        """Read-only recovery; None is not proof of nonexecution after expiry/restart."""
        self._session(session)
        if not isinstance(request_id, str) or not 1 <= len(request_id) <= 128:
            raise ValueError("invalid request ID")
        if generation is not None and (not isinstance(generation, str) or not generation.isascii() or not generation.isdecimal() or len(generation) > 20 or str(int(generation)) != generation or int(generation) > 18446744073709551615):
            raise ValueError("generation must be a canonical u64 string")
        tasks = [t for t in self.periodic_tasks()["tasks"] if t["session_id"] == session
                 and t["client_request_id"] == request_id and (generation is None or t["generation"] == generation)]
        if len(tasks) > 1:
            raise ValueError("multiple tasks match; specify the original generation")
        return tasks[0] if tasks else None

    def download(self, capture: str, destination: str):
        """Bounded download with no replacement of an existing destination."""
        from uuid import UUID
        capture_bytes = str(UUID(capture)).encode("ascii")
        output = Path(destination)
        if not output.is_absolute():
            raise ValueError("download destination must be absolute")
        destination_bytes = str(output).encode("utf-8")
        with self._lock:
            handle = self._handle
        if not handle:
            raise CanHarborError(4, {"error": "client is closed"})
        if not hasattr(self._lib, "canharbor_capture_download"):
            raise CanHarborError(2, {"error": "native library lacks the download extension"})
        response = C.c_uint64()
        status = self._lib.canharbor_capture_download(handle, capture_bytes, len(capture_bytes),
            destination_bytes, len(destination_bytes), C.byref(response))
        if status:
            raise CanHarborError(status)
        return self._response(response.value)

    def subscribe(self, session: str, *, filter: Optional[dict] = None, after_sequence: Optional[str] = None):
        """Create a read-only observer; no control acquisition or renewal."""
        self._session(session)
        with self._lock:
            handle = self._handle
        if not handle:
            raise CanHarborError(4, {"error": "client is closed"})
        if not hasattr(self._lib, "canharbor_stream_open"):
            raise CanHarborError(2, {"error": "native library lacks event streams"})
        body = {"type": "subscribe", "protocol_version": "1.0", "filter": filter or {}}
        if after_sequence is not None:
            body["after_sequence"] = after_sequence
        raw = json.dumps(body, allow_nan=False, separators=(",", ":")).encode("utf-8")
        session_raw = session.encode("utf-8")
        stream = C.c_uint64()
        status = self._lib.canharbor_stream_open(handle, session_raw, len(session_raw), raw, len(raw), C.byref(stream))
        if status:
            raise CanHarborError(status)
        return EventStream(self, stream.value)


class EventStream:
    """Bounded pull subscription. next() returns None when idle; never reconnects."""
    def __init__(self, client: Client, handle: int):
        self._client = client
        self._handle = handle
        self._lock = Lock()
        self._finalizer = finalize(self, client._lib.canharbor_stream_close, handle)

    def next(self, *, timeout_ms: int = 1000):
        if not isinstance(timeout_ms, int) or isinstance(timeout_ms, bool) or not 1 <= timeout_ms <= 1000:
            raise ValueError("timeout_ms must be 1–1000")
        with self._lock:
            handle = self._handle
        if not handle:
            raise CanHarborError(4, {"error": "event subscription is closed"})
        response = C.c_uint64()
        status = self._client._lib.canharbor_stream_next(handle, timeout_ms, C.byref(response))
        if status == 10:
            return None
        if status:
            raise CanHarborError(status)
        return self._client._response(response.value)

    def close(self):
        with self._lock:
            handle, self._handle = self._handle, 0
        if handle:
            self._finalizer.detach()
            status = self._client._lib.canharbor_stream_close(handle)
            if status not in (0, 4):
                raise CanHarborError(status)

    def __enter__(self):
        return self

    def __exit__(self, *_):
        self.close()
