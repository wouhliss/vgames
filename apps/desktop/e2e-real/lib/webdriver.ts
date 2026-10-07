// A minimal W3C WebDriver client: the protocol tauri-driver serves, over fetch, with no
// dependencies (INS-09). WebdriverIO was the first choice, but its dependency tree includes
// jszip ("MIT OR GPL-3.0-or-later"), which the repository's dependency review refuses, and
// this suite needs a dozen commands.

// The W3C web element identifier.
const ELEMENT = "element-6066-11e4-a52e-4f735466cecf";

/** A CSS selector or an XPath expression. */
export type Locator = { css: string } | { xpath: string };

export class WebDriverError extends Error {
  readonly code: string;

  constructor(code: string, message: string) {
    super(`${code}: ${message}`);
    this.code = code;
  }
}

type Reply = { value: unknown };

export class Session {
  private readonly base: string;
  readonly id: string;

  private constructor(base: string, id: string) {
    this.base = base;
    this.id = id;
  }

  /** Starts the application under tauri-driver on `port`. */
  static async create(port: number, application: string): Promise<Session> {
    const base = `http://127.0.0.1:${port}`;
    const value = (await call(base, "POST", "/session", {
      capabilities: { alwaysMatch: { "tauri:options": { application } } },
    })) as { sessionId: string };
    return new Session(`${base}/session/${value.sessionId}`, value.sessionId);
  }

  private command(method: string, path: string, body?: unknown): Promise<unknown> {
    return call(this.base, method, path, body);
  }

  /** The first matching element, or `null`. */
  async find(locator: Locator): Promise<string | null> {
    const [using, value] =
      "css" in locator ? ["css selector", locator.css] : ["xpath", locator.xpath];
    try {
      const found = (await this.command("POST", "/element", { using, value })) as Record<
        string,
        string
      >;
      return found[ELEMENT] ?? null;
    } catch (error) {
      if (error instanceof WebDriverError && error.code === "no such element") return null;
      throw error;
    }
  }

  async displayed(element: string): Promise<boolean> {
    return (await this.command("GET", `/element/${element}/displayed`)) === true;
  }

  async enabled(element: string): Promise<boolean> {
    return (await this.command("GET", `/element/${element}/enabled`)) === true;
  }

  async click(element: string): Promise<void> {
    await this.command("POST", `/element/${element}/click`, {});
  }

  /** Focuses the field (WebKitWebDriver refuses to type into an unfocused one), clears it
   * and types `text`. */
  async type(element: string, text: string): Promise<void> {
    await this.click(element);
    await this.command("POST", `/element/${element}/clear`, {});
    await this.command("POST", `/element/${element}/value`, { text });
  }

  async text(element: string): Promise<string> {
    return String(await this.command("GET", `/element/${element}/text`));
  }

  /** Runs `script` (a function body; `arguments` holds `args`) and returns its result. */
  execute(script: string, args: unknown[] = []): Promise<unknown> {
    return this.command("POST", "/execute/sync", { script, args });
  }

  /** Like `execute`, but the body awaits: its last argument is the callback. */
  executeAsync(script: string, args: unknown[] = []): Promise<unknown> {
    return this.command("POST", "/execute/async", { script, args });
  }

  async url(): Promise<string> {
    return String(await this.command("GET", "/url"));
  }

  async refresh(): Promise<void> {
    await this.command("POST", "/refresh", {});
  }

  /** A PNG, base64. */
  async screenshot(): Promise<string> {
    return String(await this.command("GET", "/screenshot"));
  }

  async close(): Promise<void> {
    await this.command("DELETE", "");
  }
}

async function call(base: string, method: string, path: string, body?: unknown): Promise<unknown> {
  const response = await fetch(`${base}${path}`, {
    method,
    headers: { "content-type": "application/json" },
    ...(body === undefined ? {} : { body: JSON.stringify(body) }),
  });
  const reply = (await response.json()) as Reply;
  const value = reply.value as { error?: string; message?: string } | null;
  if (!response.ok || (value && typeof value === "object" && "error" in value && value.error)) {
    throw new WebDriverError(value?.error ?? String(response.status), value?.message ?? "");
  }
  return reply.value;
}
