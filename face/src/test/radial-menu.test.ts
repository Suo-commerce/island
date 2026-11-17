import { describe, expect, it } from "vitest";
import { radialSelectionRotation } from "@/lib/radial-menu";

describe("Radial menu selection", () => {
  it("places every selected sector midpoint directly below the top pointer", () => {
    for (let index = 0; index < 7; index++) {
      const rotation = radialSelectionRotation(0, index, 7);
      const midpoint = (index + 0.5) * (360 / 7);
      expect(((rotation + midpoint) % 360 + 360) % 360).toBeCloseTo(0);
    }
  });

  it("takes the shortest turn when crossing the last and first sectors", () => {
    const current = radialSelectionRotation(0, 6, 7);
    const next = radialSelectionRotation(current, 0, 7);
    expect(next - current).toBeCloseTo(-360 / 7);
  });
});