// Geometry for the Fairspoken mark explorations. All marks live on a 64 × 64 grid.
export const C = { ink: '#0E1318', blue: '#2C4BD0', red: '#E5512B', paper: '#F3F1EA', rule: '#C5D1EC', night: '#0A0D12', mist: '#ECEEF1', blue300: '#93A8FF', red400: '#FF7148' };
const f = (n) => +n.toFixed(2);

// Smooth path through points (Catmull-Rom → cubic Bézier).
export function smooth(pts, tension = 1) {
  let d = `M${f(pts[0][0])} ${f(pts[0][1])}`;
  for (let i = 0; i < pts.length - 1; i++) {
    const p0 = pts[i - 1] || pts[i], p1 = pts[i], p2 = pts[i + 1], p3 = pts[i + 2] || p2;
    const c1 = [p1[0] + (p2[0] - p0[0]) / 6 * tension, p1[1] + (p2[1] - p0[1]) / 6 * tension];
    const c2 = [p2[0] - (p3[0] - p1[0]) / 6 * tension, p2[1] - (p3[1] - p1[1]) / 6 * tension];
    d += ` C${f(c1[0])} ${f(c1[1])} ${f(c2[0])} ${f(c2[1])} ${f(p2[0])} ${f(p2[1])}`;
  }
  return d;
}

// A. "Settle": a damped voice wave that settles into one straight line and stops at a caret.
export function settlePath({ x0 = 7, x1 = 41, x2 = 47, y = 32, amp = 15, cycles = 2.25, steps = 48 } = {}) {
  const pts = [];
  for (let i = 0; i <= steps; i++) {
    const t = i / steps, x = x0 + (x1 - x0) * t;
    const env = Math.pow(1 - t, 1.35);
    pts.push([x, y - amp * env * Math.sin(Math.PI * 2 * cycles * t)]);
  }
  return smooth(pts) + ` L${x2} ${y}`;
}

// B. "Rough / fair": a scribbled line above a clean one, a margin rule on the left.
export function roughPath({ x0 = 18, x1 = 54, y = 23 } = {}) {
  const pts = []; let seed = 7;
  const rnd = () => ((seed = (seed * 16807) % 2147483647) / 2147483647) - 0.5;
  for (let x = x0; x <= x1; x += 3) pts.push([x, y + rnd() * 9]);
  return smooth(pts, 1.2);
}

// D. "Quote to line": a speech-mark curl that runs out into a ruled line.
export const quotePath = 'M20 14 C11 16 9 27 15 30 C20 32.5 25 28 22.5 23 C21 20 17 20.5 15.5 23 M15 30 C17 37 21 41 28 42 L54 42';
