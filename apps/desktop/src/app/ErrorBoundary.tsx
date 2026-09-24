// Last line of defense: a render error shows a calm screen with "copy diagnostics" instead of a
// blank window. Diagnostics come from Rust, already redacted (no tokens, ids or home paths); the UI
// adds only the error name and message, never the component state.
import { Component, type ErrorInfo, type ReactNode, useState } from "react";
import { isRouteErrorResponse, useRouteError } from "react-router";
import { Button } from "../components/Button";
import { copyText, ErrorState } from "../components/Feedback";
import { t } from "../i18n";
import { commands } from "../ipc";
import styles from "./ErrorBoundary.module.css";

export function describeError(error: unknown): string {
  if (isRouteErrorResponse(error)) return `route ${error.status}`;
  if (error instanceof Error) return `${error.name}: ${error.message}`.slice(0, 500);
  return "unknown error";
}

export async function buildDiagnostics(error: unknown): Promise<string> {
  let core = "";
  try {
    core = await commands.appDiagnostics();
  } catch {
    core = "(core diagnostics unavailable)";
  }
  return `${core}\nui error: ${describeError(error)}\nui route: ${window.location.pathname}`;
}

export function CrashScreen({ error }: { error: unknown }) {
  const [status, setStatus] = useState<"idle" | "copied" | "failed">("idle");
  return (
    <div className={styles.page}>
      <ErrorState
        title={t("error.generic")}
        description={
          status === "copied"
            ? t("error.diagnosticsCopied")
            : status === "failed"
              ? t("error.diagnosticsFailed")
              : t("error.genericDescription")
        }
        action={
          <>
            <Button variant="primary" icon="refresh" onClick={() => window.location.assign("/")}>
              {t("error.reload")}
            </Button>
            <Button
              icon="copy"
              onClick={async () => {
                const ok = await copyText(await buildDiagnostics(error));
                setStatus(ok ? "copied" : "failed");
              }}
            >
              {t("error.copyDetails")}
            </Button>
          </>
        }
      />
    </div>
  );
}

/** `errorElement` for routes. */
export function RouteError() {
  return <CrashScreen error={useRouteError()} />;
}

/** Catches errors outside the router (providers, the router itself). */
export class ErrorBoundary extends Component<{ children: ReactNode }, { error: unknown }> {
  override state: { error: unknown } = { error: null };

  static getDerivedStateFromError(error: unknown) {
    return { error };
  }

  override componentDidCatch(error: unknown, info: ErrorInfo) {
    if (import.meta.env.DEV) console.error(error, info.componentStack);
  }

  override render() {
    return this.state.error ? <CrashScreen error={this.state.error} /> : this.props.children;
  }
}
