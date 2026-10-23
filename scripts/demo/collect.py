#!/usr/bin/env python3
"""Collect exact-commit Actions evidence. Missing work never means a milestone passed."""

import argparse
import functools
import json
import os
from pathlib import Path
import re
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
REPO = "wouhliss/vgames"


class EvidenceError(Exception):
    pass


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
            value = gh("run", "view", "--repo", REPO, "--job", str(int(job_id)), "--log")
            if not value.strip():
                raise EvidenceError("Actions returned an empty job log")
            return value

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
        for line in self.named_job_log(job_id).splitlines():
            fields = line.split("\t", 2)
            if len(fields) == 3 and fields[1] == step:
                lines.append(fields[2])
        if not lines:
            raise EvidenceError("Actions step-scoped log is unavailable")
        return "\n".join(lines) + "\n"

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
        log = self.step_log(job["id"], step)
        if not all(re.search(pattern, log) for pattern in patterns):
            return result(label, "failed", "test receipts missing (zero tests or a skipped scenario)", link)
        return result(label, "passed", "test receipts verified", link)

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
    return [collector.workflow_health("INT-06 signed release dry run", "release-dry-run.yml"),
            collector.workflow_health("INT-07 soak workflow", "soak.yml"),
            collector.workflow_health("INS-07 release transfer budgets", "transfer-budgets.yml"),
            result("signed release dry run / updater round trip", "pending", "pending (INT-06, INT-08): committed keys and current-base dry run receipts"),
            result("full hosted soak", "pending", "pending (INT-07): full-duration metric receipts required"),
            result("release performance budgets", "pending", "pending (INS-07, PLAY-10): collect release measurements"),
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
