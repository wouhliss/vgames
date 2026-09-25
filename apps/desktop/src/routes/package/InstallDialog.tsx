// Install dialog: what will be downloaded, where it goes (libraries with free space; offline drives
// and drives without enough space can't be chosen), then queue it.
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { useNavigate } from "react-router";
import { useLibraries } from "../../app/queries";
import { Button } from "../../components/Button";
import { Dialog } from "../../components/Dialog";
import { LoadingState } from "../../components/Feedback";
import { Notice } from "../../components/Notice";
import { RadioGroup, type RadioOption } from "../../components/RadioGroup";
import { useToast } from "../../components/Toast";
import { formatBytes, t } from "../../i18n";
import { commands, type InstallPlanError, type Library } from "../../ipc";
import { CommandError, queryKeys } from "../../ipc/query";
import { focusElement } from "../../nav/focus";
import { installErrorMessage } from "./messages";
import styles from "./Package.module.css";

/** Plan errors worth another try: the server was unreachable or failed. */
function retryable(error: InstallPlanError | null): boolean {
  return error === null || error.kind === "offline" || error.kind === "server";
}

export function InstallDialog({
  packageId,
  title,
  onClose,
}: {
  packageId: string;
  title: string;
  onClose: () => void;
}) {
  const navigate = useNavigate();
  const client = useQueryClient();
  const { toast } = useToast();
  const libraries = useLibraries();
  const plan = useQuery({
    queryKey: ["install_plan", packageId],
    queryFn: async () => {
      const result = await commands.installPlan(packageId);
      if (result.status === "error") throw new CommandError(result.error);
      return result.data;
    },
    gcTime: 0,
  });
  const [choice, setChoice] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [goneOnStart, setGoneOnStart] = useState(false);

  const planError =
    plan.error instanceof CommandError ? (plan.error.error as InstallPlanError) : null;
  const required = plan.data?.required_bytes ?? 0;
  const usable = (l: Library) => l.online && (l.free_bytes === null || l.free_bytes >= required);
  const list = libraries.data ?? [];
  const fallback = list.find((l) => l.is_default && usable(l)) ?? list.find(usable);
  const selected = choice ?? fallback?.id ?? null;
  const noneFits = plan.data !== undefined && list.length > 0 && !list.some(usable);

  // The package was removed from the server while its page was open. The dialog says so; closing it
  // reloads the details, so the page shows that the package is gone.
  const removed = goneOnStart || planError?.kind === "not_found";
  const close = () => {
    onClose();
    if (removed) void client.invalidateQueries({ queryKey: queryKeys.details(packageId) });
  };

  const options: RadioOption<string>[] = list.map((library) => ({
    value: library.id,
    label: library.label ?? library.path,
    description: !library.online
      ? t("install.offline")
      : !usable(library)
        ? t("install.notEnough")
        : library.label
          ? library.path
          : undefined,
    aside:
      library.online && library.free_bytes !== null
        ? t("install.free", { free: formatBytes(library.free_bytes) })
        : undefined,
    disabled: !usable(library),
  }));

  const confirm = async () => {
    if (!selected || busy) return;
    setBusy(true);
    setError(null);
    try {
      const result = await commands.installStart(packageId, selected);
      if (result.status === "error") {
        setError(installErrorMessage(result.error, title));
        if (result.error.kind === "not_found") setGoneOnStart(true);
        return;
      }
      await client.invalidateQueries({ queryKey: queryKeys.installs });
      toast({
        tone: "success",
        title: t("install.queued", { title }),
        action: { label: t("install.viewDownloads"), onClick: () => navigate("/downloads") },
      });
      onClose();
    } catch {
      setError(t("error.generic"));
    } finally {
      setBusy(false);
    }
  };

  const ready = plan.data !== undefined && !noneFits;

  // The dialog opens while the plan loads, so focus starts in the footer. When the libraries appear,
  // move focus to the selected one, unless the player already moved it.
  const groupRef = useRef<HTMLDivElement>(null);
  const openedWith = useRef<Element | null>(null);
  useLayoutEffect(() => {
    openedWith.current = document.activeElement;
  }, []);
  const loaded = plan.data !== undefined;
  useEffect(() => {
    if (!loaded || document.activeElement !== openedWith.current) return;
    const radio = groupRef.current?.querySelector<HTMLElement>("[role='radio'][tabindex='0']");
    if (radio) focusElement(radio);
  }, [loaded]);
  return (
    <Dialog
      open
      title={t("install.title", { title })}
      onClose={close}
      dismissible={!busy}
      footer={
        planError || plan.isError || removed ? (
          <>
            {plan.isError && retryable(planError) ? (
              <Button onClick={() => void plan.refetch()}>{t("common.retry")}</Button>
            ) : null}
            <Button variant="primary" onClick={close}>
              {t("common.cancel")}
            </Button>
          </>
        ) : noneFits ? (
          <>
            <Button onClick={close}>{t("common.cancel")}</Button>
            <Button
              variant="primary"
              onClick={() => {
                onClose();
                navigate("/settings/storage");
              }}
            >
              {t("install.openStorage")}
            </Button>
          </>
        ) : (
          <>
            <Button onClick={close} aria-disabled={busy || undefined}>
              {t("common.cancel")}
            </Button>
            <Button
              variant="primary"
              icon="download"
              loading={busy}
              aria-disabled={!ready || selected === null || undefined}
              onClick={() => void confirm()}
            >
              {t("install.confirm")}
            </Button>
          </>
        )
      }
    >
      <div className={styles.dialogStack}>
        {plan.isPending ? <LoadingState label={t("install.loading")} /> : null}
        {planError ? (
          <Notice tone="danger" role="alert">
            {installErrorMessage(planError, title)}
          </Notice>
        ) : plan.isError ? (
          <Notice tone="danger" role="alert">
            {t("error.generic")}
          </Notice>
        ) : null}
        {plan.data ? (
          <dl className={styles.planFacts}>
            <div>
              <dt>{t("package.version")}</dt>
              <dd>
                {t("install.version", { version: plan.data.release.version_label })} ·{" "}
                {t("install.buildFor", { platform: t(`platform.${plan.data.release.platform}`) })}
                {plan.data.release.via !== "native"
                  ? ` · ${t(`availability.${plan.data.release.via}`)}`
                  : ""}
              </dd>
            </div>
            <div>
              <dt>{t("install.download")}</dt>
              <dd>{formatBytes(plan.data.download_bytes)}</dd>
            </div>
            <div>
              <dt>{t("install.needed")}</dt>
              <dd>{formatBytes(plan.data.required_bytes)}</dd>
            </div>
          </dl>
        ) : null}
        {plan.data ? (
          <div ref={groupRef}>
            <RadioGroup
              label={t("install.library")}
              value={selected}
              options={options}
              onChange={setChoice}
            />
          </div>
        ) : null}
        {noneFits ? (
          <Notice tone="warning" role="status">
            <strong>{t("install.noSpaceTitle")}</strong> {t("install.noSpaceText")}
          </Notice>
        ) : null}
        {error ? (
          <Notice tone="danger" role="alert">
            {error}
          </Notice>
        ) : null}
      </div>
    </Dialog>
  );
}
