"""Concurrent, resource-bounded runtime for the dependency-free MCP adapter."""

from __future__ import annotations

import json
import os
from pathlib import Path
import re
import subprocess
import sys
import threading
import time
from typing import Any


DEFAULT_TIMEOUT_MS = 300_000
DEFAULT_REQUEST_BYTES = 4 * 1024 * 1024
DEFAULT_STDOUT_BYTES = 16 * 1024 * 1024
DEFAULT_STDERR_BYTES = 1024 * 1024


def _positive_env(name: str, default: int) -> int:
    try:
        value = int(os.environ.get(name, str(default)))
    except ValueError:
        return default
    return max(1, value)


def _response(request_id: Any, result: Any) -> dict[str, Any]:
    return {"jsonrpc": "2.0", "id": request_id, "result": result}


def _rpc_error(request_id: Any, code: int, message: str, data: Any = None) -> dict[str, Any]:
    error: dict[str, Any] = {"code": code, "message": message}
    if data is not None:
        error["data"] = data
    return {"jsonrpc": "2.0", "id": request_id, "error": error}


def _adapter_error(command: str, code: str, message: str) -> dict[str, Any]:
    return {"ok": False, "command": command, "error": {"code": code, "message": message}}


def _tool_result(payload: dict[str, Any]) -> dict[str, Any]:
    return {
        "content": [
            {
                "type": "text",
                "text": json.dumps(payload, separators=(",", ":"), ensure_ascii=False),
            }
        ],
        "structuredContent": payload,
        "isError": payload.get("ok") is False,
    }


class _Job:
    def __init__(self, request_id: Any, command: str) -> None:
        self.request_id = request_id
        self.command = command
        self.cancelled = threading.Event()
        self.lock = threading.Lock()
        self.process: subprocess.Popen[bytes] | None = None

    def attach(self, process: subprocess.Popen[bytes]) -> None:
        with self.lock:
            self.process = process
            cancelled = self.cancelled.is_set()
        if cancelled:
            self._terminate(process)

    def cancel(self) -> None:
        self.cancelled.set()
        with self.lock:
            process = self.process
        if process is not None:
            self._terminate(process)

    @staticmethod
    def _terminate(process: subprocess.Popen[bytes]) -> None:
        if process.poll() is None:
            try:
                process.terminate()
            except OSError:
                pass


class _BoundedReader(threading.Thread):
    def __init__(self, stream: Any, limit: int, process: subprocess.Popen[bytes]) -> None:
        super().__init__(daemon=True)
        self.stream = stream
        self.limit = limit
        self.process = process
        self.data = bytearray()
        self.exceeded = False

    def run(self) -> None:
        try:
            while True:
                chunk = self.stream.read(65_536)
                if not chunk:
                    break
                remaining = self.limit - len(self.data)
                if remaining > 0:
                    self.data.extend(chunk[:remaining])
                if len(chunk) > remaining:
                    self.exceeded = True
                    if self.process.poll() is None:
                        try:
                            self.process.terminate()
                        except OSError:
                            pass
                # Continue draining after the bound to avoid pipe deadlocks.
        finally:
            try:
                self.stream.close()
            except OSError:
                pass


