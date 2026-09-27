// Compatibility (09-compatibility): how Windows games run here (Proton on Linux, Wine on macOS),
// Rosetta 2 on Apple silicon, the default runner, downloaded runtimes with their disk use, per-game
// overrides, and the runtimes' licenses (Apple's for D3DMetal).
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { type ReactNode, useRef, useState } from "react";
import { Button } from "../../components/Button";
import { ConfirmDialog } from "../../components/ConfirmDialog";
import { Dialog } from "../../components/Dialog";
import { ErrorState, LoadingState } from "../../components/Feedback";
import { Notice } from "../../components/Notice";
import { Select } from "../../components/Select";
import { useToast } from "../../components/Toast";
import { formatBytes, t } from "../../i18n";
import {
  type CompatHost,
  type CompatOverview,
  commands,
  type InstalledRuntime,
  type PackageCompat,
} from "../../ipc";
import { unwrap } from "../../ipc/query";
import { CompatOverrideDialog } from "./CompatOverrideDialog";
import {
  COMPAT_OVERVIEW,
  COMPAT_PACKAGES,
  findRunner,
  overrideSummary,
  runnerValue,
  runtimeLabel,
} from "./compatModel";
import styles from "./Settings.module.css";
import { Section } from "./SettingsPage";

function Rosetta({ host }: { host: Extract<CompatHost, { kind: "wine" }> }) {
  const client = useQueryClient();
  const { toast } = useToast();
  const [confirming, setConfirming] = useState(false);
  if (!host.apple_silicon) return null;
  return (
    <div className={styles.subsection}>
      <h3>{t("settings.compat.rosetta")}</h3>
      {host.rosetta === "installed" ? (
        <p>{t("settings.compat.rosettaInstalled")}</p>
      ) : (
        <>
          <p>{t("settings.compat.rosettaMissing")}</p>
          <div className={styles.actions}>
            <Button onClick={() => setConfirming(true)}>{t("compat.installRosetta")}</Button>
          </div>
        </>
      )}
      {host.rosetta_last_macos ? (
        <Notice tone="warning">
          {t("settings.compat.rosettaSunset", { version: host.rosetta_last_macos })}
        </Notice>
      ) : null}
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
            await client.invalidateQueries({ queryKey: COMPAT_OVERVIEW });
          } else {
            const detail =
              result?.status === "error" && "detail" in result.error
                ? result.error.detail
                : t("error.generic");
            toast({ tone: "danger", title: t("compat.rosettaFailed", { detail }) });
          }
        }}
      />
    </div>
  );
}

function DefaultRunner({
  overview,
  layer,
}: {
  overview: CompatOverview;
  layer: "proton" | "wine";
}) {
  const client = useQueryClient();
  const { toast } = useToast();
  const installed = new Set(overview.runtimes.map((r) => `${r.runtime}@${r.version}`));
  return (
    <Select
      label={t(`settings.compat.default.${layer}`)}
      description={t("settings.compat.defaultText")}
      value={overview.default_runner ? runnerValue(overview.default_runner) : "auto"}
      options={[
        { value: "auto", label: t("settings.compat.automatic") },
        ...overview.runners.map((r) => ({
          value: runnerValue(r),
          label: runtimeLabel(r.runtime, r.version),
          description: installed.has(runnerValue(r))
            ? t("settings.compat.downloaded")
            : t("settings.compat.downloadsWhenNeeded"),
        })),
      ]}
      onChange={async (value) => {
        const runner = value === "auto" ? null : (findRunner(overview.runners, value) ?? null);
        const result = await commands.compatDefaultSet(runner).catch(() => null);
        if (result?.status === "ok") await client.invalidateQueries({ queryKey: COMPAT_OVERVIEW });
        else toast({ tone: "danger", title: t("settings.saveFailed") });
      }}
    />
  );
}

