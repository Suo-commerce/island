import { createRouter, rootRouteId } from "@tanstack/react-router";
import { describe, expect, it } from "vitest";

import { routeTree } from "@/routeTree.gen";

describe("App routing", () => {
  it("has a screen at every address the shell links to", () => {
    const router = createRouter({ routeTree });
    for (const path of ["/", "/session", "/sing", "/agent", "/island"]) {
      expect(router.matchRoutes(path).at(-1)?.routeId, path).not.toBe(rootRouteId);
    }
  });
});
