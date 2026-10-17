// Layer pixels ↔ frame pixels, with the 3×3 matrices the editor sends
// (`layer_map` takes a point in the layer's own pixels, measured from its
// centre, to the frame; perspective included for corner-pinned layers).

import type { Pt } from "./api";

export function apply(m: number[], [x, y]: Pt): Pt {
  const w = m[6] * x + m[7] * y + m[8];
  return [(m[0] * x + m[1] * y + m[2]) / w, (m[3] * x + m[4] * y + m[5]) / w];
}

/// A point in layer pixels (top left = 0, 0) to frame pixels.
export function layerToFrame(map: number[], size: Pt, p: Pt): Pt {
  return apply(map, [p[0] - size[0] / 2, p[1] - size[1] / 2]);
}

/// A frame pixel back to layer pixels.
export function frameToLayer(inv: number[], size: Pt, p: Pt): Pt {
  const [x, y] = apply(inv, p);
  return [x + size[0] / 2, y + size[1] / 2];
}

/// Is `p` inside polygon `poly`?
export function inside(poly: Pt[], p: Pt): boolean {
  let c = false;
  for (let i = 0, j = poly.length - 1; i < poly.length; j = i++) {
    const [a, b] = [poly[i], poly[j]];
    if (a[1] > p[1] !== b[1] > p[1] && p[0] < a[0] + ((p[1] - a[1]) / (b[1] - a[1])) * (b[0] - a[0])) c = !c;
  }
  return c;
}
