// Social fixtures (05-social-notes §5–6): friends and presence, blocks, friend codes, conversations
// with end-to-end encrypted messages (the mock just stores text), contact security and invites.
import type {
  BlockedUser,
  ContactSecurity,
  Conversation,
  Friend,
  FriendList,
  FriendTarget,
  InstalledPackage,
  Invite,
  Message,
  MyDevice,
  Presence,
  SocialConnection,
  SocialError,
  UserSummary,
} from "../ipc";
import { events } from "../ipc";
import { fail, type Handler } from "./runtime";

export const ME: UserSummary = {
  id: "01920000-0000-7000-8000-00000000a001",
  username: "sam",
  display_name: "Sam",
  avatar_url: null,
};

export function mockUser(n: number, username: string, display: string | null): UserSummary {
  return {
    id: `01920000-0000-7000-8000-0000000u${String(n).padStart(4, "0")}`,
    username,
    display_name: display,
    avatar_url: null,
  };
}

export const ALEX = mockUser(1, "alex", "Alex");
export const BEA = mockUser(2, "bea", "Bea");
export const CHEN = mockUser(3, "chen", null);
export const DANA = mockUser(4, "dana", "Dana");
export const GUS = mockUser(7, "gus", "Gus");
export const HANA = mockUser(8, "hana", "Hana");
/** The user a valid friend code belongs to. */
export const IVO = mockUser(9, "ivo", "Ivo");

export const VALID_FRIEND_CODE = "7K2MQ9XD";

const presence = (
  status: Presence["status"],
  pkg: { id: string; title: string } | null = null,
): Presence => ({
  status,
  package_id: pkg?.id ?? null,
  package_title: pkg?.title ?? null,
  updated_at: "2026-09-28T09:00:00Z",
});

const friend = (user: UserSummary, p: Presence | null, state: Friend["state"] = "accepted") => ({
  user,
  state,
  presence: p,
  since: state === "accepted" ? "2026-08-01T10:00:00Z" : null,
});

export const MOCK_GAME = { id: "01920000-0000-7000-8000-00000000c001", title: "Hollow Harbor" };

export function makeFriends(): FriendList {
  return {
    friends: [
      friend(ALEX, presence("in_game", MOCK_GAME)),
      friend(BEA, presence("online")),
      friend(CHEN, presence("away")),
      friend(DANA, presence("offline")),
      // Playing, but hides what (show_current_game off).
      friend(mockUser(5, "eli", "Eli"), presence("in_game")),
      friend(mockUser(6, "fox", "Fox"), null),
    ],
    incoming: [friend(GUS, null, "incoming")],
    outgoing: [friend(HANA, null, "outgoing")],
  };
}

function makeSecurity(user: UserSummary, seed: number): ContactSecurity {
  const groups = Array.from({ length: 12 }, (_, i) =>
    String((seed * 7919 + i * 104729) % 100000).padStart(5, "0"),
  );
  return {
    user_id: user.id,
    verified: false,
    needs_reverification: false,
    safety_number: groups.join(""),
    safety_number_groups: groups,
    devices: [
      {
        device_id: `${user.username}-desktop`,
        display_name: "vgames on Windows",
        first_seen_at: "2026-08-01T10:00:00Z",
        key_fingerprint: "a1b2 c3d4 e5f6 0718 293a 4b5c 6d7e 8f90",
        state: "trusted",
      },
    ],
  };
}

function textMessage(
  id: string,
  conversation: string,
  from: UserSummary,
  text: string,
  minutesAgo: number,
): Message {
  const at = new Date(Date.parse("2026-09-28T10:00:00Z") - minutesAgo * 60_000).toISOString();
  const mine = from.id === ME.id;
  return {
    id,
    conversation_id: conversation,
    sender_user_id: from.id,
    mine,
    body: { kind: "text", text },
    sent_at: at,
    received_at: mine ? null : at,
    status: mine ? "sent" : "received",
  };
}

export const ALEX_CONVERSATION = "01920000-0000-7000-8000-00000000d001";

