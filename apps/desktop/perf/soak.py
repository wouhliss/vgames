#!/usr/bin/env python3
"""Observe a release launcher's entire Linux process tree; never upload its profile."""

import argparse
import csv
import json
import math
import os
from pathlib import Path
import signal
import statistics
import subprocess
import tempfile
import time


def hours(value):
    value = float(value)
    if not math.isfinite(value) or not 0.1 <= value <= 25:
        raise argparse.ArgumentTypeError("hours must be finite and between 0.1 and 25")
    return value


def processes():
    result = {}
    for directory in Path("/proc").iterdir():
        if not directory.name.isdecimal():
            continue
        try:
            fields = (directory / "stat").read_text().rsplit(")", 1)[1].split()
            result[int(directory.name)] = {
                "parent": int(fields[1]), "group": int(fields[2]), "start": int(fields[19]),
                "rss_kib": max(0, int(fields[21])) * os.sysconf("SC_PAGE_SIZE") // 1024,
                "cpu_seconds": (int(fields[11]) + int(fields[12])) / os.sysconf("SC_CLK_TCK"),
                "threads": int(fields[17]),
            }
        except (FileNotFoundError, ProcessLookupError):
            continue
    return result


def sample(pid, identity, elapsed, known):
    table = processes()
    if pid not in table or table[pid]["start"] != identity:
        raise RuntimeError("launcher exited or its process identity changed")
    selected = {p for p, row in table.items()
                if row["group"] == pid or known.get(p) == row["start"]}
    selected.add(pid)
    while True:
        children = {p for p, row in table.items() if row["parent"] in selected}
        if children <= selected:
            break
        selected |= children
    known.clear()
    known.update({p: table[p]["start"] for p in selected})
    row = {"seconds": round(elapsed, 3), "processes": len(selected)}
    for name in ("rss_kib", "cpu_seconds", "threads"):
        row[name] = sum(table[p][name] for p in selected)
    descriptors, unreadable = 0, 0
    for child in selected:
        try:
            descriptors += len(list(Path(f"/proc/{child}/fd").iterdir()))
        except FileNotFoundError:
            continue
        except PermissionError:
            unreadable += 1
    row["descriptors"] = None if unreadable else descriptors
    row["descriptor_processes_unreadable"] = unreadable
    return row


def summarize(rows, duration):
    warmup = min(600, duration / 3)
    steady = [r for r in rows if r["seconds"] >= warmup]
    if len(steady) < 3 or rows[-1]["seconds"] < duration:
        raise RuntimeError("incomplete soak: need the full duration and three steady samples")
    baseline = steady[0]["rss_kib"]
    end = steady[-1]["rss_kib"]
    if baseline <= 0:
        raise RuntimeError("invalid zero process-tree RSS baseline")
    xs = [r["seconds"] for r in steady]
    if any(a >= b for a, b in zip(xs, xs[1:])):
        raise RuntimeError("sample times must increase")
    ys = [r["rss_kib"] for r in steady]
    mean_x, mean_y = statistics.mean(xs), statistics.mean(ys)
    slope = sum((x - mean_x) * (y - mean_y) for x, y in zip(xs, ys)) / sum(
        (x - mean_x) ** 2 for x in xs
    )
    growth = (end - baseline) / baseline * 100
    slope_growth = slope * (xs[-1] - xs[0]) / baseline * 100
    return {
        "duration_seconds": duration, "warmup_seconds": warmup, "samples": len(rows),
        "start_rss_kib": rows[0]["rss_kib"],
        "median_rss_kib": statistics.median(r["rss_kib"] for r in rows),
        "end_rss_kib": rows[-1]["rss_kib"], "baseline_rss_kib": baseline,
        "slope_mib_per_hour": slope * 3600 / 1024,
        "end_growth_percent": growth, "slope_growth_percent": slope_growth,
        "growth_limit_percent": 5, "passed": max(growth, slope_growth) < 5,
    }


def stop_known(known):
    for pid, identity in known.items():
        descriptor = None
        try:
            descriptor = os.pidfd_open(pid)
            fields = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
            if int(fields[19]) == identity:
                signal.pidfd_send_signal(descriptor, signal.SIGKILL)
        except (ProcessLookupError, FileNotFoundError):
            pass
        finally:
            if descriptor is not None:
                os.close(descriptor)


def wait_for_ui(process, profile, timeout=30):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise RuntimeError("launcher exited before its UI was ready")
        for log in profile.rglob("vgames.log"):
            if not log.is_symlink() and "main UI ready" in log.read_text():
                return
        time.sleep(0.1)
    raise RuntimeError("launcher did not acknowledge its first rendered screen")


def run(binary, duration, output, require_ui_ready=False):
    output.mkdir(parents=True, exist_ok=True)
    rows, process = [], None
    report = {"passed": False, "duration_seconds": duration}
    profile = tempfile.TemporaryDirectory(prefix="vgames-soak-profile-")
    known = {}
    try:
        env = os.environ.copy()
        env["VGAMES_LOG"] = "info"
        for name, suffix in (("HOME", "home"), ("XDG_CONFIG_HOME", "config"),
                             ("XDG_DATA_HOME", "data"), ("XDG_CACHE_HOME", "cache")):
            directory = Path(profile.name) / suffix
            directory.mkdir(mode=0o700)
            env[name] = str(directory)
        process = subprocess.Popen([str(binary)], env=env, start_new_session=True,
                                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        if require_ui_ready:
            wait_for_ui(process, Path(profile.name))
            report["ui_ready"] = True
        started = time.monotonic()
        table = processes()
        if process.pid not in table:
            raise RuntimeError("launcher exited before its first sample")
        identity = table[process.pid]["start"]
        known[process.pid] = identity
        with (output / "rss.csv").open("w", newline="") as target:
            writer = None
            while True:
                elapsed = time.monotonic() - started
                if process.poll() is not None:
                    raise RuntimeError("launcher exited before the soak completed")
                row = sample(process.pid, identity, elapsed, known)
                rows.append(row)
                if writer is None:
                    writer = csv.DictWriter(target, fieldnames=list(row))
                    writer.writeheader()
                writer.writerow(row)
                target.flush()
                print(json.dumps(row), flush=True)
                if elapsed >= duration:
                    break
                try:
                    process.wait(timeout=min(60, duration - elapsed))
                except subprocess.TimeoutExpired:
                    continue
                raise RuntimeError("launcher exited before the soak completed")
        report = summarize(rows, duration)
    except (OSError, RuntimeError) as error:
        report["error"] = str(error) if isinstance(error, RuntimeError) else type(error).__name__
    finally:
        try:
            if process is not None:
                # An unreaped child reserves its PID. After reaping, signal only validated pidfds.
                if process.poll() is None:
                    os.killpg(process.pid, signal.SIGTERM)
                try:
                    process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    process.kill()
                    stop_known(known)
                    process.wait(timeout=10)
            stop_known(known)
        except (OSError, subprocess.TimeoutExpired):
            report.update(passed=False, error="process cleanup failed")
        finally:
            try:
                profile.cleanup()
            except OSError:
                report.update(passed=False, error="profile cleanup failed")
        (output / "summary.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report), flush=True)
    return 0 if report["passed"] else 1


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--hours", required=True, type=hours)
    parser.add_argument("--out", required=True, type=Path)
    parser.add_argument("--require-ui-ready", action="store_true")
    args = parser.parse_args()
    def interrupted(_signal, _frame):
        raise KeyboardInterrupt

    signal.signal(signal.SIGTERM, interrupted)
    raise SystemExit(run(args.binary.resolve(), args.hours * 3600, args.out, args.require_ui_ready))
