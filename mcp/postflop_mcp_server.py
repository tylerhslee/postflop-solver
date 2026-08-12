#!/usr/bin/env python3
"""Dependency-free stdio MCP adapter for postflop-cli.

The transport is newline-delimited JSON-RPC 2.0. Protocol responses are the
only data written to stdout; diagnostics and solver progress stay on stderr.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any


PROTOCOL_VERSION = "2025-11-25"
SERVER_VERSION = "0.1.0"
ROOT = Path(__file__).resolve().parents[1]
DEFAULT_CLI = ROOT / "target" / "release" / "postflop-cli"


def strict_object(properties: dict[str, Any], required: list[str] | None = None) -> dict[str, Any]:
    schema: dict[str, Any] = {
        "type": "object",
        "properties": properties,
        "additionalProperties": False,
    }
    if required:
        schema["required"] = required
    return schema


PLAYER = {"type": "string", "enum": ["oop", "ip"]}
ACTION_PATH = {
    "type": "array",
    "items": {
        "type": "string",
        "description": "Canonical action label, including chance:<card> at chance nodes.",
    },
}
BET_PAIR = strict_object(
    {"bet": {"type": "string"}, "raise": {"type": "string"}},
    ["bet", "raise"],
)
PLAYERS = strict_object({"oop": BET_PAIR, "ip": BET_PAIR}, ["oop", "ip"])
BET_SIZES = strict_object(
    {"flop": PLAYERS, "turn": PLAYERS, "river": PLAYERS},
    ["flop", "turn", "river"],
)
THRESHOLDS = strict_object(
    {
        "add_allin": {"type": "number", "minimum": 0},
        "force_allin": {"type": "number", "minimum": 0},
        "merge": {"type": "number", "minimum": 0},
    },
    ["add_allin", "force_allin", "merge"],
)
SOLVE_OPTIONS = strict_object(
    {
        "max_iterations": {"type": "integer", "minimum": 1},
        "target_exploitability": {"type": "number", "minimum": 0},
        "compressed": {"type": "boolean"},
        "deadline_ms": {"type": "integer", "minimum": 0},
    },
    ["max_iterations", "target_exploitability", "compressed"],
)
RAKE = strict_object(
    {
        "rate": {"type": "number", "minimum": 0, "maximum": 1},
        "cap": {"type": "number", "minimum": 0},
    },
)
NODELOCK = strict_object(
    {
        "path": ACTION_PATH,
        "player": PLAYER,
        "strategy": {
            "type": "object",
            "description": "Exact combos mapped to canonical action frequencies.",
            "additionalProperties": {
                "type": "object",
                "additionalProperties": {"type": "number", "minimum": 0, "maximum": 1},
            },
        },
    },
    ["path", "player", "strategy"],
)
INSPECTION = strict_object(
    {
        "path": ACTION_PATH,
        "player": PLAYER,
        "hands": {"type": "array", "items": {"type": "string"}},
    },
    ["path", "player", "hands"],
)
SOLVE_SCHEMA = strict_object(
    {
        "board": {"type": "string", "description": "Three, four, or five board cards."},
        "oop_range": {"type": "string"},
        "ip_range": {"type": "string"},
        "pot": {"type": "integer", "minimum": 1},
        "effective_stack": {"type": "integer", "minimum": 1},
        "bet_sizes": BET_SIZES,
        "thresholds": THRESHOLDS,
        "solve": SOLVE_OPTIONS,
        "rake": RAKE,
        "nodelocks": {"type": "array", "items": NODELOCK},
        "inspect_hands": {"type": "array", "items": INSPECTION},
    },
    [
        "board",
        "oop_range",
        "ip_range",
        "pot",
        "effective_stack",
        "bet_sizes",
        "thresholds",
        "solve",
    ],
)

TOOLS = [
    {
        "name": "solver_capabilities",
        "description": "Report the local postflop solver's supported features and action labels.",
        "inputSchema": strict_object({}),
        "annotations": {
            "title": "Postflop solver capabilities",
            "readOnlyHint": True,
            "destructiveHint": False,
            "idempotentHint": True,
            "openWorldHint": False,
        },
    },
    {
        "name": "solve_postflop",
        "description": (
            "Solve a heads-up flop, turn, or river tree. Supports custom sizing, deadlines, "
            "partial/downstream nodelocks, and selective hand inspection."
        ),
        "inputSchema": SOLVE_SCHEMA,
        "annotations": {
            "title": "Solve postflop",
            "readOnlyHint": True,
            "destructiveHint": False,
            "idempotentHint": True,
            "openWorldHint": False,
        },
    },
]


if __name__ == "__main__":
    from postflop_mcp_runtime import main as runtime_main

    raise SystemExit(
        runtime_main(TOOLS, PROTOCOL_VERSION, SERVER_VERSION, DEFAULT_CLI)
    )