function makeConversations(): {
  conversations: Conversation[];
  messages: Record<string, Message[]>;
} {
  const alex: Message[] = [
    textMessage("m-1", ALEX_CONVERSATION, ALEX, "Up for a round tonight?", 90),
    textMessage("m-2", ALEX_CONVERSATION, ME, "Sure, after dinner.", 88),
    {
      id: "m-3",
      conversation_id: ALEX_CONVERSATION,
      sender_user_id: ALEX.id,
      mine: false,
      body: {
        kind: "notice",
        notice: {
          kind: "new_device",
          user_id: ALEX.id,
          device_id: "alex-laptop",
          device_name: "vgames on Linux",
        },
      },
      sent_at: "2026-09-28T09:00:00Z",
      received_at: "2026-09-28T09:00:00Z",
      status: "received",
    },
    textMessage("m-4", ALEX_CONVERSATION, ALEX, "<b>not bold</b> and **not markdown**", 30),
    textMessage("m-5", ALEX_CONVERSATION, ALEX, "Installing Hollow Harbor now", 5),
  ];
  const bea = "01920000-0000-7000-8000-00000000d002";
  const beaMessages = [textMessage("b-1", bea, ME, "Thanks for the invite!", 600)];
  return {
    conversations: [
      {
        id: ALEX_CONVERSATION,
        kind: "direct",
        members: [ME, ALEX],
        last_message: alex[alex.length - 1] ?? null,
        unread: 2,
        created_at: "2026-08-01T10:00:00Z",
      },
      {
        id: bea,
        kind: "direct",
        members: [ME, BEA],
        last_message: beaMessages[0] ?? null,
        unread: 0,
        created_at: "2026-08-02T10:00:00Z",
      },
    ],
    messages: { [ALEX_CONVERSATION]: alex, [bea]: beaMessages },
  };
}

export function makeInvite(overrides: Partial<Invite> = {}): Invite {
  return {
    id: "01920000-0000-7000-8000-00000000e001",
    direction: "incoming",
    from: ALEX,
    to: ME,
    package: { id: MOCK_GAME.id, title: MOCK_GAME.title, cover_url: null },
    state: "pending",
    progress: null,
    message: "Join us, we're one player short!",
    failure_reason: null,
    created_at: "2026-09-28T09:55:00Z",
    updated_at: "2026-09-28T09:55:00Z",
    expires_at: "2026-09-28T10:25:00Z",
    has_join_secret: false,
    ...overrides,
  };
}

export interface SocialState {
  socialConnection: SocialConnection;
  friendList: FriendList;
  blocks: BlockedUser[];
  conversations: Conversation[];
  messages: Record<string, Message[]>;
  security: Record<string, ContactSecurity>;
  myDevices: MyDevice[];
  invites: Invite[];
  /** Forced errors by command name. */
  socialErrors: Record<string, SocialError>;
  /** Outgoing messages fail to send (then succeed on retry). */
  messageSendFails: boolean;
  /** How long sending, typing and invite progress take (ms). 0 = immediately (unit tests). */
  socialDelayMs: number;
}

export function defaultSocialState(): SocialState {
  const { conversations, messages } = makeConversations();
  const friends = makeFriends();
  const security: Record<string, ContactSecurity> = {};
  [...friends.friends, ...friends.incoming, ...friends.outgoing].forEach((f, i) => {
    security[f.user.id] = makeSecurity(f.user, i + 1);
  });
  return {
    socialConnection: {
      server_id: "01920000-0000-7000-8000-000000000001",
      state: "connected",
      retry_at: null,
    },
    friendList: friends,
    blocks: [],
    conversations,
    messages,
    security,
    myDevices: [
      {
        id: "me-desktop",
        display_name: "vgames on Linux",
        platform: "linux",
        current: true,
        created_at: "2026-08-01T10:00:00Z",
        last_seen_at: "2026-09-28T10:00:00Z",
      },
      {
        id: "me-laptop",
        display_name: "vgames on Windows",
        platform: "windows",
        current: false,
        created_at: "2026-09-01T10:00:00Z",
        last_seen_at: "2026-09-20T10:00:00Z",
      },
    ],
    invites: [],
    socialErrors: {},
    messageSendFails: false,
    socialDelayMs: 0,
  };
}

const everyone = (list: FriendList) => [...list.friends, ...list.incoming, ...list.outgoing];

