import contextlib
import io
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

from collect import Collector, EvidenceError, main, marked_step_log, requirements, result, rust_test, state


class CollectorTests(unittest.TestCase):
    def setUp(self):
        self.collector = Collector("a" * 40)
        source = patch.object(self.collector, "source", return_value="name: CI\n")
        source.start()
        self.addCleanup(source.stop)
        self.run = {"id": 42, "head_sha": "a" * 40, "run_number": 2,
                    "run_attempt": 1, "status": "completed", "conclusion": "success"}
        self.job = {"id": 13, "name": "tests", "status": "completed", "conclusion": "success",
                    "steps": [{"name": "scenario", "conclusion": "success"}]}

    def proof(self, log="test required_case ... ok\n"):
        with patch.object(self.collector, "runs", return_value=[self.run]), \
                patch.object(self.collector, "jobs", return_value=[self.job]), \
                patch.object(self.collector, "step_log", return_value=log):
            return self.collector.proof("test", "ci.yml", "tests", "scenario", [rust_test("required_case")])

    def main_workflow(self, **changes):
        return dict(self.run, head_sha="b" * 40, head_branch="main", event="workflow_dispatch",
                    head_repository={"full_name": "wouhliss/vgames"}, **changes)

    def main_health(self, runs=None, files=None, comparison=None):
        compared = {"status": "ahead", "merge_base_commit": {"sha": "b" * 40},
                    "files": [] if files is None else files}
        if comparison is not None:
            compared.update(comparison)
        with patch.object(self.collector, "runs", return_value=[]), \
             patch("collect.api", side_effect=[{"workflow_runs": runs or [self.main_workflow()]}, compared]):
            return self.collector.latest_main_health("soak", "soak.yml")

    def test_latest_main_health_prefers_an_exact_proof_including_its_failure(self):
        for conclusion, expected in [("success", "passed"), ("failure", "failed")]:
            with patch.object(self.collector, "runs", return_value=[dict(self.run, conclusion=conclusion)]), \
                 patch("collect.api") as api:
                self.assertEqual(self.collector.latest_main_health("soak", "soak.yml")["state"], expected)
                api.assert_not_called()

    def test_latest_main_failure_and_attempt_never_fall_back_to_older_green(self):
        older = dict(self.main_workflow(), run_number=1)
        latest = dict(self.main_workflow(), run_attempt=2, conclusion="failure")
        foreign = dict(latest, run_number=100, head_repository={"full_name": "foreign/fork"})
        branch = dict(latest, run_number=101, head_branch="feature")
        self.assertEqual(self.main_health([older, self.main_workflow(), latest, foreign, branch])["state"], "failed")
        self.assertEqual(self.main_health([dict(latest, status="in_progress")])["state"], "pending")

    def test_latest_main_health_allows_only_statuses_and_new_internal_fragments(self):
        fragment = "---\naudience: internal\ncomponent: launcher\ntype: fixed\n---\nValidation receipt.\n"
        files = [{"filename": f"docs/agents/status/{owner}.md", "status": "modified"}
                 for owner in ("ins", "play", "game", "int")]
        files.append({"filename": ".changes/validation-receipt.md", "status": "added"})
        with patch.object(self.collector, "source", return_value=fragment):
            self.assertEqual(self.main_health(files=files)["state"], "passed")
        self.assertEqual(self.main_health(files=files[:-1])["state"], "passed")
        for header in [fragment.replace("internal", "user"),
                       fragment.replace("audience: internal", "audience: internal\naudience: user"),
                       fragment.replace("audience: internal", "audience: internal # receipt"),
                       fragment.replace("component: launcher", "component: unknown"),
                       fragment.replace("---\n", "", 1), ""]:
            with self.subTest(header=header), patch.object(self.collector, "source", return_value=header):
                self.assertEqual(self.main_health(files=files)["state"], "pending")
        with patch.object(self.collector, "source", side_effect=lambda path: None if path.startswith(".changes/") else "workflow"):
            self.assertEqual(self.main_health(files=files)["state"], "pending")

    def test_source_changes_renames_deletions_and_comparison_truncation_require_fresh_proof(self):
        for file in [
            {"filename": "apps/desktop/src/app/router.tsx", "status": "modified"},
            {"filename": ".github/workflows/soak.yml", "status": "modified"},
            {"filename": "runtimes/catalog.pub", "status": "modified"},
            {"filename": "Cargo.toml", "status": "modified"},
            {"filename": "docs/architecture/05-social.md", "status": "modified"},
            {"filename": "docs/agents/status/int.md", "status": "removed"},
            {"filename": ".changes/receipt.md", "status": "modified"},
            {"filename": "docs/agents/status/int.md", "status": "modified", "previous_filename": "source.rs"},
        ]:
            with self.subTest(file=file):
                self.assertEqual(self.main_health(files=[file])["state"], "pending")
        repeated = [{"filename": "docs/agents/status/int.md", "status": "modified"}] * 300
        self.assertEqual(self.main_health(files=repeated)["state"], "pending")
        for comparison in [{"status": "behind"}, {"status": "diverged"},
                           {"merge_base_commit": {"sha": "c" * 40}}]:
            self.assertEqual(self.main_health(comparison=comparison)["state"], "pending")
        self.assertEqual(self.main_health(comparison={"files": None})["state"], "failed")

    def test_unverifiable_main_repository_identity_never_uses_an_older_green(self):
        unknown = dict(self.main_workflow(), run_number=3, head_repository={})
        self.assertEqual(self.main_health([self.main_workflow(), unknown])["state"], "failed")
        foreign = dict(self.main_workflow(), run_number=3, head_repository={"full_name": "foreign/fork"})
        self.assertEqual(self.main_health([self.main_workflow(), foreign])["state"], "passed")
        with patch.object(self.collector, "runs", return_value=[]), \
             patch.object(self.collector, "source", return_value=None), patch("collect.api") as api:
            self.assertEqual(self.collector.latest_main_health("release", "release-dry-run.yml")["state"], "pending")
            api.assert_not_called()

    def test_zero_tests_ignored_and_missing_receipts_fail(self):
        self.assertEqual(self.proof()["state"], "passed")
        for log in ("test result: ok. 0 passed", "test required_case ... ignored\n",
                    "test other_case ... ok\n", "test required_case ... FAILED\n"):
            self.assertEqual(self.proof(log)["state"], "failed")

    def test_existing_failed_cancelled_skipped_jobs_and_steps_fail(self):
        for conclusion in ("failure", "cancelled", "skipped", "timed_out"):
            self.job["conclusion"] = conclusion
            self.assertEqual(self.proof()["state"], "failed")
        self.job["conclusion"] = "success"
        self.job["steps"][0]["conclusion"] = "skipped"
        self.assertEqual(self.proof()["state"], "failed")

    def test_missing_and_duplicate_jobs_fail_but_running_job_is_pending(self):
        with patch.object(self.collector, "runs", return_value=[self.run]):
            for jobs in ([], [self.job, self.job]):
                with patch.object(self.collector, "jobs", return_value=jobs):
                    self.assertEqual(self.collector.proof("x", "ci.yml", "tests", "scenario", ["ok"])["state"], "failed")
        self.job["status"] = "in_progress"
        self.assertEqual(self.proof()["state"], "pending")

    def test_latest_failed_attempt_never_falls_back_to_green(self):
        older = dict(self.run, run_number=1)
        latest = dict(self.run, run_attempt=2, conclusion="failure")
        other_sha = dict(self.run, head_sha="b" * 40, run_number=100)
        with patch("collect.api", return_value={"workflow_runs": [older, other_sha, self.run, latest]}):
            self.assertEqual(self.collector.runs("ci.yml"), [latest, self.run, older])
            self.assertEqual(self.collector.workflow_health("CI", "ci.yml")["state"], "failed")

    def test_pagination_keeps_the_current_attempt(self):
        with patch("collect.api", side_effect=[{"jobs": [self.job] * 100}, {"jobs": [self.job]}]) as api:
            self.assertEqual(len(self.collector.jobs(42, 2)), 101)
            self.assertIn("attempts/2/jobs", api.call_args.args[0])
            self.assertIn("page=2", api.call_args.args[0])

    def test_unavailable_evidence_does_not_turn_green(self):
        with patch.object(self.collector, "runs", side_effect=EvidenceError("unavailable")):
            self.assertEqual(self.collector.workflow_health("CI", "ci.yml")["state"], "failed")
        self.assertEqual(state([result("missing", "pending", "pending")]), "not reached")
        self.assertEqual(state([result("missing", "pending", "pending"), result("existing", "failed", "failed")]), "failed")

    def test_empty_job_logs_use_the_same_job_fallback_and_still_fail_if_empty(self):
        with patch("collect.gh", side_effect=["", "test required_case ... ok\n"]) as gh:
            self.assertIn("required_case", self.collector.log(13))
            self.assertIn("13", gh.call_args.args)
        with patch("collect.gh", return_value=""):
            with self.assertRaisesRegex(EvidenceError, "empty job log"):
                self.collector.log(14)

    def test_history_requires_a_successful_named_main_gate_and_never_changes_current_failure(self):
        older = dict(self.run, id=41, run_number=1, head_branch="main")
        latest = dict(self.run, head_branch="main", conclusion="failure")
        foreign = dict(self.run, id=43, run_number=3, head_branch="feature")
        passed = dict(self.job, name="Milestone M1")
        skipped = dict(passed, conclusion="skipped")
        with patch("collect.api", return_value={"workflow_runs": [older, latest, foreign]}), \
             patch.object(self.collector, "jobs", side_effect=[[skipped], [passed]]):
            self.assertTrue(self.collector.last_green("m1").endswith("/41"))
        self.assertEqual(state([result("current test", "failed", "failed")]), "failed")
        with patch("collect.api", return_value={"workflow_runs": [latest]}), \
             patch.object(self.collector, "jobs", return_value=[skipped]):
            self.assertEqual(self.collector.last_green("m1"), "")

    def test_report_outputs_distinguish_pending_failed_and_reached_despite_green_history(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / 'outputs'
            for status, expected, code in (('pending', 'not reached', 0),
                                           ('failed', 'failed', 1), ('passed', 'reached', 0)):
                with patch('sys.argv', ['collect.py', 'm1', '--sha', 'a' * 40, '--report']), \
                     patch('collect.requirements', return_value=[result('proof', status, 'fixture')]), \
                     patch.object(Collector, 'last_green', return_value='https://github.com/wouhliss/vgames/actions/runs/41'), \
                     patch.dict(os.environ, {'GITHUB_OUTPUT': str(output), 'GITHUB_STEP_SUMMARY': ''}), \
                     contextlib.redirect_stdout(io.StringIO()):
                    output.write_text('')
                    self.assertEqual(main(), code)
                    self.assertEqual(output.read_text(), f'm1={expected}\n')

    def test_selected_commit_source_ignores_worktree_changes_and_distinguishes_missing_objects(self):
        with tempfile.TemporaryDirectory() as directory:
            def git(*args):
                return subprocess.check_output(['git', '-C', directory, *args], stderr=subprocess.DEVNULL)
            git('init', '-q')
            git('config', 'user.name', 'Evidence fixture')
            git('config', 'user.email', 'fixture@example.invalid')
            git('config', 'commit.gpgsign', 'false')
            path = Path(directory) / 'workflow.yml'
            path.write_text('committed evidence\n')
            git('add', 'workflow.yml')
            git('commit', '-qm', 'fixture')
            sha = git('rev-parse', 'HEAD').decode().strip()
            path.write_text('uncommitted changes\n')
            with patch('collect.ROOT', Path(directory)):
                collector = Collector(sha)
                self.assertEqual(collector.source('workflow.yml'), 'committed evidence\n')
                self.assertIsNone(collector.source('missing.yml'))
                with self.assertRaisesRegex(EvidenceError, 'Fetch the selected'):
                    Collector('a' * 40).source('workflow.yml')

    def test_a_later_backend_receipt_cannot_satisfy_the_requested_step(self):
        log = ('job\tscenario\t2026-10-23T10:00:00Z test result: ok. 0 passed\n'
               'job\tother backend\t2026-10-23T10:00:01Z test required_case ... ok\n')
        with patch.object(self.collector, 'runs', return_value=[self.run]), \
             patch.object(self.collector, 'jobs', return_value=[self.job]), \
             patch.object(self.collector, 'named_job_log', return_value=log):
            self.assertEqual(self.collector.proof('x', 'ci.yml', 'tests', 'scenario',
                                                  [rust_test('required_case')])['state'], 'failed')
            self.assertNotIn('required_case', self.collector.step_log(13, 'scenario'))
        with patch.object(self.collector, 'named_job_log', return_value='test required_case ... ok\n'), \
             patch.object(self.collector, 'log', return_value='test required_case ... ok\n'):
            with self.assertRaisesRegex(EvidenceError, 'step-scoped'):
                self.collector.step_log(13, 'scenario')

    def test_real_wrapper_preserves_failure_and_requires_complete_unique_markers(self):
        wrapper = Path(__file__).resolve().parents[1] / 'ci/evidence.sh'
        def run(step='scenario', code=0):
            return subprocess.run(['bash', str(wrapper), step, 'bash', '-c',
                                   f"printf 'test required_case ... ok\\n'; exit {code}"],
                                  capture_output=True, text=True, timeout=10)
        passed, failed = run(), run(code=7)
        self.assertEqual(passed.returncode, 0)
        self.assertEqual(failed.returncode, 7)
        with patch.object(self.collector, 'named_job_log', return_value=''.join(
                'job\tscenario\t' + line + '\n' for line in failed.stdout.splitlines())):
            with self.assertRaisesRegex(EvidenceError, 'command failed'):
                self.collector.step_log(13, 'scenario')
        stamped = ''.join('2026-10-23T10:00:00.123Z ' + line + '\n' for line in passed.stdout.splitlines())
        self.assertEqual(marked_step_log(stamped, 'scenario'), 'test required_case ... ok\n')
        for output in (failed.stdout, passed.stdout + passed.stdout, run('other backend').stdout,
                       passed.stdout.rsplit('VGAMES_STEP_END', 1)[0],
                       passed.stdout.replace('test required_case ... ok', passed.stdout)):
            with self.subTest(output_length=len(output)), self.assertRaises(EvidenceError):
                marked_step_log(output, 'scenario')

    def test_wrapper_stdin_keeps_errexit_and_pipefail(self):
        wrapper = Path(__file__).resolve().parents[1] / 'ci/evidence.sh'
        for command in ('false\nprintf unexpected\\n\n', 'false | true\nprintf unexpected\\n\n'):
            with self.subTest(command=command):
                run = subprocess.run(['bash', str(wrapper), 'scenario'], input=command,
                                     capture_output=True, text=True, timeout=10)
                self.assertNotEqual(run.returncode, 0)
                self.assertNotIn('unexpected', run.stdout)
                with self.assertRaisesRegex(EvidenceError, 'command failed'):
                    marked_step_log(run.stdout, 'scenario')

    def test_unknown_step_logs_use_only_marked_output(self):
        wrapper = Path(__file__).resolve().parents[1] / 'ci/evidence.sh'
        raw = subprocess.check_output(['bash', str(wrapper), 'scenario', 'printf', 'real receipt\n'], text=True)
        with patch.object(self.collector, 'named_job_log', return_value='job\tUNKNOWN STEP\tborrowed receipt\n'), \
             patch.object(self.collector, 'log', return_value=raw):
            self.assertEqual(self.collector.step_log(13, 'scenario'), 'real receipt\n')

    def test_marked_cli_fallback_keeps_scope_when_the_raw_log_endpoint_is_unavailable(self):
        wrapper = Path(__file__).resolve().parents[1] / 'ci/evidence.sh'
        raw = subprocess.check_output(['bash', str(wrapper), 'scenario', 'printf', 'real receipt\n'], text=True)
        def decorated(output):
            return ''.join('job\tUNKNOWN STEP\t2026-10-24T12:00:00.123Z ' + line + '\n'
                           for line in output.splitlines())
        with patch.object(self.collector, 'named_job_log', return_value=decorated(raw)), \
             patch('collect.gh', side_effect=[EvidenceError('unavailable'), decorated(raw)]):
            self.assertEqual(self.collector.step_log(13, 'scenario'), 'real receipt\n')
        self.collector.log.cache_clear()
        unmarked = decorated('test required_case ... ok\n')
        with patch.object(self.collector, 'named_job_log', return_value=unmarked), \
             patch('collect.gh', side_effect=[EvidenceError('unavailable'), unmarked]):
            with self.assertRaisesRegex(EvidenceError, 'step-scoped'):
                self.collector.step_log(13, 'scenario')

    def test_m3b_requires_each_wine_backend_despite_a_green_workflow(self):
        with patch.object(self.collector, 'proof', return_value=result('fixture', 'passed', 'fixture')) as proof:
            requirements('m3b', self.collector)
        cases = [call.args for call in proof.call_args_list if call.args[0].startswith('GAME-06 Wine ')]
        self.assertEqual(len(cases), 3)
        receipt = 'test compat::plan::wine_e2e::the_d3d11_test_game_runs_through_wine ... ok\n'
        for args, backend in zip(cases, ('Dxmt', 'Dxvk', 'Wined3d'), strict=True):
            job = dict(self.job, name=args[2], steps=[{'name': args[3], 'conclusion': 'success'}])
            with self.subTest(backend=backend), \
                 patch.object(self.collector, 'runs', return_value=[self.run]), \
                 patch.object(self.collector, 'jobs', return_value=[job]):
                with patch.object(self.collector, 'step_log', return_value=f'Wine plan: fixture with {backend}\n{receipt}'):
                    self.assertEqual(self.collector.proof(*args)['state'], 'passed')
                    job['steps'][0]['conclusion'] = 'failure'
                    self.assertEqual(self.collector.proof(*args)['state'], 'failed')
                job['steps'][0]['conclusion'] = 'success'
                for log in (receipt, f'Wine plan: fixture with OtherBackend\n{receipt}'):
                    with patch.object(self.collector, 'step_log', return_value=log):
                        self.assertEqual(self.collector.proof(*args)['state'], 'failed')

    def test_m4_collects_release_measurements_without_claiming_the_throughput_target(self):
        with patch.object(self.collector, 'proof', return_value=result('fixture', 'passed', 'fixture')) as proof, \
             patch.object(self.collector, 'latest_main_health', return_value=result('fixture', 'passed', 'fixture')):
            rows = requirements('m4', self.collector)
        self.assertEqual(len(proof.call_args_list), 6)
        self.assertEqual(state(rows), 'not reached')
        footprint = ('test memory_stays_bounded_at_32_connections ... ok\n'
                     'test disk_usage_never_exceeds_the_final_size_plus_journal ... ok\n'
                     'budget memory: 32 connections, 512 MiB: peak RSS 100 MiB (release build, fixture)\n'
                     'budget disk: peak 42 bytes\n')
        throughput = ('test loopback_throughput ... ok\n'
                      'budget throughput: 2 GiB loopback, download phase 1 s\n'
                      'budget throughput: the same folder takes plain sequential writes at 1 GB/s\n')
        for call in proof.call_args_list:
            args = call.args
            log = footprint if args[3] == 'Memory and disk footprint' else throughput
            job = dict(self.job, name=args[2], steps=[{'name': args[3], 'conclusion': 'success'}])
            with self.subTest(label=args[0]), \
                 patch.object(self.collector, 'runs', return_value=[self.run]), \
                 patch.object(self.collector, 'jobs', return_value=[job]):
                with patch.object(self.collector, 'step_log', return_value=log):
                    self.assertEqual(self.collector.proof(*args)['state'], 'passed')
                for missing in (log.replace('budget ', 'missing '),
                                log.replace('release build', 'debug build').replace('2 GiB', '0.1 GiB')):
                    with patch.object(self.collector, 'step_log', return_value=missing):
                        self.assertEqual(self.collector.proof(*args)['state'], 'failed')

    def test_unreadable_matrix_log_does_not_hide_failed_compatibility_steps(self):
        wine_step = 'The D3D11 test game through Wine + DXMT (recorded; missing Metal goes to human-checklist §B4)'
        matrix = [dict(self.job, name=name, steps=[{'name': 'Rust tests (launcher and the crates it ships)',
                                                  'conclusion': 'success'}])
                  for name in ('Windows x64', 'Linux x64', 'macOS arm64')]
        wine = dict(self.job, name='Wine (macos-15 arm64)',
                    steps=[{'name': wine_step, 'conclusion': 'failure'}])
        with patch.object(self.collector, 'runs', return_value=[self.run]), \
             patch.object(self.collector, 'jobs', return_value=matrix + [wine]), \
             patch.object(self.collector, 'step_log', side_effect=EvidenceError('unreadable fixture')):
            rows = requirements('m3b', self.collector)
        dxmt = next(row for row in rows if row['label'] == 'GAME-06 Wine DXMT')
        self.assertEqual(dxmt['state'], 'failed')
        self.assertEqual(dxmt['detail'], 'required test step did not pass')
        self.assertEqual(rows[0]['detail'], 'unreadable fixture')
        self.assertEqual(state(rows), 'failed')


if __name__ == "__main__":
    unittest.main()
