/* patala — the code block, as behaviour. Loaded by index.html and docs.html.
   ===========================================================================

   ONE FILE, BOTH PAGES, on purpose. The landing hand-writes its five blocks
   and the docs viewer generates its blocks from markdown at runtime; before
   this file existed the copy control was implemented twice, character for
   character, in two inline <script> blocks that could drift. Everything a
   .codeblock DOES now lives here and both pages call the same entry point:

     patalaCode.dress(root)   — highlight, line-split, wire the controls, and
                                measure the overflow of every .codeblock and
                                every [data-scroller] inside `root`.

   The four jobs, in the order they matter:

   1. HIGHLIGHT. The vendored highlight.js core (../vendor/highlight.core.js,
      BSD-3) registers rust, go, python, ini (which is what carries the `toml`
      alias), bash and json, and exposes itself as window.hljs. If a fence
      names a language that build does not have, highlight.js would throw
      "Unknown language" — that case is caught, reported once to the console
      and the block is left as plain text, which is a legible fallback rather
      than an empty <pre>.

   2. LINE-SPLIT. The highlighted output is rebuilt as one block element per
      SOURCE line (<span class="cl">). This is the whole reason a hanging
      indent is possible: `text-indent` inside a <pre> applies to the first
      formatted line of the box and to nothing else, so with the usual single
      text run there is no way to hang the second visual line of line 40. The
      split walks the DOM rather than slicing innerHTML — a multi-line block
      comment or a multi-line string is ONE hljs span containing newlines, and
      slicing the HTML string at "\n" tears it in half. Every open span is
      re-opened inside the next line, so each line is independently valid.

   3. WRAP / EXACT. See the long note in patala.css beside .codeblock. Default:
      an overflowing block wraps, because a reader must be able to read a line,
      not merely reach it. A block whose COLUMNS are the content declares
      data-wrap="never" and keeps exact scroll. The header's Wrap control flips
      either decision, per block, and says which state it is in.

   4. AFFORDANCE. A block that scrolls says so: a fade at the clipped edge that
      is shown only while there is genuinely more to the right, and a real tab
      stop (tabindex=0) so a keyboard user can scroll it at all. Both are
      re-measured on resize and after a wrap toggle. The same treatment is
      given to [data-scroller] — §06's captured terminal panes, which are
      images and so can never wrap.

   No state is persisted and nothing is written to storage: the wrap decision
   is per block and per visit, which keeps it predictable and keeps this file
   free of a preference that would then have to be honoured in two places. */
