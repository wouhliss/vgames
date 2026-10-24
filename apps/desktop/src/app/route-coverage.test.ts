import type { RouteObject } from "react-router";
import { describe, expect, it } from "vitest";
import { ROUTE_SCANS } from "../../e2e/route-scans";
import readme from "../../README.md?raw";
import { routes } from "./router";

function paths(nodes: RouteObject[], parent = ""): string[] {
  return nodes.flatMap((route) => {
    const path =
      route.path === undefined
        ? parent
        : (route.path.startsWith("/") ? route.path : `${parent}/${route.path}`).replace(
            /\/+/g,
            "/",
          );
    return [...(route.path === undefined ? [] : [path]), ...paths(route.children ?? [], path)];
  });
}

describe("launcher route accessibility coverage", () => {
  it("documents every route in the input walkthrough", () => {
    const table = readme
      .split("<!-- int-route-walkthrough:start -->")[1]
      ?.split("<!-- int-route-walkthrough:end -->")[0];
    if (!table) throw new Error("The route walkthrough table is missing");
    const documented = [...table.matchAll(/^\| `([^`]+)` \|/gm)].map((match) => match[1]);
    expect(documented.sort()).toEqual(ROUTE_SCANS.map((route) => route.pattern).sort());
  });
  it("scans every declared route, including development and redirect paths", () => {
    const planned = ROUTE_SCANS.map((route) => route.pattern);
    expect(new Set(planned).size).toBe(planned.length);
    expect([...new Set(paths(routes))].sort()).toEqual([...planned].sort());
  });
});
