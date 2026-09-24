#!/usr/bin/env python3
"""Measure launcher idle cost on Linux (00-overview §7: idle CPU ~0%, idle RAM < 200 MB).

Samples a process and all its descendants (WebKit web/network processes included)
over a window and reports CPU time, CPU %, PSS memory, and context switches per second per
process. Context switches are the closest /proc proxy for wakeups: an idle,
event-driven process sits in blocking waits and switches only when the OS or the
compositor pokes it.

Usage: idle.py <pid> [seconds=60]
"""

import os
import sys
import time

CLK_TCK = os.sysconf("SC_CLK_TCK")
PAGE = os.sysconf("SC_PAGE_SIZE")


def children_of(pid):
    kids = []
    for task in os.listdir(f"/proc/{pid}/task"):
        try:
            with open(f"/proc/{pid}/task/{task}/children") as f:
                kids += [int(c) for c in f.read().split()]
        except OSError:
            pass
    return kids


def tree(pid):
    out, todo = [], [pid]
    while todo:
        p = todo.pop()
        out.append(p)
        todo += children_of(p)
    return out


def name(pid):
    try:
        with open(f"/proc/{pid}/comm") as f:
            return f.read().strip()
    except OSError:
        return "?"


def cpu_ticks(pid):
    with open(f"/proc/{pid}/stat") as f:
        fields = f.read().rsplit(")", 1)[1].split()
    # utime and stime are fields 14 and 15 (1-based); after the comm split they start at index 11.
    return int(fields[11]) + int(fields[12])


def pss_bytes(pid):
    """Proportional set size: shared pages split between the processes using them."""
    try:
        with open(f"/proc/{pid}/smaps_rollup") as f:
            for line in f:
                if line.startswith("Pss:"):
                    return int(line.split()[1]) * 1024
    except OSError:
        pass
    with open(f"/proc/{pid}/statm") as f:
        return int(f.read().split()[1]) * PAGE


def switches(pid):
    total = 0
    for task in os.listdir(f"/proc/{pid}/task"):
        try:
            with open(f"/proc/{pid}/task/{task}/status") as f:
                for line in f:
                    if line.startswith(("voluntary_ctxt_switches", "nonvoluntary_ctxt_switches")):
                        total += int(line.split()[1])
        except OSError:
            pass
    return total


def sample(pids):
    data = {}
    for p in pids:
        try:
            data[p] = (cpu_ticks(p), switches(p), pss_bytes(p))
        except OSError:
            pass
    return data


def main():
    if len(sys.argv) < 2:
        print(__doc__)
        sys.exit(2)
    root = int(sys.argv[1])
    seconds = float(sys.argv[2]) if len(sys.argv) > 2 else 60.0
    pids = tree(root)
    before = sample(pids)
    start = time.monotonic()
    time.sleep(seconds)
    elapsed = time.monotonic() - start
    after = sample(pids)

    total_cpu = total_sw = total_rss = 0
    print(f"{'pid':>8} {'process':<22} {'cpu s':>7} {'cpu %':>6} {'switch/s':>9} {'pss MB':>7}")
    for p in pids:
        if p not in before or p not in after:
            continue
        cpu = (after[p][0] - before[p][0]) / CLK_TCK
        # Threads that exit during the window take their counters with them.
        sw = max(0, after[p][1] - before[p][1]) / elapsed
        rss = after[p][2] / 1e6
        total_cpu += cpu
        total_sw += sw
        total_rss += rss
        print(f"{p:>8} {name(p):<22} {cpu:>7.2f} {100 * cpu / elapsed:>6.2f} {sw:>9.1f} {rss:>7.1f}")
    print(f"{'':>8} {'total':<22} {total_cpu:>7.2f} {100 * total_cpu / elapsed:>6.2f} {total_sw:>9.1f} {total_rss:>7.1f}")
    print(f"window: {elapsed:.1f} s")


if __name__ == "__main__":
    main()
