#!/usr/bin/env python3
"""Cache Wine's own checksum-bound Mono/Gecko prerequisites for unattended CI."""

import hashlib
import json
import os
from pathlib import Path
import re
import sys
import tempfile
import time
import urllib.request

MAX_SOURCE_BYTES = 256 * 1024
MAX_INSTALLER_BYTES = 256 * 1024 * 1024


def version(source, name):
    values = re.findall(r'^#define ' + name + r' "([0-9]+(?:\.[0-9]+){1,2})"$', source, re.M)
    if len(values) != 1:
        raise ValueError("unsupported Wine prerequisite version")
    return values[0]


def prerequisites(source):
    result = []
    for kind, wanted in (("GECKO", ("x86", "x86_64")), ("MONO", ("x86",))):
        release = version(source, kind + "_VERSION")
        pairs = re.findall(r'^#define ' + kind + r'_ARCH "([a-z0-9_]+)"\s+'
                           r'#define ' + kind + r'_SHA "([0-9a-f]{64})"$', source, re.M)
        checksums = dict(pairs)
        if len(checksums) != len(pairs) or any(arch not in checksums for arch in wanted):
            raise ValueError("unsupported Wine prerequisite checksums")
        for arch in wanted:
            name = f"wine-{kind.lower()}-{release}-{arch}.msi"
            url = f"https://dl.winehq.org/wine/wine-{kind.lower()}/{release}/{name}"
            result.append((name, url, checksums[arch]))
    return result


def cached_installer(cache, name, url, expected):
    cache.mkdir(parents=True, exist_ok=True)
    target = cache / name
    if target.is_symlink():
        raise ValueError("refusing a linked prerequisite")
    if target.exists():
        with target.open('rb') as existing:
            if hashlib.file_digest(existing, 'sha256').hexdigest() != expected:
                raise ValueError("cached prerequisite checksum mismatch")
        return
    temporary = None
    try:
        started, count, digest = time.monotonic(), 0, hashlib.sha256()
        with urllib.request.urlopen(url, timeout=30) as response, tempfile.NamedTemporaryFile(
                dir=cache, prefix='.vgames-addon-', delete=False) as output:
            temporary = Path(output.name)
            if not response.geturl().startswith('https://'):
                raise ValueError("refusing an insecure prerequisite redirect")
            while chunk := response.read(1024 * 1024):
                count += len(chunk)
                if count > MAX_INSTALLER_BYTES or time.monotonic() - started > 300:
                    raise ValueError("prerequisite download exceeds its bounds")
                digest.update(chunk)
                output.write(chunk)
        if digest.hexdigest() != expected:
            raise ValueError("prerequisite checksum mismatch")
        temporary.replace(target)
    finally:
        if temporary is not None:
            temporary.unlink(missing_ok=True)


def main():
    entries = json.loads(Path(os.environ['VGAMES_WINE_E2E_RUNTIMES']).read_text())
    wine = [entry for entry in entries if entry.get('id') == 'wine-macos']
    if len(wine) != 1 or wine[0].get('arch') != 'x86_64':
        raise ValueError("expected one x86_64 Wine runtime")
    release = wine[0]['version']
    if not isinstance(release, str) or not re.fullmatch(r'[0-9]+(?:\.[0-9]+){1,2}', release):
        raise ValueError("unsupported Wine source tag")
    url = f'https://raw.githubusercontent.com/wine-mirror/wine/wine-{release}/dlls/appwiz.cpl/addons.c'
    with urllib.request.urlopen(url, timeout=30) as response:
        if not response.geturl().startswith('https://'):
            raise ValueError("refusing an insecure prerequisite source redirect")
        source = response.read(MAX_SOURCE_BYTES + 1)
    if len(source) > MAX_SOURCE_BYTES:
        raise ValueError("Wine prerequisite source exceeds its bounds")
    cache = Path(os.environ['HOME']) / '.cache' / 'wine'
    for name, url, checksum in prerequisites(source.decode('utf8')):
        cached_installer(cache, name, url, checksum)
        print(f'Wine prerequisite verified: {name}, sha256={checksum}')


if __name__ == '__main__':
    try:
        main()
    except (OSError, ValueError, KeyError, TypeError) as error:
        # Network exceptions may contain signed redirects. Never print their values.
        print(f'Wine prerequisite provisioning failed ({type(error).__name__})', file=sys.stderr)
        raise SystemExit(1)
