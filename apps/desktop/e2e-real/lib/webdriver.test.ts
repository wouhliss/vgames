import assert from "node:assert/strict";
import { createServer } from "node:http";
import { test } from "node:test";
import { Session, WebDriverError } from "./webdriver.ts";

test("restart attachment only connects to the existing local inspector", async () => {
  const requests: unknown[] = [];
  const server = createServer(async (request, response) => {
    let body = "";
    for await (const chunk of request) body += String(chunk);
    requests.push(JSON.parse(body));
    response.setHeader("content-type", "application/json");
    response.end(JSON.stringify({ value: { sessionId: "restarted" } }));
  });
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  try {
    const address = server.address();
    assert.ok(address && typeof address === "object");
    const session = await Session.attach(address.port, "127.0.0.1:43210");
    assert.equal(session.id, "restarted");
    assert.deepEqual(requests, [
      {
        capabilities: {
          alwaysMatch: {
            "webkitgtk:browserOptions": { targetAddress: "127.0.0.1:43210" },
          },
        },
      },
    ]);
    for (const bad of [
      "example.test:43210",
      "127.0.0.1:65536",
      "127.0.0.1:0",
      "127.0.0.1:1/path",
    ]) {
      await assert.rejects(Session.attach(address.port, bad), /Invalid local/);
    }
    assert.equal(requests.length, 1, "invalid targets never reach the driver");
  } finally {
    await new Promise<void>((resolve, reject) =>
      server.close((error) => (error ? reject(error) : resolve())),
    );
  }
});

test("an unavailable restarted process fails without launching a replacement", async () => {
  let requests = 0;
  const server = createServer((request, response) => {
    requests++;
    request.resume();
    response.writeHead(500, { "content-type": "application/json" });
    response.end(
      JSON.stringify({ value: { error: "session not created", message: "No existing browser" } }),
    );
  });
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  try {
    const address = server.address();
    assert.ok(address && typeof address === "object");
    await assert.rejects(
      Session.attach(address.port, "127.0.0.1:43210"),
      (error: unknown) => error instanceof WebDriverError && error.code === "session not created",
    );
    assert.equal(requests, 1, "no application-launch fallback");
  } finally {
    await new Promise<void>((resolve, reject) =>
      server.close((error) => (error ? reject(error) : resolve())),
    );
  }
});
