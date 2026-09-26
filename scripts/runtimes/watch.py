#!/usr/bin/env python3
"""Upstream runtime watcher (A5-T12, runtimes.yml). Owner: Agent 5.

For every upstream in runtimes/upstreams.toml: find the newest matching release, download the asset,
verify it (declared size, GitHub's SHA-256 digest when published, the upstream checksum file), check
licenses, and write what runtimes.yml needs to propose it:

  OUT/entries.json   new catalog entries, for `cargo xtask runtimes upsert`
  OUT/smoke.json     [{id, version, os, smoke, file}] for the smoke-test jobs
  OUT/files/...      the verified archives
  OUT/summary.md     the report (job summary and PR body)

Exit status: 0 when every upstream is either proposed, already pinned or waiting for a human (reported
under "Needs attention"); 1 on an integrity or license problem, in which case nothing is proposed.
Standard library only (Python 3.11+). GITHUB_TOKEN, when set, is used for the API only.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import sys
import tomllib
import urllib.error
import urllib.request
from dataclasses import dataclass, field
from pathlib import Path

API = "https://api.github.com"
TIMEOUT = 60
MAX_BYTES = 8 * 1024**3
VERSION_RE = re.compile(r"[A-Za-z0-9._+\-]{1,64}")
REPO_RE = re.compile(r"[A-Za-z0-9_.\-]+/[A-Za-z0-9_.\-]+")
SPDX_OPERATORS = {"AND", "OR", "WITH"}
SMOKE_KINDS = {"proton", "umu", "wine-macos", "none"}
ARCHIVES = {"tar", "tar.gz", "tar.xz", "tar.zst", "zip"}


class IntegrityError(Exception):
    """The bytes or licenses are not what they should be: stop, propose nothing."""


class Attention(Exception):
    """Needs a human (configuration, onboarding), but is not a security problem."""


@dataclass
class Upstream:
    id: str
    repo: str
    tag: re.Pattern[str]
    asset: re.Pattern[str]
    archive: str
    os: str
    arch: str
    license: str
    smoke: str
    repo_license: str | None = None
    checksum_asset: re.Pattern[str] | None = None
    checksum: str = "sha512"
    version_prefix: str = ""
    rosetta_required: bool = False


@dataclass
class Config:
    license_allowlist: set[str]
    min_launcher_version: str
    upstreams: list[Upstream] = field(default_factory=list)


def load_config(text: str) -> Config:
    raw = tomllib.loads(text)
    allowed = {
        "id", "repo", "tag", "asset", "archive", "os", "arch", "license", "smoke", "repo_license",
        "checksum_asset", "checksum", "version_prefix", "rosetta_required",
    }
    config = Config(set(raw["license_allowlist"]), raw["min_launcher_version"])
    ids = set()
    for u in raw.get("upstream", []):
        unknown = set(u) - allowed
        if unknown:
            raise ValueError(f"upstream {u.get('id')}: unknown fields {sorted(unknown)}")
        up = Upstream(
            id=u["id"],
            repo=u["repo"],
            tag=re.compile(u["tag"]),
            asset=re.compile(u["asset"]),
            archive=u["archive"],
            os=u["os"],
            arch=u["arch"],
            license=u["license"],
            smoke=u["smoke"],
            repo_license=u.get("repo_license"),
            checksum_asset=re.compile(u["checksum_asset"]) if "checksum_asset" in u else None,
            checksum=u.get("checksum", "sha512"),
            version_prefix=u.get("version_prefix", ""),
            rosetta_required=u.get("rosetta_required", False),
        )
        if not REPO_RE.fullmatch(up.repo):
            raise ValueError(f"upstream {up.id}: bad repo {up.repo!r}")
        if up.smoke not in SMOKE_KINDS or up.archive not in ARCHIVES:
            raise ValueError(f"upstream {up.id}: bad smoke or archive")
        if up.checksum not in ("sha256", "sha512"):
            raise ValueError(f"upstream {up.id}: checksum must be sha256 or sha512")
        if up.id in ids:
            raise ValueError(f"upstream {up.id} is listed twice")
        ids.add(up.id)
        config.upstreams.append(up)
    return config


def license_ids(expression: str) -> set[str]:
    tokens = expression.replace("(", " ").replace(")", " ").split()
    return {t for t in tokens if t not in SPDX_OPERATORS}


def check_license(up: Upstream, allowlist: set[str], repo_license: str | None) -> None:
    missing = license_ids(up.license) - allowlist
    if missing:
        raise IntegrityError(f"{up.id}: {sorted(missing)} not in license_allowlist")
    if up.repo_license is None:
        raise Attention(
            f"{up.id}: GitHub reports the license of {up.repo} as {repo_license!r}. Confirm it covers "
            f"{up.license!r}, then record repo_license = {repo_license!r} in upstreams.toml"
        )
    if repo_license != up.repo_license:
        raise IntegrityError(
            f"{up.id}: the license of {up.repo} changed from {up.repo_license!r} to {repo_license!r}"
        )


def pick_release(up: Upstream, releases: list[dict]) -> tuple[dict, dict]:
    """The newest published, non-prerelease release with a matching tag and exactly one matching asset."""
    for release in releases:
        if release.get("draft") or release.get("prerelease"):
            continue
        tag = release.get("tag_name", "")
        if not up.tag.fullmatch(tag):
            continue
        assets = [a for a in release.get("assets", []) if up.asset.fullmatch(a.get("name", ""))]
        if len(assets) == 1:
            return release, assets[0]
        if len(assets) > 1:
            raise Attention(f"{up.id}: {tag} has {len(assets)} assets matching {up.asset.pattern!r}")
    raise Attention(f"{up.id}: no release of {up.repo} matches tag {up.tag.pattern!r} and asset "
                    f"{up.asset.pattern!r} (upstream renamed something?)")


def version_of(up: Upstream, tag: str) -> str:
    version = tag.removeprefix(up.version_prefix)
    if not VERSION_RE.fullmatch(version):
        raise Attention(f"{up.id}: version {version!r} does not fit the catalog's version rule")
    return version


def asset_url(up: Upstream, tag: str, asset: dict) -> str:
    url = f"https://github.com/{up.repo}/releases/download/{tag}/{asset['name']}"
    if asset.get("browser_download_url") != url:
        raise IntegrityError(f"{up.id}: unexpected download URL {asset.get('browser_download_url')!r}")
    if any(c in url for c in "?#@\\%") or not url.isascii():
        raise Attention(f"{up.id}: {url!r} cannot be a catalog URL")
    return url


def parse_checksum(text: str, filename: str) -> str:
    """`<hex>  <name>` / `<hex> *<name>` lines, or a file holding a single hash."""
    lines = [line.split() for line in text.splitlines() if line.strip()]
    for parts in lines:
        if len(parts) >= 2 and parts[-1].lstrip("*").rsplit("/", 1)[-1] == filename:
            return parts[0].lower()
    if len(lines) == 1 and len(lines[0]) == 1:
        return lines[0][0].lower()
    raise IntegrityError(f"no checksum for {filename} in the upstream checksum file")


def pinned(catalog: dict, up: Upstream, version: str) -> dict | None:
    for entry in catalog.get("runtime", []):
        if (entry["id"], entry["version"], entry["os"], entry["arch"]) == (up.id, version, up.os, up.arch):
            return entry
    return None


# ---- network ----------------------------------------------------------------------------------


def _request(url: str, api: bool) -> urllib.request.Request:
    headers = {"User-Agent": "vgames-runtimes-watcher"}
    if api:
        headers["Accept"] = "application/vnd.github+json"
        token = os.environ.get("GITHUB_TOKEN")
        if token:
            headers["Authorization"] = f"Bearer {token}"
    return urllib.request.Request(url, headers=headers)


def api_get(path: str):
    with urllib.request.urlopen(_request(f"{API}{path}", api=True), timeout=TIMEOUT) as response:
        return json.load(response)


def download(url: str, dest: Path, expected_size: int) -> tuple[str, str]:
    """Streams url to dest; returns (sha256, sha512). Never reads past the declared size."""
    if not 0 < expected_size <= MAX_BYTES:
        raise IntegrityError(f"{url}: declared size {expected_size} out of range")
    sha256, sha512, size = hashlib.sha256(), hashlib.sha512(), 0
    dest.parent.mkdir(parents=True, exist_ok=True)
    with urllib.request.urlopen(_request(url, api=False), timeout=TIMEOUT) as response, dest.open("wb") as out:
        while chunk := response.read(1 << 20):
            size += len(chunk)
            if size > expected_size:
                raise IntegrityError(f"{url}: larger than the declared {expected_size} bytes")
            sha256.update(chunk)
            sha512.update(chunk)
            out.write(chunk)
    if size != expected_size:
        raise IntegrityError(f"{url}: {size} bytes, declared {expected_size}")
    return sha256.hexdigest(), sha512.hexdigest()


def fetch_text(url: str, limit: int = 64 * 1024) -> str:
    with urllib.request.urlopen(_request(url, api=False), timeout=TIMEOUT) as response:
        data = response.read(limit + 1)
    if len(data) > limit:
        raise IntegrityError(f"{url}: checksum file larger than {limit} bytes")
    return data.decode("utf-8", errors="strict")


# ---- one upstream -----------------------------------------------------------------------------


def watch(up: Upstream, config: Config, catalog: dict, out: Path) -> tuple[dict, dict] | None:
    """Returns (catalog entry, smoke job) for a new version, None when it is already pinned."""
    repo = api_get(f"/repos/{up.repo}")
    check_license(up, config.license_allowlist, (repo.get("license") or {}).get("spdx_id"))
    release, asset = pick_release(up, api_get(f"/repos/{up.repo}/releases?per_page=30"))
    tag = release["tag_name"]
    version = version_of(up, tag)
    url = asset_url(up, tag, asset)
    existing = pinned(catalog, up, version)
    if existing is not None and existing["url"] == url:
        # Already pinned: `cargo xtask runtimes upsert` refuses other bytes if it ever changes.
        return None
    file = out / "files" / up.id / asset["name"]
    sha256, sha512 = download(url, file, asset["size"])
    digest = asset.get("digest")
    if digest and digest != f"sha256:{sha256}":
        raise IntegrityError(f"{up.id} {version}: GitHub's digest {digest} does not match the download")
    if up.checksum_asset is not None:
        sums = [a for a in release.get("assets", []) if up.checksum_asset.fullmatch(a.get("name", ""))]
        if len(sums) != 1:
            raise IntegrityError(f"{up.id} {version}: expected one checksum file, found {len(sums)}")
        expected = parse_checksum(fetch_text(sums[0]["browser_download_url"]), asset["name"])
        actual = sha512 if up.checksum == "sha512" else sha256
        if expected != actual:
            raise IntegrityError(f"{up.id} {version}: {up.checksum} does not match the upstream checksum file")
    entry = {
        "id": up.id, "version": version, "os": up.os, "arch": up.arch, "url": url, "sha256": sha256,
        "size": asset["size"], "archive": up.archive, "license": up.license,
        "min_launcher_version": config.min_launcher_version,
    }
    if up.rosetta_required:
        entry["rosetta_required"] = True
    smoke = {"id": up.id, "version": version, "os": up.os, "smoke": up.smoke,
             "file": str(file.relative_to(out))}
    return entry, smoke


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--upstreams", type=Path, default=Path("runtimes/upstreams.toml"))
    parser.add_argument("--catalog", type=Path, default=Path("runtimes/catalog.toml"))
    parser.add_argument("--pending", type=Path,
                        help="catalog.toml of the open update PR: its versions count as pinned")
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--only", action="append", help="watch only these ids")
    args = parser.parse_args(argv)

    config = load_config(args.upstreams.read_text(encoding="utf-8"))
    catalog = tomllib.loads(args.catalog.read_text(encoding="utf-8"))
    if args.pending is not None:
        pending = tomllib.loads(args.pending.read_text(encoding="utf-8"))
        catalog["runtime"] = catalog.get("runtime", []) + pending.get("runtime", [])
    args.out.mkdir(parents=True, exist_ok=True)
    entries, smoke, attention, errors, current = [], [], [], [], []
    for up in config.upstreams:
        if args.only and up.id not in args.only:
            continue
        try:
            found = watch(up, config, catalog, args.out)
        except IntegrityError as e:
            errors.append(str(e))
        except Attention as e:
            attention.append(str(e))
        except (urllib.error.URLError, TimeoutError, OSError, KeyError, ValueError) as e:
            attention.append(f"{up.id}: {type(e).__name__}: {e}")
        else:
            if found is None:
                current.append(up.id)
            else:
                entries.append(found[0])
                smoke.append(found[1])

    lines = ["## Runtime catalog update", ""]
    if errors:
        lines += ["### Integrity or license problems (nothing proposed)", ""]
        lines += [f"- {e}" for e in errors] + [""]
    if entries and not errors:
        lines += ["### New versions", "", "| Runtime | Version | OS / arch | Size | SHA-256 | Smoke test |",
                  "|---|---|---|---|---|---|"]
        for e, s in zip(entries, smoke):
            lines.append(f"| {e['id']} | {e['version']} | {e['os']} / {e['arch']} | {e['size']} | "
                         f"`{e['sha256']}` | {s['smoke']} |")
        lines += ["", "Each download matched its declared size, GitHub's digest when published, and the upstream "
                  "checksum file where the upstream publishes one.", ""]
    if attention:
        lines += ["### Needs attention", ""] + [f"- {a}" for a in attention] + [""]
    if current:
        lines += [f"Already pinned: {', '.join(current)}.", ""]
    (args.out / "summary.md").write_text("\n".join(lines), encoding="utf-8")
    (args.out / "entries.json").write_text(json.dumps([] if errors else entries, indent=2), encoding="utf-8")
    (args.out / "smoke.json").write_text(json.dumps([] if errors else smoke, indent=2), encoding="utf-8")
    print("\n".join(lines))
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main())