(function () {
  'use strict';

  var reportedMissingLang = {};

  /* ── 1 · highlight ─────────────────────────────────────────────────────── */
  function highlight(code) {
    if (!window.hljs || code.dataset.hl === 'done') return;
    var m = (code.className || '').match(/language-([\w-]+)/);
    var lang = m ? m[1] : null;
    try {
      if (lang && !hljs.getLanguage(lang)) {
        if (!reportedMissingLang[lang]) {
          reportedMissingLang[lang] = true;
          // Loud, once, and not swallowed: a fence naming a grammar this
          // bundle does not carry is a build defect to fix, not a shrug.
          console.warn('[patala] no grammar for "' + lang + '" in the vendored highlight.js build — left unhighlighted');
        }
        code.dataset.hl = 'done';
        return;
      }
      hljs.highlightElement(code);
    } catch (e) {
      console.warn('[patala] highlight failed', e);
    }
    code.dataset.hl = 'done';
  }

  /* ── 2 · one block element per source line ─────────────────────────────── */
  function splitLines(code) {
    if (code.dataset.lines === 'done') return;
    var lines = [];
    var open = [];                       // spans currently open on this line
    var line = mk();

    function mk() { var s = document.createElement('span'); s.className = 'cl'; return s; }
    function tip() { return open.length ? open[open.length - 1] : line; }
    function newline() {
      lines.push(line);
      line = mk();
      var rebuilt = [], parent = line;
      for (var i = 0; i < open.length; i++) {
        var c = open[i].cloneNode(false);
        parent.appendChild(c);
        rebuilt.push(c);
        parent = c;
      }
      open = rebuilt;
    }
    (function walk(node) {
      var kids = node.childNodes;
      for (var i = 0; i < kids.length; i++) {
        var child = kids[i];
        if (child.nodeType === 3) {
          var parts = child.data.split('\n');
          for (var j = 0; j < parts.length; j++) {
            if (j > 0) newline();
            if (parts[j]) tip().appendChild(document.createTextNode(parts[j]));
          }
        } else if (child.nodeType === 1) {
          var clone = child.cloneNode(false);
          tip().appendChild(clone);
          open.push(clone);
          walk(child);
          open.pop();
        }
      }
    })(code);
    lines.push(line);

    // marked closes a fence with a trailing newline, which lands here as one
    // empty final line. Drop it rather than shipping a blank row at the foot
    // of every docs block.
    while (lines.length > 1 && !lines[lines.length - 1].textContent) lines.pop();

    while (code.firstChild) code.removeChild(code.firstChild);
    for (var k = 0; k < lines.length; k++) {
      // The hang is measured from the LINE's own indent, not from the block's
      // left edge. A flat 2.6ch hang put the continuation of a four-space
      // indented line further LEFT than the line it continues, which reads as
      // a new, outdented statement — precisely the confusion the hanging
      // indent exists to prevent. --lead is that line's leading whitespace in
      // characters (tabs counted as four), capped so a deeply nested line
      // cannot eat a phone's whole measure.
      var lead = /^[ \t]*/.exec(lines[k].textContent)[0].replace(/\t/g, '    ').length;
      if (lead) lines[k].style.setProperty('--lead', Math.min(lead, 12));
      code.appendChild(lines[k]);
    }
    code.dataset.lines = 'done';
  }

  /* ── 4 · overflow affordance ───────────────────────────────────────────── */
  function measure(scroller, frame) {
    var more = scroller.scrollWidth - scroller.clientWidth - Math.ceil(scroller.scrollLeft) > 2;
    frame.setAttribute('data-x', more ? 'more' : 'end');
    var overflows = scroller.scrollWidth - scroller.clientWidth > 2;
    if (overflows) {
      if (!scroller.hasAttribute('tabindex')) scroller.setAttribute('tabindex', '0');
    } else {
      scroller.removeAttribute('tabindex');
      frame.removeAttribute('data-x');
    }
    return overflows;
  }

  function watch(scroller, frame) {
    if (scroller.dataset.watched) return;
    scroller.dataset.watched = '1';
    scroller.addEventListener('scroll', function () { measure(scroller, frame); }, { passive: true });
  }

  /* ── 3 · the header controls ───────────────────────────────────────────── */
  var WRAP_BTN =
    '<button class="cb-wrap" type="button" aria-pressed="false" title="Soft-wrap long lines">' +
      '<span class="off"><svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">' +
        '<path d="M4 6.5h16M4 12h11.5a3.2 3.2 0 0 1 0 6.4h-2.4"/><path d="M13.6 16.5 11.4 18.9l2.2 2.4"/><path d="M4 17.5h4"/></svg>Wrap</span>' +
      '<span class="on"><svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">' +
        '<path d="M4 6.5h16M4 12h11.5a3.2 3.2 0 0 1 0 6.4h-2.4"/><path d="M13.6 16.5 11.4 18.9l2.2 2.4"/><path d="M4 17.5h4"/></svg>Wrapped</span>' +
    '</button>';

  function applyWrap(block, on) {
    var pre = block.querySelector('pre');
    var btn = block.querySelector('.cb-wrap');
    block.classList.toggle('wrapped', !!on);
    if (btn) {
      btn.setAttribute('aria-pressed', String(!!on));
      btn.setAttribute('aria-label', on ? 'Stop wrapping — show exact line breaks' : 'Soft-wrap long lines so they fit');
    }
    if (pre) measure(pre, block);
  }

  function dressBlock(block) {
    var pre = block.querySelector('pre');
    var code = pre && pre.querySelector('code');
    if (!pre || !code) return;

    highlight(code);
    // The un-highlighted source, kept for the clipboard: once the block is
    // split into per-line elements the newlines are structure, not text, so
    // textContent alone would put the whole file on one line.
    if (pre.patalaSource === undefined) pre.patalaSource = code.textContent.replace(/\n$/, '');
    splitLines(code);

    if (!pre.parentNode.classList.contains('cb-body')) {
      var body = document.createElement('div');
      body.className = 'cb-body';
      pre.parentNode.insertBefore(body, pre);
      body.appendChild(pre);
      var fade = document.createElement('span');
      fade.className = 'cb-fade';
      fade.setAttribute('aria-hidden', 'true');
      body.appendChild(fade);
    }

    var head = block.querySelector('.cb-head');
    if (head && !head.querySelector('.cb-wrap')) {
      var copy = head.querySelector('.cb-copy');
      var holder = document.createElement('span');
      holder.style.cssText = 'display:contents';
      holder.innerHTML = WRAP_BTN;
      var btn = holder.firstChild;
      if (copy) head.insertBefore(btn, copy); else head.appendChild(btn);
    }

    watch(pre, block);
    decide(block);
  }

  // The initial wrap decision needs a laid-out block: a <pre> inside a
  // display:none tab pane measures 0 wide and would always look as though it
  // fits. Blocks in that state are left UNDECIDED and decided the first time
  // they have a width — which is what the tab handlers' remeasure() call is
  // for. Once decided, the decision is the reader's to change and nothing
  // here overrides it again.
  function decide(block) {
    var pre = block.querySelector('pre');
    if (!pre || !pre.clientWidth) return;
    if (block.dataset.decided === '1') return;
    block.dataset.decided = '1';
    applyWrap(block, measure(pre, block) && block.getAttribute('data-wrap') !== 'never');
  }

  // A [data-scroller] gets the same edge fade a code block gets, and by the
  // same means: a wrapper that is NOT itself the scroller. The fade cannot
  // live on the scroller as a pseudo-element with height:100% — a percentage
  // height resolves against an auto-height containing block as ZERO, so the
  // gradient was 30px wide and 0px tall and nothing was ever drawn. Absolutely
  // positioned inside a static-height wrapper it spans the full height and
  // stays put while the content scrolls under it.
  function dressScroller(el) {
    if (!el.parentNode.classList.contains('sc-frame')) {
      var frame = document.createElement('div');
      frame.className = 'sc-frame';
      el.parentNode.insertBefore(frame, el);
      frame.appendChild(el);
      var fade = document.createElement('span');
      fade.className = 'sc-fade';
      fade.setAttribute('aria-hidden', 'true');
      frame.appendChild(fade);
    }
    watch(el, el);
    measure(el, el);
  }

  function dress(root) {
    root = root || document;
    var blocks = root.querySelectorAll('.codeblock');
    for (var i = 0; i < blocks.length; i++) dressBlock(blocks[i]);
    var scrollers = root.querySelectorAll('[data-scroller]');
    for (var j = 0; j < scrollers.length; j++) dressScroller(scrollers[j]);
  }

  function remeasure() {
    var blocks = document.querySelectorAll('.codeblock');
    for (var i = 0; i < blocks.length; i++) {
      decide(blocks[i]);
      var pre = blocks[i].querySelector('pre');
      if (pre) measure(pre, blocks[i]);
    }
    var scrollers = document.querySelectorAll('[data-scroller]');
    for (var j = 0; j < scrollers.length; j++) measure(scrollers[j], scrollers[j]);
  }

  /* ── delegated controls ────────────────────────────────────────────────── */
  // Copy. execCommand is kept as the fallback path because navigator.clipboard
  // is unavailable on a plain http:// origin in some browsers, and a copy
  // button that silently does nothing is worse than no copy button.
  function copyText(text) {
    if (navigator.clipboard && window.isSecureContext) return navigator.clipboard.writeText(text);
    return new Promise(function (res, rej) {
      var ta = document.createElement('textarea');
      ta.value = text; ta.setAttribute('readonly', '');
      ta.style.cssText = 'position:fixed;top:-1000px;opacity:0';
      document.body.appendChild(ta); ta.select();
      var ok = false;
      try { ok = document.execCommand('copy'); } catch (e) {}
      document.body.removeChild(ta);
      ok ? res() : rej(new Error('copy failed'));
    });
  }

  document.addEventListener('click', function (ev) {
    if (!ev.target.closest) return;

    var wrapBtn = ev.target.closest('.cb-wrap');
    if (wrapBtn) {
      var wblock = wrapBtn.closest('.codeblock');
      if (wblock) applyWrap(wblock, wrapBtn.getAttribute('aria-pressed') !== 'true');
      return;
    }

    var btn = ev.target.closest('.cb-copy');
    if (!btn) return;
    var block = btn.closest('.codeblock');
    var pre = block && block.querySelector('pre');
    if (!pre) return;
    var src = pre.patalaSource !== undefined ? pre.patalaSource : pre.textContent;
    copyText(src).then(function () {
      btn.setAttribute('data-state', 'done');
      clearTimeout(btn._t);
      btn._t = setTimeout(function () { btn.removeAttribute('data-state'); }, 1600);
    }).catch(function () {
      btn.setAttribute('aria-label', 'Copy failed — select the code and copy manually');
    });
  });

  var rt;
  window.addEventListener('resize', function () {
    clearTimeout(rt);
    rt = setTimeout(remeasure, 120);
  });

  window.patalaCode = { dress: dress, remeasure: remeasure };

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', function () { dress(document); });
  } else {
    dress(document);
  }
})();
