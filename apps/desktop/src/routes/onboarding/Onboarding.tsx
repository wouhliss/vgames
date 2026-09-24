// First run and "add a server": address → fingerprint check → Discord sign-in → library folder →
// done. Also the landing point for `vgames://server/add` links (RootLayout routes them here with
// the link's URL and fingerprint in the navigation state).
import { useQueryClient } from "@tanstack/react-query";
import { useRef, useState } from "react";
import { Navigate, useLocation, useNavigate, useSearchParams } from "react-router";
import { useActiveServer, useLibraries, useOnboardingNeed } from "../../app/queries";
import type { OnboardingLinkState } from "../../app/RootLayout";
import { Button } from "../../components/Button";
import { ErrorState, LoadingState } from "../../components/Feedback";
import { t } from "../../i18n";
import { commands, type ServerPreview, type ServerProfile } from "../../ipc";
import { queryKeys } from "../../ipc/query";
import { CheckStep } from "./CheckStep";
import { LibraryStep } from "./LibraryStep";
import { Notice } from "./Notice";
import styles from "./Onboarding.module.css";
import { ServerStep } from "./ServerStep";
import { SignInStep } from "./SignInStep";
import { StepHeading } from "./StepHeading";

type Step =
  | { kind: "server"; message: string | null }
  | { kind: "check"; preview: ServerPreview }
  | { kind: "sign_in"; server: ServerProfile }
  | { kind: "library" }
  | { kind: "done" };

const ORDER = ["server", "check", "sign_in", "library"] as const;

export function Onboarding() {
  const { need, loading, error } = useOnboardingNeed();
  const active = useActiveServer();
  const location = useLocation();
  const [params] = useSearchParams();
  const link = (location.state as OnboardingLinkState | null) ?? null;
  const adding = params.get("add") === "1";
  // Decided once per navigation: steps change the data this is computed from (e.g. adding the
  // library completes onboarding), and the flow must still reach its "done" step.
  const started = useRef<{ key: string; initial: Step; adding: boolean } | null>(null);

  if (started.current?.key !== location.key) {
    if (loading) return <LoadingState />;
    if (error)
      return <ErrorState title={t("error.generic")} onRetry={() => void active.refetch()} />;
    if (need === null && !adding && !link) return <Navigate to="/library" replace />;
    let initial: Step;
    if (link || adding || need === "server" || !active.server)
      initial = { kind: "server", message: null };
    else if (need === "sign_in") initial = { kind: "sign_in", server: active.server };
    else initial = { kind: "library" };
    started.current = { key: location.key, initial, adding: adding && need === null };
  }

  // A new link (new navigation) restarts the flow.
  return (
    <Flow
      key={location.key}
      initial={started.current.initial}
      link={link}
      adding={started.current.adding}
    />
  );
}

function Flow({
  initial,
  link,
  adding,
}: {
  initial: Step;
  link: OnboardingLinkState | null;
  adding: boolean;
}) {
  const [step, setStep] = useState<Step>(initial);
  const [lastUrl, setLastUrl] = useState(link?.url ?? "");
  const navigate = useNavigate();
  const client = useQueryClient();
  const libraries = useLibraries();

  const afterSignIn = async () => {
    await client.invalidateQueries({ queryKey: queryKeys.servers });
    setStep((libraries.data ?? []).length > 0 ? { kind: "done" } : { kind: "library" });
  };

  const current = step.kind === "done" ? ORDER.length : ORDER.indexOf(step.kind);
  const labels = [
    t("onboarding.stepServer"),
    t("onboarding.stepCheck"),
    t("onboarding.stepSignIn"),
    t("onboarding.stepLibrary"),
  ];

  return (
    <div className={styles.page}>
      <main className={styles.panel}>
        <nav aria-label={t("onboarding.title")}>
          <ol className={styles.steps}>
            {labels.map((label, i) => (
              <li
                key={label}
                className={styles.step}
                aria-current={i === current ? "step" : undefined}
                data-state={i < current ? "done" : undefined}
              >
                {label}
              </li>
            ))}
          </ol>
        </nav>
        {step.kind === "server" ? (
          <>
            {step.message ? (
              <Notice tone="danger" role="alert">
                {step.message}
              </Notice>
            ) : null}
            <ServerStep
              adding={adding}
              link={step.message === null ? link : null}
              initialUrl={lastUrl}
              onPreview={(preview) => {
                setLastUrl(preview.url);
                setStep({ kind: "check", preview });
              }}
              onSwitchExisting={async (serverId) => {
                const result = await commands.serverSwitch(serverId);
                if (result.status === "ok") {
                  await client.invalidateQueries();
                  navigate("/library", { replace: true });
                }
              }}
              onCancel={adding ? () => navigate("/library") : null}
              onRestart={() => navigate("/onboarding", { replace: true, state: null })}
            />
          </>
        ) : null}
        {step.kind === "check" ? (
          <CheckStep
            preview={step.preview}
            onConfirmed={(server) => {
              void client.invalidateQueries({ queryKey: queryKeys.servers });
              setStep({ kind: "sign_in", server });
            }}
            onBack={(message) => setStep({ kind: "server", message })}
          />
        ) : null}
        {step.kind === "sign_in" ? (
          <SignInStep server={step.server} onSignedIn={() => void afterSignIn()} />
        ) : null}
        {step.kind === "library" ? <LibraryStep onDone={() => setStep({ kind: "done" })} /> : null}
        {step.kind === "done" ? (
          <section className={styles.card} aria-labelledby="step-title">
            <StepHeading title={t("onboarding.done.title")} text={t("onboarding.done.text")} />
            <div className={styles.actions}>
              <Button
                variant="primary"
                size="lg"
                onClick={() => navigate("/library", { replace: true })}
                data-autofocus=""
              >
                {t("onboarding.done.action")}
              </Button>
            </div>
          </section>
        ) : null}
      </main>
    </div>
  );
}
