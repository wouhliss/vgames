// Controllers (07-controllers): connected pads with their battery and player slot, the status of the
// virtual-pad backend with help, a live tester, and per-game emulation and mapping. The tester
// streams only while it is on and stops when the section goes away.
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useRef, useState } from "react";
import { Button } from "../../components/Button";
import { Badge, EmptyState, ErrorState, LoadingState } from "../../components/Feedback";
import { Notice } from "../../components/Notice";
import { ProgressBar } from "../../components/ProgressBar";
import { Select } from "../../components/Select";
import { useToast } from "../../components/Toast";
import { t } from "../../i18n";
import {
  type BackendStatus,
  commands,
  type EmulationMode,
  events,
  type PackageControllers,
  type Pad,
  type PadInput,
} from "../../ipc";
import { useTauriEvent } from "../../ipc/events";
import { queryKeys, unwrap } from "../../ipc/query";
import { padButtonLabel } from "./controllersModel";
import { ProfileDialog } from "./ProfileDialog";
import styles from "./Settings.module.css";
import { Section } from "./SettingsPage";

function backendMessage(status: BackendStatus): {
  tone: "info" | "success" | "warning";
  text: string;
  help: string | null;
} {
  switch (status.kind) {
    case "ready":
      return {
        tone: "success",
        text: t(
          status.backend === "vigem"
            ? "controllers.backend.readyVigem"
            : status.backend === "uinput"
              ? "controllers.backend.readyUinput"
              : "controllers.backend.readyCorehid",
        ),
        help: null,
      };
    case "driver_missing":
      return {
        tone: "warning",
        text: t("controllers.backend.driverMissing"),
        help: status.help_url,
      };
    case "no_permission":
      return {
        tone: "warning",
        text: t("controllers.backend.noPermission"),
        help: status.help_url,
      };
    case "disabled_build":
      return { tone: "info", text: t("controllers.backend.disabledBuild"), help: null };
    case "not_needed":
      return { tone: "info", text: t("controllers.backend.notNeeded"), help: null };
  }
}

function PadRow({ pad }: { pad: Pad }) {
  const battery = pad.battery
    ? t(pad.battery.charging ? "controllers.batteryCharging" : "controllers.battery", {
        percent: `${pad.battery.percent}%`,
      })
    : null;
  return (
    <li className={styles.row}>
      <div className={styles.rowMain}>
        <span className={styles.rowTitle}>{pad.name}</span>
        <span>
          {[
            t(`controllers.kind.${pad.kind}`),
            t(`controllers.connection.${pad.connection}`),
            battery,
          ]
            .filter(Boolean)
            .join(" · ")}
        </span>
      </div>
      {pad.player !== null ? (
        <Badge tone="accent">{t("controllers.player", { n: pad.player })}</Badge>
      ) : null}
    </li>
  );
}

/** Live input of every pad. Updates are applied once per frame, however fast events arrive. */
function Tester({ pads }: { pads: Pad[] }) {
  const latest = useRef(new Map<number, PadInput>());
  const [shown, setShown] = useState<PadInput[]>([]);
  const frame = useRef<number | null>(null);

  useTauriEvent(events.controllerInput, (input) => {
    latest.current.set(input.instance_id, input);
    if (frame.current === null) {
      frame.current = requestAnimationFrame(() => {
        frame.current = null;
        setShown([...latest.current.values()]);
      });
    }
  });
  useEffect(
    () => () => {
      if (frame.current !== null) cancelAnimationFrame(frame.current);
    },
    [],
  );

  if (pads.length === 0) return <p>{t("controllers.tester.noPads")}</p>;
  return (
    <ul className={styles.cards} data-nav-group="">
      {pads.map((pad) => {
        const input = shown.find((s) => s.instance_id === pad.instance_id);
        return (
          <li key={pad.instance_id} className={styles.card}>
            <h4>{pad.name}</h4>
            {input ? (
              <>
                <p>
                  {input.pressed.length === 0
                    ? t("controllers.tester.nothingPressed")
                    : t("controllers.tester.pressed", {
                        buttons: input.pressed.map(padButtonLabel).join(", "),
                      })}
                </p>
                <p>
                  {t("controllers.tester.leftStick", { x: input.left[0], y: input.left[1] })}
                  {" · "}
                  {t("controllers.tester.rightStick", { x: input.right[0], y: input.right[1] })}
                </p>
                <div className={styles.meters}>
                  <ProgressBar
                    label={t("controllers.tester.leftTrigger")}
                    value={input.left_trigger / 32767}
                  />
                  <ProgressBar
                    label={t("controllers.tester.rightTrigger")}
                    value={input.right_trigger / 32767}
                  />
                </div>
              </>
            ) : (
              <p className={styles.muted}>{t("controllers.tester.waiting")}</p>
            )}
          </li>
        );
      })}
    </ul>
  );
}

