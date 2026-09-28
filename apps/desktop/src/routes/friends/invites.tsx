// Game invites (05-social §5, 05-social-notes §6): the incoming invite cards in the main window
// (stacked, dismissible, focusable; accepting opens the install dialog at once when the game is
// missing), and the lists of invites you sent (with the invitee's progress) and received.
import { useQueryClient } from "@tanstack/react-query";
import { lazy, type ReactNode, Suspense, useState } from "react";
import { Button, IconButton } from "../../components/Button";
import { ProgressBar } from "../../components/ProgressBar";
import { useToast } from "../../components/Toast";
import { formatPercent, formatRelativeTime, t } from "../../i18n";
import { commands, events, type Invite, type InviteInstallRequested } from "../../ipc";
import { useTauriEvent } from "../../ipc/events";
import { queryKeys } from "../../ipc/query";
import { Avatar } from "./Avatar";
import styles from "./Friends.module.css";
import { displayName, socialErrorText } from "./model";
import { useInvites } from "./queries";

const InstallDialog = lazy(async () => ({
  default: (await import("../package/InstallDialog")).InstallDialog,
}));

const ACTIVE = new Set<Invite["state"]>(["pending", "accepted", "installing", "ready"]);

/** What the sender sees about the invitee. */
export function outgoingStateText(invite: Invite): string {
  const name = displayName(invite.to);
  switch (invite.state) {
    case "installing":
      return invite.progress !== null
        ? t("invite.state.installingPercent", { name, percent: formatPercent(invite.progress) })
        : t("invite.state.installing", { name });
    case "failed":
      return invite.failure_reason
        ? t(`invite.failure.${invite.failure_reason}`, { name })
        : t("invite.state.failed");
    case "cancelled":
    case "expired":
      return t(`invite.state.${invite.state}`);
    default:
      return t(`invite.state.${invite.state}`, { name });
  }
}

/** What the invitee sees about an invite they received. */
export function incomingStateText(invite: Invite): string {
  const name = displayName(invite.from);
  switch (invite.state) {
    case "accepted":
    case "installing":
    case "ready":
    case "declined":
      return t(`invite.mine.${invite.state}`, { name });
    case "failed":
      return invite.failure_reason
        ? t(`invite.mineFailure.${invite.failure_reason}`)
        : t("invite.state.failed");
    case "joined":
      return t("invite.state.joined", { name: displayName(invite.to) });
    default:
      return t(`invite.state.${invite.state}`, { name });
  }
}

function InviteRow({ invite, children }: { invite: Invite; children?: ReactNode }) {
  const other = invite.direction === "outgoing" ? invite.to : invite.from;
  const text =
    invite.direction === "outgoing" ? outgoingStateText(invite) : incomingStateText(invite);
  return (
    <li className={styles.row} data-nav-group="" data-state={invite.state}>
      <Avatar user={other} />
      <div className={styles.who}>
        <span className={styles.name}>
          {invite.package.title} · {displayName(other)}
        </span>
        <span className={styles.muted}>{text}</span>
        {invite.state === "installing" ? (
          <ProgressBar
            label={t("invite.progress", { name: displayName(other) })}
            hideLabel
            size="sm"
            value={invite.progress}
          />
        ) : null}
        {invite.direction === "outgoing" && invite.has_join_secret && ACTIVE.has(invite.state) ? (
          <span className={styles.muted}>{t("invite.joinSecret")}</span>
        ) : null}
      </div>
      <div className={styles.actions}>{children}</div>
    </li>
  );
}

/** Invites you sent that are still going, or ended recently, with Cancel while it makes sense. */
export function OutgoingInvites() {
  const invites = useInvites();
  const { toast } = useToast();
  const [busy, setBusy] = useState<string | null>(null);
  const outgoing = (invites.data ?? []).filter((i) => i.direction === "outgoing");
  const incoming = (invites.data ?? []).filter(
    (i) => i.direction === "incoming" && i.state !== "pending" && i.state !== "declined",
  );
  if (outgoing.length === 0 && incoming.length === 0) return null;

  const cancel = async (invite: Invite) => {
    setBusy(invite.id);
    try {
      const result = await commands.inviteCancel(invite.id);
      if (result.status === "ok") toast({ tone: "success", title: t("invite.cancelled") });
      else toast({ tone: "danger", title: socialErrorText(result.error) });
    } finally {
      setBusy(null);
    }
  };

  return (
    <>
      {incoming.length > 0 ? (
        <section aria-labelledby="invites-received">
          <h2 id="invites-received" className={styles.groupTitle}>
            {t("invite.receivedTitle")}
          </h2>
          <ul className={styles.list}>
            {incoming.map((invite) => (
              <InviteRow key={invite.id} invite={invite} />
            ))}
          </ul>
        </section>
      ) : null}
      {outgoing.length > 0 ? (
        <section aria-labelledby="invites-sent">
          <h2 id="invites-sent" className={styles.groupTitle}>
            {t("invite.sentTitle")}
          </h2>
          <ul className={styles.list}>
            {outgoing.map((invite) => (
              <InviteRow key={invite.id} invite={invite}>
                {invite.state === "pending" ||
                invite.state === "accepted" ||
                invite.state === "installing" ? (
                  <Button
                    loading={busy === invite.id}
                    aria-label={`${t("invite.cancel")}: ${invite.package.title}, ${displayName(invite.to)}`}
                    onClick={() => void cancel(invite)}
                  >
                    {t("invite.cancel")}
                  </Button>
                ) : null}
              </InviteRow>
            ))}
          </ul>
        </section>
      ) : null}
    </>
  );
}

