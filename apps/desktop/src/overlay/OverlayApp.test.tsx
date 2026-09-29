import { mockIPC } from "@tauri-apps/api/mocks";
import { act, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { events, type OverlayAction, type OverlayView } from "../bindings";
import { OverlayApp } from "./OverlayApp";

const VIEW: OverlayView = {
  visible_panel: true,
  toasts: [
    {
      id: "01920000-0000-7000-8000-0000000000t1",
      kind: "invite",
      title: "Sam invites you to play",
      body: "Arena",
      expires_at: "2099-01-01T00:00:00Z",
    },
  ],
  friends_online: [
    { user_id: "u1", name: "Ana", status: "in_game", playing: "Racer" },
    { user_id: "u2", name: "Sam", status: "away", playing: null },
  ],
  invites: [
    { invite_id: "i1", from: "Sam", package_title: "Arena", state: "pending" },
    { invite_id: "i2", from: "Ana", package_title: "Racer", state: "installing" },
  ],
  recent_messages: [
    { conversation_id: "c1", from: "Sam", text: "<b>hi</b>", sent_at: "2026-09-29T10:00:00Z" },
    { conversation_id: "c1", from: "Sam", text: "join?", sent_at: "2026-09-29T10:01:00Z" },
  ],
};

function backend(view: OverlayView) {
  const actions: OverlayAction[] = [];
  mockIPC(
    (cmd, payload) => {
      if (cmd === "overlay_view") return view;
      if (cmd === "overlay_action") {
        actions.push((payload as { action: OverlayAction }).action);
        return null;
      }
      throw new Error(`unexpected command ${cmd}`);
    },
    { shouldMockEvents: true },
  );
  return actions;
}

describe("overlay window", () => {
  it("shows toasts, invites, the latest message per conversation and friends as plain text", async () => {
    backend(VIEW);
    render(<OverlayApp />);
    expect(await screen.findByText("Sam invites you to play")).toBeInTheDocument();
    expect(screen.getByRole("region", { name: "vgames overlay" })).toBeInTheDocument();
    expect(screen.getByText("Playing Racer")).toBeInTheDocument();
    expect(screen.getByText("Away")).toBeInTheDocument();
    expect(screen.getByText("Getting ready…")).toBeInTheDocument();
    // One row per conversation, the newest message; markup is text, never HTML.
    expect(screen.getByText(/join\?/)).toBeInTheDocument();
    expect(screen.queryByText(/<b>hi<\/b>/)).not.toBeInTheDocument();
    expect(document.querySelector("b")).toBeNull();
  });

  it("sends actions: accept, decline, quick reply, open launcher, close", async () => {
    const actions = backend(VIEW);
    const user = userEvent.setup();
    render(<OverlayApp />);
    await user.click(await screen.findByRole("button", { name: "Accept" }));
    await user.click(screen.getByRole("button", { name: "Decline" }));
    const reply = screen.getByRole("textbox", { name: "Reply to Sam" });
    expect(screen.getByRole("button", { name: "Send" })).toBeDisabled();
    await user.type(reply, "  on my way  ");
    await user.click(screen.getByRole("button", { name: "Send" }));
    expect(reply).toHaveValue("");
    await user.click(screen.getByRole("button", { name: "Open vgames" }));
    await user.keyboard("{Escape}");
    await waitFor(() => expect(actions).toHaveLength(5));
    expect(actions).toEqual([
      { kind: "accept_invite", invite_id: "i1" },
      { kind: "decline_invite", invite_id: "i1" },
      { kind: "quick_reply", conversation_id: "c1", text: "on my way" },
      { kind: "open_launcher" },
      { kind: "close_panel" },
    ]);
  });

  it("follows overlay-view events and hides the panel when closed", async () => {
    backend({ ...VIEW, visible_panel: false, toasts: [] });
    render(<OverlayApp />);
    await waitFor(() => expect(screen.queryByRole("region")).not.toBeInTheDocument());
    await act(async () => {
      await events.overlayView.emit(VIEW);
    });
    expect(await screen.findByRole("region", { name: "vgames overlay" })).toBeInTheDocument();
    expect(screen.getByText("Sam invites you to play")).toBeInTheDocument();
    await act(async () => {
      await events.overlayView.emit({ ...VIEW, visible_panel: false, toasts: [] });
    });
    await waitFor(() => expect(screen.queryByRole("region")).not.toBeInTheDocument());
  });
});
