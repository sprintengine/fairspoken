// WCAG 2.2 contrast check for the Fairspoken palette. `node contrast.mjs` prints a Markdown table.
const hex = (h) => h.replace('#', '').match(/../g).map((x) => parseInt(x, 16) / 255);
const lin = (c) => (c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4);
const L = (h) => { const [r, g, b] = hex(h).map(lin); return 0.2126 * r + 0.7152 * g + 0.0722 * b; };
const ratio = (a, b) => { const [x, y] = [L(a), L(b)].sort((m, n) => n - m); return (x + 0.05) / (y + 0.05); };
export const pairs = [
  // [mode, role, fg, bg, minimum]
  ['light', 'Body text: Bog Oak on Limestone', '#0E1318', '#F3F1EA', 4.5],
  ['light', 'Secondary text: Slate 700 on Limestone', '#3D4752', '#F3F1EA', 4.5],
  ['light', 'Muted text: Slate 500 on Limestone', '#5E6873', '#F3F1EA', 4.5],
  ['light', 'Muted text: Slate 500 on Paper White', '#5E6873', '#FBFAF6', 4.5],
  ['light', 'Link / primary: Copybook Blue on Limestone', '#2C4BD0', '#F3F1EA', 4.5],
  ['light', 'Button label: white on Copybook Blue', '#FFFFFF', '#2C4BD0', 4.5],
  ['light', 'Signal text: Margin Red 600 on Limestone', '#B23B16', '#F3F1EA', 4.5],
  ['light', 'Margin Red (graphic) on Limestone', '#E5512B', '#F3F1EA', 3.0],
  ['light', 'Success on Limestone', '#1D7347', '#F3F1EA', 4.5],
  ['light', 'Warning (Gorse 700) on Limestone', '#8F5A00', '#F3F1EA', 4.5],
  ['light', 'Error on Limestone', '#B21F3B', '#F3F1EA', 4.5],
  ['light', 'Ruling line (decorative) on Limestone', '#C5D1EC', '#F3F1EA', 1.0],
  ['dark', 'Body text: Mist on Night', '#ECEEF1', '#0A0D12', 4.5],
  ['dark', 'Secondary text: Slate 300 on Night', '#B0B8C3', '#0A0D12', 4.5],
  ['dark', 'Muted text: Slate 400 on Night', '#8B95A2', '#0A0D12', 4.5],
  ['dark', 'Muted text: Slate 400 on Night Surface', '#8B95A2', '#141922', 4.5],
  ['dark', 'Link / primary: Copybook Blue 300 on Night', '#93A8FF', '#0A0D12', 4.5],
  ['dark', 'Button label: Night on Copybook Blue 300', '#0A0D12', '#93A8FF', 4.5],
  ['dark', 'Signal: Margin Red 400 on Night', '#FF7148', '#0A0D12', 4.5],
  ['dark', 'Success on Night', '#5ED39A', '#0A0D12', 4.5],
  ['dark', 'Warning (Gorse 300) on Night', '#F4BE4F', '#0A0D12', 4.5],
  ['dark', 'Error on Night', '#FF7A8F', '#0A0D12', 4.5],
];
const rows = pairs.map(([m, role, fg, bg, min]) => {
  const r = ratio(fg, bg);
  const grade = r >= 7 ? 'AAA' : r >= 4.5 ? 'AA' : r >= 3 ? 'AA large / graphics' : 'decorative only';
  return `| ${m} | ${role} | \`${fg}\` on \`${bg}\` | ${r.toFixed(2)} : 1 | ${grade} | ${r >= min ? 'pass' : 'FAIL'} |`;
});
console.log('| Mode | Use | Colours | Ratio | Grade | Meets target |\n|---|---|---|---|---|---|\n' + rows.join('\n'));
