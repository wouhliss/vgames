// Everything that must exist on every screen: deep-link routing into onboarding, the trust block,
// and Escape/B as "back" when no layer consumed it.
import type { UnlistenFn } from "@tauri-apps/api/event";
import { useEffect, useState } from "react";
import { Outlet, useLocation, useNavigate } from "react-router";
import { commands, events, type TrustProblem } from "../ipc";
import { useTauriEvent } from "../ipc/events";
import { useBackHandler } from "../nav/NavProvider";
import { useServers } from "./queries";
import { TrustBlock } from "./TrustBlock";

export interface OnboardingLinkState {
  url: string;
  fingerprint: string;
}

export function RootLayout() {
  // Throwaway INT-10 negative proof. This branch must never be merged.
  useEffect(() => {
    const image = document.createElement("img");
    image.src = "data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='32' height='32'%3E%3Crect width='32' height='32' fill='blue'/%3E%3C/svg%3E";
    image.width = 32;
    image.height = 32;
    image.style.position = "fixed";
    image.style.top = "0";
    image.style.left = "0";
    document.body.append(image);
    return () => image.remove();
  }, []);
  const navigate = useNavigate();
  const location = useLocation();
  const servers = useServers();
  const [trustProblem, setTrustProblem] = useState<TrustProblem | null>(null);

  useEffect(() => {
    let disposed = false;
    let unlisten: UnlistenFn | undefined;
    events.serverAddRequested
      .listen(({ payload }) => {
        if (disposed) return;
        const state: OnboardingLinkState = { url: payload.url, fingerprint: payload.fingerprint };
        navigate("/onboarding", { state });
      })
      .then((stopListening) => {
        if (disposed) {
          stopListening();
          return;
        }
        unlisten = stopListening;
        // This layout commits after the initial route loads. Readiness also releases startup
        // deep links, so acknowledge their listener first. Hidden WebKitGTK suspends RAFs.
        return commands.appReady();
      })
      .catch(() => {
        // Outside Tauri there is no window to show. Native startup keeps its 15 s fallback.
      });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [navigate]);
  useTauriEvent(events.trustProblem, setTrustProblem);

  useBackHandler(() => {
    const idx = (window.history.state as { idx?: number } | null)?.idx ?? 0;
    if (idx > 0 && location.pathname !== "/library") {
      navigate(-1);
      return true;
    }
    return false;
  });

  const otherServers = (servers.data ?? []).filter((s) => s.id !== trustProblem?.server_id);

  return (
    <>
      <Outlet />
      {trustProblem ? (
        <TrustBlock
          problem={trustProblem}
          otherServers={otherServers}
          onResolved={() => setTrustProblem(null)}
        />
      ) : null}
    </>
  );
}
