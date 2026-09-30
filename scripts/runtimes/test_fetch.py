"""Offline tests for fetch.py (runtimes.yml runs them with test_watch.py)."""

from __future__ import annotations

import hashlib
import http.server
import io
import json
import tarfile
import tempfile
import threading
import unittest
from pathlib import Path

import fetch
import verify_new
import watch


def tarball(members: dict[str, bytes], links: dict[str, str] | None = None) -> bytes:
    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode="w:gz") as tar:
        for name, data in members.items():
            info = tarfile.TarInfo(name)
            info.size = len(data)
            info.mode = 0o755
            tar.addfile(info, io.BytesIO(data))
        for name, target in (links or {}).items():
            info = tarfile.TarInfo(name)
            info.type = tarfile.SYMTYPE
            info.linkname = target
            tar.addfile(info)
    return buf.getvalue()


class Server:
    def __init__(self, body: bytes):
        class Handler(http.server.BaseHTTPRequestHandler):
            def do_GET(self):  # noqa: N802 (http.server API)
                self.send_response(200)
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, *args):
                pass

        self.httpd = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        threading.Thread(target=self.httpd.serve_forever, daemon=True).start()
        self.url = f"http://127.0.0.1:{self.httpd.server_address[1]}/GE-Proton10-17.tar.gz"

    def close(self):
        self.httpd.shutdown()
        self.httpd.server_close()


def entry(url: str, body: bytes, **overrides) -> dict:
    e = {"id": "ge-proton", "version": "GE-Proton10-17", "url": url, "size": len(body),
         "sha256": hashlib.sha256(body).hexdigest(), "archive": "tar.gz"}
    e.update(overrides)
    return e


class FetchTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.dest = Path(self.tmp.name)

    def tearDown(self):
        self.tmp.cleanup()

    def serve(self, body: bytes) -> Server:
        server = Server(body)
        self.addCleanup(server.close)
        return server

    def test_pick_prefers_new_entries(self):
        catalog = {"runtime": [{"id": "umu-launcher", "version": "1.0"}, {"id": "umu-launcher", "version": "1.1"}]}
        self.assertEqual(fetch.pick([], catalog, "umu-launcher")["version"], "1.1")
        self.assertEqual(fetch.pick([{"id": "umu-launcher", "version": "2.0"}], catalog, "umu-launcher")["version"],
                         "2.0")
        self.assertIsNone(fetch.pick([], catalog, "ge-proton"))

    def test_verifies_then_extracts(self):
        body = tarball({"GE-Proton10-17/proton": b"#!/bin/sh\n", "GE-Proton10-17/files/x": b"x"},
                       {"GE-Proton10-17/files/y": "x"})
        server = self.serve(body)
        out = fetch.fetch(entry(server.url, body), self.dest)
        self.assertEqual((out / "GE-Proton10-17/proton").read_bytes(), b"#!/bin/sh\n")
        self.assertTrue((out / "GE-Proton10-17/proton").stat().st_mode & 0o100)
        self.assertTrue((out / "GE-Proton10-17/files/y").is_symlink())

    def test_a_wrong_hash_is_refused_before_extracting(self):
        body = tarball({"a": b"a"})
        server = self.serve(body)
        with self.assertRaises(watch.IntegrityError):
            fetch.fetch(entry(server.url, body, sha256="0" * 64), self.dest)
        self.assertEqual([p.name for p in self.dest.iterdir()], ["GE-Proton10-17.tar.gz"])

    def test_escaping_members_are_refused(self):
        for members, links in [({"../evil": b"x"}, None), ({}, {"link": "../../outside"}),
                               ({}, {"abs": "/etc/passwd"})]:
            body = tarball(members, links)
            server = self.serve(body)
            with self.subTest(members=members, links=links), self.assertRaises(tarfile.FilterError):
                fetch.fetch(entry(server.url, body), self.dest / "run")
        self.assertFalse((self.dest / "evil").exists())

    def test_absolute_member_paths_stay_inside(self):
        # The `data` filter strips the leading slash: the file lands under the extraction directory.
        body = tarball({"/tmp/vgames-fetch-test-evil": b"x"})
        server = self.serve(body)
        out = fetch.fetch(entry(server.url, body), self.dest)
        self.assertEqual((out / "tmp/vgames-fetch-test-evil").read_bytes(), b"x")
        self.assertFalse(Path("/tmp/vgames-fetch-test-evil").exists())


class VerifyNewTests(unittest.TestCase):
    def test_only_new_entries_are_downloaded_and_every_one_must_match(self):
        body = b"runtime bytes"
        server = Server(body)
        self.addCleanup(server.close)
        good = dict(entry(server.url, body), os="linux", arch="x86_64")
        old = dict(good, version="GE-Proton10-16", url="http://127.0.0.1:1/unreachable")
        with tempfile.TemporaryDirectory() as tmp:
            catalog, previous = Path(tmp) / "new.json", Path(tmp) / "old.json"
            previous.write_text(json.dumps({"runtimes": [old]}))
            self.assertEqual(verify_new.new_entries({"runtimes": [old, good]}, {"runtimes": [old]}), [good])
            catalog.write_text(json.dumps({"runtimes": [old, good]}))
            self.assertEqual(verify_new.main([str(catalog), "--previous", str(previous)]), 0)
            catalog.write_text(json.dumps({"runtimes": [old, dict(good, sha256="0" * 64)]}))
            self.assertEqual(verify_new.main([str(catalog), "--previous", str(previous)]), 1)
            catalog.write_text(json.dumps({"runtimes": [dict(good, size=len(body) + 1)]}))
            self.assertEqual(verify_new.main([str(catalog)]), 1)


if __name__ == "__main__":
    unittest.main()
