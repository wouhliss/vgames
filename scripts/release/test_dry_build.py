"""Failure-path proof: fake signer output cannot escape, and private files are removed."""
import json
import os
from pathlib import Path
import stat
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
MARKER = "synthetic-private-output-must-not-reach-build-logs"


class DryBuildFailureTests(unittest.TestCase):
    def test_generator_output_and_private_files_are_discarded_on_build_failure(self):
        # No cryptographic key is generated. The fake signer deliberately prints a
        # sentinel and creates fake key files; the real orchestration must contain it.
        with tempfile.TemporaryDirectory(prefix="vgames-dry-build-failure-") as tmp:
            scratch = Path(tmp)
            tools = scratch / "bin"
            tools.mkdir()
            pnpm = tools / "pnpm"
            pnpm.write_text('''#!/usr/bin/env python3
import json, os, pathlib, stat, sys
args = sys.argv[1:]
if 'generate' not in args:
    sys.exit(27)
key = pathlib.Path(args[args.index('-w') + 1])
key.write_text('synthetic-key-file')
pathlib.Path(str(key) + '.pub').write_text('synthetic-public-key')
password = key.parent / 'password'
pathlib.Path(os.environ['PROOF_RECEIPT']).write_text(json.dumps({
    'directory_mode': stat.S_IMODE(key.parent.stat().st_mode),
    'password_mode': stat.S_IMODE(password.stat().st_mode),
    'password_present': bool(password.read_bytes()),
}))
print(os.environ['PROOF_SENTINEL'])
print(os.environ['PROOF_SENTINEL'], file=sys.stderr)
''')
            pnpm.chmod(0o700)
            receipt = scratch / "receipt.json"
            result = subprocess.run(
                ["bash", "scripts/release/dry-build.sh"], cwd=ROOT,
                env={**os.environ, "PATH": str(tools) + os.pathsep + os.environ["PATH"],
                     "RUNNER_TEMP": tmp, "TARGET": "synthetic-target", "BUNDLES": "appimage",
                     "PROOF_RECEIPT": str(receipt), "PROOF_SENTINEL": MARKER},
                capture_output=True, text=True, timeout=30,
            )
            self.assertEqual(result.returncode, 27, result.stdout + result.stderr)
            self.assertNotIn(MARKER, result.stdout + result.stderr)
            self.assertFalse((scratch / "updater-dry-key").exists())
            proof = json.loads(receipt.read_text())
            self.assertTrue(proof["password_present"])
            # Windows does not expose POSIX mode bits through pathlib.stat().
            if os.name == "posix":
                self.assertEqual(proof["directory_mode"], stat.S_IRWXU)
                self.assertEqual(proof["password_mode"], stat.S_IRUSR | stat.S_IWUSR)


if __name__ == "__main__":
    unittest.main()