function Runtimes({ runtimes }: { runtimes: InstalledRuntime[] }) {
  const client = useQueryClient();
  const { toast } = useToast();
  const heading = useRef<HTMLHeadingElement>(null);
  const total = runtimes.reduce((sum, r) => sum + r.size_bytes, 0);

  const remove = async (runtime: InstalledRuntime) => {
    const label = runtimeLabel(runtime.runtime, runtime.version);
    const result = await commands.runtimeRemove(runtime.runtime, runtime.version).catch(() => null);
    if (result?.status === "ok") {
      await client.invalidateQueries({ queryKey: COMPAT_OVERVIEW });
      toast({ tone: "success", title: t("settings.compat.runtimeRemoved", { name: label }) });
      // The row and its button are gone: keep focus in the list's area.
      heading.current?.focus();
    } else if (result?.status === "error" && result.error.kind === "in_use") {
      toast({
        tone: "danger",
        title: t("settings.compat.runtimeInUse", { name: label, count: result.error.used_by }),
      });
    } else
      toast({ tone: "danger", title: t("settings.compat.runtimeRemoveFailed", { name: label }) });
  };

  return (
    <div className={styles.subsection}>
      <h3 ref={heading} tabIndex={-1}>
        {t("settings.compat.runtimes")}
      </h3>
      {runtimes.length === 0 ? (
        <p className={styles.muted}>{t("settings.compat.runtimesEmpty")}</p>
      ) : (
        <>
          <p className={styles.muted}>
            {t("settings.compat.runtimesTotal", { size: formatBytes(total) })}
          </p>
          <ul className={styles.rows} data-nav-group="">
            {runtimes.map((r) => {
              const label = runtimeLabel(r.runtime, r.version);
              return (
                <li key={`${r.runtime}@${r.version}`} className={styles.row}>
                  <div>
                    <p data-selectable="">{label}</p>
                    <p className={styles.muted}>
                      {formatBytes(r.size_bytes)} ·{" "}
                      {r.used_by > 0
                        ? t("settings.compat.usedBy", { count: r.used_by })
                        : t("settings.compat.unused")}
                    </p>
                  </div>
                  {r.used_by === 0 ? (
                    <Button
                      size="sm"
                      variant="ghost"
                      icon="trash"
                      aria-label={t("settings.compat.removeTitle", { name: label })}
                      onClick={() => void remove(r)}
                    >
                      {t("settings.compat.remove")}
                    </Button>
                  ) : null}
                </li>
              );
            })}
          </ul>
        </>
      )}
    </div>
  );
}

function GameRow({ pkg, overview }: { pkg: PackageCompat; overview: CompatOverview }) {
  const client = useQueryClient();
  const { toast } = useToast();
  const [editing, setEditing] = useState(false);
  const customize = useRef<HTMLButtonElement>(null);

  const reset = async () => {
    const result = await commands.compatOverrideReset(pkg.package).catch(() => null);
    if (result?.status !== "ok") {
      toast({ tone: "danger", title: t("settings.saveFailed") });
      return;
    }
    await client.invalidateQueries({ queryKey: COMPAT_PACKAGES });
    toast({ tone: "success", title: t("settings.compat.resetDone", { title: pkg.title }) });
    // Reset disappears with the override: focus goes back to Customize.
    customize.current?.focus();
  };

  return (
    <li className={styles.row}>
      <div>
        <p data-selectable="">{pkg.title}</p>
        <p className={styles.muted}>{overrideSummary(pkg.override)}</p>
      </div>
      <div className={styles.actions}>
        <Button
          ref={customize}
          size="sm"
          aria-label={t("settings.compat.customizeTitle", { title: pkg.title })}
          onClick={() => setEditing(true)}
        >
          {t("settings.compat.customize")}
        </Button>
        {pkg.override ? (
          <Button
            size="sm"
            variant="ghost"
            aria-label={t("settings.compat.resetTitle", { title: pkg.title })}
            onClick={() => void reset()}
          >
            {t("settings.compat.reset")}
          </Button>
        ) : null}
      </div>
      {editing ? (
        <CompatOverrideDialog pkg={pkg} overview={overview} onClose={() => setEditing(false)} />
      ) : null}
    </li>
  );
}

