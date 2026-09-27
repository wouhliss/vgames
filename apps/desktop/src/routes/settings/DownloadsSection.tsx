// Download settings: an optional speed limit (MB/s in the UI, KiB/s on the wire) and how many
// installs run at once.
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { type ReactNode, useEffect, useState } from "react";
import { ErrorState, LoadingState } from "../../components/Feedback";
import { RadioGroup } from "../../components/RadioGroup";
import { Switch } from "../../components/Switch";
import { TextField } from "../../components/TextField";
import { useToast } from "../../components/Toast";
import { t } from "../../i18n";
import { commands, type DownloadSettings } from "../../ipc";
import styles from "./Settings.module.css";
import { Section } from "./SettingsPage";

const KEY = ["download_settings_get"];
const KIB_PER_MB = 1_000_000 / 1024;

/** "12.5" → 12207 KiB/s, or null when it isn't a speed between 0.1 and 1000 MB/s. */
export function parseLimit(text: string): number | null {
  const value = Number(text.trim().replace(",", "."));
  if (!/^\d+([.,]\d+)?$/.test(text.trim()) || !Number.isFinite(value)) return null;
  if (value < 0.1 || value > 1000) return null;
  return Math.round(value * KIB_PER_MB);
}

const formatLimit = (kib: number) => String(Math.round((kib / KIB_PER_MB) * 10) / 10);

export function DownloadsSection() {
  const client = useQueryClient();
  const { toast } = useToast();
  const settings = useQuery({ queryKey: KEY, queryFn: () => commands.downloadSettingsGet() });
  const [draft, setDraft] = useState("");
  const [invalid, setInvalid] = useState(false);

  const limit = settings.data?.bandwidth_limit_kib ?? null;
  useEffect(() => {
    if (limit !== null) setDraft(formatLimit(limit));
  }, [limit]);

  const save = async (next: DownloadSettings) => {
    const result = await commands.downloadSettingsSet(next).catch(() => null);
    if (result?.status === "ok") client.setQueryData(KEY, result.data);
    else toast({ tone: "danger", title: t("settings.saveFailed") });
  };

  let body: ReactNode;
  if (settings.data) {
    const current = settings.data;
    body = (
      <>
        <Switch
          label={t("settings.downloads.limit")}
          description={t("settings.downloads.limitText")}
          checked={current.bandwidth_limit_kib !== null}
          onCheckedChange={(on) => {
            setInvalid(false);
            void save({
              ...current,
              bandwidth_limit_kib: on ? (parseLimit(draft) ?? parseLimit("10")) : null,
            });
          }}
        />
        {current.bandwidth_limit_kib !== null ? (
          <div className={styles.narrowField}>
            <TextField
              label={t("settings.downloads.limitValue")}
              description={t("settings.downloads.limitHint")}
              error={invalid ? t("settings.downloads.limitInvalid") : undefined}
              inputMode="decimal"
              value={draft}
              onChange={(e) => {
                setDraft(e.target.value);
                setInvalid(false);
              }}
              onBlur={() => {
                const kib = parseLimit(draft);
                if (kib === null) setInvalid(true);
                else if (kib !== current.bandwidth_limit_kib)
                  void save({ ...current, bandwidth_limit_kib: kib });
              }}
              onKeyDown={(e) => {
                if (e.key === "Enter") e.currentTarget.blur();
              }}
            />
          </div>
        ) : null}
        <RadioGroup
          label={t("settings.downloads.concurrent")}
          value={String(current.concurrent_installs)}
          onChange={(value) => void save({ ...current, concurrent_installs: Number(value) })}
          options={[1, 2, 3].map((count) => ({
            value: String(count),
            label: t("settings.downloads.concurrentOption", { count }),
            description: count === 2 ? t("settings.downloads.concurrentText") : undefined,
          }))}
        />
      </>
    );
  } else if (settings.isError) {
    body = <ErrorState title={t("error.generic")} onRetry={() => void settings.refetch()} />;
  } else body = <LoadingState />;

  return (
    <Section id="downloads" title={t("settings.section.downloads")}>
      {body}
    </Section>
  );
}