class Server:
    def __init__(
        self,
        tools: list[dict[str, Any]],
        protocol_version: str,
        server_version: str,
        default_cli: Path,
    ) -> None:
        self.tools = tools
        self.protocol_version = protocol_version
        self.server_version = server_version
        self.default_cli = default_cli
        self.initialized = False
        self.max_request_bytes = _positive_env(
            "POSTFLOP_MCP_MAX_REQUEST_BYTES", DEFAULT_REQUEST_BYTES
        )
        self.max_stdout_bytes = _positive_env(
            "POSTFLOP_SOLVER_MAX_STDOUT_BYTES", DEFAULT_STDOUT_BYTES
        )
        self.max_stderr_bytes = _positive_env(
            "POSTFLOP_SOLVER_MAX_STDERR_BYTES", DEFAULT_STDERR_BYTES
        )
        self.timeout_ms = _positive_env("POSTFLOP_SOLVER_TIMEOUT_MS", DEFAULT_TIMEOUT_MS)
        self.state_lock = threading.RLock()
        self.output_lock = threading.Lock()
        self.stderr_lock = threading.Lock()
        self.jobs: dict[Any, _Job] = {}
        self.active_solve_id: Any = None
        self.workers: set[threading.Thread] = set()

    def emit(self, value: dict[str, Any]) -> None:
        encoded = json.dumps(value, separators=(",", ":"), ensure_ascii=False)
        with self.output_lock:
            sys.stdout.write(encoded + "\n")
            sys.stdout.flush()

    def run(self) -> int:
        stream = sys.stdin.buffer
        while True:
            line = stream.readline(self.max_request_bytes + 1)
            if not line:
                break
            if len(line) > self.max_request_bytes:
                prefix = line
                while not line.endswith(b"\n"):
                    line = stream.readline(self.max_request_bytes + 1)
                    if not line:
                        break
                request_id = self._id_from_prefix(prefix)
                self.emit(_rpc_error(request_id, -32600, "Request line is too large"))
                continue
            if not line.strip():
                continue
            try:
                message = json.loads(line.decode("utf-8"))
            except (UnicodeDecodeError, json.JSONDecodeError):
                self.emit(_rpc_error(None, -32700, "Parse error"))
                continue
            try:
                outgoing = self.dispatch(message)
            except Exception as error:  # One malformed request must not kill the server.
                request_id = message.get("id") if isinstance(message, dict) else None
                outgoing = _rpc_error(request_id, -32603, "Internal error", str(error))
            if outgoing is not None:
                self.emit(outgoing)
        self.shutdown()
        return 0

    @staticmethod
    def _id_from_prefix(prefix: bytes) -> Any:
        match = re.search(rb'"id"\s*:\s*(null|-?\d+|"(?:[^"\\]|\\.)*")', prefix)
        if match is None:
            return None
        try:
            return json.loads(match.group(1))
        except (UnicodeDecodeError, json.JSONDecodeError):
            return None

    def dispatch(self, message: Any) -> dict[str, Any] | None:
        if not isinstance(message, dict):
            return _rpc_error(None, -32600, "Invalid Request")
        request_id = message.get("id")
        notification = "id" not in message
        if message.get("jsonrpc") != "2.0" or not isinstance(message.get("method"), str):
            return None if notification else _rpc_error(request_id, -32600, "Invalid Request")

        method = message["method"]
        params = message.get("params", {})
        if notification:
            if method == "notifications/cancelled" and isinstance(params, dict):
                self.cancel(params.get("requestId"))
            return None

        if method == "initialize":
            if not isinstance(params, dict) or not isinstance(params.get("protocolVersion"), str):
                return _rpc_error(request_id, -32602, "Invalid initialize params")
            self.initialized = True
            return _response(
                request_id,
                {
                    "protocolVersion": self.protocol_version,
                    "capabilities": {"tools": {"listChanged": False}},
                    "serverInfo": {"name": "postflop-solver", "version": self.server_version},
                    "instructions": (
                        "This is a heads-up postflop solver. Call solver_capabilities first, then "
                        "solve_postflop with ranges and a compact action tree. The agent should "
                        "choose a useful abstraction and may use partial or downstream nodelock "
                        "constraints."
                    ),
                },
            )
        if method == "ping":
            return _response(request_id, {})
        if method in ("tools/list", "tools/call") and not self.initialized:
            return _rpc_error(request_id, -32002, "Server must be initialized first")
        if method == "tools/list":
            return _response(request_id, {"tools": self.tools})
        if method == "tools/call":
            validation = self._validate_tool_call(params)
            if validation is not None:
                return _rpc_error(request_id, -32602, validation)
            name = params["name"]
            arguments = params["arguments"]
            command = "capabilities" if name == "solver_capabilities" else "solve"
            if command == "solve":
                with self.state_lock:
                    if self.active_solve_id is not None:
                        return _rpc_error(request_id, -32602, "A solve is already in progress")
                    self.active_solve_id = request_id
            job = _Job(request_id, command)
            worker = threading.Thread(
                target=self._tool_worker,
                args=(job, arguments if command == "solve" else None),
                daemon=True,
            )
            with self.state_lock:
                self.jobs[request_id] = job
                self.workers.add(worker)
            worker.start()
            return None
        return _rpc_error(request_id, -32601, "Method not found")

    @staticmethod
    def _validate_tool_call(params: Any) -> str | None:
        if not isinstance(params, dict):
            return "tools/call params must be an object"
        name = params.get("name")
        if name not in ("solver_capabilities", "solve_postflop"):
            return "Unknown or missing tool name"
        arguments = params.get("arguments", {})
        if not isinstance(arguments, dict):
            return "Tool arguments must be an object"
        if name == "solver_capabilities" and arguments:
            return "solver_capabilities accepts no arguments"
        return None

    def cancel(self, request_id: Any) -> None:
        with self.state_lock:
            job = self.jobs.get(request_id)
        if job is not None:
            job.cancel()

    def _tool_worker(self, job: _Job, arguments: dict[str, Any] | None) -> None:
        try:
            payload = self._invoke_solver(job, arguments)
            self.emit(_response(job.request_id, _tool_result(payload)))
        finally:
            with self.state_lock:
                self.jobs.pop(job.request_id, None)
                if self.active_solve_id == job.request_id:
                    self.active_solve_id = None
                self.workers.discard(threading.current_thread())

    def _invoke_solver(self, job: _Job, arguments: dict[str, Any] | None) -> dict[str, Any]:
        executable = Path(
            os.environ.get("POSTFLOP_SOLVER_CLI", str(self.default_cli))
        ).expanduser()
        if not executable.is_file() or not os.access(executable, os.X_OK):
            return _adapter_error(
                job.command,
                "solver_unavailable",
                f"postflop solver CLI is missing or not executable: {executable}",
            )
        argv = [str(executable), "capabilities" if job.command == "capabilities" else "solve"]
        try:
            process = subprocess.Popen(
                argv,
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
            )
        except (OSError, ValueError) as error:
            return _adapter_error(job.command, "solver_unavailable", f"could not start solver: {error}")
        job.attach(process)

        if process.stdin is not None:
            try:
                if arguments is not None:
                    encoded = json.dumps(
                        arguments, separators=(",", ":"), ensure_ascii=False
                    ).encode("utf-8")
                    process.stdin.write(encoded)
                    process.stdin.flush()
            except (BrokenPipeError, OSError):
                pass
            finally:
                process.stdin.close()

        assert process.stdout is not None and process.stderr is not None
        stdout_reader = _BoundedReader(process.stdout, self.max_stdout_bytes, process)
        stderr_reader = _BoundedReader(process.stderr, self.max_stderr_bytes, process)
        stdout_reader.start()
        stderr_reader.start()

        timed_out = False
        deadline = time.monotonic() + self.timeout_ms / 1000
        terminate_at: float | None = None
        while process.poll() is None:
            now = time.monotonic()
            if job.cancelled.is_set() or now >= deadline or stdout_reader.exceeded:
                if now >= deadline:
                    timed_out = True
                if terminate_at is None:
                    try:
                        process.terminate()
                    except OSError:
                        pass
                    terminate_at = now
                elif now - terminate_at >= 0.25:
                    try:
                        process.kill()
                    except OSError:
                        pass
            time.sleep(0.005)
        stdout_reader.join(timeout=1)
        stderr_reader.join(timeout=1)

        if stderr_reader.data:
            diagnostic = stderr_reader.data.decode("utf-8", errors="replace")
            with self.stderr_lock:
                sys.stderr.write(diagnostic)
                if not diagnostic.endswith("\n"):
                    sys.stderr.write("\n")
                if stderr_reader.exceeded:
                    sys.stderr.write("[postflop MCP: solver stderr truncated]\n")
                sys.stderr.flush()

        if job.cancelled.is_set():
            return _adapter_error(job.command, "solver_cancelled", "Solver request was cancelled")
        if timed_out:
            return _adapter_error(
                job.command,
                "solver_timeout",
                f"postflop solver exceeded {self.timeout_ms} ms",
            )
        if stdout_reader.exceeded:
            return _adapter_error(
                job.command,
                "solver_output_too_large",
                f"solver stdout exceeded {self.max_stdout_bytes} bytes",
            )
        try:
            payload = json.loads(stdout_reader.data.decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError) as error:
            return _adapter_error(
                job.command,
                "invalid_solver_output",
                f"solver stdout was not one JSON object: {error}",
            )
        if not isinstance(payload, dict):
            return _adapter_error(
                job.command, "invalid_solver_output", "solver stdout must contain a JSON object"
            )
        if process.returncode != 0 and payload.get("ok") is not False:
            return _adapter_error(
                job.command, "solver_failed", f"solver exited with status {process.returncode}"
            )
        return payload

    def shutdown(self) -> None:
        with self.state_lock:
            jobs = list(self.jobs.values())
            workers = list(self.workers)
        for job in jobs:
            job.cancel()
        for worker in workers:
            worker.join(timeout=0.5)


def main(
    tools: list[dict[str, Any]],
    protocol_version: str,
    server_version: str,
    default_cli: Path,
) -> int:
    return Server(tools, protocol_version, server_version, default_cli).run()
