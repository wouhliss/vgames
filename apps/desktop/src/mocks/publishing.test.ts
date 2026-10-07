// The publishing fixture behaves like the contract says, so the screen tests built on it mean something.
import { describe, expect, it } from "vitest";
import { commands, type PublishStart } from "../ipc";
import { installMockBackend, MOCK_ACCOUNT, MOCK_SERVER } from "./backend";
import { PUBLISH_FOLDERS, PUBLISH_KEYS } from "./publishing";

const ADMIN_SERVER = { ...MOCK_SERVER, account: { ...MOCK_ACCOUNT, role: "admin" as const } };

function start(patch: Partial<PublishStart> = {}): PublishStart {
  return {
    package_id: "0199d000-0000-7000-8000-000000000001",
    platform: "windows-x86_64",
    version_label: "1.4.0",
    folder: PUBLISH_FOLDERS.ok,
    launch: { executable: "Starfall.exe", args: [], working_dir: null },
    key_path: PUBLISH_KEYS.ok,
    passphrase: "correct horse",
    ...patch,
  };
}

describe("publishing mock", () => {
  it("refuses accounts that are not admins", async () => {
    installMockBackend({ servers: [MOCK_SERVER] });
    expect(await commands.publishPackages(MOCK_SERVER.id, null, null)).toEqual({
      status: "error",
      error: { kind: "forbidden" },
    });
  });

  it("uploads every pack, verifies, stops at ready, then releases", async () => {
    const backend = installMockBackend({ servers: [ADMIN_SERVER] });
    const result = await commands.publishStart(ADMIN_SERVER.id, start());
    if (result.status !== "ok") throw new Error("start failed");
    const job = result.data;
    expect(job.phase).toBe("ready");
    expect(job.bytes_confirmed).toBe(job.bytes_total);
    expect(job.packs.length).toBeGreaterThan(1);
    expect(job.packs.every((p) => p.state === "done")).toBe(true);

    const released = await commands.publishRelease(ADMIN_SERVER.id, job.version_id ?? "");
    expect(released.status === "ok" && released.data.is_current_release).toBe(true);
    expect(backend.state.publishJobs[0]?.phase).toBe("published");
  });

  it("gives each failure its typed error", async () => {
    installMockBackend({ servers: [ADMIN_SERVER] });
    expect(await commands.publishStart(ADMIN_SERVER.id, start({ passphrase: "wrong" }))).toEqual({
      status: "error",
      error: { kind: "wrong_passphrase" },
    });
    expect(
      await commands.publishStart(ADMIN_SERVER.id, start({ key_path: PUBLISH_KEYS.untrusted })),
    ).toEqual({ status: "error", error: { kind: "untrusted_key", reason: "unknown" } });
    expect(
      await commands.publishStart(ADMIN_SERVER.id, start({ folder: PUBLISH_FOLDERS.invalid })),
    ).toEqual({ status: "error", error: { kind: "invalid_paths", count: 3 } });

    const failed = await commands.publishStart(
      ADMIN_SERVER.id,
      start({ folder: PUBLISH_FOLDERS.failsVerification }),
    );
    expect(failed.status === "ok" && failed.data.phase).toBe("failed");
    expect(failed.status === "ok" && failed.data.error?.kind).toBe("verification_failed");
  });

  it("cancels halfway and resumes with the key, keeping uploaded bytes", async () => {
    const backend = installMockBackend({ servers: [ADMIN_SERVER], publishHold: true });
    const started = await commands.publishStart(ADMIN_SERVER.id, start());
    if (started.status !== "ok") throw new Error("start failed");
    const half = started.data.bytes_confirmed;
    expect(started.data.phase).toBe("uploading");
    expect(half).toBeGreaterThan(0);

    const cancelled = await commands.publishCancel(started.data.id);
    expect(cancelled.status === "ok" && cancelled.data.phase).toBe("cancelled");
    expect(cancelled.status === "ok" && cancelled.data.resume_needs_key).toBe(true);

    backend.state.publishHold = false;
    expect(await commands.publishResume(started.data.id, null)).toEqual({
      status: "error",
      error: { kind: "wrong_passphrase" },
    });
    const resumed = await commands.publishResume(started.data.id, {
      key_path: PUBLISH_KEYS.ok,
      passphrase: "correct horse",
    });
    expect(resumed.status === "ok" && resumed.data.phase).toBe("ready");
  });
});
