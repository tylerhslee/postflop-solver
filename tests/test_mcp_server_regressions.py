#!/usr/bin/env python3
"""Resource and lifecycle regression contract for the postflop MCP server."""

import json
import os
from pathlib import Path
import selectors
import subprocess
import sys
import tempfile
import time
import unittest

from tests.test_mcp_server import PROTOCOL_VERSION, REPO, SAMPLE_REQUEST, SERVER


FAKE_CLI = REPO / "tests" / "fixtures" / "fake_postflop_cli_limits.py"


class Client:
    def __init__(self, **environment: str) -> None:
        env = os.environ.copy()
        env["POSTFLOP_SOLVER_CLI"] = str(FAKE_CLI)
        env.update(environment)
        self.process = subprocess.Popen(
            [sys.executable, str(SERVER)], cwd=REPO, env=env,
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            text=True, encoding="utf-8", bufsize=1,
        )
        self.next_id = 1

    def close(self) -> None:
        if self.process.poll() is None:
            self.process.terminate()
        try:
            self.process.communicate(timeout=1)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.communicate(timeout=1)

    def send(self, method: str, params=None) -> int:
        request_id = self.next_id
        self.next_id += 1
        message = {"jsonrpc": "2.0", "id": request_id, "method": method}
        if params is not None:
            message["params"] = params
        self.write(message)
        return request_id

    def notify(self, method: str, params=None) -> None:
        message = {"jsonrpc": "2.0", "method": method}
        if params is not None:
            message["params"] = params
        self.write(message)

    def write(self, message: dict) -> None:
        assert self.process.stdin
        self.process.stdin.write(json.dumps(message, separators=(",", ":")) + "\n")
        self.process.stdin.flush()

    def read(self, expected_id: int, timeout: float = 2) -> dict:
        assert self.process.stdout
        selector = selectors.DefaultSelector()
        selector.register(self.process.stdout, selectors.EVENT_READ)
        try:
            self.assert_ready(selector.select(timeout), timeout)
            response = json.loads(self.process.stdout.readline())
        finally:
            selector.close()
        self.assertEqual(response.get("id"), expected_id)
        return response

    def request(self, method: str, params=None) -> dict:
        request_id = self.send(method, params)
        return self.read(request_id)

    def initialize(self) -> None:
        response = self.request("initialize", {
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": {"name": "regression-test", "version": "1.0"},
        })
        self.assertNotIn("error", response)
        self.notify("notifications/initialized")

    def assert_ready(self, ready, timeout: float) -> None:
        if not ready:
            raise AssertionError(f"no response within {timeout}s")

    def assertEqual(self, left, right) -> None:
        if left != right:
            raise AssertionError(f"expected {right!r}, got {left!r}")

    def assertNotIn(self, key, mapping) -> None:
        if key in mapping:
            raise AssertionError(f"unexpected {key!r}: {mapping!r}")


class McpRegressionTests(unittest.TestCase):
    def setUp(self) -> None:
        self.clients = []

    def tearDown(self) -> None:
        for client in self.clients:
            client.close()

    def client(self, **environment: str) -> Client:
        client = Client(**environment)
        self.clients.append(client)
        return client

    def test_cancel_terminates_in_flight_solver_and_server_survives(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            lifecycle = Path(directory) / "lifecycle"
            client = self.client(
                FAKE_SOLVER_MODE="cancellable",
                FAKE_SOLVER_LIFECYCLE_FILE=str(lifecycle),
                POSTFLOP_SOLVER_TIMEOUT_MS="5000",
            )
            client.initialize()
            solve_id = client.send("tools/call", {
                "name": "solve_postflop", "arguments": SAMPLE_REQUEST,
            })
            deadline = time.monotonic() + 1
            while not lifecycle.exists() and time.monotonic() < deadline:
                time.sleep(0.01)
            self.assertTrue(lifecycle.exists(), "fake solver did not start")

            started = time.monotonic()
            client.notify("notifications/cancelled", {
                "requestId": solve_id, "reason": "player acted",
            })
            response = client.read(solve_id, timeout=1)
            self.assertLess(time.monotonic() - started, 1)
            self.assertTrue(response["result"]["isError"])
            self.assertEqual(response["result"]["structuredContent"]["error"]["code"], "solver_cancelled")
            self.assertEqual(lifecycle.read_text(encoding="utf-8"), "terminated")
            self.assertEqual(client.request("ping")["result"], {})

    def test_request_byte_limit_rejects_line_and_server_survives(self) -> None:
        client = self.client(POSTFLOP_MCP_MAX_REQUEST_BYTES="512")
        client.initialize()
        request_id = client.send("tools/list", {"padding": "x" * 700})
        response = client.read(request_id)
        self.assertEqual(response["error"]["code"], -32600)
        self.assertIn("large", response["error"]["message"].lower())
        self.assertEqual(client.request("ping")["result"], {})

    def test_solver_stdout_byte_limit_is_tool_error_and_server_survives(self) -> None:
        client = self.client(
            FAKE_SOLVER_MODE="oversized_output",
            POSTFLOP_SOLVER_MAX_STDOUT_BYTES="256",
        )
        client.initialize()
        result = client.request("tools/call", {
            "name": "solve_postflop", "arguments": SAMPLE_REQUEST,
        })["result"]
        self.assertTrue(result["isError"])
        self.assertEqual(result["structuredContent"]["error"]["code"], "solver_output_too_large")
        self.assertEqual(client.request("ping")["result"], {})

    def test_initialize_requires_string_protocol_version(self) -> None:
        bad_params = [
            {"capabilities": {}, "clientInfo": {"name": "x", "version": "1"}},
            {"protocolVersion": 20251125, "capabilities": {}, "clientInfo": {"name": "x", "version": "1"}},
        ]
        for params in bad_params:
            client = self.client()
            response = client.request("initialize", params)
            self.assertEqual(response["error"]["code"], -32602)

    def test_tools_rejected_before_initialize(self) -> None:
        client = self.client()
        response = client.request("tools/list", {})
        self.assertIn("error", response)
        self.assertIn("initial", response["error"]["message"].lower())

    def test_unknown_or_malformed_tool_calls_are_invalid_params(self) -> None:
        client = self.client()
        client.initialize()
        for params in (
            {"name": "unknown", "arguments": {}},
            {"name": "solve_postflop", "arguments": []},
            {"arguments": {}},
        ):
            response = client.request("tools/call", params)
            self.assertEqual(response["error"]["code"], -32602)


if __name__ == "__main__":
    unittest.main()
