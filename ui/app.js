'use strict';

// PlanOS review page. Comments anchor to DOM Ranges in the rendered plan;
// each block carries data-s/data-e (plan source lines) so a comment can tell
// Claude exactly which lines it is about.

const $ = (id) => document.getElementById(id);
const token = new URLSearchParams(location.search).get('t') || '';
const doc = $('doc');
const rail = $('rail');
const selbtn = $('selbtn');
const gutter = $('gutter');
const general = $('general');
const stacked = window.matchMedia('(max-width: 1000px)');
const canHighlight = typeof CSS !== 'undefined' && CSS.highlights && typeof Highlight !== 'undefined';

let comments = []; // { id, start_line, end_line, quote, text, range }
let draft = null; // { range, start_line, end_line, quote, text, editingId }
let activeId = null;
let nextId = 1;
let submitted = false;
let approveArmed = null;
let headings = [];

// ---------- Theme (same switch and storage pattern as MemoryOS) ----------

function storageGet(key) {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}
function storageSet(key, value) {
  try {
    localStorage.setItem(key, value);
  } catch {
    /* private window or blocked storage: theme just isn't remembered */
  }
}
const savedTheme = storageGet('planos-theme');
if (savedTheme) document.documentElement.dataset.theme = savedTheme;
$('theme').addEventListener('click', () => {
  const t = document.documentElement.dataset.theme;
  const dark = t === 'dark' || (!t && matchMedia('(prefers-color-scheme: dark)').matches);
  document.documentElement.dataset.theme = dark ? 'light' : 'dark';
  storageSet('planos-theme', document.documentElement.dataset.theme);
});

// ---------- API ----------

async function api(path, body) {
  const res = await fetch(path, {
    method: body ? 'POST' : 'GET',
    headers: { 'X-PlanOS-Token': token, 'Content-Type': 'application/json' },
    body: body ? JSON.stringify(body) : undefined,
  });
  if (!res.ok) throw new Error(`${res.status} ${await res.text()}`);
  return res.status === 204 ? null : res.json();
}

async function load() {
  try {
    const plan = await api('/api/plan');
    document.title = `${plan.title} · PlanOS`;
    $('title').textContent = plan.title;
    $('path').textContent = (plan.path || '').replace(/^\/(Users|home)\/[^/]+/, '~');
    $('path').title = plan.path || '';
    doc.innerHTML = plan.html; // rendered server side; raw HTML in the plan is escaped there

    // The plan's own title is the page heading, so its first h1 would repeat it.
    const h1 = doc.querySelector('h1');
    if (h1 && h1.textContent.trim() === plan.title.trim()) h1.classList.add('dup-title');

    doc.querySelectorAll('pre > code[class^="language-"]').forEach((c) => {
      c.parentElement.dataset.lang = c.className.replace('language-', '');
    });

    headings = [...doc.querySelectorAll('h1:not(.dup-title), h2, h3')];
    $('n-sections').textContent = headings.length;
    $('n-lines').textContent = plan.markdown.replace(/\n$/, '').split('\n').length;
    render();
  } catch (e) {
    $('title').textContent = 'Could not load the plan';
    doc.innerHTML = '';
    doc.append(el('div', 'loading', e.message));
  }
}

// ---------- Ranges and lines ----------

function blockAt(container, offset, atEnd) {
  let node = container;
  if (node.nodeType === Node.ELEMENT_NODE && node.childNodes.length) {
    node = node.childNodes[atEnd ? offset - 1 : offset] || node;
  }
  const e = node.nodeType === Node.ELEMENT_NODE ? node : node.parentElement;
  if (!e || !doc.contains(e)) return null;
  const own = e.closest('[data-s]');
  if (own) return own;
  const inside = e.querySelectorAll('[data-s]');
  return inside.length ? (atEnd ? inside[inside.length - 1] : inside[0]) : null;
}

