// The release launcher against the real API (INS-09). Started by run.sh, which provides
// the API (behind TLS), one isolated launcher instance behind tauri-driver, and these
// variables. Part 1 (M1): add server → fingerprint → sign in → library folder → empty
// library and catalog. Part 2 (M2): browse → install → progress → play → stop → verify →
// uninstall.
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { createReadStream, existsSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { after, before, test } from "node:test";
import { nativeAxeViolations } from "../lib/accessibility.ts";
import { type Locator, Session, WebDriverError } from "../lib/webdriver.ts";

const env = (name: string): string => {
  const value = process.env[name];
  if (!value) throw new Error(`run.sh sets ${name}`);
  return value;
};

const app = env("E2E_APP");
const port = Number(env("E2E_DRIVER_PORT"));
const server = env("E2E_SERVER");
const home = env("E2E_INSTANCE_HOME");
const library = env("E2E_LIBRARY");
const work = env("E2E_WORK");
const parts = (process.env.E2E_PARTS ?? "1").split(",");
const title = "E2E Game";

let session: Session;

// ---- what a player sees, by role and name (strings from src/i18n/en.ts) -------------------

const quote = (text: string) => (text.includes('"') ? `'${text}'` : `"${text}"`);
const named = (text: string) => `(normalize-space(.)=${quote(text)} or @aria-label=${quote(text)})`;
const button = (name: string): Locator => ({ xpath: `//button[${named(name)}]` });
const heading = (name: string): Locator => ({ xpath: `//h2[normalize-space(.)=${quote(name)}]` });
const menuItem = (name: string): Locator => ({
  xpath: `//*[@role="menuitem"][normalize-space(.)=${quote(name)}]`,
});
/** A form field by its label (`<label for>`) or `aria-label`. */
const field = (label: string): Locator => ({
  xpath: `//*[@id=//label[normalize-space(.)=${quote(label)}]/@for or @aria-label=${quote(label)}]`,
});
const navLink = (name: string): Locator => ({
  xpath: `//nav//a[normalize-space(.)=${quote(name)}]`,
});
const css = (selector: string): Locator => ({ css: selector });

// ---- waiting --------------------------------------------------------------------------------

/** Polls until `check` holds; fails with `what` after `ms`. */
async function until(what: string, check: () => boolean | Promise<boolean>, ms = 30_000) {
  const end = Date.now() + ms;
  while (Date.now() < end) {
    if (await check()) return;
    await new Promise((r) => setTimeout(r, 250));
  }
  throw new Error(`timed out: ${what}`);
}

const describe = (locator: Locator) => ("css" in locator ? locator.css : locator.xpath);

/** The element once it is displayed (and enabled, with `clickable`). */
async function element(locator: Locator, ms = 30_000, clickable = false): Promise<string> {
  let found: string | null = null;
  await until(
    `${describe(locator)} to be ${clickable ? "clickable" : "displayed"}`,
    async () => {
      found = await session.find(locator);
      if (!found || !(await session.displayed(found))) return false;
      return !clickable || (await session.enabled(found));
    },
    ms,
  );
  return found as unknown as string;
}

const shown = (locator: Locator, ms = 30_000) => element(locator, ms);

/** WebDriver refusals that clear on their own (an entrance animation, a re-render). */
const TRANSIENT = new Set([
  "element not interactable",
  "element click intercepted",
  "stale element reference",
]);

/** Acts on the element once it is clickable, retrying transient refusals until `ms`. */
async function act(locator: Locator, ms: number, action: (element: string) => Promise<void>) {
  const end = Date.now() + ms;
  for (;;) {
    try {
      await action(await element(locator, Math.max(end - Date.now(), 1_000), true));
      return;
    } catch (error) {
      if (!(error instanceof WebDriverError && TRANSIENT.has(error.code)) || Date.now() > end) {
        throw error;
      }
      await new Promise((r) => setTimeout(r, 250));
    }
  }
}

async function click(locator: Locator, ms = 30_000) {
  await act(locator, ms, (el) => session.click(el));
}

async function type(locator: Locator, text: string) {
  await act(locator, 30_000, (el) => session.type(el, text));
}

const exists = async (locator: Locator) => (await session.find(locator)) !== null;

/** On failure: a screenshot and the visible text next to the logs (fake data only). */
async function evidence(name: string, run: () => Promise<void>): Promise<void> {
  try {
    await run();
  } catch (error) {
    // A fresh sign-in code is a credential even when the Discord identity is synthetic.
    if (name === "part1") throw error;
    try {
      writeFileSync(join(work, `${name}.png`), Buffer.from(await session.screenshot(), "base64"));
      const text = await session.execute("return document.body.innerText;");
      writeFileSync(join(work, `${name}.txt`), `${await session.url()}\n\n${String(text)}`);
    } catch {
      // The session may be gone; the original error matters more.
    }
    throw error;
  }
}

async function scanAccessibility(): Promise<void> {
  const serious = (await nativeAxeViolations(session)).filter(
    (finding) => finding.impact === "serious" || finding.impact === "critical",
  );
  assert.deepEqual(serious, [], "The real WebView has no serious accessibility violations");
}

before(async () => {
  session = await Session.create(port, process.env.E2E_AUTOMATION_APP ?? app);
});

after(async () => {
  await session?.close();
});

test("part 1: add the server, sign in, choose a library (M1)", () => evidence("part1", part1));

test("part 2: install, play, stop, verify, uninstall (M2)", { skip: !parts.includes("2") }, () =>
  evidence("part2", part2),
);

test(
  "part 3: signed updater round trip and player-facing notes (M4)",
  {
    skip: !parts.includes("3"),
    timeout: 360_000,
  },
  part3,
);

async function part3() {
  const artifacts = env("E2E_UPDATE_ARTIFACTS");
  const latest = JSON.parse(readFileSync(join(artifacts, "latest.json"), "utf8")) as {
    version: string;
    platforms: Record<string, { url: string }>;
  };
  const changelog = JSON.parse(readFileSync(join(artifacts, "changelog-user.json"), "utf8")) as {
    releases: { version: string; entries: { text: string }[] }[];
  };
  const release = changelog.releases.find((item) => item.version === latest.version);
  assert.ok(release, "the generated player changelog contains this release");
  const download = latest.platforms["linux-x86_64"];
  assert.ok(download, "a real Linux updater artifact is required");
  const expected = join(
    artifacts,
    decodeURIComponent(new URL(download.url).pathname.split("/").at(-1) ?? ""),
  );
  const digest = async (file: string) => {
    const hash = createHash("sha256");
    for await (const block of createReadStream(file)) hash.update(block);
    return hash.digest("hex");
  };
  const before = await digest(app);
  const after = await digest(expected);
  assert.notEqual(before, after, "the lower-version artifact differs from the new build");
  await click(navLink("Settings"));
  await click({ xpath: '//a[normalize-space(.)="Updates"]' });
  await shown({ xpath: '//p[normalize-space(.)="vgames 0.0.1"]' });
  await click(button("Check for updates"));
  await click(button(`What's new in ${latest.version}`));
  await shown(heading(`What's new in vgames ${latest.version}`));
  await shown({ xpath: `//*[@role="dialog"]//h3[normalize-space(.)="vgames ${latest.version}"]` });
  await scanAccessibility();
  const visible = await session.execute(
    `return Array.from(document.querySelectorAll('section[aria-label="What\\'s new"] li'), e => e.textContent);`,
  );
  assert.ok(Array.isArray(visible));
  assert.deepEqual(
    [...visible].sort(),
    release.entries.map((entry) => entry.text).sort(),
    "What's new contains exactly the generated player-facing entries",
  );
  await click(button("Install and restart"));
  await until(
    "the signed new AppImage to replace the lower build",
    async () => {
      try {
        return (await digest(app)) === after;
      } catch (error) {
        // The plugin briefly renames the previous image before writing its replacement.
        if (error instanceof Error && "code" in error && error.code === "ENOENT") return false;
        throw error;
      }
    },
    300_000,
  );
  // WebKit ends the old automation session when its owning process exits.
  // Attach to the inherited inspector of the actual restarted process; never
  // supply an application path here, which would start a replacement ourselves.
  try {
    await session.close();
  } catch (error) {
    if (!(error instanceof WebDriverError && error.code === "invalid session id")) throw error;
  }
  const inspector = readFileSync(env("E2E_UPDATE_INSPECTOR_RECEIPT"), "utf8").trim();
  await until(
    "the relaunched application to expose its automation window",
    async () => {
      try {
        session = await Session.attach(port + 1, inspector);
        return true;
      } catch (error) {
        if (error instanceof WebDriverError) return false;
        throw error;
      }
    },
    60_000,
  );
  await shown(navLink("Settings"));
  await click(navLink("Settings"));
  await click({ xpath: '//a[normalize-space(.)="Updates"]' });
  await shown({ xpath: `//p[normalize-space(.)="vgames ${latest.version}"]` });
  await scanAccessibility();
}

async function part1() {
  await shown(field("Server address"));
  await scanAccessibility();
  // Add the server: the fingerprint shown is the one the server publishes.
  await type(field("Server address"), server);
  await click(button("Continue"));
  const fingerprint = await shown(css("[data-testid=fingerprint]"));
  await scanAccessibility();
  const shownPrint = (await session.text(fingerprint)).replace(/[\s-]/g, "");
  const info = (await (
    await fetch(`${server}/.well-known/vgames.json`, {
      signal: AbortSignal.timeout(30_000),
    })
  ).json()) as {
    root_key_fingerprint: string;
  };
  assert.ok(
    shownPrint.endsWith(info.root_key_fingerprint.replace(/-/g, "")),
    `the launcher shows ${shownPrint}, the server publishes ${info.root_key_fingerprint}`,
  );
  await click(button("It matches, continue"));

  // Sign in: the instance's browser (fixtures/xdg-open) completes the fake Discord page and
  // leaves the vgames:// link, which a player would paste when the browser cannot reach back.
  await shown(button("Sign in with Discord"));
  await scanAccessibility();
  await click(button("Sign in with Discord"));
  const link = join(home, "signin-link");
  await until("the browser to finish the sign-in", () => existsSync(link));
  if (await exists(button("Paste a code instead"))) await click(button("Paste a code instead"));
  await type(field("Sign-in code"), readFileSync(link, "utf8").trim());
  await scanAccessibility();
  await click(button("Sign in"));

  // Library folder: "Choose folder" opens the native folder picker, which WebDriver cannot
  // drive; the suite calls the command the picker's result goes to, then reloads.
  await shown(button("Choose folder"));
  await scanAccessibility();
  const added = await session.executeAsync(
    `const [path, done] = arguments;
     window.__TAURI_INTERNALS__.invoke("library_add", { path, makeDefault: true })
       .then(() => done("ok"), (e) => done("error: " + JSON.stringify(e)));`,
    [library],
  );
  assert.equal(added, "ok");
  await session.refresh();

  // An empty library and an empty catalog.
  await shown(heading("Your library is empty"));
  await scanAccessibility();
  await click(navLink("Browse"));
  await shown(heading("This server has no packages yet"));
  await scanAccessibility();
}

async function part2() {
  execFileSync(join(import.meta.dirname, "../fixtures/publish.sh"), [work, server], {
    stdio: "inherit",
  });

  // Browse → package page → install into the default library.
  await click(navLink("Library"));
  await click(navLink("Browse"));
  await click(css('a[data-package="e2e-game"]'), 60_000);
  await click(button(`Install ${title}`));
  await shown(css("[role=dialog]"));
  await scanAccessibility();
  await click({ xpath: `//*[@role="dialog"]//button[normalize-space(.)="Install"]` });

  // Progress on the Downloads page, then the finished install. The job is listed while it
  // runs (a small game can finish before the page opens).
  await click(navLink("Downloads"));
  await until(
    "the install to be listed",
    async () => (await exists(css("[data-job]"))) || (await exists(css("li[data-outcome]"))),
    60_000,
  );
  await shown(css('li[data-outcome="installed"]'), 300_000);
  await scanAccessibility();

  // Play: the game records its start in the instance's HOME; then stop it.
  await click(navLink("Library"));
  const runs = join(home, ".vgames-e2e-runs");
  await click(button(`Play ${title}`));
  await until("the game to start", () => existsSync(runs), 60_000);
  await click(button(`Stop ${title}`));
  await shown(button("Stop game"));
  await scanAccessibility();
  await click(button("Stop game"));
  await shown(button(`Play ${title}`), 60_000);

  // Verify files: a repair job that finds nothing to fix.
  await click(button(`More actions for ${title}`));
  await click(menuItem("Verify files"));
  await click(navLink("Downloads"));
  await shown(
    { xpath: `//li[@data-outcome="installed"][contains(., "Repaired version")]` },
    120_000,
  );

  // Uninstall. The confirmation's button reads "Uninstall" (the menu item "Uninstall…"); it
  // is enabled once the dialog has listed what will be removed.
  await click(navLink("Library"));
  await click(button(`More actions for ${title}`));
  await click(menuItem("Uninstall…"));
  await shown(button("Uninstall"));
  await scanAccessibility();
  await click(button("Uninstall"));
  await shown(heading("Your library is empty"), 60_000);
  await scanAccessibility();
}