function TesterSection({ pads }: { pads: Pad[] }) {
  const [on, setOn] = useState(false);
  const [error, setError] = useState(false);
  const onRef = useRef(false);
  onRef.current = on;

  // Leaving the page turns the stream off.
  useEffect(
    () => () => {
      if (onRef.current) void commands.controllerTesterStop().catch(() => undefined);
    },
    [],
  );

  const toggle = async () => {
    setError(false);
    try {
      const result = await (on
        ? commands.controllerTesterStop()
        : commands.controllerTesterStart());
      if (result.status === "error") {
        setError(true);
        return;
      }
      setOn(!on);
    } catch {
      setError(true);
    }
  };

  return (
    <div className={styles.subsection}>
      <h3>{t("controllers.tester.title")}</h3>
      <p className={styles.muted}>{t("controllers.tester.text")}</p>
      {error ? (
        <Notice tone="danger" role="alert">
          {t("controllers.tester.failed")}
        </Notice>
      ) : null}
      <div>
        <Button onClick={() => void toggle()}>
          {on ? t("controllers.tester.stop") : t("controllers.tester.start")}
        </Button>
      </div>
      {on ? <Tester pads={pads} /> : null}
    </div>
  );
}

function GameRow({ game, onEdit }: { game: PackageControllers; onEdit: () => void }) {
  const client = useQueryClient();
  const { toast } = useToast();
  const set = async (mode: EmulationMode) => {
    const result = await commands.controllerEmulationSet(game.package, mode).catch(() => null);
    if (result?.status === "ok") {
      await client.invalidateQueries({ queryKey: queryKeys.controllerPackages });
      toast({ tone: "success", title: t("controllers.games.saved", { title: game.title }) });
    } else toast({ tone: "danger", title: t("controllers.games.failed") });
  };
  return (
    <li className={styles.row}>
      <div className={styles.rowMain}>
        <span className={styles.rowTitle}>{game.title}</span>
        {game.compat_layer ? <span>{t("controllers.games.compat")}</span> : null}
        {game.profile ? (
          <span>
            <Badge tone="info">{t("controllers.games.custom")}</Badge>
          </span>
        ) : null}
      </div>
      <div className={styles.cardHeader}>
        {game.compat_layer ? null : (
          <Select<EmulationMode>
            label={t("controllers.games.emulation", { title: game.title })}
            hideLabel
            value={game.emulation}
            options={[
              { value: "auto", label: t("controllers.games.auto") },
              { value: "always", label: t("controllers.games.always") },
              { value: "never", label: t("controllers.games.never") },
            ]}
            onChange={(mode) => void set(mode)}
          />
        )}
        <Button aria-label={t("controllers.games.editFor", { title: game.title })} onClick={onEdit}>
          {t("controllers.games.edit")}
        </Button>
      </div>
    </li>
  );
}

export function ControllersSection() {
  const overview = useQuery({
    queryKey: queryKeys.controllers,
    queryFn: async () => unwrap(await commands.controllersOverview()),
  });
  const games = useQuery({
    queryKey: queryKeys.controllerPackages,
    queryFn: async () => unwrap(await commands.controllersPackages()),
  });
  const [editing, setEditing] = useState<"default" | PackageControllers | null>(null);

  let body: React.ReactNode;
  if (overview.data) {
    const { pads, backend } = overview.data;
    const status = backendMessage(backend);
    body = (
      <>
        <div className={styles.subsection}>
          <h3>{t("controllers.connected")}</h3>
          {pads.length === 0 ? (
            <EmptyState title={t("controllers.none")} description={t("controllers.noneHint")} />
          ) : (
            <ul className={styles.rows} data-nav-group="">
              {pads.map((p) => (
                <PadRow key={p.instance_id} pad={p} />
              ))}
            </ul>
          )}
        </div>
        <TesterSection pads={pads} />
        <div className={styles.subsection}>
          <h3>{t("controllers.backend.title")}</h3>
          <Notice tone={status.tone}>
            {status.text}
            {status.help ? (
              <>
                {" "}
                <Button size="sm" onClick={() => void commands.openExternalUrl(status.help ?? "")}>
                  {t("controllers.backend.help")}
                </Button>
              </>
            ) : null}
          </Notice>
        </div>
      </>
    );
  } else if (overview.isError) {
    body = <ErrorState title={t("error.generic")} onRetry={() => void overview.refetch()} />;
  } else body = <LoadingState />;

  return (
    <Section
      id="controllers"
      title={t("settings.section.controllers")}
      text={t("controllers.text")}
    >
      {body}
      <div className={styles.subsection}>
        <h3>{t("controllers.profile.defaultTitle")}</h3>
        <p className={styles.muted}>{t("controllers.profile.defaultText")}</p>
        <div>
          <Button onClick={() => setEditing("default")}>
            {t("controllers.profile.editDefault")}
          </Button>
        </div>
      </div>
      <div className={styles.subsection}>
        <h3>{t("controllers.games.title")}</h3>
        <p className={styles.muted}>{t("controllers.games.text")}</p>
        {games.data ? (
          games.data.length === 0 ? (
            <p>{t("controllers.games.empty")}</p>
          ) : (
            <ul className={styles.rows} data-nav-group="">
              {games.data.map((g) => (
                <GameRow key={g.package.package_id} game={g} onEdit={() => setEditing(g)} />
              ))}
            </ul>
          )
        ) : games.isError ? (
          <ErrorState title={t("error.generic")} onRetry={() => void games.refetch()} />
        ) : (
          <LoadingState />
        )}
      </div>
      {editing ? (
        <ProfileDialog
          game={editing === "default" ? null : editing}
          onClose={() => setEditing(null)}
        />
      ) : null}
    </Section>
  );
}
