// The Curves effect's curve: a smooth line through the points that never
// overshoots (monotone cubic, Fritsch–Carlson), flat past the end points.
// The same maths as reelforge_core::lut::eval_curve, so what's drawn is what renders.

export type P = [number, number];

export function evalCurve(points: P[], x: number): number {
  const p = [...points].sort((a, b) => a[0] - b[0]).filter((v, i, a) => i === 0 || Math.abs(v[0] - a[i - 1][0]) >= 1e-6);
  const n = p.length;
  if (n === 0) return x;
  if (n === 1) return p[0][1];
  if (x <= p[0][0]) return p[0][1];
  if (x >= p[n - 1][0]) return p[n - 1][1];
  const d: number[] = [];
  for (let i = 0; i < n - 1; i++) d.push((p[i + 1][1] - p[i][1]) / (p[i + 1][0] - p[i][0]));
  const m = new Array(n).fill(0);
  m[0] = d[0];
  m[n - 1] = d[n - 2];
  for (let i = 1; i < n - 1; i++) m[i] = d[i - 1] * d[i] <= 0 ? 0 : (d[i - 1] + d[i]) / 2;
  for (let i = 0; i < n - 1; i++) {
    if (d[i] === 0) {
      m[i] = 0;
      m[i + 1] = 0;
    } else {
      const a = m[i] / d[i];
      const b = m[i + 1] / d[i];
      const s = a * a + b * b;
      if (s > 9) {
        const t = 3 / Math.sqrt(s);
        m[i] = t * a * d[i];
        m[i + 1] = t * b * d[i];
      }
    }
  }
  let i = p.findIndex((_, k) => k < n - 1 && x >= p[k][0] && x <= p[k + 1][0]);
  if (i < 0) i = n - 2;
  const h = p[i + 1][0] - p[i][0];
  const t = (x - p[i][0]) / h;
  const t2 = t * t;
  const t3 = t2 * t;
  return (2 * t3 - 3 * t2 + 1) * p[i][1] + (t3 - 2 * t2 + t) * h * m[i] + (-2 * t3 + 3 * t2) * p[i + 1][1] + (t3 - t2) * h * m[i + 1];
}
