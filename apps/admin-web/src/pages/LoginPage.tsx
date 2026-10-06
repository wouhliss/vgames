import { useMutation } from "@tanstack/react-query";
import { Navigate, useSearchParams } from "react-router";
import { call, client } from "../api/http";
import { AuthStartResponseSchema } from "../api/schemas";
import { signedInAgain } from "../app/drafts";
import { ErrorView } from "../app/ErrorView";
import { safeReturnTo, useMe } from "../app/session";

const CALLBACK_ERRORS: Record<string, string> = {
  registration_closed: "This server isn't accepting new members.",
  not_allowlisted: "Your Discord account isn't on this server's allowlist.",
  user_disabled: "Your account has been disabled.",
  access_denied: "Sign-in was cancelled in Discord.",
};

/** Where the browser goes after Discord; replaced in tests. */
export const navigation = { assign: (url: string) => window.location.assign(url) };

export function LoginPage() {
  const [params] = useSearchParams();
  const returnTo = safeReturnTo(params.get("return_to"));
  const callbackError = params.get("error");
  const me = useMe();
  const start = useMutation({
    mutationFn: async () => {
      const { data } = await call(AuthStartResponseSchema, (o) =>
        client.POST("/v1/auth/discord/start", {
          ...o,
          body: { client: "web", return_to: returnTo },
        }),
      );
      return data;
    },
    onSuccess: (data) => navigation.assign(data.authorize_url),
  });

  if (me.isSuccess) {
    // Still signed in (the 401 was a blip): leave again with the usual unsaved-changes prompts.
    signedInAgain();
    return <Navigate to={returnTo.replace(/^\/admin/, "") || "/"} replace />;
  }

  return (
    <main className="center">
      <h1>Sign in</h1>
      <p>Sign in with the Discord account that is an admin on this server.</p>
      {callbackError ? (
        <div className="banner error" role="alert">
          {CALLBACK_ERRORS[callbackError] ?? "Sign-in failed. Try again."}
        </div>
      ) : null}
      {start.error ? <ErrorView error={start.error} onRetry={() => start.mutate()} /> : null}
      <button
        type="button"
        className="primary"
        disabled={start.isPending || start.isSuccess}
        onClick={() => start.mutate()}
      >
        {start.isPending || start.isSuccess ? "Opening Discord…" : "Sign in with Discord"}
      </button>
    </main>
  );
}
