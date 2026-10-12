import { useEffect, useState } from "react";
import { Button } from "../../components/Button";
import { Dialog } from "../../components/Dialog";
import { ErrorState, LoadingState } from "../../components/Feedback";
import { t } from "../../i18n";
import { type ConnectedPad, type ControllerTesterState, commands, events } from "../../ipc";

export function ControllerTesterDialog({
  pad,
  onClose,
}: {
  pad: ConnectedPad;
  onClose: () => void;
}) {
  const [value, setValue] = useState<ControllerTesterState | null>(null);
  const [ready, setReady] = useState(false);
  const [failed, setFailed] = useState(false);
  useEffect(() => {
    let active = true;
    let subscription: string | null = null;
    let unlisten: (() => void) | null = null;
    const early = new Map<string, ControllerTesterState>();
    const start = async () => {
      try {
        // Listen first; ignore other/replaced subscriptions. An unmount during
        // either asynchronous step still releases the listener and subscription.
        const stop = await events.controllerTesterState.listen(({ payload }) => {
          if (!active || payload.instance_id !== pad.instance_id) return;
          if (payload.subscription_id === subscription) setValue(payload);
          else if (subscription === null && (early.size < 16 || early.has(payload.subscription_id)))
            early.set(payload.subscription_id, payload);
        });
        if (!active) {
          stop();
          return;
        }
        unlisten = stop;
        const result = await commands.controllersTesterStart(pad.instance_id);
        if (result.status === "error") {
          if (active) setFailed(true);
          stop();
          unlisten = null;
          return;
        }
        subscription = result.data.subscription_id;
        if (!active) {
          await commands.controllersTesterStop(subscription);
          return;
        }
        const initial = early.get(subscription);
        early.clear();
        if (initial) setValue(initial);
        setReady(true);
      } catch {
        unlisten?.();
        unlisten = null;
        if (active) setFailed(true);
      }
    };
    void start();
    return () => {
      active = false;
      unlisten?.();
      if (subscription) void commands.controllersTesterStop(subscription).catch(() => null);
    };
  }, [pad.instance_id]);

  const percent = (axis: number) => `${Math.round((axis / 32767) * 100)}%`;
  return (
    <Dialog
      open
      title={t("settings.controllers.tester", { name: pad.name })}
      onClose={onClose}
      footer={<Button onClick={onClose}>{t("common.close")}</Button>}
    >
      {failed ? (
        <ErrorState title={t("settings.controllers.testerFailed")} />
      ) : !ready ? (
        <LoadingState />
      ) : (
        <dl>
          <dt>{t("settings.controllers.buttons")}</dt>
          <dd>
            {value?.buttons.length
              ? value.buttons
                  .map((button) => t(`settings.controllers.buttonNames.${button}`))
                  .join(", ")
              : t("settings.controllers.noButtons")}
          </dd>
          <dt>{t("settings.controllers.leftStick")}</dt>
          <dd>{value ? `${percent(value.left[0])}, ${percent(value.left[1])}` : "0%, 0%"}</dd>
          <dt>{t("settings.controllers.rightStick")}</dt>
          <dd>{value ? `${percent(value.right[0])}, ${percent(value.right[1])}` : "0%, 0%"}</dd>
          <dt>{t("settings.controllers.leftTrigger")}</dt>
          <dd>{percent(value?.left_trigger ?? 0)}</dd>
          <dt>{t("settings.controllers.rightTrigger")}</dt>
          <dd>{percent(value?.right_trigger ?? 0)}</dd>
        </dl>
      )}
    </Dialog>
  );
}
