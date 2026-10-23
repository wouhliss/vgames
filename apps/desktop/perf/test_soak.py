import argparse
import contextlib
import io
import signal
import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('soak', Path(__file__).with_name('soak.py'))
soak = importlib.util.module_from_spec(spec)
spec.loader.exec_module(soak)


class SoakTests(unittest.TestCase):
    def test_only_a_live_launcher_with_a_rendered_screen_receipt_is_ready(self):
        from unittest.mock import Mock
        process = Mock()
        process.poll.return_value = None
        with tempfile.TemporaryDirectory() as temporary:
            profile = Path(temporary)
            log = profile / 'vgames.log'
            log.write_text('starting vgames\nthe UI did not call app_ready\n')
            with self.assertRaisesRegex(RuntimeError, 'first rendered screen'):
                soak.wait_for_ui(process, profile, timeout=0.01)
            log.write_text('INFO main UI ready\n')
            soak.wait_for_ui(process, profile)
            process.poll.return_value = 1
            with self.assertRaisesRegex(RuntimeError, 'exited'):
                soak.wait_for_ui(process, profile)

    def rows(self, growth=0):
        return [{"seconds": t, "rss_kib": 100000 + max(0, t - 600) * growth / 3000}
                for t in range(0, 3601, 60)]

    def test_flat_tree_passes_and_growth_at_the_limit_fails(self):
        self.assertTrue(soak.summarize(self.rows(), 3600)["passed"])
        self.assertTrue(soak.summarize(self.rows(4999), 3600)["passed"])
        self.assertFalse(soak.summarize(self.rows(5000), 3600)["passed"])
        self.assertFalse(soak.summarize(self.rows(8000), 3600)["passed"])

    def test_late_growth_and_a_sustained_slope_cannot_hide_at_the_end(self):
        rows = self.rows()
        rows[-1]["rss_kib"] += 6000
        self.assertFalse(soak.summarize(rows, 3600)["passed"])
        rows = self.rows(8000)
        rows[-1]["rss_kib"] = 100000
        self.assertFalse(soak.summarize(rows, 3600)["passed"])

    def test_short_or_nonmonotonic_observations_never_pass(self):
        with self.assertRaises(RuntimeError):
            soak.summarize(self.rows()[:-1], 3600)
        rows = self.rows()
        rows[-1]["seconds"] = rows[-2]["seconds"]
        with self.assertRaises(RuntimeError):
            soak.summarize(rows, 3540)
        for value in ('nan', 'inf', '-1', '0', '26'):
            with self.assertRaises(argparse.ArgumentTypeError):
                soak.hours(value)

    def test_root_children_groups_and_known_orphans_count_but_reused_pids_do_not(self):
        def process(parent, group, identity, rss):
            return {"parent": parent, "group": group, "start": identity,
                    "rss_kib": rss, "cpu_seconds": 1, "threads": 2}
        table = {10: process(1, 10, 100, 100), 11: process(10, 20, 101, 200),
                 12: process(1, 10, 102, 300), 13: process(1, 20, 103, 400),
                 14: process(1, 30, 999, 9000), 15: process(1, 30, 105, 9000)}
        known = {13: 103, 14: 104}
        with patch.object(soak, 'processes', return_value=table), \
             patch.object(Path, 'iterdir', autospec=True, side_effect=lambda _path: iter([Path('0')])):
            result = soak.sample(10, 100, 60, known)
            self.assertEqual(result['rss_kib'], 1000)
            self.assertEqual(result['processes'], 4)
            self.assertEqual(result['threads'], 8)
            self.assertEqual(result['descriptors'], 4)
            self.assertEqual(set(known), {10, 11, 12, 13})
            with self.assertRaises(RuntimeError):
                soak.sample(10, 999, 60, known)

    def test_cleanup_uses_a_stable_handle_and_rejects_a_reused_identity(self):
        def stat(path, **_kwargs):
            fields = ["0"] * 20
            fields[19] = "999" if path.parent.name == "7" else "100"
            return f"{path.parent.name} (fixture process) " + " ".join(fields)
        with patch.object(soak.os, 'pidfd_open', side_effect=[70, 80]), \
             patch.object(Path, 'read_text', autospec=True, side_effect=stat), \
             patch.object(soak.signal, 'pidfd_send_signal') as send, \
             patch.object(soak.os, 'close') as close:
            soak.stop_known({7: 100, 8: 100})
            send.assert_called_once_with(80, signal.SIGKILL)
            self.assertEqual([c.args[0] for c in close.call_args_list], [70, 80])

    def test_validated_pidfd_stops_a_real_child(self):
        child = soak.subprocess.Popen(['/bin/sleep', '30'], start_new_session=True)
        try:
            identity = soak.processes()[child.pid]['start']
            soak.stop_known({child.pid: identity})
            self.assertEqual(child.wait(timeout=5), -signal.SIGKILL)
        finally:
            if child.poll() is None:
                child.kill()
                child.wait(timeout=5)

    def test_incomplete_run_stops_the_child_and_deletes_its_private_profile(self):
        temporary = tempfile.TemporaryDirectory
        popen = soak.subprocess.Popen
        profiles, children = [], []
        def profile(**kwargs):
            result = temporary(**kwargs)
            profiles.append(Path(result.name))
            return result
        def launch(*args, **kwargs):
            child = popen(*args, **kwargs)
            children.append(child)
            return child
        with temporary() as directory:
            binary = Path(directory) / 'fixture-launcher'
            binary.write_text('#!/bin/sh\nprintf "observer-output-fixture"\nexec sleep 30\n')
            binary.chmod(0o755)
            output = Path(directory) / 'metrics'
            captured = io.StringIO()
            with patch.object(soak.tempfile, 'TemporaryDirectory', side_effect=profile), \
                 patch.object(soak.subprocess, 'Popen', side_effect=launch), \
                 contextlib.redirect_stdout(captured):
                self.assertEqual(soak.run(binary, 0.02, output), 1)
            self.assertNotIn('observer-output-fixture', captured.getvalue())
            self.assertIsNotNone(children[0].poll())
            self.assertFalse(profiles[0].exists())
            self.assertEqual({p.name for p in output.iterdir()}, {'rss.csv', 'summary.json'})

    def test_launch_failure_writes_a_failing_summary_without_a_profile(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / 'metrics'
            self.assertEqual(soak.run(Path(directory) / 'absent', 360, output), 1)
            report = (output / 'summary.json').read_text()
            self.assertIn('"passed": false', report)
            self.assertNotIn('vgames-soak-profile-', report)
            self.assertEqual({p.name for p in output.iterdir()}, {'summary.json'})


if __name__ == '__main__':
    unittest.main()
