// Everything that must exist on every screen: deep-link routing into onboarding, the trust block,
// and Escape/B as "back" when no layer consumed it.
import { useState } from "react";
import { Outlet, useLocation, useNavigate } from "react-router";
import { events, type TrustProblem } from "../ipc";
import { useTauriEvent } from "../ipc/events";
import { useBackHandler } from "../nav/NavProvider";
import { useServers } from "./queries";
import { TrustBlock } from "./TrustBlock";

export interface OnboardingLinkState {
  url: string;
  fingerprint: string;
}

export function RootLayout() {
  const navigate = useNavigate();
  const location = useLocation();
  const servers = useServers();
  const [trustProblem, setTrustProblem] = useState<TrustProblem | null>(null);

  useTauriEvent(events.serverAddRequested, (payload) => {
    const state: OnboardingLinkState = { url: payload.url, fingerprint: payload.fingerprint };
    navigate("/onboarding", { state });
  });
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
