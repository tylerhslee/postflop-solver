#!/usr/bin/env python3
"""Black-box JSON-RPC contract tests for the dependency-free MCP adapter."""

from __future__ import annotations

import json
import os
from pathlib import Path
import selectors
import stat
import subprocess
import sys
import tempfile
import unittest


REPO = Path(__file__).resolve().parents[1]
SERVER = REPO / "mcp" / "postflop_mcp_server.py"
FAKE_CLI = REPO / "tests" / "fixtures" / "fake_postflop_cli.py"
PROTOCOL_VERSION = "2025-11-25"

SAMPLE_REQUEST = {
    "board": "2s3h4d6c7c",
    "oop_range": "AsAh,QsQh,JsJh",
    "ip_range": "KsKh",
    "pot": 20,
    "effective_stack": 10,
    "bet_sizes": {
        "flop": {
            "oop": {"bet": "", "raise": ""},
            "ip": {"bet": "", "raise": ""},
        },
        "turn": {
            "oop": {"bet": "", "raise": ""},
            "ip": {"bet": "", "raise": ""},
        },
        "river": {
            "oop": {"bet": "50%,a", "raise": "a"},
            "ip": {"bet": "40%,a", "raise": "2.5x,a"},
        },
    },
    "thresholds": {"add_allin": 1.5, "force_allin": 0.15, "merge": 0.1},
    "solve": {
        "max_iterations": 100,
        "target_exploitability": 0.01,
        "compressed": False,
        "deadline_ms": 17,
    },
    "rake": {"rate": 0.0, "cap": 0.0},
    "nodelocks": [
        {
            "path": [],
            "player": "oop",
            "strategy": {"JsJh": {"check": 0.8, "all-in": 0.2}},
        }
    ],
    "inspect_hands": [
        {"path": [], "player": "oop", "hands": ["AsAh", "JsJh"]}
    ],
}


class McpProcess:
    def __init__(self, *, solver_path: Path = FAKE_CLI, **environment: str) -> None:
        env = os.environ.copy()
        env["POSTFLOP_SOLVER_CLI"] = str(solver_path)
        env.update(environment)
        self.process = subprocess.Popen(
            [sys.executable, str(SERVER)],
            cwd=REPO,
            env=env,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            encoding="utf-8",
            bufsize=1,
        )
        self._next_id = 1

    def close(self) -> None:
        if self.process.poll() is None:
            self.process.terminate()
        try:
            self.process.communicate(timeout=2)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.communicate(timeout=2)

    def request(self, method: str, params: dict | None = None) -> dict:
        request_id = self._next_id
        self._next_id += 1
        message = {"jsonrpc": "2.0", "id": request_id, "method": method}
        if params is not None:
            message["params"] = params
        assert self.process.stdin is not None
        try:
            self.process.stdin.write(json.dumps(message, separators=(",", ":")) + "\n")
            self.process.stdin.flush()
        except BrokenPipeError as error:
            stderr = self.process.stderr.read() if self.process.stderr else ""
            raise AssertionError(f"MCP server exited before responding: {stderr}") from error

        raw = self._readline(timeout=3)
        try:
            response = json.loads(raw)
        except json.JSONDecodeError as error:
            self.fail_with_stderr(f"stdout was not one JSON-RPC line: {raw!r}", error)
        if response.get("id") != request_id:
            self.fail_with_stderr(
                f"response id {response.get('id')!r} did not match {request_id!r}"
            )
        return response

    def notify(self, method: str, params: dict | None = None) -> None:
        message = {"jsonrpc": "2.0", "method": method}
        if params is not None:
            message["params"] = params
        assert self.process.stdin is not None
        self.process.stdin.write(json.dumps(message, separators=(",", ":")) + "\n")
        self.process.stdin.flush()

    def _readline(self, timeout: float) -> str:
        assert self.process.stdout is not None
        selector = selectors.DefaultSelector()
        selector.register(self.process.stdout, selectors.EVENT_READ)
        try:
            if not selector.select(timeout):
                self.fail_with_stderr("timed out waiting for one MCP response")
            raw = self.process.stdout.readline()
        finally:
            selector.close()
        if not raw:
            self.fail_with_stderr("MCP server closed stdout before responding")
        if not raw.endswith("\n"):
            self.fail_with_stderr(f"MCP response was not newline-delimited: {raw!r}")
        return raw

    def fail_with_stderr(self, message: str, cause: Exception | None = None) -> None:
        stderr = ""
        if self.process.poll() is not None and self.process.stderr is not None:
            stderr = self.process.stderr.read()
        failure = AssertionError(f"{message}; server stderr: {stderr}")
        if cause:
            raise failure from cause
        raise failure


