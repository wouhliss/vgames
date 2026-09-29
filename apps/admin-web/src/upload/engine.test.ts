import { HttpResponse, http } from "msw";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { createVersion } from "../api/versions";
import { createDb, type MockDb, OTHER_KEY_ID, packageId } from "../mocks/db";
import { createHandlers } from "../mocks/handlers";
import { MOCK_PASSPHRASE, mockKeyFile } from "../mocks/keyfile";
import { GCS } from "../mocks/versions";
import { server } from "../test/setup";
import { inProcessWorkers } from "../test/workers";
import { UploadEngine } from "./engine";
import { gcsFetchOptions } from "./gcs";
import type { PackChannel } from "./rpc";
import { memoryStore, type SavedUpload, type UploadStore } from "./store";
import type { PlannedFile } from "./types";

const MiB = 1024 * 1024;
const HARBOR = packageId(1);

function makeFile(path: string, size: number, fill: number, mtime = 1_700_000_000_000): File {
  return new File([new Uint8Array(size).fill(fill)], path.split("/").pop() ?? path, {
    lastModified: mtime,
  });
}

interface Folder {
  planned: PlannedFile[];
  blobs: File[];
}

function folder(): Folder {
  const specs: [string, number][] = [
    ["Game/big.bin", 9 * MiB + 17],
    ["Game/data/level.pak", 5 * MiB],
    ["Game/readme.txt", 1234],
    ["Game/empty.flag", 0],
  ];
  const blobs = specs.map(([path, size], i) => makeFile(path, size, i + 1));
  return {
    planned: specs.map(([path, size], i) => ({
      path,
      size,
      mtime_ms: blobs[i]?.lastModified ?? 0,
    })),
    blobs,
  };
}

let db: MockDb;
let store: UploadStore;

beforeEach(() => {
  gcsFetchOptions.redirect = "manual";
  db = createDb("admin");
  // biome-ignore lint/suspicious/noDocumentCookie: simulates the server-set CSRF cookie.
  document.cookie = `__Host-vgames_csrf=${db.csrf}; path=/; secure`;
  server.use(...createHandlers(db));
  store = memoryStore();
});

afterEach(() => {
  server.resetHandlers();
  gcsFetchOptions.redirect = "follow";
});

/** Creates a version, plans the folder (8 MiB packs), unlocks the key: what the wizard does. */
async function prepare(opts: { keyfile?: string; files?: Folder } = {}) {
  const workers = inProcessWorkers(8 * MiB);
  const packs = workers.pack();
  const keys = workers.key();
  const { data: version } = await createVersion(
    HARBOR,
    { platform: "linux-x86_64", version_label: "2.0" },
    "k".repeat(20),
  );
  const f = opts.files ?? folder();
  const planned = await packs.call({
    kind: "plan",
    files: f.planned,
    directories: [],
    blobs: f.blobs,
  });
  if (planned.kind !== "planned") throw new Error(JSON.stringify(planned));
  const unlocked = await keys.call({
    kind: "unlock",
    keyfile: new TextEncoder().encode(opts.keyfile ?? mockKeyFile()),
    passphrase: MOCK_PASSPHRASE,
  });
  if (unlocked.kind !== "unlocked") throw new Error(JSON.stringify(unlocked));
  const saved: SavedUpload = {
    versionId: version.id,
    packageId: HARBOR,
    serverId: version.server_id,
    platform: version.platform,
    label: version.version_label,
    sequence: version.sequence,
    createdAt: Math.floor(Date.parse(version.created_at) / 1000),
    files: planned.summary.files,
    directories: [],
    packs: planned.summary.packs,
    execution: undefined,
    sessions: {},
    confirmed: {},
    complete: [],
    savedAt: new Date().toISOString(),
  };
  await store.put(saved);
  const instant = async () => {};
  const engine = new UploadEngine(saved, {
    packs,
    keys,
    store,
    sleep: instant,
    waitForNetwork: instant,
  });
  return { engine, saved, packs, keys, version, workers };
}

function countPutBytes() {
  let bytes = 0;
  server.events.on("request:start", async ({ request }) => {
    if (request.method === "PUT" && request.url.startsWith(`${GCS}/session/`)) {
      const body = await request.clone().arrayBuffer();
      bytes += body.byteLength;
    }
  });
  return () => bytes;
}

