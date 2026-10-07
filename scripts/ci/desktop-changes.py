"""Git-native matrix scope detection; no third-party action or shell interpolation."""
import os
import re
import subprocess
from pathlib import Path


def needs_desktop(paths):
    exact = {"Cargo.toml", "Cargo.lock", "pnpm-lock.yaml", "rust-toolchain.toml",
             ".github/workflows/desktop-matrix.yml", "scripts/ci/desktop-changes.py"}
    return any(path in exact or path.startswith(("apps/desktop/", "crates/")) for path in paths)


def main():
    desktop = True
    if os.environ["CHANGE_EVENT"] == "pull_request":
        base = os.environ["CHANGE_BASE"]
        if not re.fullmatch(r"[0-9a-f]{40}", base):
            raise ValueError("Expected the PR base commit SHA")
        paths = subprocess.check_output(["git", "diff", "--name-only", "-z", base, "HEAD", "--"])
        desktop = needs_desktop(paths.decode("utf-8", errors="surrogateescape").split("\0"))
    with Path(os.environ["GITHUB_OUTPUT"]).open("a") as output:
        output.write(f"desktop={str(desktop).lower()}\n")


if __name__ == "__main__":
    main()
