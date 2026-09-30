"""Offline tests for watch.py: `python3 -m unittest discover -s scripts/runtimes` (runtimes.yml runs them first)."""

from __future__ import annotations

import hashlib
import http.server
import json
import tempfile
import threading
import unittest
from pathlib import Path
from unittest import mock

import watch

ROOT = Path(__file__).resolve().parents[2]
ARCHIVE = b"not really a tarball, but bytes with a known hash\n"
SHA256 = hashlib.sha256(ARCHIVE).hexdigest()
SHA512 = hashlib.sha512(ARCHIVE).hexdigest()
TAG = "GE-Proton10-17"
NAME = f"{TAG}.tar.gz"
BASE = f"https://github.com/GloriousEggroll/proton-ge-custom/releases/download/{TAG}"


def upstream(**overrides) -> watch.Upstream:
    config = watch.load_config((ROOT / "runtimes/upstreams.toml").read_text(encoding="utf-8"))
    up = next(u for u in config.upstreams if u.id == "ge-proton")
    up.repo_license = "NOASSERTION"
    for key, value in overrides.items():
        setattr(up, key, value)
    return up


def release(tag=TAG, assets=None, **flags) -> dict:
    if assets is None:
        assets = [
            {"name": NAME, "size": len(ARCHIVE), "browser_download_url": f"{BASE}/{NAME}",
             "digest": f"sha256:{SHA256}"},
            {"name": f"{TAG}.sha512sum", "size": 150, "browser_download_url": f"{BASE}/{TAG}.sha512sum"},
        ]
    return {"tag_name": tag, "assets": assets, **flags}


class FakeGitHub:
    def __init__(self, releases, license_id="NOASSERTION", checksum=f"{SHA512}  {NAME}\n", body=ARCHIVE):
        self.releases, self.license_id, self.checksum, self.body = releases, license_id, checksum, body

    def api_get(self, path):
        if path.endswith("/releases?per_page=30"):
            return self.releases
        return {"license": {"spdx_id": self.license_id}}

    def download(self, url, dest, expected_size):
        dest.parent.mkdir(parents=True, exist_ok=True)
        dest.write_bytes(self.body)
        if len(self.body) != expected_size:
            raise watch.IntegrityError("size")
        return hashlib.sha256(self.body).hexdigest(), hashlib.sha512(self.body).hexdigest()

    def fetch_text(self, url):
        return self.checksum


class ConfigTests(unittest.TestCase):
    def test_the_committed_upstreams_load(self):
        config = watch.load_config((ROOT / "runtimes/upstreams.toml").read_text(encoding="utf-8"))
        ids = [u.id for u in config.upstreams]
        self.assertEqual(ids, ["ge-proton", "umu-proton", "umu-launcher", "wine-macos", "dxmt", "dxvk-macos",
                               "moltenvk"])
        for up in config.upstreams:
            self.assertFalse(watch.license_ids(up.license) - config.license_allowlist, up.id)

    def test_unknown_fields_and_duplicates_are_refused(self):
        base = 'license_allowlist = ["MIT"]\nmin_launcher_version = "0.1.0"\n'
        entry = ('[[upstream]]\nid = "moltenvk"\nrepo = "a/b"\ntag = "v1"\nasset = "x"\narchive = "tar"\n'
                 'os = "macos"\narch = "x86_64"\nlicense = "MIT"\nsmoke = "none"\n')
        watch.load_config(base + entry)
        with self.assertRaises(ValueError):
            watch.load_config(base + entry + "surprise = 1\n")
        with self.assertRaises(ValueError):
            watch.load_config(base + entry + entry)
        with self.assertRaises(ValueError):
            watch.load_config(base + entry.replace('"a/b"', '"a/b/../c"'))