describe("upload engine", () => {
  it("uploads every pack, signs, finalizes and follows verification to ready", async () => {
    const { engine, version, saved } = await prepare();
    expect(saved.packs.length).toBe(2);
    const phases: string[] = [];
    engine.subscribe(() => phases.push(engine.getSnapshot().phase.kind));
    await engine.start();
    const snap = engine.getSnapshot();
    expect(snap.phase.kind).toBe("verified");
    expect(snap.sent).toBe(snap.total);
    expect(snap.total).toBe(saved.packs.reduce((a, b) => a + b, 0));
    expect(phases).toEqual(
      expect.arrayContaining(["uploading", "signing", "finalizing", "verifying", "verified"]),
    );
    const stored = db.versions[HARBOR]?.find((v) => v.id === version.id);
    expect(stored?.state).toBe("ready");
    expect(Object.values(db.gcs).every((s) => s.complete)).toBe(true);
    // The saved resume state is gone once finalized.
    expect(await store.get(version.id)).toBeNull();
  });

  it("pauses and resumes without sending bytes twice", async () => {
    const { engine, saved } = await prepare();
    const sent = countPutBytes();
    const unsubscribe = engine.subscribe(() => {
      if (engine.getSnapshot().packs.some((p) => p.status === "done")) engine.pause();
    });
    await engine.start();
    unsubscribe();
    expect(engine.getSnapshot().phase.kind).toBe("paused");
    expect(engine.getSnapshot().packs.some((p) => p.status === "done")).toBe(true);
    await engine.start();
    expect(engine.getSnapshot().phase.kind).toBe("verified");
    expect(sent()).toBe(saved.packs.reduce((a, b) => a + b, 0));
  });

  it("waits for the network and resumes by itself", async () => {
    const { engine } = await prepare();
    let cuts = 3;
    server.use(
      http.put(`${GCS}/session/:vid/:pack`, () => {
        if (cuts > 0) {
          cuts -= 1;
          return HttpResponse.error();
        }
        return undefined;
      }),
    );
    const phases = new Set<string>();
    engine.subscribe(() => phases.add(engine.getSnapshot().phase.kind));
    await engine.start();
    expect(phases.has("waiting_network")).toBe(true);
    expect(engine.getSnapshot().phase.kind).toBe("verified");
  });

  it("asks for a new start URL when one expired", async () => {
    db.faults.expiredStarts = 2;
    const { engine } = await prepare();
    let starts = 0;
    server.events.on("request:start", ({ request }) => {
      if (request.url.includes("/upload-session")) starts += 1;
    });
    await engine.start();
    expect(engine.getSnapshot().phase.kind).toBe("verified");
    expect(starts).toBe(2 + 2);
  });

  it("retries storage errors (503) with backoff", async () => {
    db.faults.gcsErrors = 2;
    const { engine } = await prepare();
    await engine.start();
    expect(engine.getSnapshot().phase.kind).toBe("verified");
  });

  it("resumes after a reload from the saved sessions and offsets", async () => {
    const { engine, saved, packs, keys } = await prepare();
    engine.subscribe(() => {
      if (engine.getSnapshot().sent > 0) engine.pause();
    });
    await engine.start();
    const persisted = await store.get(saved.versionId);
    expect(persisted?.sessions[0]).toMatch(/^https:\/\/storage\.mock\.test\/session\//);
    const confirmed = Object.values(persisted?.confirmed ?? {}).reduce((a, b) => a + b, 0);
    expect(confirmed).toBeGreaterThan(0);

    // "Reload": a new engine from what was saved (same folder, key unlocked again).
    const sent = countPutBytes();
    if (!persisted) throw new Error("not saved");
    const instant = async () => {};
    const again = new UploadEngine(persisted, {
      packs,
      keys,
      store,
      sleep: instant,
      waitForNetwork: instant,
    });
    expect(again.getSnapshot().sent).toBe(confirmed);
    await again.start();
    expect(again.getSnapshot().phase.kind).toBe("verified");
    expect(sent()).toBe(saved.packs.reduce((a, b) => a + b, 0) - confirmed);
  });

  it("explains a key that isn't in the trust bundle (422)", async () => {
    const { engine } = await prepare({ keyfile: mockKeyFile({ key_id: "f".repeat(32) }) });
    await engine.start();
    const phase = engine.getSnapshot().phase;
    expect(phase).toMatchObject({ kind: "failed", title: "The server refused the version" });
    expect(phase.kind === "failed" && phase.text).toMatch(/isn't in the server's trust bundle/);
  });

  it("explains a key held by someone else (422)", async () => {
    db.trustedKeys[OTHER_KEY_ID] = "01920000-0000-7000-8000-00000000ffff";
    const { engine } = await prepare({ keyfile: mockKeyFile({ key_id: OTHER_KEY_ID }) });
    await engine.start();
    const phase = engine.getSnapshot().phase;
    expect(phase.kind === "failed" && phase.text).toMatch(/belongs to another admin/);
  });

  it("stops when a file changed after it was picked", async () => {
    const f = folder();
    const { engine } = await prepare({ files: f });
    // The file on disk now has another modification time than when it was planned.
    const changed = makeFile("Game/big.bin", 9 * MiB + 17, 9, 1_800_000_000_000);
    f.blobs[0] = changed;
    const packs = (engine as unknown as { deps: { packs: PackChannel } }).deps.packs;
    await packs.call({ kind: "plan", files: f.planned, directories: [], blobs: f.blobs });
    await engine.start();
    const phase = engine.getSnapshot().phase;
    expect(phase).toMatchObject({ kind: "failed", title: "A file changed" });
    expect(phase.kind === "failed" && phase.text).toMatch(
      /Game\/big\.bin changed since you picked the folder/,
    );
  });

  it("reports a failed verification", async () => {
    const { engine, version } = await prepare();
    db.verifyOutcome[version.id] = "fail";
    await engine.start();
    const phase = engine.getSnapshot().phase;
    expect(phase).toMatchObject({ kind: "failed", title: "Verification failed" });
    expect(phase.kind === "failed" && phase.text).toMatch(/chunk 17 hash mismatch/);
  });
});
