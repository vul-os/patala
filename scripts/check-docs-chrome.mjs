#!/usr/bin/env node
// check-docs-chrome.mjs — the site's structural gate.
//
// The Vulos suite's own checkers (vulos-static/scripts/check-suite-chrome.mjs
// and check-site.mjs) are not present in this checkout, so the invariants they
// hold were, in practice, held by review. Review does not catch a regression
// that renders correctly, and every rule below has regressed somewhere in this
// suite at least once:
//
//   · a docs page grew a <footer> back after the rule was ratified;
//   · a sibling product shipped an external Spline iframe, silently breaking
//     its own "no outbound calls" claim, and nobody saw it in a screenshot;
//   · a fenced block quietly fell back to plaintext because the vendored
//     highlight.js bundle did not carry that grammar.
//
// So they are assertions here instead. Every one of them was mutation-tested
// when written: broken deliberately, watched to fail with a message that names
// the problem, then restored.
//
// Usage:  node scripts/check-docs-chrome.mjs
// Exit 0 = every invariant holds. Exit 1 = at least one does not, named.

import { readFileSync, readdirSync, statSync, existsSync } from 'node:fs';
import { resolve, dirname, join, relative, extname } from 'node:path';
import { fileURLToPath } from 'node:url';
import vm from 'node:vm';

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const SITE = join(ROOT, 'site');
const DOCS_HTML = join(SITE, 'docs.html');
const INDEX_HTML = join(SITE, 'index.html');

const failures = [];
let checks = 0;

function assert(ok, label, detail) {
  checks += 1;
  if (!ok) failures.push(detail ? `${label}\n      ${detail}` : label);
}

// ───────────────────────────────────────────────────────────────────────────
// 1. NOT CHECKED HERE, ON PURPOSE — the suite chrome rules.
//
// The four ratified chrome rules (one Vulos element in the top bar, one
// .vulos-foot line in the landing footer, "Vulos" nowhere else in the visible
// body, no licence text in the footer) and the no-<footer>-on-docs rule are
// owned and mutation-tested by the suite's own gate, which is not in this
// repo:
//
//     cd ../vulos-cloud && node scripts/check-suite-chrome.mjs
//
// It auto-discovers every sibling repo with a site/index.html — patala
// included — and checks both pages. Re-implementing those five rules here
// would give this repo a second opinion that can disagree with the ratified
// one, which is worse than having none. Run that gate; this file covers only
// what it does not.
// ───────────────────────────────────────────────────────────────────────────
const docsHtml = readFileSync(DOCS_HTML, 'utf8');

// ───────────────────────────────────────────────────────────────────────────
// 2. The docs sidebar is pinned, and the shell is packed left.
//
// The rail is `position:fixed; left:0` above the mobile breakpoint — a plane
// the page is laid on, not a card floating in a centred wrapper. It regressed
// to `position:sticky` inside a 1160px centred wrap once, which put 164px of
// empty canvas to its left and let the top drift as the container ran out.
// ───────────────────────────────────────────────────────────────────────────
const railRule = docsHtml.match(/\.docs-nav\s*\{[^}]*\}/s);
assert(!!railRule, 'site/docs.html must define a .docs-nav rule');

const pinned = /@media\(min-width:861px\)\s*\{[\s\S]{0,600}?\.docs-nav\s*\{[^}]*position:\s*fixed[^}]*left:\s*0/;
assert(
  pinned.test(docsHtml),
  'the docs sidebar must be position:fixed at left:0 above 861px',
  'a sticky rail inside a centred wrapper drifts and floats; the rail is a plane, not a card'
);

