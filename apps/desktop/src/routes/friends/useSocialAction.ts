import { useState } from "react";
import { useToast } from "../../components/Toast";
import type { Result, SocialError } from "../../ipc";
import { socialErrorText } from "./model";

/**
 * Runs a social command, marks its key busy meanwhile, and toasts the typed error as a sentence.
 * Resolves to `{ data }`, or null when the command failed.
 */
export function useSocialAction() {
  const { toast } = useToast();
  const [busy, setBusy] = useState<ReadonlySet<string>>(() => new Set());
  const run = async <T>(
    key: string,
    action: () => Promise<Result<T, SocialError>>,
    success?: (data: T) => string,
  ): Promise<{ data: T } | null> => {
    setBusy((prev) => new Set(prev).add(key));
    try {
      const result = await action();
      if (result.status === "error") {
        toast({ tone: "danger", title: socialErrorText(result.error) });
        return null;
      }
      if (success) toast({ tone: "success", title: success(result.data) });
      return { data: result.data };
    } catch {
      toast({ tone: "danger", title: socialErrorText(null) });
      return null;
    } finally {
      setBusy((prev) => {
        const next = new Set(prev);
        next.delete(key);
        return next;
      });
    }
  };
  return { run, busy };
}
