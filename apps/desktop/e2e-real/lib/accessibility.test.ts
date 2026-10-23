import assert from "node:assert/strict";
import { test } from "node:test";
import { runInNewContext } from "node:vm";
import { nativeAxeViolations } from "./accessibility.ts";

test("real-WebView scans retain findings without logging HTML or input values", async () => {
  let injected = false;
  const findings = await nativeAxeViolations({
    async execute(script) {
      assert.ok(script.includes("axe"));
      injected = true;
      return null;
    },
    async executeAsync(script, args) {
      assert.ok(injected);
      assert.ok(script.includes("axe.run"));
      assert.deepEqual(args, [null]);
      return new Promise((resolve) => {
        const done = (value: unknown) => {
          assert.ok(!JSON.stringify(value).includes("MOCK_SENSITIVE_FIELD"));
          resolve(value);
        };
        runInNewContext(script, {
          arguments: [...(args ?? []), done],
          document: {},
          axe: {
            run(
              _scope: unknown,
              _options: unknown,
              callback: (error: null, value: unknown) => void,
            ) {
              callback(null, {
                violations: [
                  { id: "label", impact: "serious", nodes: [{ html: "MOCK_SENSITIVE_FIELD" }] },
                ],
              });
            },
          },
        });
      });
    },
  });
  assert.deepEqual(findings, [{ id: "label", impact: "serious", affectedNodes: 1 }]);
  assert.ok(!JSON.stringify(findings).includes("MOCK_SENSITIVE_FIELD"));
});

test("an unavailable or malformed engine result cannot become a clean scan", async () => {
  for (const value of [null, {}, { violations: [{ id: "rule", impact: "unknown", nodes: [] }] }]) {
    await assert.rejects(
      nativeAxeViolations({
        async execute() {
          return null;
        },
        async executeAsync() {
          return value;
        },
      }),
      /Malformed accessibility result/,
    );
  }
});