function lineSpan(range) {
  const a = blockAt(range.startContainer, range.startOffset, false);
  const b = blockAt(range.endContainer, range.endOffset, true) || a;
  if (!a) return [0, 0];
  const s = Number(a.dataset.s);
  let e = Number(b.dataset.e);
  // A list item's span includes its nested list. When the selection ends
  // before that nested list, stop at the line before it.
  const nested = b.querySelector(':scope > ul > li[data-s], :scope > ol > li[data-s]');
  if (nested && !nested.parentElement.contains(range.endContainer)) {
    e = Math.min(e, Number(nested.dataset.s) - 1);
  }
  return [s, Math.max(e, s)];
}

function lineLabel(c) {
  if (!c.start_line) return 'General';
  return c.end_line > c.start_line ? `Lines ${c.start_line}-${c.end_line}` : `Line ${c.start_line}`;
}

function selectionInDoc() {
  const sel = window.getSelection();
  if (!sel.rangeCount || sel.isCollapsed) return null;
  const r = sel.getRangeAt(0);
  if (!doc.contains(r.commonAncestorContainer) || !r.toString().trim()) return null;
  return r;
}

// Heading that owns a range: the last heading before it in the document.
function headingOf(range) {
  let owner = null;
  for (const h of headings) {
    const pos = h.compareDocumentPosition(range.startContainer);
    if (pos & Node.DOCUMENT_POSITION_FOLLOWING || h.contains(range.startContainer)) owner = h;
  }
  return owner;
}

// ---------- Rendering ----------

function el(tag, cls, text) {
  const n = document.createElement(tag);
  if (cls) n.className = cls;
  if (text !== undefined) n.textContent = text;
  return n;
}

function render() {
  rail.querySelectorAll('.card').forEach((c) => c.remove());
  $('hint').hidden = comments.length > 0 || draft !== null;

  const entries = comments
    .filter((c) => !(draft && draft.editingId === c.id))
    .map((c) => ({ node: commentCard(c), range: c.range }));
  if (draft) entries.push({ node: draftCard(draft), range: draft.range });
  entries.sort((a, b) => a.range.compareBoundaryPoints(Range.START_TO_START, b.range));
  for (const e of entries) {
    e.node._range = e.range;
    rail.appendChild(e.node);
  }

  paint();
  updateCounts();
  renderToc();
  layout();
  const ta = rail.querySelector('.card.draft textarea');
  if (ta) {
    grow(ta);
    ta.focus();
    ta.setSelectionRange(ta.value.length, ta.value.length);
  }
}

function cardHead(c) {
  const head = el('div', 'card-head');
  head.append(el('span', 'avatar', 'Y'), el('span', 'who', 'You'), el('span', 'pill waiting', lineLabel(c)));
  return head;
}

function commentCard(c) {
  const card = el('div', 'card' + (c.id === activeId ? ' active' : ''));
  card.dataset.id = c.id;
  const head = cardHead(c);
  const tools = el('div', 'card-tools');
  const edit = el('button', 'link-btn', 'Edit');
  const del = el('button', 'link-btn danger', 'Delete');
  edit.addEventListener('click', (e) => {
    e.stopPropagation();
    draft = { range: c.range, start_line: c.start_line, end_line: c.end_line, quote: c.quote, text: c.text, editingId: c.id };
    render();
  });
  del.addEventListener('click', (e) => {
    e.stopPropagation();
    comments = comments.filter((x) => x.id !== c.id);
    if (activeId === c.id) activeId = null;
    render();
  });
  tools.append(edit, del);
  head.append(tools);
  card.append(head, el('div', 'quote', c.quote), el('div', 'note-text', c.text));
  card.addEventListener('click', () => {
    setActive(c.id);
    revealRange(c.range);
  });
  return card;
}

