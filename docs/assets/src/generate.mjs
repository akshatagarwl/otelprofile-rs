import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import rough from 'roughjs';

const here = path.dirname(fileURLToPath(import.meta.url));
const out = path.resolve(here, '..');
const font = fs.readFileSync(path.join(here, 'node_modules/@fontsource/kalam/files/kalam-latin-400-normal.woff2')).toString('base64');
const ink = '#252a34', blue = '#4263eb', green = '#087f5b', orange = '#d9480f';
const esc = (s) => s.replaceAll('&', '&amp;').replaceAll('<', '&lt;');

function drawing(title, height = 330) {
  const g = rough.generator(), parts = [];
  let seed = 10;
  const draw = (shape) => parts.push(...g.toPaths(shape).map(p => `<path d="${p.d}" stroke="${p.stroke}" stroke-width="${p.strokeWidth}" fill="${p.fill ?? 'none'}"/>`));
  const options = (fill) => ({seed: seed++, roughness: 1.1, bowing: 1, stroke: ink, strokeWidth: 2, fill, fillStyle: 'hachure', hachureGap: 9});
  const text = (x, y, s, size = 22, color = ink) => parts.push(`<text x="${x}" y="${y}" text-anchor="middle" font-size="${size}" fill="${color}">${esc(s)}</text>`);
  const box = (x, y, w, h, fill, lines) => {
    draw(g.rectangle(x, y, w, h, options(fill)));
    lines.forEach((line, i) => text(x + w / 2, y + h / 2 + (i - (lines.length - 1) / 2) * 29 + 8, line));
  };
  const arrow = (x1, y1, x2, y2, color = ink) => {
    const opt = {...options(), stroke: color};
    draw(g.line(x1, y1, x2, y2, opt));
    const a = Math.atan2(y2-y1, x2-x1);
    for (const d of [-0.5, 0.5]) draw(g.line(x2, y2, x2-12*Math.cos(a+d), y2-12*Math.sin(a+d), opt));
  };
  const dot = (route, color, delay = 0) => parts.push(`<circle r="6" opacity="0" fill="${color}"><animateMotion path="${route}" dur="3s" begin="${delay}s" repeatCount="indefinite"/><animate attributeName="opacity" values="0;1;1;0" keyTimes="0;0.1;0.9;1" dur="3s" begin="${delay}s" repeatCount="indefinite"/></circle>`);
  text(480, 42, title, 29);
  return {box, text, arrow, dot, save(name) {
    fs.writeFileSync(path.join(out, name), `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 960 ${height}" role="img" aria-label="${esc(title)}"><style>@font-face{font-family:Kalam;src:url(data:font/woff2;base64,${font})}text{font-family:Kalam,cursive}@media(prefers-reduced-motion:reduce){circle{display:none}}</style><rect width="960" height="${height}" rx="16" fill="#fcfaf5"/>${parts.join('\n')}</svg>\n`);
  }};
}

{
  const c = drawing('One call. Profiles leave your process.');
  c.box(25, 100, 230, 140, '#e7f5ff', ['your Rust service', 'start(Config { ... })']);
  c.box(345, 100, 240, 140, '#ebfbee', ['in-process samplers', 'CPU + heap + spans']);
  c.box(690, 100, 240, 140, '#fff4e6', ['any OTLP receiver', 'Collector / sigiro']);
  c.arrow(260, 170, 338, 170); c.arrow(590, 170, 682, 170);
  c.text(637, 145, 'OTLP', 19, orange);
  c.text(480, 290, 'No sidecar. No eBPF agent. Export every 15 seconds.', 23);
  c.dot('M 260 170 L 682 170', blue); c.dot('M 260 170 L 682 170', green, 1.5);
  c.save('flow.svg');
}
{
  const c = drawing('Which span was running when CPU was sampled?');
  c.box(35, 100, 250, 135, '#e7f5ff', ['checkout span', 'trace ID + span ID']);
  c.box(360, 100, 235, 135, '#ebfbee', ['100 Hz SIGPROF', 'stack + active IDs']);
  c.box(675, 100, 250, 135, '#fff4e6', ['sample links to span', 'follow it in sigiro']);
  c.arrow(290, 170, 353, 170); c.arrow(600, 170, 668, 170);
  c.text(480, 284, 'OpenTelemetry context attaches and detaches with the span.', 22);
  c.dot('M 290 170 L 668 170', blue); c.save('span-links.svg');
}
{
  const c = drawing('Send repeated names once per request.');
  c.box(30, 95, 245, 150, '#e7f5ff', ['many sampled stacks', 'price → checkout', 'receipt → checkout']);
  c.box(355, 95, 250, 150, '#ebfbee', ['one dictionary', 'functions · stacks', 'strings · span links']);
  c.box(680, 95, 250, 150, '#fff4e6', ['small sample records', 'stack index + value', '+ optional link index']);
  c.arrow(280, 170, 348, 170); c.arrow(610, 170, 673, 170);
  c.dot('M 280 170 L 673 170', green); c.dot('M 280 170 L 673 170', orange, 1.5);
  c.text(480, 291, 'Upstream profcheck checks the encoded request in CI.', 23);
  c.save('dictionary.svg');
}
{
  const c = drawing('Two ways to sample heap allocations.', 410);
  c.box(30, 90, 250, 100, '#e7f5ff', ['already use jemalloc?', 'enable its profiler']);
  c.box(355, 90, 250, 100, '#ebfbee', ['parse heap_v2 dump', 'keep accum counters']);
  c.box(680, 90, 250, 100, '#fff4e6', ['in-use + alloc deltas', 'no heap span links']);
  c.box(30, 235, 250, 100, '#e7f5ff', ['any GlobalAlloc', 'wrap with Sampled<A>']);
  c.box(355, 235, 250, 100, '#ebfbee', ['random byte intervals', 'stack + active span']);
  c.box(680, 235, 250, 100, '#fff4e6', ['in-use + alloc deltas', 'with heap span links']);
  for (const y of [140, 285]) {c.arrow(285, y, 348, y); c.arrow(610, y, 673, y); c.dot(`M 285 ${y} L 673 ${y}`, y === 140 ? green : blue);}
  c.text(480, 382, '512 KiB mean interval. Estimates use the sampling probability.', 22);
  c.save('heap.svg');
}
fs.copyFileSync(path.join(here, 'node_modules/@fontsource/kalam/LICENSE'), path.join(out, 'Kalam-OFL.txt'));
