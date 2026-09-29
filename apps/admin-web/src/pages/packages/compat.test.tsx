import { screen, waitFor, within } from "@testing-library/react";
import type { ReactNode } from "react";
import { describe, expect, it } from "vitest";
import { packageId, SERVER_ID } from "../../mocks/db";
import { MOCK_PASSPHRASE, mockKeyFile } from "../../mocks/keyfile";
import { renderAt } from "../../test/render";
import { inProcessWorkers } from "../../test/workers";
import { UploadDepsContext } from "../../upload/context";
import { memoryStore } from "../../upload/store";
import { buildDocument, emptyForm, validate } from "./compatModel";

const HARBOR = packageId(1);

function wrapper({ children }: { children: ReactNode }) {
  return (
    <UploadDepsContext.Provider value={{ workers: inProcessWorkers(), store: memoryStore }}>
      {children}
    </UploadDepsContext.Provider>
  );
}

describe("compat model", () => {
  it("applies the vgames-core rules", () => {
    const base = emptyForm("linux");
    expect(validate(base, "linux")).toEqual({});
    expect(validate({ ...base, env: "LD_PRELOAD=/x.so" }, "linux").env).toMatch(
      /set by the launcher/,
    );
    expect(validate({ ...base, env: "STEAM_COMPAT_DATA_PATH=/x" }, "linux").env).toMatch(
      /set by the launcher/,
    );
    expect(validate({ ...base, env: "lower=1" }, "linux").env).toMatch(/isn't a valid name/);
    expect(validate({ ...base, env: "NOEQUALS" }, "linux").env).toMatch(/NAME=value/);
    expect(validate({ ...base, env: "A=1\nA=2" }, "linux").env).toMatch(/twice/);
    expect(validate({ ...base, dllOverrides: "d3d11=native,builtin" }, "linux")).toEqual({});
    expect(validate({ ...base, dllOverrides: "D3D11=native" }, "linux").dllOverrides).toMatch(
      /Line 1/,
    );
    expect(validate({ ...base, dllOverrides: "d3d11=wrong" }, "linux").dllOverrides).toMatch(
      /Line 1/,
    );
    expect(validate({ ...base, prefer: "umu-proton, UMU" }, "linux").prefer).toMatch(
      /isn't a runtime id/,
    );
    expect(validate({ ...base, prefer: "a, a" }, "linux").prefer).toMatch(/twice/);
    expect(validate({ ...base, umuGameId: "12345" }, "linux").umuGameId).toMatch(/umu-/);
    expect(validate({ ...base, notes: "x".repeat(2001) }, "linux").notes).toMatch(/2,000/);
    expect(validate({ ...base, minSequence: "0" }, "linux").minSequence).toBeTruthy();
    expect(
      validate({ ...base, minSequence: "5", maxSequence: "4" }, "linux").maxSequence,
    ).toBeTruthy();
  });

  it("builds the document with the runner for the target", () => {
    const doc = JSON.parse(
      new TextDecoder().decode(
        buildDocument(
          {
            ...emptyForm("macos"),
            status: "playable",
            env: "DXVK_HUD=1",
            winetricks: ["vcrun2022"],
          },
          {
            serverId: SERVER_ID,
            packageId: HARBOR,
            target: "macos",
            revision: 2,
            now: new Date("2026-09-28T10:00:00.123Z"),
          },
        ),
      ),
    );
    expect(doc).toEqual({
      format: "vgames.compat/1",
      server_id: SERVER_ID,
      package_id: HARBOR,
      target: "macos",
      revision: 2,
      created_at: "2026-09-28T10:00:00Z",
      applies_to: { platform: "windows-x86_64", min_sequence: 1, max_sequence: null },
      status: "playable",
      runner: {
        kind: "wine",
        prefer: [],
        graphics: ["d3dmetal", "dxmt", "dxvk", "wined3d"],
        env: { DXVK_HUD: "1" },
        winetricks: ["vcrun2022"],
      },
    });
  });
});

async function unlock(user: ReturnType<typeof renderAt>["user"], key = mockKeyFile()) {
  await user.upload(await screen.findByLabelText("Publisher key file"), new File([key], "k.vgkey"));
  await user.type(screen.getByLabelText("Passphrase"), MOCK_PASSPHRASE);
  await user.click(screen.getByRole("button", { name: "Unlock key" }));
  await screen.findByText(/Key ready/);
}

describe("compatibility tab", () => {
  it("signs and publishes a Linux profile, then shows it as the current revision", async () => {
    const { user, db } = renderAt(`/packages/${HARBOR}/compatibility`, "admin", { wrapper });
    const linux = await screen.findByRole("region", { name: "Linux (Proton)" });
    expect(within(linux).getByText(/No profile yet/)).toBeInTheDocument();
    const publish = within(linux).getByRole("button", { name: "Sign and publish revision 1" });
    expect(publish).toHaveAttribute("aria-disabled", "true");
    await unlock(user);
    await user.selectOptions(within(linux).getByLabelText("Status"), "verified");
    await user.type(within(linux).getByLabelText("Notes for players (plain text)"), "Works great.");
    await user.type(
      within(linux).getByLabelText("Environment (NAME=value per line)"),
      "PROTON_ENABLE_NVAPI=1",
    );
    await user.click(within(linux).getByRole("checkbox", { name: "vcrun2022" }));
    await user.click(publish);
    expect(await within(linux).findByText("Revision 1 is published.")).toBeInTheDocument();
    const stored = db.compat[HARBOR]?.[0];
    expect(stored?.signature.context).toBe("vgames/compat/v1");
    const doc = JSON.parse(atob(stored?.document ?? ""));
    expect(doc).toMatchObject({
      target: "linux",
      revision: 1,
      status: "verified",
      notes: "Works great.",
      runner: { kind: "proton", env: { PROTON_ENABLE_NVAPI: "1" }, winetricks: ["vcrun2022"] },
    });
    expect(stored?.signature.payload_blake3).toMatch(/^[0-9a-f]{64}$/);
    await waitFor(() =>
      expect(within(linux).getByText(/Current: revision 1, Verified/)).toBeInTheDocument(),
    );
    expect(
      within(linux).getByRole("button", { name: "Sign and publish revision 2" }),
    ).toBeInTheDocument();
  });

  it("shows invalid fields and signs nothing", async () => {
    const { user, db } = renderAt(`/packages/${HARBOR}/compatibility`, "admin", { wrapper });
    await unlock(user);
    const linux = screen.getByRole("region", { name: "Linux (Proton)" });
    await user.type(
      within(linux).getByLabelText("Environment (NAME=value per line)"),
      "LD_PRELOAD=/evil.so",
    );
    await user.click(within(linux).getByRole("button", { name: /Sign and publish/ }));
    expect(within(linux).getByText(/LD_PRELOAD is set by the launcher/)).toBeInTheDocument();
    expect(db.compat[HARBOR]).toBeUndefined();
  });

  it("explains an untrusted key and a newer revision published meanwhile", async () => {
    const { user, db } = renderAt(`/packages/${HARBOR}/compatibility`, "admin", { wrapper });
    await unlock(user, mockKeyFile({ key_id: "e".repeat(32) }));
    const mac = screen.getByRole("region", { name: "macOS (Wine)" });
    await user.click(within(mac).getByRole("button", { name: "Sign and publish revision 1" }));
    expect(await within(mac).findByRole("alert")).toHaveTextContent(
      /isn't in the server's trust bundle/,
    );

    db.trustedKeys["e".repeat(32)] = "01920000-0000-7000-8000-00000000a002";
    db.compat[HARBOR] = [
      {
        target: "macos",
        revision: 4,
        status: "playable",
        document: btoa("{}"),
        signature: {
          format: "vgames.sig/1",
          alg: "ed25519",
          context: "vgames/compat/v1",
          key_id: "e".repeat(32),
          payload_blake3: "0".repeat(64),
          signature: "AA==",
        },
        created_at: "2026-09-28T10:00:00Z",
      },
    ];
    await user.click(within(mac).getByRole("button", { name: "Sign and publish revision 1" }));
    expect(await within(mac).findByRole("alert")).toHaveTextContent(/newer revision meanwhile/);
  });
});
