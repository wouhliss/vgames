import { useToast } from "../components/Toast";
import { type DeeplinkRefusalReason, events } from "../ipc";
import { useTauriEvent } from "../ipc/events";

const MESSAGES: Record<DeeplinkRefusalReason, string> = {
  rate_limited: "Wait a moment before opening another game link.",
  not_installed: "Install this game before opening its launch link.",
  not_verified: "Verify this game's files before opening its launch link.",
  unknown_package:
    "This game link is unavailable on the selected server. Check that the correct server is selected.",
};

export function DeeplinkNotices(): null {
  const { toast } = useToast();
  useTauriEvent(events.deeplinkRefused, ({ reason }) => {
    toast({ title: "Game link could not open", description: MESSAGES[reason], tone: "warning" });
  });
  return null;
}
