import { act, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { createMemoryRouter, RouterProvider } from "react-router";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { routes } from "../../app/router";
import { events, type Invite } from "../../ipc";
import {
  installMockBackend,
  MOCK_LIBRARY,
  MOCK_SERVER,
  type MockBackend,
  type MockState,
} from "../../mocks/backend";
import { makeCatalog } from "../../mocks/catalog";
import { makeInstalls } from "../../mocks/library";
import {
  ALEX,
  ALEX_CONVERSATION,
  BEA,
  GUS,
  HANA,
  makeInvite,
  VALID_FRIEND_CODE,
} from "../../mocks/social";
import { flush, renderWithProviders } from "../../test/render";

const CATALOG = makeCatalog(60);
// Installed: the first 3 catalog packages. Not installed: package 50.
const INSTALLS = makeInstalls(3, MOCK_SERVER, [MOCK_LIBRARY]);
const MISSING = CATALOG[50];
if (!MISSING) throw new Error("fixture");

function start(path: string, overrides: Partial<MockState> = {}) {
  const backend = installMockBackend({
    servers: [MOCK_SERVER],
    libraries: [MOCK_LIBRARY],
    installs: INSTALLS,
    packages: CATALOG,
    ...overrides,
  });
  const router = createMemoryRouter(routes, { initialEntries: [path] });
  renderWithProviders(<RouterProvider router={router} />);
  return { backend, router, user: userEvent.setup() };
}

async function emit<T>(event: { emit: (payload: T) => Promise<void> }, payload: T) {
  await act(async () => {
    await event.emit(payload);
    await flush();
  });
}

/** The core stores an invite, then tells the UI. */
async function receive(
  backend: MockBackend,
  invite: Invite,
  kind: "received" | "changed" = "received",
) {
  backend.state.invites = [invite, ...backend.state.invites.filter((i) => i.id !== invite.id)];
  await emit(kind === "received" ? events.inviteReceived : events.inviteChanged, invite);
}

const row = (name: RegExp | string) =>
  screen.getByText(name, { selector: "span" }).closest("li") as HTMLElement;

beforeEach(() => {
  vi.spyOn(HTMLElement.prototype, "offsetWidth", "get").mockReturnValue(1000);
  vi.spyOn(HTMLElement.prototype, "offsetHeight", "get").mockReturnValue(800);
});

describe("friends list", () => {
  it("groups friends by presence and names the game only when shared", async () => {
    start("/friends");
    await screen.findByRole("heading", { name: /^Playing · 2$/ });
    expect(screen.getByRole("heading", { name: /^Online · 2$/ })).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: /^Offline · 2$/ })).toBeInTheDocument();
    expect(row("Alex")).toHaveTextContent("Playing Hollow Harbor");
    expect(row("Eli")).toHaveTextContent("Playing a game");
    expect(row("chen")).toHaveTextContent("Away");
    expect(row("Fox")).toHaveTextContent("Offline");
  });

  it("follows presence changes from the core", async () => {
    start("/friends");
    await screen.findByText("Bea");
    await emit(events.presenceChanged, {
      user_id: BEA.id,
      presence: {
        status: "in_game",
        package_id: "x",
        package_title: "Night Canyon",
        updated_at: null,
      },
    });
    expect(row("Bea")).toHaveTextContent("Playing Night Canyon");
    expect(screen.getByRole("heading", { name: /^Playing · 3$/ })).toBeInTheDocument();
  });

  it("says when the friends list may be out of date", async () => {
    start("/friends", {
      socialConnection: { server_id: MOCK_SERVER.id, state: "reconnecting", retry_at: null },
    });
    expect(
      await screen.findByText("You're offline. Your friends list may be out of date."),
    ).toBeInTheDocument();
    await emit(events.socialConnectionChanged, {
      server_id: MOCK_SERVER.id,
      state: "connected",
      retry_at: null,
    });
    expect(screen.queryByText(/may be out of date/)).not.toBeInTheDocument();
  });

  it("shows the empty state with Add friend", async () => {
    const { user } = start("/friends", {
      friendList: { friends: [], incoming: [], outgoing: [] },
    });
    await screen.findByText("No friends yet");
    const [, add] = screen.getAllByRole("button", { name: "Add friend" });
    if (!add) throw new Error("no empty-state button");
    await user.click(add);
    expect(await screen.findByRole("dialog", { name: "Add a friend" })).toBeInTheDocument();
  });

  it("shows a load error with a retry", async () => {
    const { user, backend } = start("/friends", {
      socialErrors: { friends_list: { kind: "offline" } },
    });
    await screen.findByText("Couldn't load your friends");
    expect(screen.getByText(/You're offline/)).toBeInTheDocument();
    backend.state.socialErrors = {};
    await user.click(screen.getByRole("button", { name: "Try again" }));
    expect(await screen.findByText("Alex")).toBeInTheDocument();
  });

  it("removes a friend after confirmation", async () => {
    const { user, backend } = start("/friends");
    await screen.findByText("Bea");
    await user.click(within(row("Bea")).getByRole("button", { name: "More for Bea" }));
    await user.click(await screen.findByRole("menuitem", { name: "Remove friend" }));
    const dialog = await screen.findByRole("alertdialog", {
      name: "Remove Bea from your friends?",
    });
    await user.click(within(dialog).getByRole("button", { name: "Remove friend" }));
    await waitFor(() =>
      expect(screen.queryByText("Bea", { selector: "span" })).not.toBeInTheDocument(),
    );
    expect(backend.callsTo("friend_remove")[0]?.args).toEqual({ userId: BEA.id });
  });

  it("blocks a friend, who then shows under Blocked and can be unblocked", async () => {
    const { user, backend } = start("/friends");
    await screen.findByText("Bea");
    await user.click(within(row("Bea")).getByRole("button", { name: "More for Bea" }));
    await user.click(await screen.findByRole("menuitem", { name: "Block" }));
    const dialog = await screen.findByRole("alertdialog", { name: "Block Bea?" });
    await user.click(within(dialog).getByRole("button", { name: "Block" }));
    await waitFor(() => expect(backend.state.blocks).toHaveLength(1));
    await user.click(screen.getByRole("tab", { name: "Blocked" }));
    const unblock = await screen.findByRole("button", { name: "Unblock: bea" });
    await user.click(unblock);
    expect(await screen.findByText("Nobody is blocked")).toBeInTheDocument();
  });
});

