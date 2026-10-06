// INT-10: count every script needed by the initial document, including modulepreloads.
import { readFile } from "node:fs/promises";
import path from "node:path";
import { gzipSync } from "node:zlib";

const dist = path.resolve(process.argv[2] ?? "apps/desktop/dist");
const budget = 250_000;
const html = await readFile(path.join(dist, "index.html"), "utf8");
const scripts = new Set();
for (const match of html.matchAll(/<(script|link)\b[^>]*>/gi)) {
  const attributes = Object.fromEntries(
    [...match[0].matchAll(/([a-z][-a-z0-9]*)\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s>]+))/gi)].map(
      (a) => [a[1].toLowerCase(), a[2] ?? a[3] ?? a[4]],
    ),
  );
  const src =
    match[1].toLowerCase() === "script"
      ? attributes.src
      : attributes.rel?.toLowerCase().split(/\s+/).includes("modulepreload")
        ? attributes.href
        : undefined;
  if (!src) continue;
  const url = new URL(src, "https://bundle.invalid/");
  if (url.origin !== "https://bundle.invalid")
    throw new Error("Initial script must be bundled locally");
  const file = path.resolve(dist, `.${decodeURIComponent(url.pathname)}`);
  if (
    !path.relative(dist, file) ||
    path.relative(dist, file).startsWith("..") ||
    path.isAbsolute(path.relative(dist, file))
  ) {
    throw new Error("Script path escapes the bundle directory");
  }
  scripts.add(file);
}
if (scripts.size === 0) throw new Error("Initial document has no scripts");
let total = 0;
for (const file of scripts) {
  const size = gzipSync(await readFile(file)).length;
  total += size;
  console.log(`${path.relative(dist, file)}: ${size} bytes gzip`);
}
console.log(`Initial JavaScript: ${total} / ${budget} bytes gzip`);
if (total > budget) {
  console.error("Initial JavaScript exceeds the 250 KB gzip budget");
  process.exitCode = 1;
}
