// Build-time only. Rust embeds the resulting text; the WebView never reads files.
import { spawnSync } from "node:child_process";
import { readdirSync, readFileSync, realpathSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, isAbsolute, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const MAX_TEXT = 512 * 1024;
const repo = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const json = (path) => JSON.parse(readFileSync(path, "utf8"));

export function licenseTexts(directory, declaredFile) {
  const root = realpathSync(directory);
  const files = new Set();
  function add(path) {
    const real = realpathSync(path);
    const name = relative(root, real);
    if (
      isAbsolute(name) ||
      name === ".." ||
      name.startsWith(`..${process.platform === "win32" ? "\\" : "/"}`)
    ) {
      throw new Error("License file escapes its package.");
    }
    const text = readFileSync(real);
    if (text.length > MAX_TEXT || text.includes(0)) throw new Error("Invalid license text.");
    files.add(real);
  }
  function walk(path, depth) {
    for (const entry of readdirSync(path, { withFileTypes: true })) {
      // No symlink traversal. Native vendored sources have their own notices.
      if (entry.isFile() && /^(licen[cs]e|copying|notice|copyright)([._-]|$)/i.test(entry.name))
        add(join(path, entry.name));
      if (
        depth > 0 &&
        entry.isDirectory() &&
        /^(licen[cs]es?|vendor|sdl\d*|zstd|externals)$/i.test(entry.name)
      )
        walk(join(path, entry.name), depth - 1);
    }
  }
  if (declaredFile) add(resolve(root, declaredFile));
  walk(root, 3);
  return [...files].sort().map((file) => readFileSync(file, "utf8").trim());
}

export function rustNotices(metadata) {
  const packages = new Map(metadata.packages.map((pkg) => [pkg.id, pkg]));
  const nodes = new Map(metadata.resolve.nodes.map((node) => [node.id, node]));
  const root = metadata.packages.find((pkg) => pkg.name === "vgames-desktop");
  if (!root) throw new Error("Launcher dependency graph is missing.");
  const pending = [root.id];
  const seen = new Set();
  while (pending.length) {
    const id = pending.pop();
    if (seen.has(id)) continue;
    seen.add(id);
    const node = nodes.get(id);
    if (!node || !packages.has(id)) throw new Error("Incomplete Rust dependency graph.");
    for (const dep of node.deps) {
      // Include build-time/native-source notices, exclude test-only crates.
      if (dep.dep_kinds.some((kind) => kind.kind !== "dev")) pending.push(dep.pkg);
    }
  }
  return [...seen]
    .map((id) => packages.get(id))
    .filter((pkg) => pkg && !metadata.workspace_members.includes(pkg.id))
    .map((pkg) => ({
      name: pkg.name,
      version: pkg.version,
      license: pkg.license,
      authors: pkg.authors,
      texts: licenseTexts(dirname(pkg.manifest_path), pkg.license_file),
    }));
}

function packageDirectory(name, from) {
  const require = createRequire(join(from, "package.json"));
  let path;
  try {
    path = dirname(require.resolve(`${name}/package.json`));
  } catch {
    path = dirname(require.resolve(name));
  }
  for (;;) {
    try {
      if (json(join(path, "package.json")).name === name) return realpathSync(path);
    } catch {
      /* Keep walking from an exported entry point to its package. */
    }
    const parent = dirname(path);
    if (parent === path) throw new Error(`Cannot resolve npm package ${name}.`);
    path = parent;
  }
}
export function npmNotices(directory) {
  const queue = [directory];
  const seen = new Set();
  const notices = [];
  while (queue.length) {
    const path = realpathSync(queue.pop());
    if (seen.has(path)) continue;
    seen.add(path);
    const pkg = json(join(path, "package.json"));
    if (path !== realpathSync(directory)) {
      notices.push({
        name: pkg.name,
        version: pkg.version,
        license: typeof pkg.license === "string" ? pkg.license : pkg.license?.type,
        authors: [typeof pkg.author === "string" ? pkg.author : pkg.author?.name].filter(Boolean),
        texts: licenseTexts(path),
      });
    }
    for (const name of Object.keys(pkg.dependencies ?? {}).sort())
      queue.push(packageDirectory(name, path));
    for (const name of Object.keys(pkg.optionalDependencies ?? {}).sort()) {
      try {
        queue.push(packageDirectory(name, path));
      } catch {
        /* Not installed on this platform. */
      }
    }
    // Peer dependencies are supplied by the consuming package. Include those
    // actually installed, without making optional peers mandatory.
    for (const name of Object.keys(pkg.peerDependencies ?? {}).sort()) {
      try {
        queue.push(packageDirectory(name, path));
      } catch {
        if (!pkg.peerDependenciesMeta?.[name]?.optional)
          throw new Error(`Missing npm peer ${name}.`);
      }
    }
  }
  return notices;
}
export function renderNotices(groups) {
  const sections = Object.entries(groups)
    .map(([group, notices]) => {
      const unique = new Map();
      for (const notice of notices) unique.set(`${notice.name}@${notice.version}`, notice);
      const entries = [...unique.values()].sort((a, b) =>
        `${a.name}@${a.version}`.localeCompare(`${b.name}@${b.version}`, "en"),
      );
      return `${group}\n${"=".repeat(group.length)}\n\n${entries
        .map((entry) => {
          if (!entry.license && !entry.texts.length)
            throw new Error(`No license information for ${entry.name}.`);
          return `${entry.name} ${entry.version}\nDeclared license: ${entry.license ?? "See notice below"}\n${entry.authors?.length ? `Authors: ${entry.authors.join(", ")}\n` : ""}\n${entry.texts.length ? entry.texts.join("\n\n") : "No separate license text was included in this package."}`;
        })
        .join("\n\n--------------------\n\n")}`;
    })
    .join("\n\n");
  return `${sections}\n`;
}

function run() {
  const args = process.argv.slice(2);
  const option = (name) => args[args.indexOf(name) + 1];
  const target = option("--target");
  const out = option("--out");
  if (!target || !out || !args.includes("--out") || !args.includes("--target"))
    throw new Error("Use --out and --target.");
  const result = spawnSync(
    process.env.CARGO ?? "cargo",
    [
      "metadata",
      "--format-version=1",
      "--locked",
      "--filter-platform",
      target,
      "--manifest-path",
      join(repo, "apps/desktop/src-tauri/Cargo.toml"),
    ],
    { encoding: "utf8", timeout: 120_000, maxBuffer: 32 * 1024 * 1024 },
  );
  if (result.status !== 0) throw new Error("Cannot read the locked Rust dependency graph.");
  const groups = {
    "Rust dependencies": rustNotices(JSON.parse(result.stdout)),
    "npm production dependencies": npmNotices(join(repo, "apps/desktop")),
  };
  writeFileSync(out, renderNotices(groups));
}
if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) run();
