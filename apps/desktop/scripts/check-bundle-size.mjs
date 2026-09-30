// Bundle budget (A3-T12, 00-overview §7): the JavaScript the launcher loads before the first screen
// (the entry and everything index.html preloads) stays within 250 KB gzipped, and no route chunk
// grows past 100 KB gzipped. Run after `vite build`; exits 1 over budget. `--json` prints the numbers.
import { readdirSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { gzipSync } from "node:zlib";

const dist = join(dirname(fileURLToPath(import.meta.url)), "..", "dist");
const INITIAL_BUDGET = 250_000;
const CHUNK_BUDGET = 100_000;

const gz = (file) => gzipSync(readFileSync(join(dist, file)), { level: 9 }).length;

const html = readFileSync(join(dist, "index.html"), "utf8");
const initial = new Set(
  [...html.matchAll(/(?:src|href)="\/(assets\/[^"]+\.js)"/g)].map((m) => m[1]),
);
if (initial.size === 0) {
  console.error("index.html references no scripts; run `vite build` first");
  process.exit(1);
}

const initialBytes = [...initial].reduce((sum, file) => sum + gz(file), 0);
const chunks = readdirSync(join(dist, "assets"))
  .filter((f) => f.endsWith(".js"))
  .map((f) => ({ file: `assets/${f}`, bytes: gz(`assets/${f}`) }))
  .filter((c) => !initial.has(c.file))
  .sort((a, b) => b.bytes - a.bytes);

const kb = (n) => `${(n / 1000).toFixed(1)} KB`;
if (process.argv.includes("--json")) {
  console.log(JSON.stringify({ initial: initialBytes, chunks }, null, 2));
}
console.log(`initial JS: ${kb(initialBytes)} gzipped (budget ${kb(INITIAL_BUDGET)})`);
for (const c of chunks.slice(0, 5)) console.log(`  ${c.file}: ${kb(c.bytes)}`);

let failed = false;
if (initialBytes > INITIAL_BUDGET) {
  console.error(`initial JS is over budget by ${kb(initialBytes - INITIAL_BUDGET)}`);
  failed = true;
}
for (const c of chunks) {
  if (c.bytes > CHUNK_BUDGET) {
    console.error(
      `${c.file} is ${kb(c.bytes)} gzipped, over the ${kb(CHUNK_BUDGET)} route chunk budget`,
    );
    failed = true;
  }
}
process.exit(failed ? 1 : 0);