function draftCard(d) {
  const card = el('div', 'card draft');
  const ta = el('textarea');
  ta.rows = 2;
  ta.placeholder = 'What should Claude change here?';
  ta.value = d.text || '';
  ta.addEventListener('input', () => {
    d.text = ta.value;
    grow(ta);
    layout();
    updateCounts();
  });
  ta.addEventListener('keydown', (e) => {
    if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) {
      e.preventDefault();
      saveDraft();
    } else if (e.key === 'Escape') {
      e.preventDefault();
      cancelDraft();
    }
  });
  const actions = el('div', 'card-actions');
  const tip = el('span', 'help');
  tip.append(el('kbd', '', '⌘ ↵'), document.createTextNode(' to save'));
  const cancel = el('button', 'btn ghost', 'Cancel');
  const save = el('button', 'btn primary', d.editingId ? 'Save' : 'Comment');
  cancel.addEventListener('click', cancelDraft);
  save.addEventListener('click', saveDraft);
  actions.append(tip, cancel, save);
  card.append(cardHead(d), el('div', 'quote', d.quote), ta, actions);
  return card;
}

function grow(ta) {
  ta.style.height = 'auto';
  ta.style.height = `${ta.scrollHeight}px`;
}

function paint() {
  if (!canHighlight) return;
  const normal = comments.filter((c) => c.id !== activeId).map((c) => c.range);
  const active = comments.filter((c) => c.id === activeId).map((c) => c.range);
  CSS.highlights.set('planos-comment', new Highlight(...normal));
  CSS.highlights.set('planos-active', new Highlight(...active));
  CSS.highlights.set('planos-draft', new Highlight(...(draft ? [draft.range] : [])));
}

// Cards sit beside the text they point at, pushed down when they would overlap.
function layout() {
  const cards = [...rail.querySelectorAll('.card')];
  if (stacked.matches) {
    cards.forEach((c) => (c.style.top = ''));
    rail.style.height = '';
    return;
  }
  const railTop = rail.getBoundingClientRect().top;
  let bottom = 0;
  for (const card of cards) {
    const rects = card._range.getClientRects();
    const anchor = (rects.length ? rects[0] : card._range.getBoundingClientRect()).top - railTop;
    const top = Math.max(anchor - 12, bottom + 10);
    card.style.top = `${top}px`;
    bottom = top + card.offsetHeight;
  }
  rail.style.height = `${bottom}px`;
}

function renderToc() {
  const toc = $('toc');
  toc.replaceChildren();
  const counts = new Map();
  for (const c of comments) {
    const h = headingOf(c.range);
    if (h) counts.set(h, (counts.get(h) || 0) + 1);
  }
  for (const h of headings) {
    const a = el('button', 'toc-item' + (h.tagName === 'H3' ? ' sub' : ''));
    a.append(el('span', 'toc-name', h.textContent));
    const n = counts.get(h);
    if (n) a.append(el('span', 'tab-badge', String(n)));
    a.addEventListener('click', () => h.scrollIntoView({ behavior: 'smooth', block: 'start' }));
    a._heading = h;
    toc.append(a);
  }
  markCurrentSection();
}

function markCurrentSection() {
  let current = headings[0];
  for (const h of headings) if (h.getBoundingClientRect().top < 140) current = h;
  $('toc').querySelectorAll('.toc-item').forEach((a) => a.setAttribute('aria-current', String(a._heading === current)));
}

function setActive(id) {
  activeId = id;
  rail.querySelectorAll('.card:not(.draft)').forEach((c) => c.classList.toggle('active', Number(c.dataset.id) === id));
  paint();
}

function revealRange(range) {
  const r = range.getBoundingClientRect();
  if (r.top < 40 || r.bottom > window.innerHeight - 120) {
    window.scrollBy({ top: r.top - window.innerHeight / 3, behavior: 'smooth' });
  }
}