describe("add friend", () => {
  it("creates a code with a countdown and copy", async () => {
    const { user } = start("/friends");
    await user.click(await screen.findByRole("button", { name: "Add friend" }));
    const dialog = await screen.findByRole("dialog", { name: "Add a friend" });
    await user.click(within(dialog).getByRole("button", { name: "Create a code" }));
    expect(await within(dialog).findByText("Q4TR 8WZN")).toBeInTheDocument();
    expect(within(dialog).getByText(/Expires in 1[45]:\d\d/)).toBeInTheDocument();
    expect(within(dialog).getByRole("button", { name: "Copy code" })).toBeInTheDocument();
  });

  it("says when the code has expired and offers a new one", async () => {
    const { user, backend } = start("/friends");
    backend.on("friend_code_create", () => ({
      code: "Q4TR8WZN",
      expires_at: new Date(Date.now() - 1000).toISOString(),
    }));
    await user.click(await screen.findByRole("button", { name: "Add friend" }));
    const dialog = await screen.findByRole("dialog", { name: "Add a friend" });
    await user.click(within(dialog).getByRole("button", { name: "Create a code" }));
    expect(await within(dialog).findByText("This code has expired.")).toBeInTheDocument();
    expect(within(dialog).getByRole("button", { name: "Create a new code" })).toBeInTheDocument();
  });

  it("normalizes a typed code and sends it once complete", async () => {
    const { user, backend } = start("/friends");
    await user.click(await screen.findByRole("button", { name: "Add friend" }));
    const field = await screen.findByRole("textbox", { name: "Friend code" });
    expect(field).toHaveFocus();
    await user.type(field, "7k2m-q9xd");
    await waitFor(() =>
      expect(backend.callsTo("friend_request_send")[0]?.args).toEqual({
        target: { kind: "code", code: VALID_FRIEND_CODE },
      }),
    );
    expect(await screen.findByText("Request sent to Ivo")).toBeInTheDocument();
    expect(screen.queryByRole("dialog", { name: "Add a friend" })).not.toBeInTheDocument();
  });

  it("explains a code that doesn't work, and rate limits", async () => {
    const { user, backend } = start("/friends");
    await user.click(await screen.findByRole("button", { name: "Add friend" }));
    const field = await screen.findByRole("textbox", { name: "Friend code" });
    await user.type(field, "AAAAAAAA");
    expect(await screen.findByText(/That code doesn't work/)).toBeInTheDocument();
    expect(field).toHaveAttribute("aria-invalid", "true");

    backend.state.socialErrors = {
      friend_request_send: { kind: "rate_limited", retry_after_seconds: 300 },
    };
    await user.clear(field);
    await user.type(field, "BBBBBBBB");
    expect(await screen.findByText("Too many tries. Try again in 5 minutes.")).toBeInTheDocument();
  });
});

describe("requests", () => {
  it("accepts, declines and cancels requests", async () => {
    const { user, backend } = start("/friends/requests", {
      friendList: {
        friends: [],
        incoming: [
          { user: GUS, state: "incoming", presence: null, since: null },
          { user: BEA, state: "incoming", presence: null, since: null },
        ],
        outgoing: [{ user: HANA, state: "outgoing", presence: null, since: null }],
      },
    });
    expect(await screen.findByRole("tab", { name: "Requests (3)", selected: true })).toBeVisible();
    await user.click(screen.getByRole("button", { name: "Accept: Gus" }));
    expect(await screen.findByText("You and Gus are now friends")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Decline: Bea" }));
    await user.click(screen.getByRole("button", { name: "Cancel request: Hana" }));
    expect(await screen.findByText("No requests")).toBeInTheDocument();
    expect(backend.state.friendList.friends.map((f) => f.user.id)).toEqual([GUS.id]);
  });
});

describe("messages", () => {
  it("opens a chat from a friend and shows the history as plain text", async () => {
    const { user, router } = start("/friends");
    await screen.findByText("Alex");
    await user.click(within(row("Alex")).getByRole("button", { name: "Message: Alex" }));
    await waitFor(() =>
      expect(router.state.location.pathname).toBe(`/friends/messages/${ALEX_CONVERSATION}`),
    );
    const log = await screen.findByRole("log", { name: "Messages with Alex" });
    expect(within(log).getByText("<b>not bold</b> and **not markdown**")).toBeInTheDocument();
    expect(log.querySelector("b, strong")).toBeNull();
    expect(
      within(log).getByText("Alex signed in on a new device: vgames on Linux."),
    ).toBeInTheDocument();
  });

  it("marks the conversation read when opened", async () => {
    const { backend } = start(`/friends/messages/${ALEX_CONVERSATION}`);
    await screen.findByRole("log", { name: "Messages with Alex" });
    await waitFor(() => expect(backend.callsTo("conversation_mark_read")).toHaveLength(1));
    await waitFor(() => expect(screen.getByRole("tab", { name: "Messages" })).toBeInTheDocument());
  });

  it("sends with Enter: pending, then sent; Shift+Enter adds a line", async () => {
    const { user, backend } = start(`/friends/messages/${ALEX_CONVERSATION}`);
    const box = await screen.findByRole("textbox", { name: "Message Alex" });
    await user.type(box, "See you{Shift>}{Enter}{/Shift}at 8");
    expect(box).toHaveValue("See you\nat 8");
    await user.keyboard("{Enter}");
    await waitFor(() =>
      expect(backend.callsTo("message_send")[0]?.args).toEqual({
        conversationId: ALEX_CONVERSATION,
        text: "See you\nat 8",
      }),
    );
    expect(box).toHaveValue("");
    const log = screen.getByRole("log", { name: "Messages with Alex" });
    const mine = within(log)
      .getByText("See you\nat 8", { normalizer: (s) => s })
      .closest("li");
    await waitFor(() => expect(mine).toHaveTextContent("Sent"));
  });

  it("offers Try again for a message that wasn't sent", async () => {
    const { user } = start(`/friends/messages/${ALEX_CONVERSATION}`, { messageSendFails: true });
    const box = await screen.findByRole("textbox", { name: "Message Alex" });
    await user.type(box, "Hello?{Enter}");
    const log = screen.getByRole("log", { name: "Messages with Alex" });
    const item = (await within(log).findByText("Hello?")).closest("li") as HTMLElement;
    await waitFor(() => expect(item).toHaveTextContent("Not sent"));
    await user.click(within(item).getByRole("button", { name: "Try again" }));
    await waitFor(() => expect(item).toHaveTextContent("Sent"));
  });

  it("refuses messages over 4,000 characters", async () => {
    const { user, backend } = start(`/friends/messages/${ALEX_CONVERSATION}`);
    const box = await screen.findByRole("textbox", { name: "Message Alex" });
    await user.click(box);
    await user.paste("x".repeat(4001));
    expect(await screen.findByText("Messages can be up to 4,000 characters.")).toBeInTheDocument();
    await user.keyboard("{Enter}");
    await user.click(screen.getByRole("button", { name: "Send" }));
    expect(backend.callsTo("message_send")).toHaveLength(0);
    await user.click(box);
    await user.keyboard("{Backspace}");
    expect(screen.queryByText("Messages can be up to 4,000 characters.")).not.toBeInTheDocument();
  });

  it("pauses sending after a key change until the new key is trusted", async () => {
    const start_ = start(`/friends/messages/${ALEX_CONVERSATION}`);
    const { user, backend } = start_;
    const alex = backend.state.security[ALEX.id];
    if (!alex) throw new Error("fixture");
    backend.state.security[ALEX.id] = {
      ...alex,
      devices: [
        ...alex.devices,
        {
          device_id: "alex-new",
          display_name: "vgames on macOS",
          first_seen_at: "2026-09-28T09:00:00Z",
          key_fingerprint: "ffff eeee dddd cccc",
          state: "key_changed",
        },
      ],
    };
    const box = await screen.findByRole("textbox", { name: "Message Alex" });
    await user.type(box, "Is this you?{Enter}");
    const alert = await screen.findByText(/Messages to Alex are paused/);
    expect(box).toHaveValue("Is this you?");
    await user.click(
      within(alert.closest("[role=alert]") as HTMLElement).getByRole("button", {
        name: "Review safety number",
      }),
    );
    const dialog = await screen.findByRole("dialog", { name: "Safety number with Alex" });
    await user.click(await within(dialog).findByRole("button", { name: "Trust new key" }));
    const confirm = await screen.findByRole("alertdialog", { name: "Trust Alex's new key?" });
    await user.click(within(confirm).getByRole("button", { name: "Trust new key" }));
    await waitFor(() =>
      expect(backend.state.security[ALEX.id]?.devices.every((d) => d.state === "trusted")).toBe(
        true,
      ),
    );
    await user.click(within(dialog).getByRole("button", { name: "Close" }));
    await user.click(screen.getByRole("button", { name: "Send" }));
    await waitFor(() => expect(backend.callsTo("message_send")).toHaveLength(2));
  });

  it("shows incoming messages and typing live", async () => {
    start(`/friends/messages/${ALEX_CONVERSATION}`);
    const log = await screen.findByRole("log", { name: "Messages with Alex" });
    await emit(events.typing, { conversation_id: ALEX_CONVERSATION, user_id: ALEX.id });
    expect(screen.getByText("Alex is typing…")).toBeInTheDocument();
    await emit(events.messageReceived, {
      id: "live-1",
      conversation_id: ALEX_CONVERSATION,
      sender_user_id: ALEX.id,
      mine: false,
      body: { kind: "text", text: "Ready when you are" },
      sent_at: "2026-09-28T10:00:00Z",
      received_at: "2026-09-28T10:00:00Z",
      status: "received",
    });
    expect(within(log).getByText("Ready when you are")).toBeInTheDocument();
    expect(screen.queryByText("Alex is typing…")).not.toBeInTheDocument();
  });

  it("says messages don't move to new devices when there are none", async () => {
    start("/friends/messages", { conversations: [], messages: {} });
    expect(await screen.findByText("No messages yet")).toBeInTheDocument();
    expect(screen.getByText(/They don't move to a new device/)).toBeInTheDocument();
  });
});

describe("safety number", () => {
  it("shows 12 groups and keeps the verified toggle", async () => {
    const { user, backend } = start(`/friends/messages/${ALEX_CONVERSATION}`);
    await user.click(await screen.findByRole("button", { name: "Safety number" }));
    const dialog = await screen.findByRole("dialog", { name: "Safety number with Alex" });
    const number = await within(dialog).findByTestId("safety-number");
    expect(number.querySelectorAll("span[aria-hidden]")).toHaveLength(12);
    const toggle = within(dialog).getByRole("switch", { name: "Mark as verified" });
    expect(toggle).toHaveAttribute("aria-checked", "false");
    await user.click(toggle);
    await waitFor(() => expect(toggle).toHaveAttribute("aria-checked", "true"));
    expect(backend.state.security[ALEX.id]?.verified).toBe(true);
    expect(within(dialog).getByText(/Messages don't move to new devices/)).toBeInTheDocument();
  });
});

describe("invites", () => {
  const incoming = (patch: Partial<Invite> = {}) =>
    makeInvite({
      package: { id: MISSING.id, title: MISSING.title, cover_url: null },
      ...patch,
    });

  it("shows an incoming invite card; Accept opens the install dialog at once", async () => {
    const { user, backend } = start("/library");
    await screen.findByRole("heading", { level: 1, name: "Library" });
    await receive(backend, incoming());
    const card = screen.getByRole("region", { name: "Alex invites you to play" });
    expect(card).toHaveTextContent(MISSING.title);
    expect(card).toHaveTextContent("Join us, we're one player short!");
    await user.click(within(card).getByRole("button", { name: "Accept" }));
    await waitFor(() => expect(backend.callsTo("invite_accept")).toHaveLength(1));
    expect(await screen.findByRole("dialog", { name: `Install ${MISSING.title}` })).toBeVisible();
    expect(
      screen.queryByRole("region", { name: "Alex invites you to play" }),
    ).not.toBeInTheDocument();
  });

  it("declines, or keeps the invite for later", async () => {
    const { user, backend } = start("/library");
    await screen.findByRole("heading", { level: 1, name: "Library" });
    await receive(backend, incoming());
    await receive(backend, incoming({ id: "second", from: BEA }));
    const alex = screen.getByRole("region", { name: "Alex invites you to play" });
    const bea = screen.getByRole("region", { name: "Bea invites you to play" });
    await user.click(within(alex).getByRole("button", { name: "Decline" }));
    await waitFor(() => expect(backend.callsTo("invite_decline")).toHaveLength(1));
    await user.click(within(bea).getByRole("button", { name: "Decide later" }));
    expect(screen.queryByRole("region", { name: /invites you to play/ })).not.toBeInTheDocument();
  });

  it("tells when an invite expired or was cancelled while its card was open", async () => {
    const { backend } = start("/library");
    await screen.findByRole("heading", { level: 1, name: "Library" });
    await receive(backend, incoming());
    const card = screen.getByRole("region", { name: "Alex invites you to play" });
    for (const state of ["expired", "cancelled"] as const) {
      await receive(backend, incoming({ state }), "changed");
      await waitFor(() => expect(card).toHaveTextContent("This invite isn't active anymore."));
      expect(within(card).queryByRole("button", { name: "Accept" })).not.toBeInTheDocument();
    }
  });

  it("shows the sender every state of an invite", async () => {
    const cases: [Partial<Invite>, string][] = [
      [{ state: "pending" }, "Waiting for Alex to answer"],
      [{ state: "accepted" }, "Alex accepted"],
      [{ state: "installing", progress: 0.42 }, "Alex is installing (42%)"],
      [{ state: "installing", progress: null }, "Alex is installing"],
      [{ state: "ready" }, "Alex is ready to play"],
      [{ state: "joined" }, "Alex joined"],
      [{ state: "declined" }, "Alex declined"],
      [{ state: "cancelled" }, "Cancelled"],
      [{ state: "expired" }, "Expired"],
      [
        { state: "failed", failure_reason: "no_build_for_platform" },
        "isn't available for Alex's computer",
      ],
      [
        { state: "failed", failure_reason: "install_failed" },
        "The install failed on Alex's computer.",
      ],
      [
        { state: "failed", failure_reason: "insufficient_space" },
        "Alex doesn't have enough disk space.",
      ],
      [{ state: "failed", failure_reason: "cancelled_by_user" }, "Alex cancelled the install."],
      [{ state: "failed", failure_reason: null }, "Didn't work out"],
    ];
    const invites = cases.map(([patch], i) =>
      makeInvite({ id: `out-${i}`, direction: "outgoing", from: BEA, to: ALEX, ...patch }),
    );
    start("/friends", { invites });
    const section = await screen.findByRole("region", { name: "Invites you sent" });
    const items = within(section).getAllByRole("listitem");
    expect(items).toHaveLength(cases.length);
    cases.forEach(([patch, text], i) => {
      const item = items[i] as HTMLElement;
      expect(item).toHaveTextContent(text);
      const cancellable = ["pending", "accepted", "installing"].includes(patch.state ?? "");
      expect(Boolean(within(item).queryByRole("button", { name: /Cancel invite/ }))).toBe(
        cancellable,
      );
    });
    expect(within(items[2] as HTMLElement).getByRole("progressbar")).toHaveAttribute(
      "aria-valuenow",
      "42",
    );
  });

  it("shows the invitee the state of invites they accepted", async () => {
    const cases: [Partial<Invite>, string][] = [
      [{ state: "accepted" }, "Accepted. Get ready to play."],
      [{ state: "installing", progress: 0.5 }, "Installing…"],
      [{ state: "ready" }, "Ready. Alex is sending the join details."],
      [
        { state: "failed", failure_reason: "insufficient_space" },
        "There wasn't enough disk space.",
      ],
      [{ state: "expired" }, "Expired"],
    ];
    start("/friends", {
      invites: cases.map(([patch], i) => incoming({ id: `in-${i}`, ...patch })),
    });
    const section = await screen.findByRole("region", { name: "Invites for you" });
    const items = within(section).getAllByRole("listitem");
    cases.forEach(([, text], i) => {
      expect(items[i]).toHaveTextContent(text);
    });
  });

  it("cancels an invite the sender sent", async () => {
    const { user, backend } = start("/friends", {
      invites: [makeInvite({ id: "out", direction: "outgoing", from: BEA, to: ALEX })],
    });
    await user.click(await screen.findByRole("button", { name: /Cancel invite/ }));
    await waitFor(() => expect(backend.callsTo("invite_cancel")).toHaveLength(1));
    expect(await screen.findByText("Invite cancelled")).toBeInTheDocument();
  });

  it("invites a friend to one of your games, checking the join info", async () => {
    const { user, backend } = start("/friends");
    await screen.findByText("Bea");
    await user.click(within(row("Bea")).getByRole("button", { name: "Invite to play: Bea" }));
    const dialog = await screen.findByRole("dialog", { name: "Invite to play: Bea" });
    await user.click(within(dialog).getByRole("combobox", { name: "Game" }));
    const title = INSTALLS[0]?.title ?? "";
    await user.click(await screen.findByRole("option", { name: title }));
    await user.type(within(dialog).getByRole("textbox", { name: /Message/ }), "Tonight?");
    const join = within(dialog).getByRole("textbox", { name: /Server address or lobby code/ });
    await user.type(join, "lobby 42");
    await user.click(within(dialog).getByRole("button", { name: "Send invite" }));
    expect(await within(dialog).findByText(/Use only letters, numbers/)).toBeInTheDocument();
    expect(backend.callsTo("invite_send")).toHaveLength(0);
    await user.clear(join);
    await user.type(join, "192.168.1.20:27015");
    await user.click(within(dialog).getByRole("button", { name: "Send invite" }));
    await waitFor(() =>
      expect(backend.callsTo("invite_send")[0]?.args).toEqual({
        toUserId: BEA.id,
        packageId: INSTALLS[0]?.package.package_id,
        message: "Tonight?",
        joinSecret: "192.168.1.20:27015",
      }),
    );
    expect(await screen.findByText("Invite sent to Bea")).toBeInTheDocument();
  });

  it("won't send a message over 200 characters", async () => {
    const { user, backend } = start("/friends");
    await screen.findByText("Bea");
    await user.click(within(row("Bea")).getByRole("button", { name: "Invite to play: Bea" }));
    const dialog = await screen.findByRole("dialog", { name: "Invite to play: Bea" });
    await user.click(within(dialog).getByRole("combobox", { name: "Game" }));
    await user.click(await screen.findByRole("option", { name: INSTALLS[0]?.title ?? "" }));
    await user.click(within(dialog).getByRole("textbox", { name: /Message/ }));
    await user.paste("y".repeat(201));
    const send = within(dialog).getByRole("button", { name: "Send invite" });
    expect(send).toHaveAttribute("aria-disabled", "true");
    await user.click(send);
    expect(backend.callsTo("invite_send")).toHaveLength(0);
  });

  it("invites a friend from a package page", async () => {
    const { user, backend } = start(`/package/${MISSING.id}`);
    await user.click(await screen.findByRole("button", { name: "Invite to play" }));
    const dialog = await screen.findByRole("dialog", { name: `Invite to play: ${MISSING.title}` });
    await user.click(within(dialog).getByRole("combobox", { name: "Friend" }));
    await user.click(await screen.findByRole("option", { name: "Alex" }));
    await user.click(within(dialog).getByRole("button", { name: "Send invite" }));
    await waitFor(() =>
      expect(backend.callsTo("invite_send")[0]?.args).toMatchObject({
        toUserId: ALEX.id,
        packageId: MISSING.id,
        message: null,
        joinSecret: null,
      }),
    );
  });
});
