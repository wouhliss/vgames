import assert from "node:assert/strict";
import test from "node:test";
import { summarizeDownloads } from "./downloads.ts";

test("download checkpoints contain fixed counts, never private job fields", () => {
  const privateValue = "PRIVATE_FIXTURE_VALUE";
  const result = summarizeDownloads({
    jobs: [
      {
        title: privateValue,
        state: {
          kind: "failed",
          error: { kind: "server", code: privateValue, message: privateValue },
        },
      },
      { state: { kind: "paused", reason: { kind: "disk_full", library_path: privateValue } } },
      { state: { kind: privateValue } },
    ],
    history: [{ title: privateValue, outcome: { kind: "installed" } }],
  });
  assert.equal(result.failed, 1);
  assert.equal(result.installed, 1);
  assert.equal(result.errors?.server, 1);
  assert.equal(result.pauses?.disk_full, 1);
  assert.ok(!JSON.stringify(result).includes(privateValue));
});

test("an unreadable queue is unavailable rather than free-form diagnostics", () => {
  for (const value of [null, "PRIVATE_FIXTURE_VALUE", [], {}, { jobs: "private", history: [] }]) {
    assert.deepEqual(summarizeDownloads(value), { available: false });
  }
});