function updateCounts() {
  const n = comments.length;
  $('n-comments').textContent = n;
  $('stat-comments').classList.toggle('attention', n > 0);
  const count = $('count');
  count.hidden = n === 0;
  count.textContent = n;
  $('changes').disabled = n === 0 && !general.value.trim() && !(draft && (draft.text || '').trim());
  disarmApprove();
}

// ---------- Drafting ----------

function startDraft(range, span) {
  const [s, e] = span || lineSpan(range);
  const quote = range.toString().replace(/\s+/g, ' ').trim();
  if (!quote) return;
  draft = { range, start_line: s, end_line: e, quote, text: '', editingId: null };
  activeId = null;
  render();
}

function saveDraft() {
  if (!draft) return;
  const text = (draft.text || '').trim();
  if (!text) {
    rail.querySelector('.card.draft textarea')?.focus();
    return;
  }
  if (draft.editingId) {
    const c = comments.find((x) => x.id === draft.editingId);
    if (c) c.text = text;
  } else {
    const { range, start_line, end_line, quote } = draft;
    comments.push({ id: nextId++, range, start_line, end_line, quote, text });
  }
  draft = null;
  render();
}

function cancelDraft() {
  draft = null;
  render();
}

// ---------- Submitting ----------

function disarmApprove() {
  if (approveArmed) clearTimeout(approveArmed);
  approveArmed = null;
  const b = $('approve');
  b.textContent = 'Approve plan';
  b.classList.remove('warn');
}

async function submit(body) {
  document.querySelectorAll('.decide button').forEach((b) => (b.disabled = true));
  try {
    await api('/api/submit', body);
    submitted = true;
    const n = (body.comments || []).length;
    const msg = {
      approve: ['current', 'Approved', 'Plan approved', 'Claude is leaving plan mode and starting the work.'],
      changes: ['waiting', 'Changes requested', 'Sent to Claude', `${n ? `${n} comment${n === 1 ? '' : 's'} sent. ` : ''}Claude will revise the plan and open a new review.`],
      terminal: ['past', 'Terminal', 'Back to the terminal', 'Answer the plan dialog in Claude Code.'],
    }[body.decision];
    showDone(...msg);
    $('status').textContent = 'Review sent';
    document.querySelector('.side-foot .status-dot').classList.remove('warn');
  } catch (e) {
    showDone('bad', 'Not sent', 'Could not send the review', `${e.message}. The review may have timed out in Claude Code.`);
  }
}

function showDone(kind, pill, title, text) {
  const p = $('done-pill');
  p.className = `pill ${kind}`;
  p.textContent = pill;
  $('done-title').textContent = title;
  $('done-text').textContent = text;
  $('done').hidden = false;
}

$('approve').addEventListener('click', () => {
  const n = comments.length;
  if (n && !approveArmed) {
    const b = $('approve');
    b.textContent = `Approve and drop ${n} comment${n === 1 ? '' : 's'}?`;
    b.classList.add('warn');
    approveArmed = setTimeout(disarmApprove, 4000);
    return;
  }
  submit({ decision: 'approve' });
});

$('changes').addEventListener('click', () => {
  if (draft && (draft.text || '').trim()) saveDraft();
  submit({
    decision: 'changes',
    comments: comments.map(({ start_line, end_line, quote, text }) => ({ start_line, end_line, quote, text })),
    general: general.value,
  });
});

$('terminal').addEventListener('click', () => submit({ decision: 'terminal' }));

general.addEventListener('input', () => {
  grow(general);
  updateCounts();
});

// ---------- Selection button and block gutter ----------

function showSelButton() {
  const r = selectionInDoc();
  if (!r) {
    selbtn.hidden = true;
    return;
  }
  const rects = r.getClientRects();
  const last = rects.length ? rects[rects.length - 1] : r.getBoundingClientRect();
  selbtn.hidden = false;
  const maxLeft = window.scrollX + document.documentElement.clientWidth - selbtn.offsetWidth - 8;
  selbtn.style.left = `${Math.min(last.right + window.scrollX + 4, maxLeft)}px`;
  selbtn.style.top = `${last.bottom + window.scrollY + 6}px`;
}

