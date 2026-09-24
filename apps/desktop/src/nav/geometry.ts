// Geometric focus movement for arrow keys, the D-pad and the left stick.
//
// Every direction is reduced to "down" by transforming coordinates, so the scoring rule is written
// once. A candidate qualifies when it lies beyond the origin in that direction; the best one has the
// smallest gap along the direction plus a weighted gap across it, so items that overlap the origin's
// column (or row) win over closer-but-diagonal ones.

export type Direction = "up" | "down" | "left" | "right";

export interface Rect {
  readonly left: number;
  readonly top: number;
  readonly right: number;
  readonly bottom: number;
}

const EPS = 1;
const CROSS_AXIS_WEIGHT = 2;
const ALIGNMENT_WEIGHT = 0.05;

function toDown(r: Rect, dir: Direction): Rect {
  switch (dir) {
    case "down":
      return r;
    case "up":
      return { left: r.left, right: r.right, top: -r.bottom, bottom: -r.top };
    case "right":
      return { left: r.top, right: r.bottom, top: r.left, bottom: r.right };
    case "left":
      return { left: r.top, right: r.bottom, top: -r.right, bottom: -r.left };
  }
}

export function isEmptyRect(r: Rect): boolean {
  return r.right - r.left <= 0 && r.bottom - r.top <= 0;
}

/** Score of moving from `origin` to `candidate` in `dir`, or `null` if it does not lie that way. */
export function score(origin: Rect, candidate: Rect, dir: Direction): number | null {
  const o = toDown(origin, dir);
  const c = toDown(candidate, dir);
  const oCenterY = (o.top + o.bottom) / 2;
  const cCenterY = (c.top + c.bottom) / 2;
  // Beyond the origin: entirely below it, or (for overlapping shapes) centered lower and extending further.
  const fullyBeyond = c.top >= o.bottom - EPS;
  const partlyBeyond = cCenterY > oCenterY + EPS && c.bottom > o.bottom + EPS;
  if (!fullyBeyond && !partlyBeyond) return null;
  const mainGap = Math.max(0, c.top - o.bottom);
  const crossGap = Math.max(0, c.left - o.right, o.left - c.right);
  const alignment = Math.abs((c.left + c.right) / 2 - (o.left + o.right) / 2);
  return mainGap + crossGap * CROSS_AXIS_WEIGHT + alignment * ALIGNMENT_WEIGHT;
}

/** Index of the best candidate in `dir`, or -1 when nothing lies that way. */
export function pickNext(origin: Rect, candidates: readonly Rect[], dir: Direction): number {
  let best = -1;
  let bestScore = Number.POSITIVE_INFINITY;
  candidates.forEach((candidate, index) => {
    if (isEmptyRect(candidate)) return;
    const s = score(origin, candidate, dir);
    if (s !== null && s < bestScore) {
      bestScore = s;
      best = index;
    }
  });
  return best;
}
