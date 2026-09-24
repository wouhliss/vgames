// Full-screen block when a pinned server presents a different root fingerprint (01-security §3.1,
// 00-overview "Add server"). There is deliberately no way to dismiss it or continue: the user can
// only switch to another server or remove this one.
import { useQueryClient } from "@tanstack/react-query";
import { useLayoutEffect, useRef, useState } from "react";
import { Button } from "../components/Button";
import { ConfirmDialog } from "../components/ConfirmDialog";
import { Icon } from "../components/Icon";
import { pushLayer } from "../components/layers";
import { t } from "../i18n";
import { commands, type ServerProfile, type TrustProblem } from "../ipc";
import { queryKeys } from "../ipc/query";
import { focusElement } from "../nav/focus";
import { useOptionalNav } from "../nav/NavProvider";
import styles from "./TrustBlock.module.css";

export function TrustBlock({
  problem,
  otherServers,
  onResolved,
}: {
  problem: TrustProblem;
  otherServers: readonly ServerProfile[];
  onResolved: () => void;
}) {
  const ref = useRef<HTMLDivElement>(null);
  const headingRef = useRef<HTMLHeadingElement>(null);
  const [confirmRemove, setConfirmRemove] = useState(false);
  const client = useQueryClient();
  const pushScope = useOptionalNav()?.pushScope;

  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const popLayer = pushLayer(el);
    const popScope = pushScope?.(el);
    if (headingRef.current) focusElement(headingRef.current);
    return () => {
      popScope?.();
      popLayer();
    };
  }, [pushScope]);

  return (
    <div ref={ref} className={styles.block}>
      <div
        role="alertdialog"
        aria-modal="true"
        aria-labelledby="trust-title"
        aria-describedby="trust-text"
        className={styles.panel}
        // Escape must not fall through to "back" navigation behind the block.
        onKeyDown={(e) => {
          if (e.key === "Escape") e.preventDefault();
        }}
      >
        <span className={styles.icon}>
          <Icon name="shield" size={40} />
        </span>
        <h1 id="trust-title" ref={headingRef} tabIndex={-1}>
          {t("shell.trust.title")}
        </h1>
        <p id="trust-text">{t("shell.trust.text", { server: problem.server_name })}</p>
        <dl className={styles.fingerprints}>
          <div>
            <dt>{t("shell.trust.pinned")}</dt>
            <dd>{problem.pinned_fingerprint}</dd>
          </div>
          <div>
            <dt>{t("shell.trust.presented")}</dt>
            <dd className={styles.presented}>{problem.presented_fingerprint}</dd>
          </div>
        </dl>
        <p>{t("shell.trust.advice")}</p>
        <div className={styles.actions}>
          {otherServers.map((server) => (
            <Button
              key={server.id}
              variant="primary"
              onClick={async () => {
                const result = await commands.serverSwitch(server.id);
                if (result.status === "ok") {
                  await client.invalidateQueries({ queryKey: queryKeys.servers });
                  onResolved();
                }
              }}
            >
              {t("shell.trust.switchTo", { server: server.name })}
            </Button>
          ))}
          <Button variant="danger" icon="trash" onClick={() => setConfirmRemove(true)}>
            {t("shell.trust.remove")}
          </Button>
        </div>
      </div>
      <ConfirmDialog
        open={confirmRemove}
        tone="danger"
        title={t("shell.trust.removeTitle", { server: problem.server_name })}
        description={t("shell.trust.removeText")}
        confirmLabel={t("shell.trust.remove")}
        onCancel={() => setConfirmRemove(false)}
        onConfirm={async () => {
          const result = await commands.serverRemove(problem.server_id);
          if (result.status === "ok") {
            await client.invalidateQueries({ queryKey: queryKeys.servers });
            setConfirmRemove(false);
            onResolved();
          }
        }}
      />
    </div>
  );
}
