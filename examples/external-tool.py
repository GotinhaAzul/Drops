#!/usr/bin/env python3
"""Tiny Drops external tool example: append a line to a daily log.

Stdio protocol (per crates/buddy-agent/src/lib.rs and examples/config.toml):
the engine spawns ``command`` (argv, never a shell string) with its working
directory set to the confined ``project_dir``, writes ONE JSON line to stdin,
and reads ONE JSON object from stdout. Stderr is diagnostic only and is never sent to the model.

Request line: ``{"name": "<tool>", "arguments": {...}}``. A bare arguments
object (``{"text": "..."}``) is also accepted, in case the engine passes
arguments unwrapped.

Response object: ``{"result": "<text handed back to the model>"}`` on success,
``{"error": "<message>"}`` with a non-zero exit status on failure.

Companion config snippet (risk ``mutating`` needs one approval per call):

    [[external_tools]]
    name = "daily_note"
    description = "Append a line to the project daily log"
    command = ["/absolute/path/to/external-tool.py"]
    risk = "mutating"
    parameters = { type = "object", properties = { text = { type = "string" } }, required = ["text"] }
"""

import json
import os
import sys
from datetime import date
from pathlib import Path


def fail(message):
    json.dump({"error": message}, sys.stdout)
    sys.stdout.write("\n")
    sys.exit(1)


def main() -> None:
    try:
        request = json.loads(sys.stdin.readline() or "{}")
    except json.JSONDecodeError as exc:
        fail(f"request is not valid JSON: {exc}")
    args = request.get("arguments", request)
    if not isinstance(args, dict):
        fail("arguments must be a JSON object")
    text = args.get("text")
    if not isinstance(text, str) or not text.strip():
        fail("arguments.text must be a non-empty string")
    day = args.get("date", date.today().isoformat())
    if not isinstance(day, str) or not day.strip():
        fail("arguments.date must be a string when given")

    log_path = Path(os.environ.get("DAILY_LOG", Path.cwd() / "daily-log.md"))
    try:
        with log_path.open("a", encoding="utf-8") as fh:
            fh.write(f"- {day}: {text.strip()}\n")
    except OSError as exc:
        fail(f"could not append to {log_path}: {exc}")

    json.dump({"result": f"appended to {log_path.name}: {text.strip()}"}, sys.stdout)
    sys.stdout.write("\n")


if __name__ == "__main__":
    main()
