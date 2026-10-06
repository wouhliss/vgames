// Users (A3-T17): search by username or Discord id, filter by role, disable/enable with a reason,
// change roles (owners only; the controls are hidden from admins, and a 403 is still handled).
// The API refuses disabling yourself and removing the last owner; both are explained.
import { useInfiniteQuery, useQueryClient } from "@tanstack/react-query";
import { type FormEvent, useEffect, useState } from "react";
import { ApiError } from "../../api/errors";
import type { AdminUser } from "../../api/schemas";
import { listUsers, serverKeys, updateUser } from "../../api/server";
import { ErrorView, ForbiddenPage, Loading } from "../../app/ErrorView";
import { Modal } from "../../components/Modal";
import { SpacerRow, useTableWindow } from "../../components/tableWindow";
import { LoadMore, useRole, useUrlFilters, when } from "./shared";

const ROLES = ["user", "admin", "owner"] as const;
type Role = (typeof ROLES)[number];

function refusal(e: unknown): string | null {
  if (!(e instanceof ApiError)) return null;
  if (e.code === "cannot_disable_self") return "You can't disable your own account.";
  if (e.code === "last_owner")
    return "The server needs at least one active owner. Make someone else an owner first.";
  if (e.status === 403) return "Your role doesn't allow this.";
  return null;
}

type Pending =
  | { kind: "disable" | "enable"; user: AdminUser }
  | { kind: "role"; user: AdminUser; role: Role }
  | null;

