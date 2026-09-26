#!/usr/bin/env python3
"""Before signing (release-runtimes.yml): download every entry of runtimes.json that the previous
published catalog did not have, and check its size and SHA-256 against the pin. Nothing is signed
that was not just verified. Exit 1 on the first mismatch.
"""

from __future__ import annotations

import argparse
import json
import sys
import tempfile
from pathlib import Path

import watch


def key(entry: dict) -> tuple:
    return entry["id"], entry["version"], entry["os"], entry["arch"], entry["sha256"]


def new_entries(catalog: dict, previous: dict | None) -> list[dict]:
    seen = {key(e) for e in (previous or {}).get("runtimes", [])}
    return [e for e in catalog["runtimes"] if key(e) not in seen]


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("catalog", type=Path, help="the runtimes.json about to be signed")
    parser.add_argument("--previous", type=Path, help="the last published runtimes.json")
    args = parser.parse_args(argv)
    catalog = json.loads(args.catalog.read_text(encoding="utf-8"))
    previous = json.loads(args.previous.read_text(encoding="utf-8")) if args.previous else None
    todo = new_entries(catalog, previous)
    with tempfile.TemporaryDirectory() as tmp:
        for entry in todo:
            file = Path(tmp) / "archive"
            try:
                sha256, _ = watch.download(entry["url"], file, entry["size"])
            except watch.IntegrityError as e:
                print(f"FAIL {entry['id']} {entry['version']}: {e}")
                return 1
            finally:
                file.unlink(missing_ok=True)
            if sha256 != entry["sha256"]:
                print(f"FAIL {entry['id']} {entry['version']}: SHA-256 {sha256}, pinned {entry['sha256']}")
                return 1
            print(f"ok   {entry['id']} {entry['version']} ({entry['os']}, {entry['arch']}): {sha256}")
    print(f"{len(todo)} new entries verified, {len(catalog['runtimes']) - len(todo)} unchanged")
    return 0


if __name__ == "__main__":
    sys.exit(main())
