// Overlay: on or off, the shortcut that opens it (recorded by pressing it; the Rust core refuses one
// that the system or another app holds), and a switch per game, including the crash safety valve.
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { type KeyboardEvent, type ReactNode, useEffect, useRef, useState } from "react";
import { Button } from "../../components/Button";
import { ErrorState, LoadingState } from "../../components/Feedback";
import { Notice } from "../../components/Notice";
import { Switch } from "../../components/Switch";
import { useToast } from "../../components/Toast";
import { formatDate, t } from "../../i18n";
import { commands, type PackageOverlay, type SocialSettingsError } from "../../ipc";
import { unwrap } from "../../ipc/query";
import { acceleratorFrom, DEFAULT_HOTKEY } from "./hotkey";
import styles from "./Settings.module.css";
import { Section } from "./SettingsPage";
import { useSocialSettings } from "./socialSettings";

const OVERLAY_PACKAGES = ["overlay_packages"] as const;

function hotkeyError(error: SocialSettingsError | "failed", hotkey: string): string {
  if (error === "failed") return t("settings.saveFailed");
  switch (error.kind) {
    case "invalid":
      return t("settings.overlay.invalid");
    case "in_use":
      return error.by
        ? t("settings.overlay.inUse", { hotkey, by: error.by })
        : t("settings.overlay.inUseUnknown", { hotkey });
    case "invalid_input":
      // Until the hotkey errors are typed (A4-T10), the core refuses a shortcut as invalid input.
      return error.field === "overlay_hotkey"
        ? t("settings.overlay.invalid")
        : t("settings.saveFailed");
    default:
      return t("settings.saveFailed");
  }
}

function HotkeyRecorder({
  current,
  onSave,
}: {
  current: string;
  onSave: (hotkey: string) => Promise<string | null>;
}) {
  const [recording, setRecording] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const fieldRef = useRef<HTMLInputElement>(null);
  const changeRef = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    if (recording) fieldRef.current?.focus();
  }, [recording]);

  const stop = () => {
    setRecording(false);
    // Back to the button that started it (after it re-renders).
    requestAnimationFrame(() => changeRef.current?.focus());
  };

  const save = async (hotkey: string) => {
    setBusy(true);
    const message = await onSave(hotkey);
    setBusy(false);
    setError(message);
    if (message === null) stop();
  };

  const onKeyDown = (e: KeyboardEvent<HTMLInputElement>) => {
    // Every key is captured while recording, including the ones the launcher normally uses.
    e.preventDefault();
    e.stopPropagation();
    if (busy) return;
    if (e.key === "Escape") {
      setError(null);
      stop();
      return;
    }
    const result = acceleratorFrom(e.nativeEvent);
    if (result.kind === "pending") return;
    if (result.kind === "needs_modifier") setError(t("settings.overlay.needsModifier"));
    else if (result.kind === "unsupported") setError(t("settings.overlay.invalid"));
    else void save(result.accelerator);
  };

  return (
    <div className={styles.subsection}>
      <h3 id="hotkey-label">{t("settings.overlay.hotkey")}</h3>
      <p className={styles.muted} id="hotkey-text">
        {t("settings.overlay.hotkeyText")}
      </p>
      {recording ? (
        <input
          ref={fieldRef}
          // A read-only field that captures the keys pressed; its value is the instruction.
          type="text"
          readOnly
          value={t("settings.overlay.recording")}
          aria-label={t("settings.overlay.recordingField")}
          aria-describedby={error ? "hotkey-error" : "hotkey-text"}
          aria-invalid={error ? true : undefined}
          className={styles.recorder}
          onKeyDown={onKeyDown}
          onBlur={() => {
            if (!busy) setRecording(false);
          }}
        />
      ) : (
        <div className={styles.actions}>
          <kbd className={styles.hotkey}>{current}</kbd>
          <Button
            ref={changeRef}
            size="sm"
            aria-label={t("settings.overlay.hotkeyChangeTitle", { current })}
            onClick={() => {
              setError(null);
              setRecording(true);
            }}
          >
            {t("settings.overlay.hotkeyChange")}
          </Button>
          {current === DEFAULT_HOTKEY ? null : (
            <Button
              size="sm"
              variant="ghost"
              loading={busy}
              aria-label={t("settings.overlay.hotkeyResetTitle", { default: DEFAULT_HOTKEY })}
              onClick={() => void save(DEFAULT_HOTKEY)}
            >
              {t("settings.overlay.hotkeyReset")}
            </Button>
          )}
        </div>
      )}
      {error ? (
        <p id="hotkey-error" role="alert" className={styles.error}>
          {error}
        </p>
      ) : null}
    </div>
  );
}

