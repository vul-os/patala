#!/usr/bin/env node
// render-shots.mjs — render the real transcripts captured by
// capture-transcripts.sh into on-brand PNG "terminal pane" images for
// site/index.html.
//
// This script does NOT invent content: it reads the plain-text files under
// site/assets/shots/transcripts/*.txt verbatim (only wrapping known
// substrings like "ok" / "PASS" / "true" in a <span> for colour — the text
// itself is untouched) and screenshots the styled result.
//
// Requires the "playwright" package with a Chromium browser installed
// somewhere on this machine. This repo does not vendor it (patala is a Rust
// workspace); point PATALA_PLAYWRIGHT_NODE_MODULES at a node_modules
// directory that has it, e.g.:
//
//   PATALA_PLAYWRIGHT_NODE_MODULES=/path/to/some/web/node_modules \
//     node scripts/render-shots.mjs
//
// Falls back to a plain `import('playwright')` first (in case it's ever
// installed locally), then to the path below as a last resort for this
// development environment.

import { createRequire } from 'node:module';
import { readFileSync, writeFileSync, mkdirSync, existsSync, rmSync, unlinkSync } from 'node:fs';
import { resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { execFileSync } from 'node:child_process';

const __dirname = dirname(fileURLToPath(import.meta.url));
const ROOT = resolve(__dirname, '..');
const TRANSCRIPTS_DIR = resolve(ROOT, 'site/assets/shots/transcripts');
const OUT_DIR = resolve(ROOT, 'site/assets/shots');

let chromium;
try {
  ({ chromium } = await import('playwright'));
} catch {
  const fallback = process.env.PATALA_PLAYWRIGHT_NODE_MODULES
    || '/Users/pc/code/vulos/openrate/web/node_modules';
  const require = createRequire(resolve(fallback, 'x.js'));
  ({ chromium } = require('playwright'));
}

mkdirSync(OUT_DIR, { recursive: true });

function esc(s) {
  return s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
}

// Wrap known success markers in <span class="ok">. Only decoration —
// the underlying text is never altered.
function highlightLine(rawLine) {
  if (/^\$ /.test(rawLine)) {
    return `<span class="prompt">$ </span><span class="cmd">${esc(rawLine.slice(2))}</span>`;
  }
  const wholeLine = [
    /^ok$/,
    /^PASS$/,
    /^TOTAL PASSED: \d+$/,
    /^ALL PYTHON SMOKE ASSERTIONS PASSED$/,
    /^go-test-gate: OK — .*/,
  ];
  for (const re of wholeLine) {
    if (re.test(rawLine)) return `<span class="ok">${esc(rawLine)}</span>`;
  }
  let l = esc(rawLine);
  l = l
    .replace(/--- PASS/g, '<span class="ok">--- PASS</span>')
    .replace(/"valid":true/g, '&quot;valid&quot;:<span class="ok">true</span>')
    .replace(/test result: ok\./, 'test result: <span class="ok">ok</span>.')
    .replace(/listening on [^\s]+ \([^)]*\)/, (m) => `<span class="ok">${m}</span>`)
    .replace(/\bOK\b:/g, '<span class="ok">OK</span>:');
  return l;
}

function renderTranscript(text) {
  return text.replace(/\n$/, '').split('\n').map(highlightLine).join('\n');
}

function pageHtml(label, bodyHtml, width) {
  // Loaded via a real file:// navigation (not page.setContent, which runs
  // the page at an opaque about:blank origin that Chromium refuses to let
  // load file:// stylesheets/fonts from) — see render() below. Because this
  // file lives at OUT_DIR/_render-tmp/, patala.css's own relative
  // `url('../fonts/...')` resolves exactly as it does on the live site.
  return `<!doctype html><html data-theme="dark"><head><meta charset="utf-8">
  <link rel="stylesheet" href="../../css/patala.css">
  <style>
    html,body{background:transparent}
    body{padding:0;margin:0;width:${width}px}
    .term{width:${width}px}
  </style>
  </head><body>
  <div class="term" id="shot">
    <div class="term-head"><span class="dots"><span></span><span></span><span></span></span><span>${esc(label)}</span></div>
    <pre>${bodyHtml}</pre>
  </div>
  </body></html>`;
}

const SHOTS = [
  { file: 'sidecar-boot.txt', label: 'terminal · sidecar boot', out: 'sidecar-boot.png', width: 720 },
  { file: 'sidecar-roundtrip.txt', label: 'terminal · http round-trip', out: 'sidecar-roundtrip.png', width: 720 },
  { file: 'test-suite.txt', label: 'terminal · cargo test --workspace', out: 'test-suite.png', width: 920 },
  { file: 'binding-python.txt', label: 'terminal · python binding', out: 'binding-python.png', width: 720 },
  { file: 'binding-go.txt', label: 'terminal · go binding', out: 'binding-go.png', width: 720 },
];

const TMP_DIR = resolve(OUT_DIR, '_render-tmp');
mkdirSync(TMP_DIR, { recursive: true });

const browser = await chromium.launch();
const page = await browser.newPage({ deviceScaleFactor: 2 });

for (const shot of SHOTS) {
  const srcPath = resolve(TRANSCRIPTS_DIR, shot.file);
  if (!existsSync(srcPath)) {
    console.error(`!! skipping ${shot.out}: ${srcPath} does not exist (run capture-transcripts.sh first)`);
    continue;
  }
  const text = readFileSync(srcPath, 'utf8');
  const body = renderTranscript(text);
  const html = pageHtml(shot.label, body, shot.width);
  const tmpHtmlPath = resolve(TMP_DIR, `${shot.out}.html`);
  writeFileSync(tmpHtmlPath, html);
  await page.setViewportSize({ width: shot.width + 40, height: 800 });
  await page.goto(`file://${tmpHtmlPath}`, { waitUntil: 'load' });
  await page.evaluate(() => document.fonts.ready);
  const el = await page.$('#shot');
  const pngPath = resolve(OUT_DIR, shot.out);
  await el.screenshot({ path: pngPath });

  // Convert to lossless WebP (text screenshots compress ~5-6x smaller than
  // PNG with zero quality loss) and drop the intermediate PNG. Falls back
  // to keeping the PNG if cwebp isn't installed (`brew install webp`).
  const webpPath = pngPath.replace(/\.png$/, '.webp');
  try {
    execFileSync('cwebp', ['-lossless', '-q', '90', pngPath, '-o', webpPath, '-quiet']);
    unlinkSync(pngPath);
    console.log(`==> wrote ${webpPath}`);
  } catch {
    console.warn(`!! cwebp not available — keeping ${pngPath} as PNG`);
  }
}

await browser.close();
rmSync(TMP_DIR, { recursive: true, force: true });
console.log('==> done.');
