// Invites (05-social-notes §6, Agent 4, A4-T09). Friends, presence, settings and messaging are
// generated already; these follow the notes. Delete each entry once `bindings.ts` has it.
import type { PackageRef, SocialError, UserSummary } from "../../bindings";
import { call, makeEvents, type Result } from "./runtime";

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
  inviteReceived: Invite;
  inviteChanged: Invite;
  inviteInstallRequested: InviteInstallRequested;
}>({
  inviteReceived: "invite-received",
  inviteChanged: "invite-changed",
  inviteInstallRequested: "invite-install-requested",
});
