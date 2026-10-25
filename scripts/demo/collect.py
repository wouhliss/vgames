#!/usr/bin/env python3
"""Collect exact-commit Actions evidence. Missing work never means a milestone passed."""

import argparse
import base64
import functools
import io
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import zipfile

from receipts import FORMAT, MAX_BYTES, filename, sanitize

ROOT = Path(__file__).resolve().parents[2]
REPO = "wouhliss/vgames"


class EvidenceError(Exception):
    pass


class LogUnavailable(EvidenceError):
    pass


def evidence(method):
    @functools.wraps(method)
    def checked(self, label, *args, **kwargs):
        try:
            return method(self, label, *args, **kwargs)
        except EvidenceError as error:
            return result(label, "failed", str(error))
    return checked


def gh(*args):
    # Logs may contain application output. Never echo a response or gh's stderr.
    try:
        result = subprocess.run(["gh", *args], capture_output=True, timeout=90)
    except (OSError, subprocess.TimeoutExpired) as error:
        raise EvidenceError("Actions evidence request failed") from error
    if result.returncode or len(result.stdout) > 32 * 1024 * 1024:
        raise EvidenceError("Actions evidence is unavailable or exceeds the size limit")
    return re.sub(r"\x1b\[[0-?]*[ -/]*[@-~]", "", result.stdout.decode("utf-8", errors="replace"))


def api(path):
    try:
        return json.loads(gh("api", f"repos/{REPO}/{path}"))
    except (ValueError, UnicodeError) as error:
        raise EvidenceError("Actions returned invalid evidence metadata") from error


def marked_step_log(log, step):
    expected = base64.b64encode(step.encode()).decode()
    active, matches, lines = None, [], []
    for line in log.splitlines():
        line = re.sub(r"^\d{4}-\d\d-\d\dT[\d:.]+Z\s+", "", line)
        start = re.fullmatch(r"VGAMES_STEP_START ([A-Za-z0-9+/=]+)", line)
        end = re.fullmatch(r"VGAMES_STEP_END ([A-Za-z0-9+/=]+) (\d+)", line)
        if start:
            if active is not None:
                raise EvidenceError("Nested evidence step markers")
            active, lines = start[1], []
        elif end:
            if end[1] != active:
                raise EvidenceError("Unbalanced evidence step markers")
            if active == expected:
                if end[2] != "0":
                    raise EvidenceError("Recorded evidence command failed")
                matches.append("\n".join(lines) + "\n")
            active = None
        elif active == expected:
            lines.append(line)
    if active is not None or len(matches) != 1:
        raise EvidenceError("Complete unique step-scoped evidence is unavailable")
    return matches[0]


