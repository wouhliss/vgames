// The non-blocking launcher update banner (08-release §2) under the top bar: "available" → What's
// new, then download progress, then "restarting". An install that failed says so until dismissed.
// Background check failures stay quiet: only an install the player started is reported.
import { type ReactNode, useEffect, useState } from "react";
import { Button } from "../../components/Button";
import { Icon } from "../../components/Icon";
import { ProgressBar } from "../../components/ProgressBar";
import { formatBytes, t } from "../../i18n";
import { updateVersion } from "./model";
import styles from "./Update.module.css";
import { useUpdaterStatus } from "./useUpdater";
import { WhatsNewDialog } from "./WhatsNewDialog";

export function UpdateBanner() {
  const status = useUpdaterStatus();
  const [laterFor, setLaterFor] = useState<string | null>(null);
  const [dialogOpen, setDialogOpen] = useState(false);
  // Set once an install download starts, so a later failure is reported (and only then).
  const [installStarted, setInstallStarted] = useState(false);
  // The last known version, so the dialog keeps its title while the state moves on.
  const [dialogVersion, setDialogVersion] = useState<string | null>(null);

  const state = status.data?.state;
  const version = updateVersion(status.data);
  useEffect(() => {
    if (state?.kind === "downloading") setInstallStarted(true);
  }, [state?.kind]);
  useEffect(() => {
    if (version) setDialogVersion(version);
  }, [version]);

  let banner: ReactNode = null;
  if (state?.kind === "available" && laterFor !== state.version) {
    banner = (
      <section
        className={styles.banner}
        aria-label={t("update.availableTitle", { version: state.version })}
      >
        <span className={styles.bannerIcon}>
          <Icon name="download" />
        </span>
        <div className={styles.bannerText}>
          <strong>{t("update.availableTitle", { version: state.version })}</strong>
          <p>{t("update.availableText")}</p>
        </div>
        <div className={styles.bannerActions}>
          <Button variant="primary" onClick={() => setDialogOpen(true)}>
            {t("update.whatsNew")}
          </Button>
          <Button variant="ghost" onClick={() => setLaterFor(state.version)}>
            {t("update.later")}
          </Button>
        </div>
      </section>
    );
  } else if (state?.kind === "downloading") {
    const value = state.total ? state.downloaded / state.total : null;
    banner = (
      <div className={styles.banner} role="status">
        <span className={styles.bannerIcon}>
          <Icon name="download" />
        </span>
        <div className={styles.bannerText}>
          <strong>{t("update.downloadingTitle", { version: state.version })}</strong>
        </div>
        <div className={styles.progress}>
          <ProgressBar
            label={t("update.progress")}
            hideLabel
            size="sm"
            value={value}
            {...(state.total
              ? { valueText: `${formatBytes(state.downloaded)} / ${formatBytes(state.total)}` }
              : {})}
          />
        </div>
      </div>
    );
  } else if (state?.kind === "installed") {
    banner = (
      <div className={styles.banner} role="status">
        <span className={styles.bannerIcon}>
          <Icon name="refresh" />
        </span>
        <div className={styles.bannerText}>
          <strong>{t("update.restarting", { version: state.version })}</strong>
        </div>
      </div>
    );
  } else if (state?.kind === "failed" && installStarted) {
    banner = (
      <div className={`${styles.banner} ${styles.bannerDanger}`} role="alert">
        <span className={styles.bannerIcon}>
          <Icon name="error" />
        </span>
        <div className={styles.bannerText}>
          <strong>{t("update.failedTitle")}</strong>
          <p>{state.message}</p>
        </div>
        <div className={styles.bannerActions}>
          <Button variant="ghost" onClick={() => setInstallStarted(false)}>
            {t("update.dismiss")}
          </Button>
        </div>
      </div>
    );
  }

  return (
    <>
      {banner}
      {dialogVersion ? (
        <WhatsNewDialog
          open={dialogOpen}
          onClose={() => setDialogOpen(false)}
          version={dialogVersion}
          status={status.data}
        />
      ) : null}
    </>
  );
}