assert(
  /\.docs-shell[^{]*\{[^}]*margin-inline:\s*0/.test(docsHtml),
  'the docs shell must be packed LEFT (margin-inline:0), not centred'
);

assert(
  /\.dn-list\s*\{[^}]*overflow-y:\s*auto/.test(docsHtml),
  'the sidebar list must scroll in its own box, never the page'
);

// The rail must remain reachable and operable, not merely present.
assert(
  /id="dnFilter"/.test(docsHtml) && /aria-label="Filter documents and headings"/.test(docsHtml),
  'the sidebar filter must exist and be labelled for assistive technology'
);
// EVERY toggle, not "at least one" — an assertion satisfied by any single
// element is satisfied by the one nobody broke.
const toggles = [...docsHtml.matchAll(/<button[^>]*class="dn-gtoggle"[^>]*>/g)];
const toggledOK = toggles.filter((m) => /aria-expanded=/.test(m[0])).length;
assert(toggles.length >= 3, 'the sidebar must be grouped', `found ${toggles.length} groups`);
assert(
  toggles.length > 0 && toggledOK === toggles.length,
  'every sidebar group toggle must expose aria-expanded',
  `${toggledOK} of ${toggles.length} do — a collapsed group that does not say so is invisible to a screen reader`
);
// The rail is rendered client-side, so this is a claim about the script that
// marks the row rather than about static markup.
assert(
  /setAttribute\(\s*'aria-current'\s*,\s*'page'\s*\)/.test(docsHtml) &&
    /removeAttribute\(\s*'aria-current'\s*\)/.test(docsHtml),
  'the sidebar must set aria-current="page" on the open document and remove it from the rest',
  'colour and a pip are the human half of "you are here"; aria-current is the machine-readable half'
);

// ───────────────────────────────────────────────────────────────────────────
// 3. No outbound origins anywhere in site/.
//
// The "nothing beyond this origin is ever requested" claim is load-bearing —
// it is the same claim the offline default build makes, applied to the site.
// A CDN script, a remote font, a tracking pixel or an embedded iframe breaks
// it, and a sibling product shipped exactly that without anyone noticing.
//
// Anchors are exempt: a link is a thing the reader chooses to follow, not a
// request the page makes.
// ───────────────────────────────────────────────────────────────────────────
const FETCHING_ATTR = /\b(src|href|srcset|data|poster|action|content)\s*=\s*"([^"]*)"/gi;
const REMOTE = /^(https?:)?\/\//i;

function siteFiles(dir) {
  const out = [];
  for (const entry of readdirSync(dir)) {
    const p = join(dir, entry);
    if (statSync(p).isDirectory()) out.push(...siteFiles(p));
    else out.push(p);
  }
  return out;
}

const TEXTUAL = new Set(['.html', '.css', '.js', '.mjs', '.svg']);
const outbound = [];
let scannedForOrigins = 0;