class Collector:
    def __init__(self, sha):
        self.sha = sha

    @functools.cache
    def source(self, path):
        try:
            value = subprocess.run(["git", "show", f"{self.sha}:{path}"], cwd=ROOT,
                                   capture_output=True, timeout=10)
            if value.returncode:
                commit = subprocess.run(["git", "cat-file", "-e", f"{self.sha}^{{commit}}"],
                                        cwd=ROOT, capture_output=True, timeout=10)
                if commit.returncode:
                    raise EvidenceError("Fetch the selected evidence commit before collecting")
                return None
            if len(value.stdout) > 1024 * 1024:
                raise EvidenceError("Evidence source exceeds the size limit")
            return value.stdout.decode("utf-8")
        except (OSError, subprocess.TimeoutExpired, UnicodeError) as error:
            raise EvidenceError("Evidence source is unavailable") from error

    @functools.cache
    def runs(self, workflow):
        if self.source(f".github/workflows/{workflow}") is None:
            return []
        # Most recent attempt wins, even if an older attempt was green.
        runs = api(f"actions/workflows/{workflow}/runs?head_sha={self.sha}&per_page=100")["workflow_runs"]
        return sorted((r for r in runs if r["head_sha"] == self.sha),
                      key=lambda r: (r["run_number"], r["run_attempt"]), reverse=True)

    @functools.cache
    def jobs(self, run_id, attempt):
        jobs, page = [], 1
        while True:
            batch = api(f"actions/runs/{run_id}/attempts/{attempt}/jobs?per_page=100&page={page}")["jobs"]
            jobs.extend(batch)
            if len(batch) < 100:
                return jobs
            page += 1
            if page > 10:
                raise EvidenceError("Actions job list exceeds the size limit")

    @functools.cache
    def log(self, job_id):
        try:
            value = gh("api", f"repos/{REPO}/actions/jobs/{int(job_id)}/logs")
            if not value.strip():
                raise EvidenceError("Actions returned an empty job log")
            return value
        except EvidenceError:
            try:
                value = gh("run", "view", "--repo", REPO, "--job", str(int(job_id)), "--log")
            except EvidenceError as error:
                raise LogUnavailable("Actions job logs are unavailable") from error
            if not value.strip():
                raise LogUnavailable("Actions returned an empty job log")
            # This value came from gh, whose archive reader prefixes every line with
            # the job and step names (including UNKNOWN STEP for whole-job archives).
            # Preserve content and markers; step_log still requires complete scoping.
            lines = [line.split("\t", 2) for line in value.splitlines()]
            return "".join((parts[2] if len(parts) == 3 else "\t".join(parts)) + "\n" for parts in lines)

    @functools.cache
    def named_job_log(self, job_id):
        # gh adds the actual job and step names to each line from the run-log archive.
        # A receipt from another backend's step must never satisfy this step's proof.
        value = gh("run", "view", "--repo", REPO, "--job", str(int(job_id)), "--log")
        if not value.strip():
            raise EvidenceError("Actions returned an empty named job log")
        return value

    def step_log(self, job_id, step):
        lines = []
        try:
            for line in self.named_job_log(job_id).splitlines():
                fields = line.split("\t", 2)
                if len(fields) == 3 and fields[1] == step:
                    lines.append(fields[2])
        except EvidenceError:
            pass
        if lines:
            scoped = "\n".join(lines) + "\n"
            return marked_step_log(scoped, step) if "VGAMES_STEP_" in scoped else scoped
        # New archives can contain only whole-job files; gh labels every line UNKNOWN STEP.
        # Only balanced markers from our exit-status-preserving wrapper can scope that output.
        return marked_step_log(self.log(job_id), step)

    @functools.cache
    def receipt_log(self, run_id, attempt, job_name, step):
        # A workflow cannot download its own final run-log archive. Producer jobs
        # publish only sanitized receipts, available before this dependent job runs.
        artifacts, page = [], 1
        while True:
            batch = api(f"actions/runs/{run_id}/artifacts?per_page=100&page={page}")["artifacts"]
            if not isinstance(batch, list) or any(not isinstance(item, dict) for item in batch):
                raise EvidenceError("Receipt artifact metadata is invalid")
            artifacts.extend(batch)
            if len(batch) < 100:
                break
            page += 1
            if page > 10:
                raise EvidenceError("Receipt artifact list exceeds the size limit")
        name = f"milestone-receipts-{job_name}-{attempt}"
        matches = [item for item in artifacts if item.get("name") == name]
        if len(matches) != 1 or matches[0].get("expired") is not False:
            raise EvidenceError("Complete unique receipt artifact is unavailable")
        artifact = matches[0]
        size = artifact.get("size_in_bytes")
        if type(size) is not int or not 0 <= size <= MAX_BYTES or type(artifact.get("id")) is not int:
            raise EvidenceError("Receipt artifact exceeds the size limit")
        try:
            response = subprocess.run(
                ["gh", "api", f"repos/{REPO}/actions/artifacts/{int(artifact['id'])}/zip"],
                capture_output=True, timeout=90)
            if response.returncode or len(response.stdout) > MAX_BYTES:
                raise EvidenceError("Receipt artifact download failed")
            with zipfile.ZipFile(io.BytesIO(response.stdout)) as archive:
                entries = archive.infolist()
                if not 1 <= len(entries) <= 32 or sum(entry.file_size for entry in entries) > MAX_BYTES or any(
                        not re.fullmatch(r"[0-9a-f]{64}\.json", entry.filename) for entry in entries):
                    raise EvidenceError("Receipt archive contains invalid entries")
                wanted = [entry for entry in entries if entry.filename == filename(step)]
                if len(wanted) != 1 or wanted[0].file_size > MAX_BYTES:
                    raise EvidenceError("Complete unique step receipt is unavailable")
                value = json.loads(archive.read(wanted[0]))
        except (OSError, subprocess.TimeoutExpired, ValueError, zipfile.BadZipFile, KeyError) as error:
            raise EvidenceError("Receipt artifact is unreadable") from error
        expected = {"format": FORMAT, "sha": self.sha, "run_id": run_id,
                    "attempt": attempt, "job": job_name, "step": step}
        if not isinstance(value, dict) or set(value) != set(expected) | {"exit_code", "lines"} or any(
                value.get(key) != item for key, item in expected.items()) or any(
                type(value.get(key)) is not int for key in ("run_id", "attempt")):
            raise EvidenceError("Receipt artifact identity mismatch")
        if type(value.get("exit_code")) is not int or value["exit_code"] != 0:
            raise EvidenceError("Recorded evidence command failed")
        lines = value.get("lines")
        if not isinstance(lines, list) or len(lines) > 4096 or any(
                not isinstance(line, str) or
                (line != "foundation.spec.ts admins see the navigation" and sanitize(line) != line)
                for line in lines):
            raise EvidenceError("Receipt artifact contains invalid public receipts")
        if len(set(lines)) != len(lines):
            raise EvidenceError("Receipt artifact contains duplicate receipts")
        return "\n".join(lines) + "\n"

    @evidence
    def proof(self, label, workflow, job_name, step, patterns):
        runs = self.runs(workflow)
        if not runs:
            return result(label, "pending", f"no {workflow} run for this commit")
        run = runs[0]
        link = f"https://github.com/{REPO}/actions/runs/{int(run['id'])}"
        jobs = [j for j in self.jobs(run["id"], run["run_attempt"]) if j["name"] == job_name]
        if len(jobs) != 1:
            return result(label, "failed", "expected evidence job is missing or ambiguous", link)
        job = jobs[0]
        if job["status"] != "completed":
            return result(label, "pending", "evidence job has not completed", link)
        if job["conclusion"] != "success":
            return result(label, "failed", "existing evidence job did not pass", link)
        steps = [s for s in job["steps"] if s["name"] == step]
        if len(steps) != 1 or steps[0].get("conclusion") != "success":
            return result(label, "failed", "required test step did not pass", link)
        try:
            log = self.step_log(job["id"], step)
        except LogUnavailable:
            log = self.receipt_log(run["id"], run["run_attempt"], job_name, step)
        if not all(re.search(pattern, log) for pattern in patterns):
            return result(label, "failed", "test receipts missing (zero tests or a skipped scenario)", link)
        return result(label, "passed", "test receipts verified", link)

    @evidence
    def workflow_health(self, label, workflow):
        runs = self.runs(workflow)
        if not runs:
            return result(label, "pending", f"pending: no {workflow} evidence for this commit")
        run = runs[0]
        link = f"https://github.com/{REPO}/actions/runs/{int(run['id'])}"
        if run["status"] != "completed":
            return result(label, "pending", "workflow has not completed", link)
        if run["conclusion"] != "success":
            return result(label, "failed", "existing workflow did not pass", link)
        return result(label, "passed", "workflow passed; scenario receipts are separate requirements", link)

    def health_source_matches(self, run_sha):
        if run_sha == self.sha:
            return True
        if not isinstance(run_sha, str) or not re.fullmatch(r"[0-9a-f]{40}", run_sha):
            raise EvidenceError("Invalid workflow source commit")
        comparison = api(f"compare/{run_sha}...{self.sha}")
        if (comparison.get("status") != "ahead"
                or comparison.get("merge_base_commit", {}).get("sha") != run_sha):
            return False
        files = comparison.get("files")
        if not isinstance(files, list):
            raise EvidenceError("Source comparison files are unavailable")
        # GitHub caps comparison files at 300, even with commit pagination.
        if len(files) >= 300:
            return False
        statuses = {f"docs/agents/status/{owner}.md" for owner in ("ins", "play", "game", "int")}
        for changed in files:
            if not isinstance(changed, dict) or not isinstance(changed.get("filename"), str):
                raise EvidenceError("Invalid source comparison file")
            name = changed["filename"]
            if "previous_filename" in changed:
                return False
            if name in statuses and changed.get("status") == "modified":
                continue
            if (changed.get("status") != "added"
                    or not re.fullmatch(r"\.changes/[a-z0-9]+(?:-[a-z0-9]+)*\.md", name)):
                return False
            source = self.source(name)
            if source is None:
                return False
            lines = source.splitlines()
            if not lines or lines[0] != "---" or "---" not in lines[1:]:
                return False
            header = lines[1:lines[1:].index("---") + 1]
            fields = [re.fullmatch(r"(audience|component|type): ([a-z]+)", line) for line in header]
            if len(fields) != 3 or not all(fields):
                return False
            metadata = {field[1]: field[2] for field in fields}
            if (len(metadata) != 3 or metadata.get("audience") != "internal"
                    or metadata.get("component") not in {"launcher", "admin", "server"}
                    or metadata.get("type") not in {"added", "changed", "fixed", "removed", "security"}):
                return False
        return True

    @evidence
    def latest_main_health(self, label, workflow):
        exact = self.runs(workflow)
        if exact:
            # A candidate fix's exact proof takes precedence over an older main failure.
            return self.workflow_health(label, workflow)
        if self.source(f".github/workflows/{workflow}") is None:
            return result(label, "pending", f"pending: {workflow} is not implemented")
        runs = api(f"actions/workflows/{workflow}/runs?branch=main&per_page=100")["workflow_runs"]
        ordered = sorted(runs, key=lambda run: (run["run_number"], run["run_attempt"]), reverse=True)
        latest = None
        for run in ordered:
            if run.get("head_branch") != "main" or run.get("event") == "pull_request":
                continue
            identity = run.get("head_repository")
            repository = identity.get("full_name") if isinstance(identity, dict) else None
            if repository is None:
                raise EvidenceError("Main workflow repository identity is unavailable")
            if repository == REPO:
                latest = run
                break
        if latest is None:
            return result(label, "pending", f"pending: no main {workflow} evidence")
        link = f"https://github.com/{REPO}/actions/runs/{int(latest['id'])}"
        if latest["status"] != "completed":
            return result(label, "pending", "latest main workflow has not completed", link)
        if latest["conclusion"] != "success":
            return result(label, "failed", "latest main workflow did not pass", link)
        if not self.health_source_matches(latest["head_sha"]):
            return result(label, "pending", "fresh workflow required after source changes", link)
        return result(label, "passed", "latest main workflow passed on equivalent executable source; "
                      "scenario receipts remain separate requirements", link)

    def last_green(self, milestone):
        # History is informational and never substitutes for current-commit proof.
        workflow = "desktop-matrix.yml" if milestone == "m3b" else "e2e.yml"
        runs = api(f"actions/workflows/{workflow}/runs?branch=main&status=completed&per_page=30")["workflow_runs"]
        for run in sorted(runs, key=lambda r: (r["run_number"], r["run_attempt"]), reverse=True):
            if run["head_branch"] != "main":
                continue
            jobs = [job for job in self.jobs(run["id"], run["run_attempt"])
                    if job["name"] == f"Milestone {milestone.upper()}"]
            if len(jobs) == 1 and jobs[0]["conclusion"] == "success":
                return f"https://github.com/{REPO}/actions/runs/{int(run['id'])}"
        return ""