function Games({ overview }: { overview: CompatOverview }) {
  const games = useQuery({
    queryKey: COMPAT_PACKAGES,
    queryFn: async () => unwrap(await commands.compatPackages()),
  });
  let list: ReactNode;
  if (games.data) {
    list =
      games.data.length === 0 ? (
        <p className={styles.muted}>{t("settings.compat.gamesEmpty")}</p>
      ) : (
        <ul className={styles.rows} data-nav-group="">
          {games.data.map((pkg) => (
            <GameRow
              key={`${pkg.package.server_id}/${pkg.package.package_id}`}
              pkg={pkg}
              overview={overview}
            />
          ))}
        </ul>
      );
  } else if (games.isError) {
    list = (
      <ErrorState title={t("settings.compat.gamesFailed")} onRetry={() => void games.refetch()} />
    );
  } else list = <LoadingState />;
  return (
    <div className={styles.subsection}>
      <h3>{t("settings.compat.games")}</h3>
      <p className={styles.muted}>{t("settings.compat.gamesText")}</p>
      {list}
    </div>
  );
}

function Licenses({ onClose }: { onClose: () => void }) {
  const licenses = useQuery({
    queryKey: ["compat_licenses"],
    queryFn: async () => unwrap(await commands.compatLicenses()),
  });
  return (
    <Dialog open size="lg" title={t("settings.compat.licensesTitle")} onClose={onClose}>
      {licenses.data ? (
        <div className={styles.licenseList}>
          {licenses.data.map((l) => (
            <section key={l.runtime} aria-labelledby={`license-${l.runtime}`}>
              <h3 id={`license-${l.runtime}`}>{l.name}</h3>
              <p className={styles.muted}>{l.spdx}</p>
              {/* Plain text: never interpreted as markup. */}
              {/* biome-ignore lint/a11y/noNoninteractiveTabindex: the scrollable text must be reachable with the keyboard. */}
              <pre className={styles.licenses} tabIndex={0} data-selectable="">
                {l.text}
              </pre>
            </section>
          ))}
        </div>
      ) : licenses.isError ? (
        <ErrorState
          title={t("settings.about.licensesFailed")}
          onRetry={() => void licenses.refetch()}
        />
      ) : (
        <LoadingState />
      )}
    </Dialog>
  );
}

export function CompatSection() {
  const overview = useQuery({
    queryKey: COMPAT_OVERVIEW,
    queryFn: async () => unwrap(await commands.compatOverview()),
  });
  const [licenses, setLicenses] = useState(false);

  let body: ReactNode;
  if (overview.data) {
    const data = overview.data;
    const host = data.host;
    body =
      host.kind === "native" ? (
        <p>{t("settings.compat.native")}</p>
      ) : (
        <>
          <p className={styles.muted}>{t(`settings.compat.intro.${host.kind}`)}</p>
          {host.kind === "wine" ? <Rosetta host={host} /> : null}
          <DefaultRunner overview={data} layer={host.kind} />
          <Runtimes runtimes={data.runtimes} />
          <Games overview={data} />
          <div className={styles.subsection}>
            <h3>{t("settings.compat.licenses")}</h3>
            <p className={styles.muted}>{t("settings.compat.licensesText")}</p>
            <div className={styles.actions}>
              <Button onClick={() => setLicenses(true)}>{t("settings.compat.licensesOpen")}</Button>
            </div>
          </div>
        </>
      );
  } else if (overview.isError) {
    body = <ErrorState title={t("error.generic")} onRetry={() => void overview.refetch()} />;
  } else body = <LoadingState />;

  return (
    <Section id="compatibility" title={t("settings.section.compatibility")}>
      {body}
      {licenses ? <Licenses onClose={() => setLicenses(false)} /> : null}
    </Section>
  );
}
