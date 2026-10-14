import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { licenseTexts, npmNotices, renderNotices, rustNotices } from "./third-party-notices.mjs";

test("notices preserve installed copyright text and exclude development dependencies", () => {
  const root = mkdtempSync(join(tmpdir(), "vgames-notices-"));
  try {
    const app = join(root, "app");
    const dependency = join(app, "node_modules", "fixture");
    mkdirSync(dependency, { recursive: true });
    writeFileSync(
      join(app, "package.json"),
      JSON.stringify({
        name: "app",
        dependencies: { fixture: "1" },
        devDependencies: { missing_dev_fixture: "1" },
      }),
    );
    writeFileSync(
      join(dependency, "package.json"),
      JSON.stringify({
        name: "fixture",
        version: "1.2.3",
        license: "MIT",
        author: "Fixture Author",
      }),
    );
    writeFileSync(
      join(dependency, "LICENSE"),
      "Copyright Fixture Author\nPermission fixture text.",
    );
    const notices = npmNotices(app);
    assert.equal(notices.length, 1);
    const text = renderNotices({ npm: notices });
    assert.match(text, /fixture 1\.2\.3/);
    assert.match(text, /Copyright Fixture Author/);
    assert.ok(!text.includes("missing_dev_fixture"));
    assert.equal(text, renderNotices({ npm: [...notices, ...notices] }));
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("Rust graph includes native build sources and excludes test-only dependencies", () => {
  const root = mkdtempSync(join(tmpdir(), "vgames-rust-notices-"));
  try {
    writeFileSync(join(root, "LICENSE"), "Native fixture license.");
    const pkg = (id, name) => ({
      id,
      name,
      version: "1",
      license: "Zlib",
      authors: [],
      manifest_path: join(root, "Cargo.toml"),
    });
    const metadata = {
      packages: [
        pkg("app", "vgames-desktop"),
        pkg("native", "native-src"),
        pkg("dev", "test-only"),
      ],
      workspace_members: ["app"],
      resolve: {
        nodes: [
          {
            id: "app",
            deps: [
              { pkg: "native", dep_kinds: [{ kind: "build" }] },
              { pkg: "dev", dep_kinds: [{ kind: "dev" }] },
            ],
          },
          { id: "native", deps: [] },
        ],
      },
    };
    assert.deepEqual(
      rustNotices(metadata).map((notice) => notice.name),
      ["native-src"],
    );
    metadata.resolve.nodes.pop();
    assert.throws(() => rustNotices(metadata), /Incomplete/);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("declared license paths cannot escape a package", () => {
  const root = mkdtempSync(join(tmpdir(), "vgames-license-path-"));
  try {
    const pkg = join(root, "pkg");
    mkdirSync(pkg);
    writeFileSync(join(root, "outside"), "Outside fixture.");
    assert.throws(() => licenseTexts(pkg, "../outside"), /escapes/);
    // Windows hosted tests run without symlink privilege; lexical escape is
    // always tested, and Unix additionally tests a replaced declared file.
    if (process.platform !== "win32") {
      symlinkSync(join(root, "outside"), join(pkg, "LICENSE"));
      assert.throws(() => licenseTexts(pkg, "LICENSE"), /escapes/);
    }
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
