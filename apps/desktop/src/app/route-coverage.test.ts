import type { RouteObject } from "react-router";
import { describe, expect, it } from "vitest";
import { ROUTE_SCANS } from "../../e2e/route-scans";
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
  it("scans every declared route, including development and redirect paths", () => {
    const planned = ROUTE_SCANS.map((route) => route.pattern);
    expect(new Set(planned).size).toBe(planned.length);
    expect([...new Set(paths(routes))].sort()).toEqual([...planned].sort());
  });
});