function commentOnSelection() {
  const r = selectionInDoc();
  selbtn.hidden = true;
  if (!r) return false;
  const range = r.cloneRange();
  window.getSelection().removeAllRanges();
  startDraft(range);
  return true;
}

document.addEventListener('mouseup', (e) => {
  if (selbtn.contains(e.target)) return;
  setTimeout(showSelButton, 0);
});
document.addEventListener('keyup', (e) => {
  if (e.shiftKey || e.key === 'Shift') showSelButton();
});
document.addEventListener('keydown', (e) => {
  const typing = e.target.closest && e.target.closest('textarea, input');
  if (!typing && (e.key === 'c' || e.key === 'C') && !e.metaKey && !e.ctrlKey && !e.altKey) {
    if (commentOnSelection()) e.preventDefault();
  }
});
selbtn.addEventListener('mousedown', (e) => e.preventDefault()); // keep the selection alive
selbtn.addEventListener('click', commentOnSelection);

// The + sits in the left margin, centred on the block's first line of text.
function firstLineRect(block) {
  const r = document.createRange();
  r.selectNodeContents(block);
  const rects = [...r.getClientRects()].filter((x) => x.height > 0 && x.width > 0);
  return rects.length ? rects[0] : block.getBoundingClientRect();
}

let hovered = null;
function setHovered(block) {
  if (hovered === block) return;
  hovered?.classList.remove('hovered');
  hovered = block;
  hovered?.classList.add('hovered');
}

doc.addEventListener('mouseover', (e) => {
  const block = e.target.closest && e.target.closest('[data-s]');
  if (!block || !doc.contains(block) || block.classList.contains('dup-title')) return;
  const d = doc.getBoundingClientRect();
  const line = firstLineRect(block);
  gutter._block = block;
  gutter.hidden = false;
  gutter.style.left = `${d.left + window.scrollX + 12}px`;
  gutter.style.top = `${line.top + line.height / 2 - 12 + window.scrollY}px`;
});
doc.addEventListener('mouseleave', (e) => {
  if (e.relatedTarget !== gutter && !gutter.contains(e.relatedTarget)) gutter.hidden = true;
});
gutter.addEventListener('mouseenter', () => setHovered(gutter._block));
gutter.addEventListener('mouseleave', (e) => {
  setHovered(null);
  if (!doc.contains(e.relatedTarget)) gutter.hidden = true;
});
gutter.addEventListener('click', () => {
  const block = gutter._block;
  if (!block) return;
  const range = document.createRange();
  range.selectNodeContents(block);
  gutter.hidden = true;
  setHovered(null);
  startDraft(range, [Number(block.dataset.s), Number(block.dataset.e)]);
});

// Clicking highlighted text focuses its comment.
doc.addEventListener('click', (e) => {
  if (selectionInDoc()) return;
  let node = null;
  let offset = 0;
  if (document.caretPositionFromPoint) {
    const p = document.caretPositionFromPoint(e.clientX, e.clientY);
    if (p) ({ offsetNode: node, offset } = p);
  } else if (document.caretRangeFromPoint) {
    const r = document.caretRangeFromPoint(e.clientX, e.clientY);
    if (r) ({ startContainer: node, startOffset: offset } = r);
  }
  if (!node) return;
  const hit = comments.find((c) => {
    try {
      return c.range.isPointInRange(node, offset);
    } catch {
      return false;
    }
  });
  setActive(hit ? hit.id : null);
});

// ---------- Housekeeping ----------

window.addEventListener('resize', layout);
window.addEventListener('scroll', markCurrentSection, { passive: true });
stacked.addEventListener('change', layout);
new ResizeObserver(layout).observe(doc);
window.addEventListener('beforeunload', (e) => {
  if (!submitted && (comments.length || draft)) e.preventDefault();
});

load();
