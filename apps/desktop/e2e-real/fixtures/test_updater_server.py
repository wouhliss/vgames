import importlib.util
import json
import tempfile
import threading
import unittest
from http.server import ThreadingHTTPServer
from pathlib import Path
from urllib.error import HTTPError
from urllib.request import urlopen

spec = importlib.util.spec_from_file_location("updater_server", Path(__file__).with_name("updater-server.py"))
fixture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fixture)


class UpdaterFixtureTests(unittest.TestCase):
    def setUp(self):
        self.work = tempfile.TemporaryDirectory()
        self.addCleanup(self.work.cleanup)
        self.directory = Path(self.work.name)
        self.artifact = self.directory / "vgames.AppImage"
        self.artifact.write_bytes(bytes(range(256)) * 10000)
        (self.directory / "changelog-user.json").write_text('{"releases": []}')
        self.url = "https://github.com/wouhliss/vgames/releases/download/desktop-v0.1.0/vgames.AppImage"
        self.announce(self.url)

    def announce(self, url):
        (self.directory / "latest.json").write_text(json.dumps({
            "version": "0.1.0", "platforms": {"linux-x86_64": {"url": url}},
        }))

    def test_exact_routes_deliver_every_byte_and_only_public_path_receipts(self):
        receipts = self.directory / "receipts"
        server = ThreadingHTTPServer(("127.0.0.1", 0), fixture.handler(fixture.routes(self.directory), receipts))
        thread = threading.Thread(target=server.serve_forever)
        thread.start()
        try:
            base = f"http://127.0.0.1:{server.server_port}"
            with urlopen(base + "/vgames-updater-e2e-ready", timeout=5) as response:
                self.assertEqual(response.status, 204)
            self.assertFalse(receipts.exists(), "readiness must not count as an application request")
            path = "/wouhliss/vgames/releases/download/desktop-v0.1.0/vgames.AppImage"
            with urlopen(base + path, timeout=5) as response:
                self.assertEqual(response.read(), self.artifact.read_bytes())
            for refused in ("/", "/../latest.json", path + "?token=synthetic-secret"):
                with self.assertRaises(HTTPError) as error:
                    urlopen(base + refused, timeout=5)
                self.assertEqual(error.exception.code, 404)
                error.exception.close()
        finally:
            server.shutdown()
            thread.join(timeout=5)
            server.server_close()
        self.assertEqual(receipts.read_text(), path + "\n")

    def test_foreign_host_http_version_and_traversal_urls_are_refused(self):
        for url in (
            self.url.replace("https:", "http:"),
            self.url.replace("github.com", "github.com.example"),
            self.url.replace("desktop-v0.1.0", "desktop-v0.0.1"),
            self.url.replace("vgames.AppImage", "..%2Fprivate.key"),
            self.url + "?token=synthetic-secret",
        ):
            with self.subTest(url=url):
                self.announce(url)
                with self.assertRaises(ValueError):
                    fixture.routes(self.directory)

    def test_symlink_outside_artifacts_is_refused(self):
        with tempfile.TemporaryDirectory() as outside:
            private = Path(outside) / "private.key"
            private.write_bytes(b"synthetic fixture, no key")
            self.artifact.unlink()
            self.artifact.symlink_to(private)
            with self.assertRaises(ValueError):
                fixture.routes(self.directory)


if __name__ == "__main__":
    unittest.main()
