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
const narrow = window.matchMedia('(max-width: 1100px)');
const canHighlight = typeof CSS !== 'undefined' && CSS.highlights && typeof Highlight !== 'undefined';

let comments = []; // { id, start_line, end_line, quote, text, range }
let draft = null; // { range, start_line, end_line, quote, text, editingId }
let activeId = null;
let nextId = 1;
let submitted = false;
let approveArmed = null;

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
    $('path').textContent = plan.path || '';
    doc.innerHTML = plan.html; // rendered server side; raw HTML in the plan is escaped there
    render();
  } catch (e) {
    $('title').textContent = 'Could not load the plan';
    doc.textContent = e.message;
  }
}

// ---------- Ranges and lines ----------

function blockAt(container, offset, atEnd) {
  let node = container;
  if (node.nodeType === Node.ELEMENT_NODE && node.childNodes.length) {
    node = node.childNodes[atEnd ? offset - 1 : offset] || node;
  }
  const el = node.nodeType === Node.ELEMENT_NODE ? node : node.parentElement;
  if (!el || !doc.contains(el)) return null;
  const own = el.closest('[data-s]');
  if (own) return own;
  const inside = el.querySelectorAll('[data-s]');
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

// ---------- Rendering ----------

function render() {
  rail.querySelectorAll('.card').forEach((c) => c.remove());
  $('hint').hidden = comments.length > 0 || draft !== null;

  const entries = comments
    .filter((c) => !(draft && draft.editingId === c.id))
    .map((c) => ({ el: commentCard(c), range: c.range }));
  if (draft) entries.push({ el: draftCard(draft), range: draft.range });
  entries.sort((a, b) => a.range.compareBoundaryPoints(Range.START_TO_START, b.range));
  for (const e of entries) {
    e.el._range = e.range;
    rail.appendChild(e.el);
  }

  paint();
  updateBar();
  layout();
  const ta = rail.querySelector('.card.draft textarea');
  if (ta) {
    ta.focus();
    ta.setSelectionRange(ta.value.length, ta.value.length);
  }
}

function el(tag, cls, text) {
  const n = document.createElement(tag);
  if (cls) n.className = cls;
  if (text !== undefined) n.textContent = text;
  return n;
}

function commentCard(c) {
  const card = el('div', 'card' + (c.id === activeId ? ' active' : ''));
  card.dataset.id = c.id;
  const tools = el('div', 'tools');
  const edit = el('button', '', 'Edit');
  const del = el('button', '', 'Delete');
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
  card.append(tools, el('div', 'where', lineLabel(c)), el('div', 'quote', `“${c.quote}”`), el('div', 'text', c.text));
  card.addEventListener('click', () => {
    setActive(c.id);
    revealRange(c.range);
  });
  return card;
}

function draftCard(d) {
  const card = el('div', 'card draft active');
  const ta = el('textarea');
  ta.placeholder = 'What should Claude change here?';
  ta.value = d.text || '';
  ta.addEventListener('input', () => {
    d.text = ta.value;
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
  const row = el('div', 'row');
  const cancel = el('button', '', 'Cancel');
  const save = el('button', 'save', d.editingId ? 'Save' : 'Comment');
  cancel.addEventListener('click', cancelDraft);
  save.addEventListener('click', saveDraft);
  row.append(cancel, save);
  card.append(el('div', 'where', lineLabel(d)), el('div', 'quote', `“${d.quote}”`), ta, row);
  return card;
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
  if (narrow.matches) {
    cards.forEach((c) => (c.style.top = ''));
    rail.style.height = '';
    return;
  }
  const railTop = rail.getBoundingClientRect().top;
  let bottom = 0;
  for (const card of cards) {
    const rects = card._range.getClientRects();
    const anchor = (rects.length ? rects[0] : card._range.getBoundingClientRect()).top - railTop;
    const top = Math.max(anchor - 4, bottom + 10);
    card.style.top = `${top}px`;
    bottom = top + card.offsetHeight;
  }
  rail.style.height = `${bottom}px`;
}

function setActive(id) {
  activeId = id;
  rail.querySelectorAll('.card:not(.draft)').forEach((c) => c.classList.toggle('active', Number(c.dataset.id) === id));
  paint();
}

function revealRange(range) {
  const r = range.getBoundingClientRect();
  if (r.top < 70 || r.bottom > window.innerHeight - 120) {
    window.scrollBy({ top: r.top - window.innerHeight / 3, behavior: 'smooth' });
  }
}

function updateBar() {
  const n = comments.length;
  const count = $('count');
  count.hidden = n === 0;
  count.textContent = `${n} comment${n === 1 ? '' : 's'}`;
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
  b.textContent = 'Approve';
  b.classList.remove('warn');
}

async function submit(body) {
  document.querySelectorAll('.actions button').forEach((b) => (b.disabled = true));
  try {
    await api('/api/submit', body);
    submitted = true;
    const n = (body.comments || []).length;
    const msg = {
      approve: ['Plan approved', 'Claude is leaving plan mode and starting the work.'],
      changes: ['Sent to Claude', `${n ? `${n} comment${n === 1 ? '' : 's'} sent. ` : ''}Claude will revise the plan and open a new review.`],
      terminal: ['Back to the terminal', 'Answer the plan dialog in Claude Code.'],
    }[body.decision];
    showDone(msg[0], msg[1]);
  } catch (e) {
    showDone('Could not send', `${e.message}. The review may have timed out in Claude Code.`);
  }
}

function showDone(title, text) {
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
  general.style.height = 'auto';
  general.style.height = `${general.scrollHeight}px`;
  updateBar();
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
  const left = Math.min(last.right + window.scrollX + 4, window.scrollX + document.documentElement.clientWidth - selbtn.offsetWidth - 8);
  selbtn.style.left = `${left}px`;
  selbtn.style.top = `${last.bottom + window.scrollY + 6}px`;
}

document.addEventListener('mouseup', (e) => {
  if (e.target === selbtn) return;
  setTimeout(showSelButton, 0);
});
document.addEventListener('keyup', (e) => {
  if (e.shiftKey || e.key === 'Shift') showSelButton();
});
selbtn.addEventListener('mousedown', (e) => e.preventDefault()); // keep the selection alive
selbtn.addEventListener('click', () => {
  const r = selectionInDoc();
  selbtn.hidden = true;
  if (!r) return;
  const range = r.cloneRange();
  window.getSelection().removeAllRanges();
  startDraft(range);
});

doc.addEventListener('mouseover', (e) => {
  const block = e.target.closest && e.target.closest('[data-s]');
  if (!block || !doc.contains(block)) return;
  const d = doc.getBoundingClientRect();
  const b = block.getBoundingClientRect();
  gutter._block = block;
  gutter.hidden = false;
  gutter.style.left = `${d.left + window.scrollX + 12}px`;
  gutter.style.top = `${b.top + window.scrollY + 2}px`;
});
doc.addEventListener('mouseleave', (e) => {
  if (e.relatedTarget !== gutter) gutter.hidden = true;
});
gutter.addEventListener('mouseleave', (e) => {
  if (!doc.contains(e.relatedTarget)) gutter.hidden = true;
});
gutter.addEventListener('click', () => {
  const block = gutter._block;
  if (!block) return;
  const range = document.createRange();
  range.selectNodeContents(block);
  gutter.hidden = true;
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
narrow.addEventListener('change', layout);
new ResizeObserver(layout).observe(doc);
window.addEventListener('beforeunload', (e) => {
  if (!submitted && (comments.length || draft)) e.preventDefault();
});

load();