for (const file of siteFiles(SITE)) {
  if (!TEXTUAL.has(extname(file))) continue;
  scannedForOrigins += 1;
  const rel = relative(ROOT, file);
  const text = readFileSync(file, 'utf8');

  if (/<iframe[\s>]/i.test(text)) {
    outbound.push(`${rel}: contains an <iframe> — the page must embed nothing it does not serve`);
  }
  if (/@import\s+(url\()?["']?https?:/i.test(text)) {
    outbound.push(`${rel}: CSS @import of a remote stylesheet`);
  }
  for (const m of text.matchAll(/url\(\s*["']?(https?:)?\/\/[^)]*\)/gi)) {
    outbound.push(`${rel}: CSS url() pointing off-origin — ${m[0].slice(0, 60)}`);
  }
  for (const m of text.matchAll(FETCHING_ATTR)) {
    const [, attr, value] = m;
    if (!REMOTE.test(value)) continue;
    // og:url / og:image / canonical name this site's own published location;
    // they are metadata, not a request the page issues. <a href> is a link.
    const before = text.slice(Math.max(0, m.index - 260), m.index);
    const isAnchor = /<a\b[^>]*$/i.test(before);
    const isMeta = /<(meta|link)\b[^>]*$/i.test(before);
    if (isAnchor || isMeta) continue;
    outbound.push(`${rel}: ${attr}="${value.slice(0, 70)}" loads from another origin`);
  }
}

assert(scannedForOrigins >= 5, 'the outbound-origin scan must actually have files to scan',
  `scanned ${scannedForOrigins}`);
assert(
  outbound.length === 0,
  'no file under site/ may load anything from another origin',
  outbound.join('\n      ')
);

// The two pages must genuinely reference the vendored copies, not a CDN.
for (const [name, html] of [['docs.html', docsHtml], ['index.html', readFileSync(INDEX_HTML, 'utf8')]]) {
  if (!/highlight/i.test(html)) continue;
  assert(
    /src="\.\/assets\/vendor\/highlight\.core\.js"/.test(html),
    `site/${name} must load highlight.js from ./assets/vendor/, vendored`
  );
}

// ───────────────────────────────────────────────────────────────────────────
// 4. Every language a fenced block names is one the vendored bundle can
//    highlight, and every fence names one.
//
// A block that falls through to plaintext is a defect, and it is invisible:
// the page renders, the text is readable, and only the colour is missing. So
// the bundle is actually loaded here — in a VM with a fake `window` — and
// asked, rather than compared against a hand-kept list that could drift from
// what was bundled.
// ───────────────────────────────────────────────────────────────────────────
const bundlePath = join(SITE, 'assets', 'vendor', 'highlight.core.js');
assert(existsSync(bundlePath), 'the vendored highlight.js bundle must exist at site/assets/vendor/');

let hljs = null;
if (existsSync(bundlePath)) {
  const ctx = { window: {} };
  vm.createContext(ctx);
  vm.runInContext(readFileSync(bundlePath, 'utf8'), ctx);
  hljs = ctx.window.hljs || null;
  assert(!!hljs, 'the vendored bundle must expose window.hljs');
}

if (hljs) {
  const registered = hljs.listLanguages();
  assert(
    registered.length >= 20,
    'the vendored bundle must carry a real language set',
    `only ${registered.length} grammars are registered — a five-language core bundle silently plaintexts most of the docs`
  );

  const unlabelled = [];
  const unknown = new Map();
  let fences = 0;

  for (const entry of readdirSync(join(ROOT, 'docs'))) {
    if (!entry.endsWith('.md')) continue;
    const lines = readFileSync(join(ROOT, 'docs', entry), 'utf8').split('\n');
    let open = false;
    lines.forEach((line, i) => {
      if (!line.startsWith('```')) return;
      if (open) { open = false; return; }
      open = true;
      fences += 1;
      const lang = line.slice(3).trim().split(/\s+/)[0];
      if (!lang) { unlabelled.push(`docs/${entry}:${i + 1}`); return; }
      if (!hljs.getLanguage(lang)) {
        if (!unknown.has(lang)) unknown.set(lang, []);
        unknown.get(lang).push(`docs/${entry}:${i + 1}`);
      }
    });
  }

  assert(fences >= 40, 'the fence scan must actually have found fences', `found ${fences}`);
  assert(
    unlabelled.length === 0,
    'every fenced block in docs/ must name its language',
    `unlabelled: ${unlabelled.join(', ')} — use \`\`\`text for ASCII diagrams and captured output, so "plaintext" is a decision rather than an omission`
  );
  assert(
    unknown.size === 0,
    'every language named by a fence must be in the vendored highlight.js bundle',
    [...unknown.entries()]
      .map(([lang, where]) => `"${lang}" (${where.join(', ')}) is not registered`)
      .join('\n      ') +
      '\n      Add the grammar to site/assets/vendor/highlight.core.entry.mjs and rebuild the bundle.'
  );
}

// ───────────────────────────────────────────────────────────────────────────
// 5. The docs bundle the site serves is the generated one.
//
// gen-site-docs.mjs --check is the authority; this only asserts the generated
// regions are present at all, so a well-meaning hand-edit that deletes the
// markers fails here rather than silently turning the generator into a no-op.
// ───────────────────────────────────────────────────────────────────────────
for (const marker of [
  '<!-- BEGIN generated:nav -->',
  '<!-- END generated:nav -->',
  '// BEGIN generated:manifest',
  '// END generated:manifest',
  '<!-- BEGIN generated:noscript -->',
  '<!-- END generated:noscript -->',
]) {
  assert(docsHtml.includes(marker), `site/docs.html must keep the ${marker} marker`);
}

// ───────────────────────────────────────────────────────────────────────────
if (failures.length) {
  console.error(`check-docs-chrome: FAIL — ${failures.length} of ${checks} invariants broken\n`);
  for (const f of failures) console.error(`  ✗ ${f}\n`);
  process.exit(1);
}
console.error(`check-docs-chrome: OK — ${checks} invariants hold`);
