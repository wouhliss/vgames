"""Linux /proc integration regressions for the named-thread probe."""
import ctypes
import importlib.util
import os
from pathlib import Path
import queue
import subprocess
import sys
import threading
import unittest

PROBE = Path(__file__).with_name("idle.py")
spec = importlib.util.spec_from_file_location("idle", PROBE)
idle = importlib.util.module_from_spec(spec)
spec.loader.exec_module(idle)


@unittest.skipUnless(sys.platform == "linux", "Linux /proc counters")
class ThreadProbe(unittest.TestCase):
    def setUp(self):
        self.requests, self.replies = queue.Queue(), queue.Queue()
        self.ready = threading.Event()
        self.thread = threading.Thread(target=self.worker)
        self.thread.start()
        self.assertTrue(self.ready.wait(5))

    def worker(self):
        libc = ctypes.CDLL(None, use_errno=True)
        result = libc.prctl(15, ctypes.c_char_p(b"vgames-probe"), 0, 0, 0)
        if result != 0:
            raise OSError(ctypes.get_errno(), "prctl thread name")
        self.tid = threading.get_native_id()
        self.ready.set()
        while self.requests.get() is not None:
            self.replies.put(True)

    def tearDown(self):
        self.requests.put(None)
        self.thread.join(5)
        self.assertFalse(self.thread.is_alive())

    def test_exact_named_thread_and_real_voluntary_wakeups(self):
        self.assertEqual(idle.named_threads(os.getpid(), "vgames-probe"), [(os.getpid(), self.tid)])
        before = idle.thread_sample(os.getpid(), self.tid)
        for _ in range(5):
            self.requests.put(True)
            self.assertTrue(self.replies.get(timeout=5))
        after = idle.thread_sample(os.getpid(), self.tid)
        self.assertEqual(before[0], after[0])
        self.assertGreater(after[2], before[2])

    def test_blocked_thread_cli_reports_zero_switches_and_missing_thread_fails(self):
        result = subprocess.run([sys.executable, str(PROBE), str(os.getpid()), "0.15", "--thread", "vgames-probe"], capture_output=True, text=True, timeout=5)
        self.assertEqual(result.returncode, 0, result.stderr)
        total = next(line for line in result.stdout.splitlines() if line.split()[:1] == ["total"])
        self.assertEqual(total.split()[-2:], ["0", "0"])
        missing = subprocess.run([sys.executable, str(PROBE), str(os.getpid()), "0.01", "--thread", "absent-probe"], capture_output=True, text=True, timeout=5)
        self.assertEqual(missing.returncode, 2)
        self.assertIn("No live thread", missing.stderr)