export function UsersPage() {
  const role = useRole();
  const client = useQueryClient();
  const { values, apply } = useUrlFilters(["q", "role"] as const);
  const [q, setQ] = useState(values.q);
  const [roleFilter, setRoleFilter] = useState(values.role);
  useEffect(() => {
    setQ(values.q);
    setRoleFilter(values.role);
  }, [values.q, values.role]);
  const pages = useInfiniteQuery({
    queryKey: serverKeys.users(values),
    queryFn: async ({ pageParam, signal }) => (await listUsers(values, pageParam, signal)).data,
    initialPageParam: null as string | null,
    getNextPageParam: (last) => last.next_cursor ?? null,
  });
  const [pending, setPending] = useState<Pending>(null);
  const [message, setMessage] = useState<string | null>(null);
  const [failure, setFailure] = useState<{ text: string | null; error: unknown } | null>(null);

  const act = async (run: () => Promise<unknown>, done: string) => {
    setFailure(null);
    setMessage(null);
    try {
      await run();
      setMessage(done);
      await client.invalidateQueries({ queryKey: ["admin-users"] });
    } catch (e) {
      setFailure({ text: refusal(e), error: e });
    } finally {
      setPending(null);
    }
  };

  const users = pages.data?.pages.flatMap((p) => p.items) ?? [];
  const win = useTableWindow(users);
  if (pages.error instanceof ApiError && pages.error.status === 403) return <ForbiddenPage />;
  const search = (e: FormEvent) => {
    e.preventDefault();
    apply({ q, role: roleFilter });
  };

  return (
    <section aria-labelledby="page-title">
      <h1 id="page-title">Users</h1>
      <form className="toolbar" aria-label="Filter users" onSubmit={search}>
        <div className="field">
          <label htmlFor="users-q">Username or Discord id</label>
          <input
            id="users-q"
            type="search"
            maxLength={64}
            value={q}
            onChange={(e) => setQ(e.target.value)}
          />
        </div>
        <div className="field">
          <label htmlFor="users-role">Role</label>
          <select
            id="users-role"
            value={roleFilter}
            onChange={(e) => setRoleFilter(e.target.value)}
          >
            <option value="">Any role</option>
            {ROLES.map((r) => (
              <option key={r} value={r}>
                {r}
              </option>
            ))}
          </select>
        </div>
        <button type="submit">Apply</button>
      </form>
      {message ? (
        <p className="banner ok" role="status">
          {message}
        </p>
      ) : null}
      {failure ? (
        failure.text ? (
          <p className="banner error" role="alert">
            {failure.text}
          </p>
        ) : (
          <ErrorView error={failure.error} />
        )
      ) : null}
      {pages.isPending ? (
        <Loading label="Loading users…" />
      ) : pages.isError && !pages.data ? (
        <ErrorView error={pages.error} onRetry={() => void pages.refetch()} />
      ) : users.length === 0 ? (
        <p role="status">No users match.</p>
      ) : (
        <>
          <div className="table-wrap" ref={win.scrollRef}>
            <table>
              <caption className="visually-hidden">Users</caption>
              <thead>
                <tr>
                  <th scope="col">User</th>
                  <th scope="col">Discord id</th>
                  <th scope="col">Role</th>
                  <th scope="col">Status</th>
                  <th scope="col">Last seen</th>
                  <th scope="col">Actions</th>
                </tr>
              </thead>
              <tbody>
                <SpacerRow height={win.before} columns={6} />
                {win.rows.map(({ item: u, index }) => {
                  const canToggle = role === "owner" || u.role === "user";
                  return (
                    <tr key={u.id} ref={win.measure} data-index={index}>
                      <th scope="row" dir="auto">
                        {u.display_name ?? u.username} <span className="muted">@{u.username}</span>
                      </th>
                      <td className="mono">{u.discord_id ?? "—"}</td>
                      <td>
                        {role === "owner" ? (
                          <select
                            aria-label={`Role of ${u.username}`}
                            value={u.role}
                            onChange={(e) =>
                              setPending({ kind: "role", user: u, role: e.target.value as Role })
                            }
                          >
                            {ROLES.map((r) => (
                              <option key={r} value={r}>
                                {r}
                              </option>
                            ))}
                          </select>
                        ) : (
                          u.role
                        )}
                      </td>
                      <td>
                        {u.disabled_at ? (
                          <>
                            Disabled{" "}
                            <span className="muted">
                              {u.disabled_reason ? `(${u.disabled_reason})` : ""}
                            </span>
                          </>
                        ) : (
                          "Active"
                        )}
                      </td>
                      <td>{when(u.last_seen_at)}</td>
                      <td>
                        {canToggle ? (
                          <button
                            type="button"
                            className={u.disabled_at ? undefined : "danger"}
                            onClick={() =>
                              setPending({ kind: u.disabled_at ? "enable" : "disable", user: u })
                            }
                          >
                            {u.disabled_at ? `Enable ${u.username}` : `Disable ${u.username}…`}
                          </button>
                        ) : (
                          <span className="muted">Owners manage admins</span>
                        )}
                      </td>
                    </tr>
                  );
                })}
                <SpacerRow height={win.after} columns={6} />
              </tbody>
            </table>
          </div>
          <LoadMore query={pages} count={users.length} />
        </>
      )}
      {pending?.kind === "disable" ? (
        <DisableDialog
          user={pending.user}
          onCancel={() => setPending(null)}
          onConfirm={(reason) =>
            act(
              () =>
                updateUser(pending.user.id, {
                  disabled: true,
                  ...(reason ? { disabled_reason: reason } : {}),
                }),
              `${pending.user.username} is disabled and signed out everywhere.`,
            )
          }
        />
      ) : null}
      {pending?.kind === "enable" ? (
        <Modal title={`Enable ${pending.user.username}?`} onClose={() => setPending(null)}>
          <p>They can sign in again.</p>
          <div className="actions">
            <button type="button" onClick={() => setPending(null)}>
              Cancel
            </button>
            <button
              type="button"
              className="primary"
              onClick={() =>
                void act(
                  () => updateUser(pending.user.id, { disabled: false }),
                  `${pending.user.username} is enabled.`,
                )
              }
            >
              Enable
            </button>
          </div>
        </Modal>
      ) : null}
      {pending?.kind === "role" ? (
        <Modal
          title={`Make ${pending.user.username} ${pending.role}?`}
          onClose={() => setPending(null)}
        >
          <p>
            From {pending.user.role} to {pending.role}.{" "}
            {pending.role === "owner"
              ? "Owners can change roles, server settings and the trust bundle."
              : pending.role === "admin"
                ? "Admins manage packages, users and the allowlist."
                : "Users can only play."}
          </p>
          <div className="actions">
            <button type="button" onClick={() => setPending(null)}>
              Cancel
            </button>
            <button
              type="button"
              className="primary"
              onClick={() =>
                void act(
                  () => updateUser(pending.user.id, { role: pending.role }),
                  `${pending.user.username} is now ${pending.role}.`,
                )
              }
            >
              Change role
            </button>
          </div>
        </Modal>
      ) : null}
    </section>
  );
}

function DisableDialog({
  user,
  onCancel,
  onConfirm,
}: {
  user: AdminUser;
  onCancel: () => void;
  onConfirm: (reason: string) => Promise<void>;
}) {
  const [reason, setReason] = useState("");
  const [busy, setBusy] = useState(false);
  const n = [...reason].length;
  return (
    <Modal title={`Disable ${user.username}?`} role="alertdialog" busy={busy} onClose={onCancel}>
      <p>They're signed out everywhere and can't sign in until enabled again.</p>
      <form
        onSubmit={async (e) => {
          e.preventDefault();
          if (n > 500 || busy) return;
          setBusy(true);
          await onConfirm(reason.trim());
          setBusy(false);
        }}
      >
        <div className="field">
          <label htmlFor="disable-reason">Reason (shown to other admins)</label>
          <textarea
            id="disable-reason"
            rows={2}
            value={reason}
            aria-invalid={n > 500 || undefined}
            aria-describedby="disable-reason-count"
            onChange={(e) => setReason(e.target.value)}
          />
          <span id="disable-reason-count" className={n > 500 ? "field-error" : "muted"}>
            {n} / 500
          </span>
        </div>
        <div className="actions">
          <button type="button" onClick={onCancel} disabled={busy}>
            Cancel
          </button>
          <button type="submit" className="danger" disabled={busy || n > 500}>
            Disable
          </button>
        </div>
      </form>
    </Modal>
  );
}
