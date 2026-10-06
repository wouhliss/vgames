"""Maintain the one stable-upgrade issue; the watcher never changes repository files."""
import json
import os
import subprocess
import tempfile
from pathlib import Path


def gh(*args):
    return subprocess.check_output(["gh", *args], text=True)


def main():
    title = "Rust stable toolchain upgrade"
    issues = json.loads(gh("issue", "list", "--state", "open", "--search", '"Rust stable toolchain upgrade" in:title', "--limit", "100", "--json", "number,title"))
    matches = [issue["number"] for issue in issues if issue["title"] == title]
    pin, stable = os.environ["WATCH_PIN"], os.environ["WATCH_STABLE"]
    if not pin or not stable:
        raise RuntimeError("Version discovery failed; cannot close the upgrade issue")
    if os.environ["WATCH_NEWER"] == "false":
        for number in matches:
            gh("issue", "close", str(number), "--comment", f"The pin {pin} has caught up with stable {stable}.")
        return
    body = (f"Stable `{stable}` is newer than the repository pin `{pin}`.\n\n"
            f"Validation result: **{os.environ['WATCH_RESULT']}**. [Workflow run]({os.environ['WATCH_RUN']}).\n\n"
            "Update both version sources together after reviewing the validation output:\n\n"
            f"```diff\n-rust-version = \"{pin}\"\n+rust-version = \"{stable}\"\n"
            f"-channel = \"{pin}\"\n+channel = \"{stable}\"\n```\n"
            "The watcher does not create pull requests or change the toolchain.")
    with tempfile.TemporaryDirectory() as directory:
        path = Path(directory) / "issue.md"
        path.write_text(body)
        if matches:
            gh("issue", "edit", str(matches[0]), "--body-file", str(path))
            for number in matches[1:]:
                gh("issue", "close", str(number), "--comment", f"Consolidated into #{matches[0]}.")
        else:
            gh("issue", "create", "--title", title, "--body-file", str(path))


if __name__ == "__main__":
    main()