function InviteCard({
  invite,
  onDismiss,
  onError,
}: {
  invite: Invite;
  onDismiss: () => void;
  onError: (message: string) => void;
}) {
  const [busy, setBusy] = useState<"accept" | "decline" | null>(null);
  const name = displayName(invite.from);
  const titleId = `invite-card-${invite.id}`;
  const live = invite.state === "pending";

  const act = async (kind: "accept" | "decline") => {
    setBusy(kind);
    try {
      const result =
        kind === "accept"
          ? await commands.inviteAccept(invite.id)
          : await commands.inviteDecline(invite.id);
      if (result.status === "error") {
        onError(t("invite.failedToAccept", { detail: socialErrorText(result.error) }));
        return;
      }
      onDismiss();
    } finally {
      setBusy(null);
    }
  };

  return (
    <section
      className={styles.card}
      aria-labelledby={titleId}
      data-nav-group=""
      data-state={invite.state}
    >
      <div className={styles.cardHeader}>
        <Avatar user={invite.from} />
        <div className={styles.who}>
          <strong id={titleId}>{t("invite.card.title", { name })}</strong>
          <span className={styles.name}>{invite.package.title}</span>
        </div>
        <IconButton icon="close" label={t("invite.card.later")} onClick={onDismiss} />
      </div>
      {invite.message ? <p className={styles.cardMessage}>{invite.message}</p> : null}
      {live ? (
        <>
          <p className={styles.muted}>
            {t("invite.card.expires", { when: formatRelativeTime(invite.expires_at) })}
          </p>
          <div className={styles.cardActions}>
            <Button
              variant="primary"
              icon="check"
              loading={busy === "accept"}
              aria-disabled={busy === "decline" || undefined}
              onClick={() => void act("accept")}
            >
              {t("invite.card.accept")}
            </Button>
            <Button
              loading={busy === "decline"}
              aria-disabled={busy === "accept" || undefined}
              onClick={() => void act("decline")}
            >
              {t("invite.card.decline")}
            </Button>
          </div>
        </>
      ) : (
        <p role="status" className={styles.muted}>
          {t("invite.gone")} {incomingStateText(invite)}
        </p>
      )}
    </section>
  );
}

/**
 * Mounted once in the shell: the incoming invite cards (newest on top) and the install dialog that an
 * accepted invite opens at once (`invite-install-requested`).
 */
export function InviteHost() {
  const client = useQueryClient();
  const { toast } = useToast();
  const invites = useInvites();
  const [dismissed, setDismissed] = useState<ReadonlySet<string>>(() => new Set());
  const [shown, setShown] = useState<ReadonlySet<string>>(() => new Set());
  const [install, setInstall] = useState<(InviteInstallRequested & { title: string }) | null>(null);

  useTauriEvent(events.inviteReceived, (invite) => {
    client.setQueryData<Invite[]>(queryKeys.invites, (list) =>
      list ? [invite, ...list.filter((i) => i.id !== invite.id)] : [invite],
    );
    setShown((prev) => new Set(prev).add(invite.id));
  });
  useTauriEvent(events.inviteInstallRequested, (request) => {
    const title = invites.data?.find((i) => i.id === request.invite_id)?.package.title ?? "";
    setInstall({ ...request, title });
  });

  // Cards: incoming invites that were pending when they arrived (or at start), until dismissed.
  const cards = (invites.data ?? []).filter(
    (i) =>
      i.direction === "incoming" &&
      !dismissed.has(i.id) &&
      (i.state === "pending" || shown.has(i.id)),
  );
  const dismiss = (id: string) => setDismissed((prev) => new Set(prev).add(id));

  return (
    <>
      {cards.length > 0 ? (
        <section className={styles.cards} aria-label={t("invite.receivedTitle")}>
          {cards.map((invite) => (
            <InviteCard
              key={invite.id}
              invite={invite}
              onDismiss={() => dismiss(invite.id)}
              onError={(message) => toast({ tone: "danger", title: message })}
            />
          ))}
        </section>
      ) : null}
      {install ? (
        <Suspense fallback={null}>
          <InstallDialog
            packageId={install.package.package_id}
            title={install.title}
            onClose={() => setInstall(null)}
          />
        </Suspense>
      ) : null}
    </>
  );
}
