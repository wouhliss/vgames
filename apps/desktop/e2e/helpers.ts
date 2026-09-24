import { expect, type Page } from "@playwright/test";

export const FINGERPRINT = "VG1-7K2M-Q9XD-4HT8-B3NW-RC5E-X1JP-V6GA-M0ZF";
export const OTHER_FINGERPRINT = "VG1-J3KD-8PQX-NW2A-TY4B-C9RM-5EHV-G7ZS-1FQ0";
export const VALID_CODE = "VALID-CODE-0123456789-abcdefghijklmnopqrstuvwxyz";

export async function open(
  page: Page,
  preset: string,
  state?: Record<string, unknown>,
): Promise<void> {
  const extra = state ? `&mockState=${encodeURIComponent(JSON.stringify(state))}` : "";
  await page.goto(`/?mock=${preset}${extra}`);
}

/** Emits a Rust event into the page (mock mode). */
export async function emit(page: Page, event: string, payload: unknown): Promise<void> {
  await page.waitForFunction(() => window.__vgamesMock !== undefined);
  await page.evaluate(([e, p]) => window.__vgamesMock?.emit(e as string, p), [
    event,
    payload,
  ] as const);
}

export async function commandCalls(page: Page, cmd: string): Promise<unknown[]> {
  return page.evaluate(
    (c) => window.__vgamesMock?.backend.callsTo(c).map((call) => call.args) ?? [],
    cmd,
  );
}

export async function submitAddress(page: Page, address: string): Promise<void> {
  const field = page.getByRole("textbox", { name: /Server address/ });
  await field.fill(address);
  await page.getByRole("button", { name: "Continue" }).click();
}

export async function reachSignIn(page: Page, address: string): Promise<void> {
  await submitAddress(page, address);
  await page.getByRole("button", { name: "It matches, continue" }).click();
  await expect(page.getByRole("heading", { name: /^Sign in to/ })).toBeVisible();
}
