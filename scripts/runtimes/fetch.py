#!/usr/bin/env python3
"""Download, verify and extract one runtime for the smoke tests (A5-T12, runtimes.yml).

Picks runtime ID from the watcher's new entries (entries.json) or, failing that, the last entry
of the catalog, downloads it, checks its size and SHA-256 against the entry, then extracts it
with tarfile's `data` filter (no absolute paths, no escaping links, no device files). Prints the
extraction directory. Exit 3 when no entry for ID exists.
"""

from __future__ import annotations

import argparse
import json
import sys
import tarfile
import tomllib
from pathlib import Path

import watch

TAR_MODES = {"tar": "r:", "tar.gz": "r:gz", "tar.xz": "r:xz"}


def pick(entries: list[dict], catalog: dict, runtime_id: str) -> dict | None:
    for entry in entries:
        if entry["id"] == runtime_id:
            return entry
    pinned = [e for e in catalog.get("runtime", []) if e["id"] == runtime_id]
    return pinned[-1] if pinned else None


def fetch(entry: dict, dest: Path) -> Path:
    archive = dest / entry["url"].rsplit("/", 1)[-1]
    sha256, _ = watch.download(entry["url"], archive, entry["size"])
    if sha256 != entry["sha256"]:
        raise watch.IntegrityError(f"{entry['id']} {entry['version']}: SHA-256 {sha256} != pinned {entry['sha256']}")
    mode = TAR_MODES.get(entry["archive"])
    if mode is None:
        raise watch.Attention(f"{entry['archive']} archives are not supported by the smoke tests yet")
    out = dest / f"{entry['id']}-{entry['version']}"
    out.mkdir(parents=True, exist_ok=True)
    with tarfile.open(archive, mode) as tar:
        tar.extractall(out, filter="data")
    archive.unlink()
    return out


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--entries", type=Path, required=True)
    parser.add_argument("--catalog", type=Path, default=Path("runtimes/catalog.toml"))
    parser.add_argument("--id", required=True)
    parser.add_argument("--dest", type=Path, required=True)
    args = parser.parse_args(argv)
    entries = json.loads(args.entries.read_text(encoding="utf-8"))
    catalog = tomllib.loads(args.catalog.read_text(encoding="utf-8"))
    entry = pick(entries, catalog, args.id)
    if entry is None:
        print(f"no {args.id} entry to test with", file=sys.stderr)
        return 3
    print(fetch(entry, args.dest))
    return 0


if __name__ == "__main__":
    sys.exit(main())
