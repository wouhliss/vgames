#!/usr/bin/env python3
"""Public, scoped test receipts; application output never enters the artifact."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import sys

FORMAT = "vgames.test-receipts/1"
MAX_BYTES = 2 * 1024 * 1024
ANSI = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]")


def sanitize(line):
    line = ANSI.sub("", line).strip()
    if re.fullmatch(r"test [A-Za-z_][A-Za-z0-9_:]{0,255} \.\.\. (?:ok|FAILED|ignored)", line):
        return line
    match = re.match(r"test result: (ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored;", line)
    if match:
        return f"test result: {match[1]}. {match[2]} passed; {match[3]} failed; {match[4]} ignored;"
    if re.fullmatch(r"ok \d+ - part (?:1: add the server, sign in, choose a library \(M1\)|2: install, play, stop, verify, uninstall \(M2\))", line):
        return line
    if re.match(r"✓\s+\d+\s+.*foundation\.spec\.ts.*› admins see the navigation(?:\s|$)", line):
        return "foundation.spec.ts admins see the navigation"
    match = re.match(r"([1-9]\d*) passed(?:\s|\(|$)", line)
    if match:
        return f"{match[1]} passed"
    match = re.fullmatch(r"Wine plan: .* with (Dxmt|Dxvk|Wined3d)", line)
    if match:
        return f"Wine plan: [redacted] with {match[1]}"
    numeric_metrics = (
        r"budget memory: 32 connections, 512 MiB: peak RSS \d+ MiB \(budget 256\), \d+ MiB/s \((?:release|debug) build, including process start\)",
        r"budget disk: peak \d+ bytes, final \d+ bytes, \d+ bytes beyond \(allowance \d+, \d+ samples\)",
        r"budget throughput: \d+ GiB loopback, download phase [\d.]+ s = [\d.]+ GB/s; whole install [\d.]+ s = [\d.]+ GB/s; peak connections \d+, \d+ buffers, \d+ ranges served",
        r"budget throughput: the same folder takes plain sequential writes at [\d.]+ GB/s \(the disk's ceiling for this run\)",
    )
    for pattern in numeric_metrics:
        match = re.search(pattern + r"$", line)
        if match:
            return match[0]
    return None


def filename(step):
    return hashlib.sha256(step.encode()).hexdigest() + ".json"


def context(step):
    sha = os.environ["GITHUB_SHA"]
    if os.environ.get("GITHUB_EVENT_NAME") == "pull_request":
        # Only public identity fields are retained from the event, never its body.
        event = json.loads(Path(os.environ["GITHUB_EVENT_PATH"]).read_text())
        sha = event["pull_request"]["head"]["sha"]
    if not re.fullmatch(r"[0-9a-f]{40}", sha):
        raise ValueError("invalid source identity")
    return {"format": FORMAT, "sha": sha, "run_id": int(os.environ["GITHUB_RUN_ID"]),
            "attempt": int(os.environ["GITHUB_RUN_ATTEMPT"]),
            "job": os.environ["VGAMES_EVIDENCE_JOB"], "step": step}


def write(path, value):
    encoded = json.dumps(value, sort_keys=True).encode()
    if len(encoded) > MAX_BYTES:
        raise ValueError("receipt size limit")
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    temporary = path.with_suffix(".tmp")
    temporary.write_bytes(encoded)
    temporary.replace(path)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=("capture", "finish"))
    parser.add_argument("step")
    parser.add_argument("status", nargs="?", type=int)
    args = parser.parse_args()
    path = Path(os.environ["VGAMES_RECEIPT_DIR"]) / filename(args.step)
    expected = context(args.step)
    if args.mode == "capture":
        lines = set()
        for line in sys.stdin:
            sys.stdout.write(line)
            sys.stdout.flush()
            safe = sanitize(line)
            if safe is not None:
                lines.add(safe)
            if len(lines) > 4096:
                raise ValueError("receipt count limit")
        write(path, dict(expected, exit_code=None, lines=sorted(lines)))
    else:
        if args.status is None or not 0 <= args.status <= 255 or path.stat().st_size > MAX_BYTES:
            raise ValueError("invalid receipt completion")
        value = json.loads(path.read_text())
        if any(value.get(key) != item for key, item in expected.items()):
            raise ValueError("receipt identity mismatch")
        value["exit_code"] = args.status
        write(path, value)


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, KeyError, TypeError):
        print("Scoped test receipt collection failed", file=sys.stderr)
        raise SystemExit(1)
