#!/usr/bin/env node
// INT-10: initial scripts plus the chunks index.html asks to preload, counted once.
import { readFile, realpath } from "node:fs/promises";
import { dirname, isAbsolute, relative, resolve, sep } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { gzipSync } from "node:zlib";

export const BUDGET_BYTES = 250_000;

function attributes(tag) {
  return Object.fromEntries(
    [...tag.matchAll(/([\w-]+)\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s"'=<>`]+))/g)].map((match) => [
      match[1].toLowerCase(),
      match[2] ?? match[3] ?? match[4],
    ]),
  );
}

export async function measureInitialJs(directory) {
  const root = await realpath(directory);
  const html = await readFile(resolve(root, "index.html"), "utf8");
  const files = new Set();
  let entries = 0;
  for (const [tag] of html.matchAll(/<(?:script|link)\b[^>]*>/gi)) {
    const attrs = attributes(tag);
    const script = /^<script\b/i.test(tag) && attrs.src;
    const preload = /^<link\b/i.test(tag) && attrs.rel?.toLowerCase() === "modulepreload";
    if (!script && !preload) continue;
    const source = script ? attrs.src : attrs.href;
    if (!source || /^(?:[a-z]+:|\/\/)/i.test(source)) {
      throw new Error("Initial JavaScript must be a local, measurable build artifact");
    }
    const path = decodeURIComponent(source.split(/[?#]/)[0]).replace(/^\//, "");
    const file = await realpath(resolve(root, path));
    const fromRoot = relative(root, file);
    if (isAbsolute(fromRoot) || fromRoot === ".." || fromRoot.startsWith(`..${sep}`)) {
      throw new Error("Initial JavaScript points outside the build directory");
    }
    files.add(file);
    if (script) entries++;
  }
  if (!entries) throw new Error("No initial JavaScript entry was found in index.html");
  const sizes = await Promise.all(
    [...files].sort().map(async (file) => ({
      file: relative(root, file),
      gzipBytes: gzipSync(await readFile(file)).byteLength,
    })),
  );
  const gzipBytes = sizes.reduce((sum, item) => sum + item.gzipBytes, 0);
  return { gzipBytes, budgetBytes: BUDGET_BYTES, passed: gzipBytes <= BUDGET_BYTES, files: sizes };
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  try {
    const root = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
    const result = await measureInitialJs(process.argv[2] ?? resolve(root, "apps/desktop/dist"));
    console.log(JSON.stringify(result, null, 2));
    if (!result.passed) process.exitCode = 1;
  } catch (error) {
    console.error(error instanceof Error ? error.message : "Cannot measure initial JavaScript");
    process.exitCode = 1;
  }
}