class PureTests(unittest.TestCase):
    def test_pick_release_skips_prereleases_drafts_and_other_tags(self):
        up = upstream()
        releases = [release("GE-Proton11-1", prerelease=True), release("GE-Proton11-0", draft=True),
                    release("nightly"), release()]
        picked, asset = watch.pick_release(up, releases)
        self.assertEqual((picked["tag_name"], asset["name"]), (TAG, NAME))
        with self.assertRaises(watch.Attention):
            watch.pick_release(up, [release("nightly")])
        twice = release(assets=[{"name": NAME}, {"name": "GE-Proton10-18.tar.gz"}])
        with self.assertRaises(watch.Attention):
            watch.pick_release(up, [twice])

    def test_checksum_files(self):
        self.assertEqual(watch.parse_checksum(f"{SHA512.upper()}  {NAME}\n", NAME), SHA512)
        self.assertEqual(watch.parse_checksum(f"{SHA512} *dist/{NAME}\n", NAME), SHA512)
        self.assertEqual(watch.parse_checksum(f"{SHA512}\n", NAME), SHA512)
        with self.assertRaises(watch.IntegrityError):
            watch.parse_checksum(f"{SHA512}  other.tar.gz\n", NAME)

    def test_licenses(self):
        up = upstream()
        watch.check_license(up, {"BSD-3-Clause", "LGPL-2.1-or-later", "Zlib", "MIT"}, "NOASSERTION")
        with self.assertRaises(watch.IntegrityError):
            watch.check_license(up, {"MIT"}, "NOASSERTION")
        with self.assertRaises(watch.IntegrityError):
            watch.check_license(up, {"BSD-3-Clause", "LGPL-2.1-or-later", "Zlib", "MIT"}, "GPL-3.0")
        with self.assertRaises(watch.Attention):
            watch.check_license(upstream(repo_license=None), {"BSD-3-Clause", "LGPL-2.1-or-later", "Zlib", "MIT"},
                                "NOASSERTION")
        self.assertEqual(watch.license_ids("(MIT OR Apache-2.0) AND GPL-2.0-or-later WITH x"),
                         {"MIT", "Apache-2.0", "GPL-2.0-or-later", "x"})

    def test_urls_and_versions(self):
        up = upstream(version_prefix="GE-")
        self.assertEqual(watch.version_of(up, TAG), "Proton10-17")
        with self.assertRaises(watch.Attention):
            watch.version_of(up, "GE-" + "x" * 65)
        with self.assertRaises(watch.IntegrityError):
            watch.asset_url(up, TAG, {"name": NAME, "browser_download_url": f"https://evil.example/{NAME}"})


class WatchTests(unittest.TestCase):
    def run_watch(self, github: FakeGitHub, catalog=None, up=None):
        config = watch.Config({"BSD-3-Clause", "LGPL-2.1-or-later", "Zlib", "MIT"}, "0.1.0")
        with tempfile.TemporaryDirectory() as out, \
                mock.patch.object(watch, "api_get", github.api_get), \
                mock.patch.object(watch, "download", github.download), \
                mock.patch.object(watch, "fetch_text", github.fetch_text):
            return watch.watch(up or upstream(), config, catalog or {}, Path(out))

    def test_a_new_release_is_verified_and_proposed(self):
        entry, smoke = self.run_watch(FakeGitHub([release()]))
        self.assertEqual(entry["sha256"], SHA256)
        self.assertEqual(entry["url"], f"{BASE}/{NAME}")
        self.assertEqual((entry["size"], entry["version"], entry["archive"]), (len(ARCHIVE), TAG, "tar.gz"))
        self.assertEqual(smoke["smoke"], "proton")
        self.assertEqual(json.loads(json.dumps(entry)), entry)

    def test_already_pinned_is_skipped(self):
        catalog = {"runtime": [{"id": "ge-proton", "version": TAG, "os": "linux", "arch": "x86_64",
                                "url": f"{BASE}/{NAME}"}]}
        self.assertIsNone(self.run_watch(FakeGitHub([release()]), catalog))

    def test_tampered_bytes_are_refused(self):
        wrong_sum = FakeGitHub([release()], checksum=f"{'0' * 128}  {NAME}\n")
        with self.assertRaisesRegex(watch.IntegrityError, "checksum"):
            self.run_watch(wrong_sum)
        other_bytes = FakeGitHub([release()], body=b"x" * len(ARCHIVE))
        with self.assertRaisesRegex(watch.IntegrityError, "digest"):
            self.run_watch(other_bytes)
        no_sums = release(assets=release()["assets"][:1])
        with self.assertRaisesRegex(watch.IntegrityError, "checksum file"):
            self.run_watch(FakeGitHub([no_sums]))

    def test_a_relicensed_upstream_stops_the_watcher(self):
        with self.assertRaises(watch.IntegrityError):
            self.run_watch(FakeGitHub([release()], license_id="GPL-3.0"))


