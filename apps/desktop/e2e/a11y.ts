import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import AxeBuilder from "@axe-core/playwright";
import type { Page } from "@playwright/test";

/** Resolve the pinned engine from the launcher's declared test dependency. */
export function axeScript(): string {
  const require = createRequire(import.meta.url);
  const engine = createRequire(require.resolve("@axe-core/playwright"));
  return readFileSync(engine.resolve("axe-core/axe.min.js"), "utf8");
}

export interface AxeAnalyzer {
  analyze(include?: string): Promise<unknown>;
}

/** The same browser-side readiness condition is used by Chromium and WebKit. */
export const animationReadiness = `async () => {
  const frame = () => new Promise(resolve => requestAnimationFrame(resolve));
  await frame();
  const animations = document.getAnimations?.() ?? [];
  await Promise.all(animations
    .filter(animation => Number.isFinite(animation.effect?.getComputedTiming().endTime))
    .map(animation => animation.finished.catch(() => {})));
  await frame();
}`;

type Impact = "minor" | "moderate" | "serious" | "critical" | null;
const IMPACTS = new Set<unknown>([null, undefined, "minor", "moderate", "serious", "critical"]);

/** Diagnostics contain rule ids and counts, never DOM HTML or entered credentials. */
export function summarizeAccessibility(value: unknown): {
  id: string;
  impact: Impact;
  affectedNodes: number;
}[] {
  if (
    !value ||
    typeof value !== "object" ||
    !("violations" in value) ||
    !Array.isArray(value.violations)
  ) {
    throw new Error("Malformed accessibility result");
  }
  return value.violations.map((item: unknown) => {
    if (!item || typeof item !== "object") throw new Error("Malformed accessibility result");
    const row = item as Record<string, unknown>;
    if (typeof row.id !== "string" || !IMPACTS.has(row.impact) || !Array.isArray(row.nodes)) {
      throw new Error("Malformed accessibility result");
    }
    return { id: row.id, impact: (row.impact ?? null) as Impact, affectedNodes: row.nodes.length };
  });
}

/** Shared by Playwright and the real WebKit WebDriver adapter; no rules are disabled. */
export async function axeViolations(target: Page | AxeAnalyzer, include?: string) {
  if ("analyze" in target) return summarizeAccessibility(await target.analyze(include));
  await target.evaluate(`(${animationReadiness})()`);
  const scanner = new AxeBuilder({ page: target });
  if (include) scanner.include(include);
  return summarizeAccessibility(await scanner.analyze());
}
