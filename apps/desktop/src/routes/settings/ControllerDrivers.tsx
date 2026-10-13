import { useQuery } from "@tanstack/react-query";
import { ErrorState, LoadingState } from "../../components/Feedback";
import { SafeMarkdown } from "../../components/SafeMarkdown";
import { t } from "../../i18n";
import { type ControllerDriverState, commands } from "../../ipc";
import { unwrap } from "../../ipc/query";
import styles from "./Settings.module.css";

function Driver({ state }: { state: ControllerDriverState }) {
  switch (state.kind) {
    case "available":
      return <p>{t("settings.controllers.drivers.available")}</p>;
    case "not_required":
      return <p>{t("settings.controllers.drivers.notRequired")}</p>;
    case "vigembus_missing":
      return <SafeMarkdown source={t("settings.controllers.drivers.vigembus")} />;
    case "hidhide_missing":
      return <SafeMarkdown source={t("settings.controllers.drivers.hidhide")} />;
    case "macos_entitlement_missing":
      return <p>{t("settings.controllers.drivers.macos")}</p>;
    case "uinput_denied":
      return (
        <>
          <SafeMarkdown source={t("settings.controllers.drivers.uinput")} />
          <pre className={styles.controllerRule}>
            <code>{state.udev_rule}</code>
          </pre>
        </>
      );
    case "probe_error":
      return (
        <p role="alert">{t("settings.controllers.drivers.error", { detail: state.detail })}</p>
      );
  }
}

export function ControllerDrivers() {
  const drivers = useQuery({
    queryKey: ["controllers_driver_status"],
    queryFn: async () => unwrap(await commands.controllersDriverStatus()),
  });
  return (
    <div className={styles.subsection}>
      <h3>{t("settings.controllers.drivers.title")}</h3>
      {drivers.data ? (
        <>
          <h4>{t("settings.controllers.drivers.emulation")}</h4>
          <Driver state={drivers.data.emulation} />
          <h4>{t("settings.controllers.drivers.hiding")}</h4>
          <Driver state={drivers.data.hiding} />
        </>
      ) : drivers.isError ? (
        <ErrorState title={t("error.generic")} onRetry={() => void drivers.refetch()} />
      ) : (
        <LoadingState />
      )}
    </div>
  );
}