def result(label, state, detail, link=""):
    return {"label": label, "state": state, "detail": detail, "run": link}


def rust_test(name):
    return rf"test {re.escape(name)} \.\.\. ok(?:\r?\n|$)"


def requirements(milestone, collector):
    proof = collector.proof
    desktop = "Desktop build check (Linux, WebKitGTK)"
    launcher = "Launcher end to end (release build, real API)"
    if milestone == "m1":
        return [
            proof("INS-09 part 1", "e2e.yml", launcher, "Parts 1 and 2 (M1, M2)",
                  [r"ok \d+ - part 1: add the server, sign in, choose a library \(M1\)(?![^\n]*# SKIP)"]),
            proof("PLAY-04 navigation", "ci.yml", desktop, "Whole launcher suite (Xvfb)",
                  [rust_test("controllers::navigation::tests::" + name) for name in (
                      "directions_repeat_only_while_held_without_catchup_bursts",
                      "guide_requires_one_second_and_fires_once_per_hold",
                      "presses_debounce_and_accept_never_repeats",
                      "sdl_classification_keeps_unknown_pads_generic",
                      "stick_and_dpad_share_debounce_and_disconnect_resets_everything",
                      "stick_hysteresis_filters_jitter_and_handles_minimum_axis")]),
            proof("INT-05 real admin sign-in", "e2e.yml", "Admin web (Playwright, real API)",
                  "Admin Playwright suite", [r"foundation\.spec\.ts.*admins see the navigation", r"[1-9]\d* passed"]),
        ]
    if milestone == "m2":
        return [
            proof("INS-08 full-scale install", "e2e.yml", "Install end to end (8 GiB)",
                  "Install end to end at full scale", [rust_test("install_e2e")]),
            proof("INS-09 part 2", "e2e.yml", launcher, "Parts 1 and 2 (M1, M2)",
                  [r"ok \d+ - part 2: install, play, stop, verify, uninstall \(M2\)(?![^\n]*# SKIP)"]),
        ]
    if milestone == "m3":
        return [
            proof("GAME-12 real invite install", "ci.yml", desktop,
                  "Database-backed launcher scenarios (no soak)",
                  [rust_test("invite_install_ready_join_on_the_real_install_path")]),
            proof("GAME-08 presented overlay toast", "ci.yml", "Rust (fmt, clippy, tests)",
                  "overlay renderer", [rust_test("the_layer_draws_a_toast_into_presented_frames"),
                                       rust_test("egl_frames_get_the_toast_and_the_panel_within_budget"),
                                       rust_test("glx_frames_get_the_toast_and_the_panel_within_budget")]),
            result("GAME-12 two-profile real launcher scenario", "pending", "pending (GAME-12): harness scenario is not implemented"),
        ]
    if milestone == "m3b":
        # Every OS's existing matrix is collected, even while the missing scenarios are pending.
        rows = [proof(f"desktop {name}", "desktop-matrix.yml", name,
                      "Rust tests (launcher and the crates it ships)", [r"test result: ok\. [1-9]\d* passed"])
                for name in ("Windows x64", "Linux x64", "macOS arm64")]
        rows.append(proof("GAME-05 Proton D3D11", "compat-e2e.yml",
                          "Proton (ubuntu-24.04, lavapipe)",
                          "The D3D11 test game runs through Proton (DXVK)",
                          [rust_test("compat::plan::proton_e2e::the_d3d11_test_game_runs_through_proton")]))
        for label, step, backend in (
            ("DXMT", "The D3D11 test game through Wine + DXMT (recorded; missing Metal goes to human-checklist §B4)", "Dxmt"),
            ("DXVK-macOS", "The D3D11 test game through Wine + DXVK-macOS (recorded)", "Dxvk"),
            ("wined3d", "The D3D11 test game through Wine + wined3d (recorded)", "Wined3d"),
        ):
            rows.append(proof(f"GAME-06 Wine {label}", "compat-e2e.yml", "Wine (macos-15 arm64)", step,
                              [rust_test("compat::plan::wine_e2e::the_d3d11_test_game_runs_through_wine"),
                               rf"Wine plan: [^\n]* with {backend}(?:\r?\n|$)"]))
        return rows + [result("D3D11 overlay / sequential cross-OS saves", "pending",
                              "pending (GAME-09, PLAY-06): hosted scenarios are not implemented")]
    rows = []
    for name in ("Windows x64", "Linux x64", "macOS arm64"):
        job = f"Download budgets ({name}, release)"
        rows.append(proof(f"INS-07 {name} release footprint", "transfer-budgets.yml", job,
                          "Memory and disk footprint",
                          [rust_test("memory_stays_bounded_at_32_connections"),
                           rust_test("disk_usage_never_exceeds_the_final_size_plus_journal"),
                           r"budget memory: 32 connections, 512 MiB: peak RSS ",
                           r"\(release build,", r"budget disk: peak "]))
        rows.append(proof(f"INS-07 {name} throughput measurements", "transfer-budgets.yml", job,
                          "Loopback throughput (2 GiB)",
                          [rust_test("loopback_throughput"), r"budget throughput: 2 GiB loopback,",
                           r"budget throughput: the same folder takes plain sequential writes at "]))
    return rows + [collector.latest_main_health("INT-06 signed release dry run", "release-dry-run.yml"),
            collector.latest_main_health("INT-07 soak workflow", "soak.yml"),
            result("signed release dry run / updater round trip", "pending", "pending (INT-06, INT-08): committed keys and current-base dry run receipts"),
            result("full hosted soak", "pending", "pending (INT-07): full-duration metric receipts required"),
            result("release performance budgets", "pending", "pending (INS-07, PLAY-10): throughput target and populated-launcher release budgets are not attested"),
            result("security sign-off", "pending", "pending (INT-11): phase-2 review is not signed off")]


