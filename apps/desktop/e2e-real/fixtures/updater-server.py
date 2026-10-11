#!/usr/bin/env python3
"""Serve dry-run artifacts at the launcher's unchanged GitHub HTTPS paths (INT-06)."""

import argparse
import json
import os
import shutil
import ssl
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import unquote, urlsplit

PREFIX = "/wouhliss/vgames/releases/"
LATEST = PREFIX + "latest/download/"


def routes(directory: Path) -> dict[str, Path]:
    directory = directory.resolve(strict=True)
    result = {LATEST + name: (directory / name).resolve(strict=True)
              for name in ("latest.json", "changelog-user.json")}
    for file in result.values():
        if file.parent != directory or not file.is_file():
            raise ValueError("metadata must be a file inside the dry-run directory")
    manifest = json.loads(result[LATEST + "latest.json"].read_text())
    for platform in manifest["platforms"].values():
        url = urlsplit(platform["url"])
        if url.scheme != "https" or url.netloc != "github.com" or url.query or url.fragment:
            raise ValueError("artifact URL must use the unchanged GitHub HTTPS host")
        expected = PREFIX + "download/desktop-v" + manifest["version"] + "/"
        if not url.path.startswith(expected):
            raise ValueError("artifact URL must belong to this release")
        name = unquote(url.path.removeprefix(expected))
        if not name or name in (".", "..") or "/" in name or "\\" in name:
            raise ValueError("artifact URL must name one file")
        artifact = (directory / name).resolve(strict=True)
        if artifact.parent != directory or not artifact.is_file():
            raise ValueError("artifact must be a file inside the dry-run directory")
        result[url.path] = artifact
    return result


def handler(files: dict[str, Path], receipts: Path):
    class Handler(BaseHTTPRequestHandler):
        def do_GET(self):
            if self.path == "/vgames-updater-e2e-ready":
                self.send_response(204)
                self.end_headers()
                return
            # Only fixed release paths; no directory listing or arbitrary file access.
            file = files.get(self.path)
            if file is None:
                self.send_error(404)
                return
            self.send_response(200)
            self.send_header("Content-Length", str(file.stat().st_size))
            self.send_header("Content-Type", "application/json" if file.suffix == ".json" else "application/octet-stream")
            self.end_headers()
            with file.open("rb") as source:
                shutil.copyfileobj(source, self.wfile, 1024 * 1024)
            # Record only known public paths after successful delivery, never headers or queries.
            with receipts.open("a") as log:
                log.write(self.path + "\n")

        def log_message(self, *args):
            pass

    return Handler


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--artifacts", type=Path, required=True)
    parser.add_argument("--certificate", required=True)
    parser.add_argument("--key", required=True)
    parser.add_argument("--receipts", type=Path, required=True)
    parser.add_argument("--pid", type=Path, required=True)
    args = parser.parse_args()
    server = ThreadingHTTPServer(("127.0.0.1", 443), handler(routes(args.artifacts), args.receipts))
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.minimum_version = ssl.TLSVersion.TLSv1_2
    context.load_cert_chain(args.certificate, args.key)
    server.socket = context.wrap_socket(server.socket, server_side=True)
    args.pid.write_text(str(os.getpid()))
    try:
        server.serve_forever()
    finally:
        server.server_close()
