// Messaging and invites (05-social-notes §5 and §6, Agent 4). Friends, presence and social settings
// are generated already. The messaging types below are copied from Agent 4's A4-T08 bindings (not on
// `main` yet); invites follow the notes. Delete each entry once `bindings.ts` has it.
import type { PackageRef, SocialError, UserSummary } from "../../bindings";
import { call, makeEvents, type Result } from "./runtime";

// ---- messaging (A4-T08) -------------------------------------------------------------------------

export type ConversationKind = "direct" | "party";
export type Conversation = {
  id: string;
  kind: ConversationKind;
  members: UserSummary[];
  last_message: Message | null;
  unread: number;
  created_at: string;
};
export type ConversationsChanged = Conversation[];
export type MessageStatus = "pending" | "sent" | "failed" | "received";
export type DeviceNotice =
  | { kind: "new_device"; user_id: string; device_id: string; device_name: string }
  | { kind: "key_changed"; user_id: string; device_id: string }
  | { kind: "device_revoked"; user_id: string; device_id: string };
export type MessageBody =
  | { kind: "text"; text: string }
  | { kind: "invite_join"; invite_id: string }
  | { kind: "notice"; notice: DeviceNotice }
  | { kind: "unsupported" };
export type Message = {
  id: string;
  conversation_id: string;
  sender_user_id: string;
  mine: boolean;
  body: MessageBody;
  sent_at: string;
  received_at: string | null;
  status: MessageStatus;
};
export type MessageReceived = Message;
export type MessageStatusChanged = {
  message_id: string;
  conversation_id: string;
  status: MessageStatus;
};
export type DeviceNoticeEvent = { conversation_id: string | null; notice: DeviceNotice };
export type TypingEvent = { conversation_id: string; user_id: string };
export type ContactDeviceState = "trusted" | "new" | "key_changed" | "revoked";
export type ContactDevice = {
  device_id: string;
  display_name: string | null;
  first_seen_at: string;
  /** Identity key, grouped for display. */
  key_fingerprint: string;
  state: ContactDeviceState;
};
export type ContactSecurity = {
  user_id: string;
  verified: boolean;
  needs_reverification: boolean;
  /** 60 digits. */
  safety_number: string;
  /** 12 × 5 digits. */
  safety_number_groups: string[];
  devices: ContactDevice[];
};
export type DevicePlatform = "windows" | "linux" | "macos";
export type MyDevice = {
  id: string;
  display_name: string;
  platform: DevicePlatform;
  current: boolean;
  created_at: string;
  last_seen_at: string | null;
};

// ---- invites (A4-T09) ---------------------------------------------------------------------------

export type InviteState =
  | "pending"
  | "accepted"
  | "installing"
  | "ready"
  | "joined"
  | "declined"
  | "cancelled"
  | "expired"
  | "failed";
export type InviteFailure =
  | "no_build_for_platform"
  | "install_failed"
  | "insufficient_space"
  | "cancelled_by_user";
export type InviteDirection = "incoming" | "outgoing";
/** `cover_url` is a `vgimg:` URL. */
export type InvitePackage = { id: string; title: string; cover_url: string | null };
export type Invite = {
  id: string;
  direction: InviteDirection;
  from: UserSummary;
  to: UserSummary;
  package: InvitePackage;
  state: InviteState;
  /** 0–1 while the invitee installs. */
  progress: number | null;
  message: string | null;
  failure_reason: InviteFailure | null;
  created_at: string;
  updated_at: string;
  expires_at: string;
  /** Outgoing only; the secret itself never leaves Rust. */
  has_join_secret: boolean;
};
export type InviteInstallReason = "missing" | "outdated";
export type InviteInstallRequested = {
  invite_id: string;
  package: PackageRef;
  reason: InviteInstallReason;
};

export const socialCommands = {
  async conversationsList(): Promise<Result<Conversation[], SocialError>> {
    return call("conversations_list");
  },
  async conversationOpenDirect(userId: string): Promise<Result<Conversation, SocialError>> {
    return call("conversation_open_direct", { userId });
  },
  async conversationCreateParty(userIds: string[]): Promise<Result<Conversation, SocialError>> {
    return call("conversation_create_party", { userIds });
  },
  /** Oldest first. `before` is a message id; `limit` 1–200. */
  async messagesList(
    conversationId: string,
    before: string | null,
    limit: number,
  ): Promise<Result<Message[], SocialError>> {
    return call("messages_list", { conversationId, before, limit });
  },
  /** Returns the message as `pending`; `message-status-changed` follows. */
  async messageSend(conversationId: string, text: string): Promise<Result<Message, SocialError>> {
    return call("message_send", { conversationId, text });
  },
  async messageRetry(messageId: string): Promise<Result<Message, SocialError>> {
    return call("message_retry", { messageId });
  },
  async conversationMarkRead(conversationId: string): Promise<Result<null, SocialError>> {
    return call("conversation_mark_read", { conversationId });
  },
  /** Throttled in Rust. */
  async typingStart(conversationId: string): Promise<Result<null, SocialError>> {
    return call("typing_start", { conversationId });
  },
  async contactSecurity(userId: string): Promise<Result<ContactSecurity, SocialError>> {
    return call("contact_security", { userId });
  },
  async contactSetVerified(
    userId: string,
    verified: boolean,
  ): Promise<Result<ContactSecurity, SocialError>> {
    return call("contact_set_verified", { userId, verified });
  },
  /** Accepts a changed key and unblocks sending. */
  async contactTrustDevice(
    userId: string,
    deviceId: string,
  ): Promise<Result<ContactSecurity, SocialError>> {
    return call("contact_trust_device", { userId, deviceId });
  },
  async devicesList(): Promise<Result<MyDevice[], SocialError>> {
    return call("devices_list");
  },
  async deviceRevoke(deviceId: string): Promise<Result<null, SocialError>> {
    return call("device_revoke", { deviceId });
  },

  async invitesList(): Promise<Result<Invite[], SocialError>> {
    return call("invites_list");
  },
  async inviteSend(
    toUserId: string,
    packageId: string,
    message: string | null,
    joinSecret: string | null,
  ): Promise<Result<Invite, SocialError>> {
    return call("invite_send", { toUserId, packageId, message, joinSecret });
  },
  async inviteAccept(inviteId: string): Promise<Result<Invite, SocialError>> {
    return call("invite_accept", { inviteId });
  },
  async inviteDecline(inviteId: string): Promise<Result<Invite, SocialError>> {
    return call("invite_decline", { inviteId });
  },
  async inviteCancel(inviteId: string): Promise<Result<Invite, SocialError>> {
    return call("invite_cancel", { inviteId });
  },
};

export const socialEvents = makeEvents<{
  conversationsChanged: ConversationsChanged;
  messageReceived: MessageReceived;
  messageStatusChanged: MessageStatusChanged;
  typing: TypingEvent;
  deviceNotice: DeviceNoticeEvent;
  inviteReceived: Invite;
  inviteChanged: Invite;
  inviteInstallRequested: InviteInstallRequested;
}>({
  conversationsChanged: "conversations-changed",
  messageReceived: "message-received",
  messageStatusChanged: "message-status-changed",
  typing: "typing",
  deviceNotice: "device-notice",
  inviteReceived: "invite-received",
  inviteChanged: "invite-changed",
  inviteInstallRequested: "invite-install-requested",
});
