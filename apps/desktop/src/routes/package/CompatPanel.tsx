// How this package runs on this computer (09-compatibility): natively, through Rosetta 2, or through
// Proton/Wine with the signed profile's status and notes, the ProtonDB tier as a hint, and anything
// that blocks it (DirectX 12 on an Intel Mac, missing Rosetta 2).
import { useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { Button } from "../../components/Button";
import { ConfirmDialog } from "../../components/ConfirmDialog";
import { Badge, type BadgeTone } from "../../components/Feedback";
import { Notice } from "../../components/Notice";
import { useToast } from "../../components/Toast";
import { t } from "../../i18n";
import { type CompatBlocker, type CompatInfo, type CompatStatus, commands } from "../../ipc";
import { queryKeys } from "../../ipc/query";
import styles from "./Package.module.css";

const STATUS_TONE: Record<CompatStatus, BadgeTone> = {
  verified: "success",
  playable: "info",
  unsupported: "danger",
  untested: "neutral",
};

function BlockerNotice({ blocker, packageId }: { blocker: CompatBlocker; packageId: string }) {
  const client = useQueryClient();
  const { toast } = useToast();
  const [confirming, setConfirming] = useState(false);
  switch (blocker.kind) {
    case "d3d12_unsupported_on_mac":
      return (
        <Notice tone="danger" role="status">
          <strong>{t("compat.needsAppleSilicon")}</strong> {t("compat.needsAppleSiliconText")}
        </Notice>
      );
    case "rosetta_sunset":
      return (
        <Notice tone="warning">
          <strong>{t("compat.rosettaSunset", { version: blocker.last_macos })}</strong>{" "}
          {t("compat.rosettaSunsetText", { version: blocker.last_macos })}
        </Notice>
      );
    case "needs_rosetta":
      return (
        <>
          <Notice tone="warning" role="status">
            <div className={styles.noticeStack}>
              <span>
                <strong>{t("compat.needsRosetta")}.</strong> {t("compat.needsRosettaText")}
              </span>
              <span>
                <Button size="sm" onClick={() => setConfirming(true)}>
                  {t("compat.installRosetta")}
                </Button>
              </span>
            </div>
          </Notice>
          <ConfirmDialog
            open={confirming}
            title={t("compat.rosettaConfirmTitle")}
            description={t("compat.rosettaConfirmText")}
            confirmLabel={t("compat.installRosetta")}
            onCancel={() => setConfirming(false)}
            onConfirm={async () => {
              const result = await commands.rosettaInstall().catch(() => null);
              setConfirming(false);
              if (result?.status === "ok") {
                toast({ tone: "success", title: t("compat.rosettaInstalled") });
                await client.invalidateQueries({ queryKey: queryKeys.details(packageId) });
              } else {
                const detail =
                  result?.status === "error" && "detail" in result.error
                    ? result.error.detail
                    : t("error.generic");
                toast({ tone: "danger", title: t("compat.rosettaFailed", { detail }) });
              }
            }}
          />
        </>
      );
  }
}

export function CompatPanel({ compat, packageId }: { compat: CompatInfo; packageId: string }) {
  if (compat.kind === "unavailable") return null;
  return (
    <section className={styles.section} aria-labelledby="compat-title">
      <h2 id="compat-title">{t("compat.title")}</h2>
      {compat.kind === "native" ? (
        <p className={styles.muted}>{t("compat.nativeText")}</p>
      ) : compat.kind === "rosetta" ? (
        <>
          <p className={styles.muted}>{t("compat.rosettaText")}</p>
          {compat.blockers.map((b) => (
            <BlockerNotice key={b.kind} blocker={b} packageId={packageId} />
          ))}
        </>
      ) : (
        <>
          <div className={styles.compatRow}>
            <Badge tone="accent">{t(`availability.${compat.layer}`)}</Badge>
            <Badge tone={STATUS_TONE[compat.status]}>{t(`compat.status.${compat.status}`)}</Badge>
          </div>
          <p className={styles.muted}>
            {t("compat.runsWith", { layer: compat.layer === "proton" ? "Proton" : "Wine" })}{" "}
            {t(`compat.statusText.${compat.status}`)}
          </p>
          {compat.notes ? (
            <div className={styles.notes}>
              <h3>{t("compat.notesLabel")}</h3>
              {/* Plain text from the signed profile. */}
              <p data-selectable="">{compat.notes}</p>
            </div>
          ) : null}
          {compat.protondb_tier ? (
            <p className={styles.hint}>
              {t("compat.protondb", { tier: t(`compat.tier.${compat.protondb_tier}`) })}.{" "}
              {t("compat.protondbHint")}
            </p>
          ) : null}
          {compat.blockers.map((b) => (
            <BlockerNotice key={b.kind} blocker={b} packageId={packageId} />
          ))}
        </>
      )}
    </section>
  );
}
