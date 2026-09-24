import { useQuery } from "@tanstack/react-query";
import { call, client, setCsrfFallback } from "../api/http";
import { MeSchema } from "../api/schemas";

export const meKey = ["me"] as const;

export function useMe() {
  return useQuery({
    queryKey: meKey,
    queryFn: async ({ signal }) => {
      const { data } = await call(MeSchema, (o) => client.GET("/v1/me", o), signal);
      setCsrfFallback(data.csrf_token ?? null);
      return data;
    },
    staleTime: 60_000,
  });
}

const RETURN_TO = /^\/admin(\/[A-Za-z0-9._~/-]*)?$/;

/** The current page as a `return_to` value the API accepts (path only, no query). */
export function currentReturnTo(pathname = window.location.pathname): string {
  return RETURN_TO.test(pathname) ? pathname : "/admin/";
}

export function safeReturnTo(value: string | null): string {
  return value && RETURN_TO.test(value) ? value : "/admin/";
}
