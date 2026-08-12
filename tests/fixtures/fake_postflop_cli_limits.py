#!/usr/bin/env python3
"""Deterministic solver fixture for cancellation and output-bound tests."""

import json
import os
from pathlib import Path
import signal
import sys
import time


mode = os.environ.get("FAKE_SOLVER_MODE", "success")


def emit(value: object) -> None:
    print(json.dumps(value, separators=(",", ":")), flush=True)


if sys.argv[1:] == ["capabilities"]:
    emit({"ok": True, "command": "capabilities", "postflop_only": True})
    raise SystemExit(0)

if sys.argv[1:] != ["solve"]:
    raise SystemExit(64)

request = json.load(sys.stdin)
if mode == "cancellable":
    lifecycle = Path(os.environ["FAKE_SOLVER_LIFECYCLE_FILE"])

    def terminate(_signum: int, _frame: object) -> None:
        lifecycle.write_text("terminated", encoding="utf-8")
        raise SystemExit(143)

    signal.signal(signal.SIGTERM, terminate)
    lifecycle.write_text(f"started:{os.getpid()}", encoding="utf-8")
    while True:
        time.sleep(0.02)

if mode == "oversized_output":
    emit({"ok": True, "command": "solve", "padding": "x" * 4096})
else:
    emit({"ok": True, "command": "solve", "received_request": request})
