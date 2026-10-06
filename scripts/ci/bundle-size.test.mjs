import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { randomBytes } from "node:crypto";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { test } from "node:test";
import { gzipSync } from "node:zlib";

async function fixture(html, files, check) {
  const dir = await mkdtemp(path.join(tmpdir(), "vgames-bundle-"));
  try {
    await writeFile(path.join(dir, "index.html"), html);
    for (const [name, body] of Object.entries(files)) await writeFile(path.join(dir, name), body);
    check(spawnSync(process.execPath, ["scripts/ci/bundle-size.mjs", dir], { encoding: "utf8" }));
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
}

test("includes modulepreloads once and ignores stylesheets", async () => {
  const entry = "console.log('entry');";
  const chunk = "export const example = 1;";
  await fixture(
    '<script src="/main.js"></script><link rel="MODULEPRELOAD" href="/chunk.js"><link rel="MODULEPRELOAD" href="/chunk.js?v=2"><link rel="stylesheet" href="/absent.css">',
    { "main.js": entry, "chunk.js": chunk },
    (result) => {
      assert.equal(result.status, 0, result.stderr);
      assert.match(
        result.stdout,
        new RegExp(`Initial JavaScript: ${gzipSync(entry).length + gzipSync(chunk).length} /`),
      );
    },
  );
});

test("fails when a required initial chunk is missing", async () => {
  await fixture('<script src="/missing.js"></script>', {}, (result) =>
    assert.notEqual(result.status, 0),
  );
});

test("fails above the compressed budget including preload chunks", async () => {
  await fixture(
    '<script src="/main.js"></script><link rel="modulepreload" href="/large.js">',
    {
      "main.js": "console.log('small');",
      "large.js": `export default '${randomBytes(600000).toString("hex")}';`,
    },
    (result) => {
      assert.equal(result.status, 1);
      assert.match(result.stderr, /exceeds the 250 KB/);
    },
  );
});
