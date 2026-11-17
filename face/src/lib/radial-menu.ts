/** Place a sector's midpoint at twelve o'clock using the shortest turn. */
export function radialSelectionRotation(current: number, index: number, count: number) {
  const target = -(index + 0.5) * (360 / count);
  const delta = ((target - current + 540) % 360 + 360) % 360 - 180;
  return current + delta;
}