def state(rows):
    if not rows or any(r["state"] == "failed" for r in rows):
        return "failed"
    return "reached" if all(r["state"] == "passed" for r in rows) else "not reached"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("milestones", nargs="+", choices=("m1", "m2", "m3", "m3b", "m4"))
    parser.add_argument("--sha", default=os.environ.get("DEMO_SHA"))
    parser.add_argument("--report", action="store_true", help="pending is a report, failures still exit 1")
    parser.add_argument("--out", type=Path)
    args = parser.parse_args()
    sha = args.sha or subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
    if not re.fullmatch(r"[0-9a-f]{40}", sha):
        parser.error("--sha must be a complete lowercase commit SHA")
    collector = Collector(sha)
    reports = {}
    for milestone in args.milestones:
        try:
            rows = requirements(milestone, collector)
        except EvidenceError as error:
            rows = [result("Actions evidence", "failed", str(error))]
        reports[milestone] = {"status": state(rows), "requirements": rows}
    table = [f"Milestone evidence for `{sha}`", "", "| Milestone | Status | Last green on main (last 30 runs) | Current evidence runs | Missing tasks / failures |",
             "|---|---|---|---|---|"]
    for milestone, report in reports.items():
        rows = report["requirements"]
        links = sorted({r["run"] for r in rows if r["state"] == "passed" and r["run"]})
        evidence = ", ".join(f"[run {url.rsplit('/', 1)[1]}]({url})" for url in links) or "none"
        missing = "; ".join(f"{r['label']}: {r['detail']}" for r in rows if r["state"] != "passed") or "none"
        try:
            history = collector.last_green(milestone)
            report["last_green"] = history
            previous = f"[run {history.rsplit('/', 1)[1]}]({history})" if history else "none"
        except EvidenceError:
            previous = "history unavailable"
        table.append(f"| {milestone.upper()} | {report['status']} | {previous} | {evidence} | {missing} |")
    rendered = "\n".join(table) + "\n"
    print(rendered)
    if os.environ.get("GITHUB_STEP_SUMMARY"):
        with open(os.environ["GITHUB_STEP_SUMMARY"], "a") as summary:
            summary.write(rendered)
    if args.out:
        args.out.write_text(json.dumps({"sha": sha, "milestones": reports}, indent=2) + "\n")
    if os.environ.get("GITHUB_OUTPUT"):
        with open(os.environ["GITHUB_OUTPUT"], "a") as output:
            for milestone, report in reports.items():
                output.write(f"{milestone}={report['status']}\n")
    if any(r["status"] == "failed" for r in reports.values()):
        return 1
    return 0 if args.report or all(r["status"] == "reached" for r in reports.values()) else 2


if __name__ == "__main__":
    sys.exit(main())
