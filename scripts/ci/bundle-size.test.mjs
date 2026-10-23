import assert from "node:assert/strict";
import { randomBytes } from "node:crypto";
import { mkdtemp, rm, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { gzipSync } from "node:zlib";
import { measureInitialJs } from "./bundle-size.mjs";

async function fixture(run) {
  const directory = await mkdtemp(join(tmpdir(), "vgames-bundle-test-"));
  try {
    await run(directory);
  } finally {
    await rm(directory, { recursive: true });
  }
}

test("counts entry and quoted modulepreloads once, without lazy chunks or styles", async () => {
  await fixture(async (directory) => {
    await writeFile(join(directory, "index.html"), `<script src='/entry.js' type='module'></script>
      <link href="/shared.js?hash=1" crossorigin rel="modulepreload">
      <link rel='modulepreload' href='./shared.js#fragment'><link rel="stylesheet" href="/style.css">`);
    const entry = Buffer.from("import './shared.js'; import('./lazy.js');");
    const shared = Buffer.from("export const value = 42;");
    await writeFile(join(directory, "entry.js"), entry);
    await writeFile(join(directory, "shared.js"), shared);
    const result = await measureInitialJs(directory);
    assert.equal(result.gzipBytes, gzipSync(entry).length + gzipSync(shared).length);
    assert.equal(result.files.length, 2);
    assert.equal(result.passed, true);
  });
});

test("an incompressible 300 KB initial chunk fails the actual gzip budget", async () => {
  await fixture(async (directory) => {
    await writeFile(join(directory, "index.html"), '<script src="/entry.js"></script>');
    await writeFile(join(directory, "entry.js"), randomBytes(300_000));
    assert.equal((await measureInitialJs(directory)).passed, false);
  });
});

test("missing entry, unreadable preload, remote script and symlink escape fail closed", async () => {
  await fixture(async (directory) => {
    for (const html of ["<html></html>", '<script src="/missing.js"></script>',
      '<script src="https://example.invalid/entry.js"></script>']) {
      await writeFile(join(directory, "index.html"), html);
      await assert.rejects(measureInitialJs(directory));
    }
    const outside = `${directory}-outside.js`;
    try {
      await writeFile(outside, "outside");
      await symlink(outside, join(directory, "entry.js"));
      await writeFile(join(directory, "index.html"), '<script src="/entry.js"></script>');
      await assert.rejects(measureInitialJs(directory), /outside/);
    } finally {
      await rm(outside, { force: true });
    }
  });
});