class PostflopMcpContractTests(unittest.TestCase):
    def setUp(self) -> None:
        self.clients: list[McpProcess] = []

    def tearDown(self) -> None:
        for client in self.clients:
            client.close()

    def client(self, *, solver_path: Path = FAKE_CLI, **env: str) -> McpProcess:
        client = McpProcess(solver_path=solver_path, **env)
        self.clients.append(client)
        return client

    def initialize(self, client: McpProcess) -> dict:
        response = client.request(
            "initialize",
            {
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "contract-test", "version": "1.0"},
            },
        )
        client.notify("notifications/initialized")
        return response["result"]

    def test_initialize_negotiates_current_protocol_and_gives_agent_guidance(self) -> None:
        result = self.initialize(self.client())

        self.assertEqual(result["protocolVersion"], PROTOCOL_VERSION)
        self.assertEqual(result["capabilities"], {"tools": {"listChanged": False}})
        self.assertEqual(result["serverInfo"]["name"], "postflop-solver")
        self.assertRegex(result["serverInfo"]["version"], r"^\d+\.\d+\.\d+$")
        instructions = result["instructions"].lower()
        for phrase in ("postflop", "solver_capabilities", "solve_postflop", "nodelock"):
            self.assertIn(phrase, instructions)

    def test_tools_list_exposes_only_two_compact_strict_tools(self) -> None:
        client = self.client()
        self.initialize(client)
        response = client.request("tools/list", {})
        tools = response["result"]["tools"]

        self.assertEqual([tool["name"] for tool in tools], ["solver_capabilities", "solve_postflop"])
        by_name = {tool["name"]: tool for tool in tools}
        capabilities_schema = by_name["solver_capabilities"]["inputSchema"]
        self.assertEqual(
            capabilities_schema,
            {"type": "object", "properties": {}, "additionalProperties": False},
        )

        solve = by_name["solve_postflop"]
        schema = solve["inputSchema"]
        self.assertEqual(schema["type"], "object")
        self.assertFalse(schema["additionalProperties"])
        self.assertEqual(
            set(schema["properties"]),
            {
                "board",
                "oop_range",
                "ip_range",
                "pot",
                "effective_stack",
                "bet_sizes",
                "thresholds",
                "solve",
                "rake",
                "nodelocks",
                "inspect_hands",
            },
        )
        self.assertEqual(
            set(schema["required"]),
            {
                "board",
                "oop_range",
                "ip_range",
                "pot",
                "effective_stack",
                "bet_sizes",
                "thresholds",
                "solve",
            },
        )
        self.assertFalse(schema["properties"]["solve"]["additionalProperties"])
        self.assertFalse(schema["properties"]["nodelocks"]["items"]["additionalProperties"])
        self.assertEqual(solve["annotations"]["readOnlyHint"], True)
        self.assertEqual(solve["annotations"]["openWorldHint"], False)

    def test_capabilities_returns_cli_json_as_text_and_structured_content(self) -> None:
        client = self.client()
        self.initialize(client)
        response = client.request(
            "tools/call", {"name": "solver_capabilities", "arguments": {}}
        )
        result = response["result"]

        self.assertFalse(result["isError"])
        self.assertTrue(result["structuredContent"]["postflop_only"])
        self.assertTrue(result["structuredContent"]["custom_bet_sizes"])
        self.assertEqual(json.loads(result["content"][0]["text"]), result["structuredContent"])

    def test_solve_forwards_the_exact_cli_request_and_keeps_stderr_off_framing(self) -> None:
        client = self.client(FAKE_SOLVER_MODE="stderr")
        self.initialize(client)
        response = client.request(
            "tools/call", {"name": "solve_postflop", "arguments": SAMPLE_REQUEST}
        )
        result = response["result"]

        self.assertFalse(result["isError"])
        self.assertEqual(result["structuredContent"]["received_request"], SAMPLE_REQUEST)
        self.assertEqual(result["structuredContent"]["root_actions"], ["check", "bet:10"])
        self.assertEqual(json.loads(result["content"][0]["text"]), result["structuredContent"])

        follow_up = client.request("ping")
        self.assertEqual(follow_up, {"jsonrpc": "2.0", "id": 3, "result": {}})

    def test_solver_input_error_is_a_structured_mcp_tool_error(self) -> None:
        client = self.client(FAKE_SOLVER_MODE="solver_error")
        self.initialize(client)
        response = client.request(
            "tools/call", {"name": "solve_postflop", "arguments": SAMPLE_REQUEST}
        )
        result = response["result"]

        self.assertTrue(result["isError"])
        self.assertEqual(result["structuredContent"]["error"]["code"], "invalid_board")
        self.assertEqual(result["structuredContent"]["error"]["field"], "board")
        self.assertEqual(json.loads(result["content"][0]["text"]), result["structuredContent"])

    def test_missing_or_unexecutable_solver_is_a_tool_error(self) -> None:
        with self.subTest("missing"):
            client = self.client(solver_path=REPO / "target" / "missing-postflop-cli")
            self.initialize(client)
            result = client.request(
                "tools/call", {"name": "solver_capabilities", "arguments": {}}
            )["result"]
            self.assertTrue(result["isError"])
            self.assertEqual(result["structuredContent"]["error"]["code"], "solver_unavailable")

        with tempfile.TemporaryDirectory() as directory, self.subTest("unexecutable"):
            path = Path(directory) / "postflop-cli"
            path.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
            path.chmod(stat.S_IRUSR | stat.S_IWUSR)
            client = self.client(solver_path=path)
            self.initialize(client)
            result = client.request(
                "tools/call", {"name": "solver_capabilities", "arguments": {}}
            )["result"]
            self.assertTrue(result["isError"])
            self.assertEqual(result["structuredContent"]["error"]["code"], "solver_unavailable")

    def test_configurable_process_timeout_returns_a_tool_error(self) -> None:
        client = self.client(
            FAKE_SOLVER_MODE="timeout", POSTFLOP_SOLVER_TIMEOUT_MS="30"
        )
        self.initialize(client)
        response = client.request(
            "tools/call", {"name": "solve_postflop", "arguments": SAMPLE_REQUEST}
        )
        result = response["result"]

        self.assertTrue(result["isError"])
        self.assertEqual(result["structuredContent"]["error"]["code"], "solver_timeout")
        self.assertEqual(result["structuredContent"]["command"], "solve")

    def test_solver_deadline_is_forwarded_and_remains_a_successful_best_so_far_result(self) -> None:
        request = json.loads(json.dumps(SAMPLE_REQUEST))
        request["solve"]["deadline_ms"] = 0
        client = self.client()
        self.initialize(client)
        result = client.request(
            "tools/call", {"name": "solve_postflop", "arguments": request}
        )["result"]

        self.assertFalse(result["isError"])
        self.assertEqual(result["structuredContent"]["status"], "deadline_reached")
        self.assertEqual(result["structuredContent"]["stop_reason"], "deadline")
        self.assertEqual(result["structuredContent"]["received_request"], request)

    def test_malformed_solver_stdout_is_reported_without_breaking_json_rpc(self) -> None:
        client = self.client(FAKE_SOLVER_MODE="malformed")
        self.initialize(client)
        result = client.request(
            "tools/call", {"name": "solver_capabilities", "arguments": {}}
        )["result"]

        self.assertTrue(result["isError"])
        self.assertEqual(result["structuredContent"]["error"]["code"], "invalid_solver_output")
        ping = client.request("ping")
        self.assertEqual(ping["result"], {})


if __name__ == "__main__":
    unittest.main()