export function socialHandlers(
  state: SocialState & { installs: InstalledPackage[] },
): Record<string, Handler> {
  const emitFriends = () => void events.friendsChanged.emit(state.friendList);
  const emitConversations = () => void events.conversationsChanged.emit(state.conversations);
  const userById = (id: string): UserSummary | undefined =>
    everyone(state.friendList).find((f) => f.user.id === id)?.user ??
    (id === IVO.id ? IVO : undefined);
  const conversation = (id: string) => {
    const found = state.conversations.find((c) => c.id === id);
    if (!found) fail({ kind: "not_found" } satisfies SocialError);
    return found;
  };
  const setInvite = (invite: Invite) => {
    state.invites = state.invites.map((i) => (i.id === invite.id ? invite : i));
    void events.inviteChanged.emit(invite);
    return invite;
  };
  const invite = (id: string) => {
    const found = state.invites.find((i) => i.id === id);
    if (!found) fail({ kind: "not_found" } satisfies SocialError);
    return found;
  };
  // Always after the command returned, as in the core (the status event follows the reply).
  const deliver = (message: Message) => {
    setTimeout(() => {
      const list = state.messages[message.conversation_id] ?? [];
      const status = state.messageSendFails ? "failed" : "sent";
      state.messages[message.conversation_id] = list.map((m) =>
        m.id === message.id ? { ...m, status } : m,
      );
      void events.messageStatusChanged.emit({
        message_id: message.id,
        conversation_id: message.conversation_id,
        status,
      });
    }, state.socialDelayMs);
  };
  let seq = 0;

  const handlers: Record<string, Handler> = {
    social_connection: () => state.socialConnection,
    friends_list: () => state.friendList,
    friend_code_create: () => ({
      code: "Q4TR8WZN",
      expires_at: new Date(Date.now() + 15 * 60_000).toISOString(),
    }),
    friend_request_send: (args) => {
      const target = args.target as FriendTarget;
      let user: UserSummary | undefined;
      if (target.kind === "code") {
        if (!/^[0-9A-HJKMNP-TV-Z]{8}$/.test(target.code))
          fail({ kind: "invalid_input", field: "code", message: "not a friend code" });
        if (target.code !== VALID_FRIEND_CODE) fail({ kind: "code_invalid" });
        user = IVO;
      } else {
        user = userById(target.user_id);
        if (!user) fail({ kind: "not_found" });
      }
      const incoming = state.friendList.incoming.find((f) => f.user.id === user.id);
      if (incoming) {
        const accepted = {
          ...incoming,
          state: "accepted" as const,
          since: new Date().toISOString(),
        };
        state.friendList = {
          ...state.friendList,
          incoming: state.friendList.incoming.filter((f) => f !== incoming),
          friends: [...state.friendList.friends, accepted],
        };
        emitFriends();
        return accepted;
      }
      if (everyone(state.friendList).some((f) => f.user.id === user.id))
        fail({ kind: "conflict", code: "already_friends", message: "Already friends." });
      const outgoing: Friend = { user, state: "outgoing", presence: null, since: null };
      state.friendList = {
        ...state.friendList,
        outgoing: [...state.friendList.outgoing, outgoing],
      };
      emitFriends();
      return outgoing;
    },
    friend_accept: (args) => {
      const incoming = state.friendList.incoming.find((f) => f.user.id === args.userId);
      if (!incoming) fail({ kind: "not_found" });
      const accepted: Friend = {
        ...incoming,
        state: "accepted",
        presence: presence("online"),
        since: new Date().toISOString(),
      };
      state.friendList = {
        ...state.friendList,
        incoming: state.friendList.incoming.filter((f) => f !== incoming),
        friends: [...state.friendList.friends, accepted],
      };
      emitFriends();
      return accepted;
    },
    friend_decline: (args) => {
      state.friendList = {
        ...state.friendList,
        incoming: state.friendList.incoming.filter((f) => f.user.id !== args.userId),
      };
      emitFriends();
      return null;
    },
    friend_remove: (args) => {
      state.friendList = {
        friends: state.friendList.friends.filter((f) => f.user.id !== args.userId),
        incoming: state.friendList.incoming,
        outgoing: state.friendList.outgoing.filter((f) => f.user.id !== args.userId),
      };
      emitFriends();
      return null;
    },
    user_block: (args) => {
      const user = userById(String(args.userId));
      state.friendList = {
        friends: state.friendList.friends.filter((f) => f.user.id !== args.userId),
        incoming: state.friendList.incoming.filter((f) => f.user.id !== args.userId),
        outgoing: state.friendList.outgoing.filter((f) => f.user.id !== args.userId),
      };
      state.blocks = [
        ...state.blocks,
        {
          user_id: String(args.userId),
          username: user?.username ?? null,
          blocked_at: new Date().toISOString(),
        },
      ];
      emitFriends();
      return null;
    },
    user_unblock: (args) => {
      state.blocks = state.blocks.filter((b) => b.user_id !== args.userId);
      return null;
    },
    blocks_list: () => state.blocks,
    user_profile: (args) => {
      const user = userById(String(args.userId));
      if (!user) fail({ kind: "not_found" });
      return user;
    },

    conversations_list: () => state.conversations,
    conversation_open_direct: (args) => {
      const existing = state.conversations.find(
        (c) => c.kind === "direct" && c.members.some((m) => m.id === args.userId),
      );
      if (existing) return existing;
      const user = userById(String(args.userId));
      if (!user) fail({ kind: "not_found" });
      seq += 1;
      const created: Conversation = {
        id: `01920000-0000-7000-8000-0000000d${String(seq).padStart(4, "0")}`,
        kind: "direct",
        members: [ME, user],
        last_message: null,
        unread: 0,
        created_at: new Date().toISOString(),
      };
      state.conversations = [created, ...state.conversations];
      state.messages[created.id] = [];
      emitConversations();
      return created;
    },
    conversation_create_party: (args) => {
      const ids = args.userIds as string[];
      if (ids.length < 1 || ids.length > 15)
        fail({ kind: "invalid_input", field: "user_ids", message: "1–15 friends" });
      seq += 1;
      const created: Conversation = {
        id: `01920000-0000-7000-8000-0000000p${String(seq).padStart(4, "0")}`,
        kind: "party",
        members: [ME, ...ids.map((id) => userById(id) ?? fail({ kind: "not_found" }))],
        last_message: null,
        unread: 0,
        created_at: new Date().toISOString(),
      };
      state.conversations = [created, ...state.conversations];
      state.messages[created.id] = [];
      emitConversations();
      return created;
    },
    messages_list: (args) => {
      const all = state.messages[conversation(String(args.conversationId)).id] ?? [];
      const limit = Number(args.limit);
      const before = args.before as string | null;
      const end = before ? all.findIndex((m) => m.id === before) : all.length;
      const stop = end < 0 ? all.length : end;
      return all.slice(Math.max(0, stop - limit), stop);
    },
    message_send: (args) => {
      const conv = conversation(String(args.conversationId));
      const text = String(args.text).trimEnd();
      if (text.length === 0 || [...text].length > 4000)
        fail({ kind: "invalid_input", field: "text", message: "1–4,000 characters" });
      for (const member of conv.members) {
        const changed = state.security[member.id]?.devices.filter((d) => d.state === "key_changed");
        if (changed && changed.length > 0)
          fail({
            kind: "key_changed",
            user_id: member.id,
            device_ids: changed.map((d) => d.device_id),
          });
      }
      seq += 1;
      const message: Message = {
        id: `sent-${seq}`,
        conversation_id: conv.id,
        sender_user_id: ME.id,
        mine: true,
        body: { kind: "text", text },
        sent_at: new Date().toISOString(),
        received_at: null,
        status: "pending",
      };
      state.messages[conv.id] = [...(state.messages[conv.id] ?? []), message];
      state.conversations = [
        { ...conv, last_message: message },
        ...state.conversations.filter((c) => c !== conv),
      ];
      emitConversations();
      deliver(message);
      return message;
    },
    message_retry: (args) => {
      for (const [id, list] of Object.entries(state.messages)) {
        const found = list.find((m) => m.id === args.messageId);
        if (!found) continue;
        const pending = { ...found, status: "pending" as const };
        state.messages[id] = list.map((m) => (m === found ? pending : m));
        state.messageSendFails = false;
        deliver(pending);
        return pending;
      }
      return fail({ kind: "not_found" });
    },
    conversation_mark_read: (args) => {
      const conv = conversation(String(args.conversationId));
      if (conv.unread > 0) {
        state.conversations = state.conversations.map((c) =>
          c === conv ? { ...c, unread: 0 } : c,
        );
        emitConversations();
      }
      return null;
    },
    typing_start: () => null,
    contact_security: (args) => {
      const found = state.security[String(args.userId)];
      if (!found) fail({ kind: "not_found" });
      return found;
    },
    contact_set_verified: (args) => {
      const found = state.security[String(args.userId)];
      if (!found) fail({ kind: "not_found" });
      const next = { ...found, verified: Boolean(args.verified), needs_reverification: false };
      state.security[found.user_id] = next;
      return next;
    },
    contact_trust_device: (args) => {
      const found = state.security[String(args.userId)];
      if (!found) fail({ kind: "not_found" });
      const next: ContactSecurity = {
        ...found,
        devices: found.devices.map((d) =>
          d.device_id === args.deviceId ? { ...d, state: "trusted" } : d,
        ),
      };
      state.security[found.user_id] = next;
      return next;
    },
    devices_list: () => state.myDevices,
    device_revoke: (args) => {
      const target = state.myDevices.find((d) => d.id === args.deviceId);
      if (!target) fail({ kind: "not_found" });
      if (target.current)
        fail({ kind: "conflict", code: "current_device", message: "Sign out instead." });
      state.myDevices = state.myDevices.filter((d) => d !== target);
      return null;
    },

    invites_list: () => state.invites,
    invite_send: (args) => {
      const to = userById(String(args.toUserId));
      if (!to) fail({ kind: "not_found" });
      const message = (args.message as string | null) ?? null;
      if (message !== null && [...message].length > 200)
        fail({ kind: "invalid_input", field: "message", message: "at most 200 characters" });
      const secret = (args.joinSecret as string | null) ?? null;
      if (secret !== null && !/^[A-Za-z0-9._:\-[\]]{1,256}$/.test(secret))
        fail({ kind: "invalid_input", field: "join_secret", message: "not a join code" });
      const pkg = String(args.packageId);
      const installed = state.installs.find((i) => i.package.package_id === pkg);
      seq += 1;
      const created: Invite = makeInvite({
        id: `01920000-0000-7000-8000-0000000i${String(seq).padStart(4, "0")}`,
        direction: "outgoing",
        from: ME,
        to,
        package: {
          id: pkg,
          title: installed?.title ?? (pkg === MOCK_GAME.id ? MOCK_GAME.title : "Unknown game"),
          cover_url: installed?.cover_url ?? null,
        },
        message,
        has_join_secret: secret !== null,
        created_at: new Date().toISOString(),
        updated_at: new Date().toISOString(),
        expires_at: new Date(Date.now() + 30 * 60_000).toISOString(),
      });
      state.invites = [created, ...state.invites];
      void events.inviteChanged.emit(created);
      return created;
    },
    invite_accept: (args) => {
      const found = invite(String(args.inviteId));
      if (found.state !== "pending")
        fail({ kind: "conflict", code: "invalid_state_transition", message: "Not pending." });
      const installed = state.installs.some(
        (i) => i.package.package_id === found.package.id && i.state === "installed",
      );
      const accepted = setInvite({
        ...found,
        state: installed ? "ready" : "accepted",
        updated_at: new Date().toISOString(),
      });
      if (!installed) {
        void events.inviteInstallRequested.emit({
          invite_id: found.id,
          package: {
            server_id: "01920000-0000-7000-8000-000000000001",
            package_id: found.package.id,
          },
          reason: "missing",
        });
      }
      return accepted;
    },
    invite_decline: (args) => {
      const found = invite(String(args.inviteId));
      return setInvite({ ...found, state: "declined", updated_at: new Date().toISOString() });
    },
    invite_cancel: (args) => {
      const found = invite(String(args.inviteId));
      if (!["pending", "accepted", "installing"].includes(found.state))
        fail({ kind: "conflict", code: "invalid_state_transition", message: "Too late." });
      return setInvite({ ...found, state: "cancelled", updated_at: new Date().toISOString() });
    },
  };

  // Forced errors win over every social command.
  const out: Record<string, Handler> = {};
  for (const [name, handler] of Object.entries(handlers)) {
    out[name] = (args) => {
      const forced = state.socialErrors[name];
      if (forced) fail(forced);
      return handler(args);
    };
  }
  return out;
}