function GameRow({ pkg }: { pkg: PackageOverlay }) {
  const client = useQueryClient();
  const { toast } = useToast();
  const set = async (enabled: boolean) => {
    const result = await commands.overlayPackageSet(pkg.package, enabled).catch(() => null);
    if (result?.status === "ok") await client.invalidateQueries({ queryKey: OVERLAY_PACKAGES });
    else toast({ tone: "danger", title: t("settings.saveFailed") });
  };
  return (
    <li className={styles.switchRow}>
      <Switch
        label={t("settings.overlay.gameSwitch", { title: pkg.title })}
        checked={pkg.enabled}
        onCheckedChange={(enabled) => void set(enabled)}
      />
      {pkg.disabled_by_safety_valve_at && !pkg.enabled ? (
        <Notice tone="warning">
          {t("settings.overlay.safetyValve", { date: formatDate(pkg.disabled_by_safety_valve_at) })}
        </Notice>
      ) : null}
    </li>
  );
}

export function OverlaySection() {
  const { query, save } = useSocialSettings();
  const { toast } = useToast();
  const games = useQuery({
    queryKey: OVERLAY_PACKAGES,
    queryFn: async () => unwrap(await commands.overlayPackages()),
  });

  let body: ReactNode;
  if (query.data) {
    const current = query.data;
    body = (
      <>
        <Switch
          label={t("settings.overlay.enabled")}
          description={t("settings.overlay.enabledText")}
          checked={current.overlay_enabled}
          onCheckedChange={async (overlay_enabled) => {
            if ((await save({ ...current, overlay_enabled })) !== null)
              toast({ tone: "danger", title: t("settings.saveFailed") });
          }}
        />
        <HotkeyRecorder
          current={current.overlay_hotkey}
          onSave={async (overlay_hotkey) => {
            const error = await save({ ...current, overlay_hotkey });
            if (error !== null) return hotkeyError(error, overlay_hotkey);
            toast({
              tone: "success",
              title: t("settings.overlay.saved", { hotkey: overlay_hotkey }),
            });
            return null;
          }}
        />
      </>
    );
  } else if (query.isError) {
    body = <ErrorState title={t("error.generic")} onRetry={() => void query.refetch()} />;
  } else body = <LoadingState />;

  let list: ReactNode;
  if (games.data) {
    list =
      games.data.length === 0 ? (
        <p className={styles.muted}>{t("settings.overlay.gamesEmpty")}</p>
      ) : (
        <ul className={styles.rows} data-nav-group="">
          {games.data.map((pkg) => (
            <GameRow key={`${pkg.package.server_id}/${pkg.package.package_id}`} pkg={pkg} />
          ))}
        </ul>
      );
  } else if (games.isError) {
    list = (
      <ErrorState
        title={t("settings.overlay.gamesLoadFailed")}
        onRetry={() => void games.refetch()}
      />
    );
  } else list = <LoadingState />;

  return (
    <Section id="overlay" title={t("settings.section.overlay")}>
      {body}
      <div className={styles.subsection}>
        <h3>{t("settings.overlay.games")}</h3>
        <p className={styles.muted}>{t("settings.overlay.gamesText")}</p>
        {list}
      </div>
    </Section>
  );
}
