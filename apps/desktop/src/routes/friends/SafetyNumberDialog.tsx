// The safety-number screen (05-social §4.2): 60 digits in 12 groups to compare out loud, the
// "verified" toggle, and the contact's devices; a changed key can be trusted only after a
// confirmation (sending stays paused until then).
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { Button } from "../../components/Button";
import { ConfirmDialog } from "../../components/ConfirmDialog";
import { Dialog } from "../../components/Dialog";
import { Badge, type BadgeTone, ErrorState, LoadingState } from "../../components/Feedback";
import { Notice } from "../../components/Notice";
import { Switch } from "../../components/Switch";
import { useToast } from "../../components/Toast";
import { formatRelativeTime, t } from "../../i18n";
import {
  type ContactDevice,
  type ContactDeviceState,
  type ContactSecurity,
  commands,
  type UserSummary,
} from "../../ipc";
import { queryKeys, unwrap } from "../../ipc/query";
import styles from "./Friends.module.css";
import { displayName, socialErrorText } from "./model";

const DEVICE_TONE: Record<ContactDeviceState, BadgeTone> = {
  trusted: "success",
  new: "info",
  key_changed: "warning",
  revoked: "neutral",
};

export function SafetyNumberDialog({ user, onClose }: { user: UserSummary; onClose: () => void }) {
  const client = useQueryClient();
  const { toast } = useToast();
  const name = displayName(user);
  const key = queryKeys.contactSecurity(user.id);
  const security = useQuery({
    queryKey: key,
    queryFn: async () => unwrap(await commands.contactSecurity(user.id)),
  });
  const [saving, setSaving] = useState(false);
  const [trusting, setTrusting] = useState<ContactDevice | null>(null);

  const apply = async (action: () => ReturnType<typeof commands.contactSetVerified>) => {
    const result = await action();
    if (result.status === "ok") {
      client.setQueryData<ContactSecurity>(key, result.data);
      return true;
    }
    toast({ tone: "danger", title: socialErrorText(result.error) });
    return false;
  };

  return (
    <Dialog open onClose={onClose} size="lg" title={t("chat.safety.title", { name })}>
      {security.isPending ? (
        <LoadingState />
      ) : security.isError ? (
        <ErrorState title={t("chat.safety.loadFailed")} onRetry={() => void security.refetch()} />
      ) : (
        <div className={styles.stack}>
          <p>{t("chat.safety.text", { name })}</p>
          {security.data.needs_reverification ? (
            <Notice tone="warning">{t("chat.safety.reverify", { name })}</Notice>
          ) : null}
          <p className={styles.safetyNumber} data-selectable="" data-testid="safety-number">
            <span className="visually-hidden">
              {`${t("chat.safety.groups")}: ${security.data.safety_number_groups.join(" ")}`}
            </span>
            {security.data.safety_number_groups.map((group, i) => (
              // biome-ignore lint/suspicious/noArrayIndexKey: groups are positional.
              <span key={i} aria-hidden="true">
                {group}
              </span>
            ))}
          </p>
          <Switch
            label={t("chat.safety.verified")}
            description={
              security.data.verified
                ? t("chat.safety.verifiedOn", { name })
                : t("chat.safety.verifiedOff", { name })
            }
            checked={security.data.verified}
            disabled={saving}
            onCheckedChange={async (verified) => {
              setSaving(true);
              await apply(() => commands.contactSetVerified(user.id, verified));
              setSaving(false);
            }}
          />
          <section aria-labelledby="contact-devices">
            <h3 id="contact-devices" className={styles.groupTitle}>
              {t("chat.safety.devices", { name })}
            </h3>
            <ul className={styles.list}>
              {security.data.devices.map((device) => (
                <li key={device.device_id} className={styles.row} data-nav-group="">
                  <div className={styles.who}>
                    <span className={styles.name}>
                      {device.display_name ?? t("chat.safety.unnamed")}{" "}
                      <Badge tone={DEVICE_TONE[device.state]}>
                        {t(`chat.safety.device.${device.state}`)}
                      </Badge>
                    </span>
                    <span className={styles.mono} data-selectable="">
                      {device.key_fingerprint}
                    </span>
                    <span className={styles.muted}>
                      {t("chat.safety.firstSeen", {
                        when: formatRelativeTime(device.first_seen_at),
                      })}
                    </span>
                  </div>
                  {device.state === "key_changed" ? (
                    <div className={styles.actions}>
                      <Button onClick={() => setTrusting(device)}>{t("chat.safety.trust")}</Button>
                    </div>
                  ) : null}
                </li>
              ))}
            </ul>
          </section>
          <p className={styles.muted}>{t("chat.safety.help")}</p>
        </div>
      )}
      <ConfirmDialog
        open={trusting !== null}
        title={t("chat.safety.trustTitle", { name })}
        description={t("chat.safety.trustText", { name })}
        confirmLabel={t("chat.safety.trust")}
        onCancel={() => setTrusting(null)}
        onConfirm={async () => {
          if (!trusting) return;
          const ok = await apply(() => commands.contactTrustDevice(user.id, trusting.device_id));
          setTrusting(null);
          if (ok) toast({ tone: "success", title: t("chat.safety.trusted") });
        }}
      />
    </Dialog>
  );
}
