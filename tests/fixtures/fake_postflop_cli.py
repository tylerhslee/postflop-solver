#!/usr/bin/env python3
"""Deterministic stand-in for postflop-cli used by MCP contract tests."""

import json
import os
import sys
import time


def emit(value: object) -> None:
    sys.stdout.write(json.dumps(value, separators=(",", ":")) + "\n")
    sys.stdout.flush()


mode = os.environ.get("FAKE_SOLVER_MODE", "success")
arguments = sys.argv[1:]

if mode == "timeout":
    time.sleep(2)

if arguments == ["capabilities"]:
    if mode == "malformed":
        sys.stdout.write("not-json\n")
        raise SystemExit(0)
    emit(
        {
            "ok": True,
            "command": "capabilities",
            "schema_version": "1",
            "postflop_only": True,
            "custom_bet_sizes": True,
            "nodelocking": {
                "supported": True,
                "partial": True,
                "downstream_paths": True,
            },
            "deadlines": True,
        }
    )
    raise SystemExit(0)

if arguments != ["solve"]:
    emit(
        {
            "ok": False,
            "command": "cli",
            "error": {
                "code": "fake_bad_arguments",
                "message": f"unexpected arguments: {arguments!r}",
            },
        }
    )
    raise SystemExit(64)

request = json.load(sys.stdin)

if mode == "stderr":
    sys.stderr.write("iteration 7: fake solver progress\n")
    sys.stderr.flush()

if mode == "solver_error":
    emit(
        {
            "ok": False,
            "command": "solve",
            "error": {
                "code": "invalid_board",
                "message": "board contains duplicate card 2s",
                "field": "board",
            },
        }
    )
    raise SystemExit(2)

if mode == "malformed":
    sys.stdout.write("solver log accidentally written to stdout\n")
    raise SystemExit(0)

emit(
    {
        "ok": True,
        "command": "solve",
        "schema_version": "1",
        "status": "deadline_reached"
        if request["solve"].get("deadline_ms") == 0
        else "solved",
        "stop_reason": "deadline"
        if request["solve"].get("deadline_ms") == 0
        else "target_exploitability",
        "iterations": 0 if request["solve"].get("deadline_ms") == 0 else 7,
        "exploitability": 0.25,
        "root_actions": ["check", "bet:10"],
        "received_request": request,
    }
)