class DownloadTests(unittest.TestCase):
    """The real streaming download, against a local server."""

    def setUp(self):
        body = ARCHIVE

        class Handler(http.server.BaseHTTPRequestHandler):
            def do_GET(self):  # noqa: N802 (http.server API)
                self.send_response(200)
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, *args):
                pass

        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.url = f"http://127.0.0.1:{self.server.server_address[1]}/a.tar.gz"
        self.tmp = tempfile.TemporaryDirectory()

    def tearDown(self):
        self.server.shutdown()
        self.server.server_close()
        self.tmp.cleanup()

    def test_hashes_and_size_limits(self):
        dest = Path(self.tmp.name) / "a.tar.gz"
        self.assertEqual(watch.download(self.url, dest, len(ARCHIVE)), (SHA256, SHA512))
        self.assertEqual(dest.read_bytes(), ARCHIVE)
        with self.assertRaisesRegex(watch.IntegrityError, "larger"):
            watch.download(self.url, dest, len(ARCHIVE) - 1)
        with self.assertRaisesRegex(watch.IntegrityError, "declared"):
            watch.download(self.url, dest, len(ARCHIVE) + 1)
        with self.assertRaisesRegex(watch.IntegrityError, "range"):
            watch.download(self.url, dest, 0)


class MainTests(unittest.TestCase):
    def test_integrity_errors_propose_nothing(self):
        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp)
            (out / "catalog.toml").write_text("commercial = false\n", encoding="utf-8")
            config = (ROOT / "runtimes/upstreams.toml").read_text(encoding="utf-8")
            (out / "upstreams.toml").write_text(
                config.replace('smoke = "proton"\n', 'smoke = "proton"\nrepo_license = "NOASSERTION"\n', 1),
                encoding="utf-8")
            args = ["--upstreams", str(out / "upstreams.toml"), "--catalog", str(out / "catalog.toml"),
                    "--out", str(out / "o"), "--only", "ge-proton"]
            good = FakeGitHub([release()])
            with mock.patch.object(watch, "api_get", good.api_get), \
                    mock.patch.object(watch, "download", good.download), \
                    mock.patch.object(watch, "fetch_text", good.fetch_text):
                self.assertEqual(watch.main(args), 0)
            self.assertEqual(len(json.loads((out / "o/entries.json").read_text())), 1)
            # Already proposed in the open update PR: not downloaded again.
            (out / "pending.toml").write_text(
                'commercial = false\n[[runtime]]\nid = "ge-proton"\nversion = "GE-Proton10-17"\nos = "linux"\n'
                f'arch = "x86_64"\nurl = "{BASE}/{NAME}"\n', encoding="utf-8")
            with mock.patch.object(watch, "api_get", good.api_get), \
                    mock.patch.object(watch, "download", side_effect=AssertionError("downloaded")), \
                    mock.patch.object(watch, "fetch_text", good.fetch_text):
                self.assertEqual(watch.main(args + ["--pending", str(out / "pending.toml")]), 0)
            self.assertEqual(json.loads((out / "o/entries.json").read_text()), [])
            bad = FakeGitHub([release()], checksum=f"{'0' * 128}  {NAME}\n")
            with mock.patch.object(watch, "api_get", bad.api_get), \
                    mock.patch.object(watch, "download", bad.download), \
                    mock.patch.object(watch, "fetch_text", bad.fetch_text):
                self.assertEqual(watch.main(args), 1)
            self.assertEqual(json.loads((out / "o/entries.json").read_text()), [])
            self.assertIn("nothing proposed", (out / "o/summary.md").read_text())


if __name__ == "__main__":
    unittest.main()
