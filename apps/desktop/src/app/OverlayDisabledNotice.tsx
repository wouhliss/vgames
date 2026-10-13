// "The in-game overlay is off for <game>" (05-social §6, GAME-11): the crash safety valve turned
// the overlay off for a game that closed twice right after starting. Plain text naming the game,
// "Turn it back on" (`package_overlay_set`, which also clears the valve) and "Dismiss".
import { useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { Button } from "../components/Button";
import { Notice } from "../components/Notice";
import { useToast } from "../components/Toast";
import { t } from "../i18n";
import { commands, events, type PackageRef } from "../ipc";
import { useTauriEvent } from "../ipc/events";
import { useInstalls } from "./queries";

const same = (a: PackageRef) => (b: PackageRef) =>
  a.server_id === b.server_id && a.package_id === b.package_id;

export function OverlayDisabledNotice() {
  const [games, setGames] = useState<PackageRef[]>([]);
  const [busy, setBusy] = useState(false);
  const installs = useInstalls();
  const client = useQueryClient();
  const { toast } = useToast();

  useTauriEvent(events.overlayPackageDisabled, ({ package: pkg }) => {
    setGames((prev) => (prev.some(same(pkg)) ? prev : [...prev, pkg]));
  });

  const pkg = games[0];
  if (!pkg) return null;
  const title = installs.data?.find((i) => same(pkg)(i.package))?.title;
  const dismiss = () => setGames((prev) => prev.filter((g) => !same(pkg)(g)));

  return (
    <section aria-label={t("shell.overlayOff.label")}>
      <Notice tone="warning" role="status">
        <p>
          <strong>
            {title ? t("shell.overlayOff.title", { title }) : t("shell.overlayOff.titleUnknown")}
          </strong>{" "}
          {t("shell.overlayOff.text")}
        </p>
        <p>
          <Button
            size="sm"
            disabled={busy}
            onClick={async () => {
              setBusy(true);
              const result = await commands.packageOverlaySet(pkg, true);
              setBusy(false);
              if (result.status === "ok") {
                dismiss();
                void client.invalidateQueries({ queryKey: ["overlay_packages"] });
                toast({ tone: "success", title: t("shell.overlayOff.restored") });
              } else {
                toast({ tone: "danger", title: t("shell.overlayOff.failed") });
              }
            }}
          >
            {t("shell.overlayOff.turnOn")}
          </Button>{" "}
          <Button size="sm" variant="ghost" onClick={dismiss}>
            {t("shell.overlayOff.dismiss")}
          </Button>
        </p>
      </Notice>
    </section>
  );
}
