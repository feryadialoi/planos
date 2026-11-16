'use strict';

// PlanOS page: a list of plans, and per session the plan with review threads,
// Claude's questions, the execution checklist and the plan check. State lives
// in the local service; the page long-polls it and never holds the only copy.

const $ = (id) => document.getElementById(id);
const token = new URLSearchParams(location.search).get('t') || '';
const main = $('main');
const selbtn = $('selbtn');
const gutter = $('gutter');
const general = $('general');
const stacked = window.matchMedia('(max-width: 1000px)');
const canHighlight = typeof CSS !== 'undefined' && CSS.highlights && typeof Highlight !== 'undefined';

const state = {
  route: { view: 'home' },
  version: 0,
  sessions: [],
  session: null,
  doc: null, // { round, draft, markdown, html, changed_lines, removed, hidden }
  docKey: '',
  viewRound: null, // number when looking at an older round
  tab: 'plan',
  showChanges: true,
  composer: null, // { range, start_line, end_line, quote, text, suggest, replacement, editingId }
  activeId: null,
  mode: storageGet('planos-mode') || 'default',
  approveArmed: null,
  pollToken: 0,
  railDirty: false,
};

// ---------- small helpers ----------

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
    /* blocked storage: the preference just isn't remembered */
  }
}

function el(tag, attrs, ...kids) {
  const n = document.createElement(tag);
  if (typeof attrs === 'string') n.className = attrs;
  else if (attrs) {
    for (const [k, v] of Object.entries(attrs)) {
      if (v === undefined || v === null || v === false) continue;
      if (k === 'class') n.className = v;
      else if (k.startsWith('on')) n.addEventListener(k.slice(2), v);
      else if (k === 'text') n.textContent = v;
      else n.setAttribute(k, v === true ? '' : v);
    }
  }
  for (const kid of kids.flat()) if (kid !== null && kid !== undefined && kid !== false) n.append(kid);
  return n;
}

function toast(text, bad) {
  const t = el('div', bad ? 'toast bad' : 'toast', text);
  $('toasts').append(t);
  setTimeout(() => t.remove(), bad ? 6000 : 3500);
}

function ago(ms) {
  const s = Math.max(0, Math.round((Date.now() - ms) / 1000));
  if (s < 60) return 'just now';
  if (s < 3600) return `${Math.round(s / 60)}m ago`;
  if (s < 86400) return `${Math.round(s / 3600)}h ago`;
  return new Date(ms).toLocaleDateString();
}

function clock(ms) {
  return new Date(ms).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
}

function tildePath(p) {
  return (p || '').replace(/^\/(Users|home)\/[^/]+/, '~');
}

// ---------- theme ----------

const savedTheme = storageGet('planos-theme');
if (savedTheme) document.documentElement.dataset.theme = savedTheme;
function isDark() {
  const t = document.documentElement.dataset.theme;
  return t === 'dark' || (!t && matchMedia('(prefers-color-scheme: dark)').matches);
}
$('theme').addEventListener('click', () => {
  document.documentElement.dataset.theme = isDark() ? 'light' : 'dark';
  storageSet('planos-theme', document.documentElement.dataset.theme);
  // Diagrams are drawn for one theme; redraw them.
  if (state.route.view === 'session') renderDoc(true);
});

// ---------- API ----------

async function api(method, path, body) {
  const res = await fetch(path, {
    method,
    headers: { 'X-PlanOS-Token': token, 'Content-Type': 'application/json' },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  if (res.status === 204) return null;
  const data = await res.json().catch(() => null);
  if (!res.ok) throw new Error((data && data.error) || `${res.status}`);
  return data;
}

// ---------- routing and polling ----------

function parseRoute() {
  const m = location.hash.match(/^#\/s\/([\w-]+)/);
  return m ? { view: 'session', id: m[1] } : { view: 'home' };
}

window.addEventListener('hashchange', () => {
  state.route = parseRoute();
  state.session = null;
  state.doc = null;
  state.docKey = '';
  state.viewRound = null;
  state.tab = 'plan';
  state.composer = null;
  state.activeId = null;
  state.version = 0;
  window.scrollTo(0, 0);
  poll();
});

async function poll() {
  const my = ++state.pollToken;
  let failures = 0;
  while (my === state.pollToken) {
    try {
      const path = state.route.view === 'session' ? `/api/s/${state.route.id}?v=${state.version}` : `/api/sessions?v=${state.version}`;
      const data = await api('GET', path);
      if (my !== state.pollToken) return;
      failures = 0;
      setConnected(true);
      state.version = data.version;
      if (state.route.view === 'session') applySession(data);
      else applySessions(data.sessions);
    } catch (e) {
      if (my !== state.pollToken) return;
      failures++;
      setConnected(false, e.message);
      if (state.route.view === 'session' && e.message === 'no such session') {
        renderMissing();
        return;
      }
      await new Promise((r) => setTimeout(r, Math.min(5000, 500 * failures)));
    }
  }
}

// Fetch now instead of waiting for the poll (after our own writes).
async function refresh() {
  state.version = 0;
  poll();
}

function setConnected(ok, why) {
  if (ok) return;
  $('status').textContent = why === '403' ? 'Link expired: open PlanOS again' : 'PlanOS service is not running';
  $('status-dot').className = 'status-dot bad';
}

// ---------- home ----------

const STATUS_LABEL = {
  drafting: ['Drafting', 'past'],
  review: ['Waiting for you', 'waiting'],
  revising: ['Revising', 'past'],
  executing: ['Working', 'current'],
  done: ['Done', 'current'],
};

function applySessions(list) {
  state.sessions = list;
  const waiting = list.filter((s) => s.waiting).length;
  const badge = $('nav-waiting');
  badge.hidden = !waiting;
  badge.textContent = waiting;
  document.title = waiting ? `(${waiting}) PlanOS` : 'PlanOS';
  $('outline-group').hidden = true;
  $('decide').hidden = true;
  $('nav-all').setAttribute('aria-current', 'true');
  $('status').textContent = waiting ? `${waiting} waiting for you` : 'Connected';
  $('status-dot').className = waiting ? 'status-dot warn' : 'status-dot';

  const working = list.filter((s) => s.status === 'executing').length;
  const done = list.filter((s) => s.status === 'done').length;
  const head = el('header', 'head',
    el('div', 'head-top', el('h1', { text: 'Plans' }), el('span', { class: 'help', text: 'Every Claude Code session that planned with PlanOS' })),
    el('div', 'stats',
      stat(waiting, 'Waiting for you', waiting > 0),
      stat(working, 'Claude working'),
      stat(done, 'Done')));

  const body = el('div', 'body body-single');
  if (!list.length) {
    body.append(el('div', 'empty',
      el('h2', { text: 'No plans yet' }),
      el('p', {}, 'In Claude Code, press ', el('kbd', { text: 'shift+tab' }), ' to enter plan mode. PlanOS shows the plan here while Claude writes it, and opens it for review when Claude presents it.')));
  } else {
    const rows = list.map((s) => {
      const [label, kind] = s.waiting === 'question' ? ['Question for you', 'waiting'] : STATUS_LABEL[s.status] || [s.status, 'past'];
      const meta = [tildePath(s.cwd), s.rounds ? `round ${s.rounds}` : 'draft'];
      if (s.open_comments) meta.push(`${s.open_comments} open comment${s.open_comments === 1 ? '' : 's'}`);
      if (s.todos) meta.push(`${s.todos_done}/${s.todos} tasks`);
      meta.push(ago(s.updated_at));
      return el('a', { class: 'row', href: `#/s/${s.id}` },
        el('div', 'row-top', el('span', { class: 'row-text', text: s.title }), el('span', { class: `pill ${kind}`, text: label })),
        el('div', 'meta', ...meta.map((m) => el('span', { text: m }))));
    });
    body.append(el('div', 'list', rows));
  }
  main.replaceChildren(head, body);
}

function stat(value, label, attention) {
  return el('div', attention ? 'stat attention' : 'stat', el('div', { class: 'stat-value', text: String(value) }), el('div', { class: 'stat-label', text: label }));
}

function renderMissing() {
  main.replaceChildren(el('div', 'empty', el('h2', { text: 'Plan not found' }), el('p', {}, 'It may have been removed. ', el('a', { href: '#/', text: 'See all plans' }))));
  $('decide').hidden = true;
}

// ---------- session ----------

function applySession(data) {
  const prev = state.session;
  state.session = data.session;
  if (state.viewRound === null) state.doc = data.doc;
  $('nav-all').removeAttribute('aria-current');
  const s = state.session;
  document.title = `${s.pending ? '● ' : ''}${s.title} · PlanOS`;

  // Lay out the page once; later updates touch only what changed.
  if (!main.querySelector('.session-view') || prev?.id !== s.id) buildSessionShell();
  renderHead();
  renderTabs();
  renderStatus();
  if (state.tab === 'plan') {
    renderDoc(false);
    renderQuestion();
    renderRail();
  } else {
    renderTabBody();
  }
  renderDecide();
}

function buildSessionShell() {
  main.replaceChildren(
    el('div', 'session-view',
      el('header', 'head', el('div', { class: 'head-top', id: 'head-top' }), el('div', { class: 'tabs', id: 'tabs', role: 'tablist' })),
      el('div', { id: 'question-slot', class: 'question-slot' }),
      el('div', { class: 'body', id: 'plan-body' },
        el('div', { class: 'doc-col' },
          el('div', { class: 'toolbar', id: 'toolbar' }),
          el('article', { id: 'doc', class: 'doc md', 'aria-label': 'Plan' })),
        el('aside', { id: 'rail', class: 'rail', 'aria-label': 'Comments' })),
      el('div', { class: 'body body-single', id: 'tab-body', hidden: true })));
  bindDocEvents($('doc'));
}

function renderHead() {
  const s = state.session;
  const [label, kind] = s.pending?.kind === 'question' ? ['Question for you', 'waiting'] : STATUS_LABEL[s.status] || [s.status, 'past'];
  const path = el('span', { class: 'scope-code', text: tildePath(s.plan_path || s.cwd), title: s.plan_path || s.cwd });
  $('head-top').replaceChildren(el('h1', { text: s.title }), el('span', { class: `pill ${kind}`, text: label }), path);
}

function renderTabs() {
  const s = state.session;
  const done = s.todos.filter((t) => t.status === 'completed').length;
  const tabs = [
    ['plan', 'Plan', s.comments.filter((c) => c.state !== 'resolved').length || null],
    ['execution', 'Execution', s.todos.length ? `${done}/${s.todos.length}` : null],
    ['check', 'Plan check', s.report ? s.report.items.length : null],
    ['history', 'History', s.rounds.length || null],
  ];
  $('tabs').replaceChildren(
    ...tabs.map(([id, label, count]) =>
      el('button', {
        class: 'tab', role: 'tab', 'aria-selected': String(state.tab === id),
        onclick: () => {
          state.tab = id;
          applySession({ session: state.session, doc: state.doc });
        },
      }, label, count !== null ? el('span', { class: id === 'plan' && count ? 'tab-badge' : 'tab-count', text: String(count) }) : null)));
  const planTab = state.tab === 'plan';
  $('plan-body').hidden = !planTab;
  $('question-slot').hidden = !planTab;
  $('tab-body').hidden = planTab;
  $('outline-group').hidden = !planTab;
}

function renderStatus() {
  const s = state.session;
  const done = s.todos.filter((t) => t.status === 'completed').length;
  const text = s.pending?.kind === 'review' ? 'Claude is waiting for your review'
    : s.pending?.kind === 'question' ? 'Claude asked you a question'
    : { drafting: 'Claude is drafting the plan', revising: 'Claude is revising the plan', executing: s.todos.length ? `Claude is working: ${done}/${s.todos.length} tasks` : 'Claude is working', done: 'Plan checked', review: 'Review closed' }[s.status] || '';
  $('status').textContent = text;
  $('status-dot').className = s.pending ? 'status-dot warn' : s.status === 'done' ? 'status-dot' : 'status-dot idle';
}

// ---------- plan document ----------

function renderDoc(force) {
  const d = state.doc;
  const docEl = $('doc');
  if (!d) {
    $('toolbar').replaceChildren();
    docEl.replaceChildren(el('div', 'loading', 'Claude has not written a plan yet.'));
    return;
  }
  const key = `${d.round}:${d.markdown}`;
  renderToolbar();
  if (!force && key === state.docKey) {
    applyChanges();
    return;
  }
  state.docKey = key;
  docEl.innerHTML = d.html; // rendered by the service; raw HTML in a plan is escaped there

  // The plan's title is already the page heading.
  const h1 = docEl.querySelector('h1');
  if (h1 && h1.textContent.trim() === state.session.title.trim()) h1.classList.add('dup-title');

  // The replies section is shown as threads, not as part of the plan.
  if (d.hidden) {
    for (const b of docEl.querySelectorAll('[data-s]')) {
      if (Number(b.dataset.s) >= d.hidden[0] && Number(b.dataset.e) <= d.hidden[1]) b.classList.add('replies-src');
    }
    const lists = [...docEl.children].filter((c) => c.querySelector?.('.replies-src') && !c.querySelector('[data-s]:not(.replies-src)'));
    lists.forEach((l) => l.classList.add('replies-src'));
  }

  for (const code of docEl.querySelectorAll('pre > code')) {
    const lang = (code.className.match(/language-([\w+-]+)/) || [])[1] || '';
    const pre = code.parentElement;
    if (lang) pre.dataset.lang = lang;
    if (lang === 'mermaid') {
      const box = el('div', { class: 'md-mermaid', 'data-source': code.textContent, 'data-s': pre.dataset.s, 'data-e': pre.dataset.e });
      box.append(el('pre', {}, el('code', { text: code.textContent })));
      pre.replaceWith(box);
    } else if (window.PlanSyntax) {
      window.PlanSyntax.highlight(code, lang);
    }
  }
  drawMermaid(docEl);
  applyChanges();
  renderOutline();
}

function renderToolbar() {
  const d = state.doc;
  const s = state.session;
  const items = [];
  if (state.viewRound !== null) {
    items.push(el('span', { class: 'pill past', text: `Round ${d.round}, read only` }),
      el('button', { class: 'link-btn', text: 'Back to the latest', onclick: () => showRound(null) }));
  } else if (d.draft) {
    items.push(el('span', { class: 'pill current', text: 'Live draft' }), el('span', { class: 'help', text: 'Updates as Claude writes the plan' }));
  } else {
    items.push(el('span', { class: 'pill past', text: `Round ${d.round}${s.rounds.length > 1 ? ` of ${s.rounds.length}` : ''}` }));
  }
  if (d.round > 1 && (d.changed_lines.length || d.removed.length)) {
    const seg = el('div', 'segmented',
      el('button', { 'aria-pressed': String(state.showChanges), text: 'Show changes', onclick: () => { state.showChanges = true; renderToolbar(); applyChanges(); layout(); } }),
      el('button', { 'aria-pressed': String(!state.showChanges), text: 'Clean', onclick: () => { state.showChanges = false; renderToolbar(); applyChanges(); layout(); } }));
    items.push(el('span', 'spacer'), el('span', { class: 'help', text: `${d.changed_lines.length} line${d.changed_lines.length === 1 ? '' : 's'} changed since round ${d.round - 1}` }), seg);
  }
  $('toolbar').replaceChildren(...items);
}

// Mark blocks that changed since the previous round and show removed text.
function applyChanges() {
  const docEl = $('doc');
  docEl.querySelectorAll('.removed-block').forEach((r) => r.remove());
  docEl.querySelectorAll('.changed').forEach((b) => b.classList.remove('changed'));
  const d = state.doc;
  if (!d || !state.showChanges || d.round < 2) return;
  const changed = new Set(d.changed_lines);
  const blocks = [...docEl.querySelectorAll('[data-s]')].filter((b) => !b.classList.contains('replies-src'));
  for (const b of blocks) {
    // Only leaf-ish blocks: a list item whose own text changed, not its parent list.
    const s = Number(b.dataset.s);
    const e = Number(b.dataset.e);
    const inner = b.querySelector('[data-s]');
    const ownEnd = inner ? Number(inner.dataset.s) - 1 : e;
    for (let l = s; l <= Math.max(s, ownEnd); l++) {
      if (changed.has(l)) {
        b.classList.add('changed');
        break;
      }
    }
  }
  for (const r of d.removed) {
    if (d.hidden && r.before_line >= d.hidden[0]) continue;
    const next = blocks.find((b) => Number(b.dataset.s) >= r.before_line && !b.querySelector('[data-s]'));
    const box = el('div', 'removed-block', el('span', { class: 'removed-label', text: 'Removed' }), el('div', { class: 'removed-text', text: r.text }));
    if (next) next.before(box);
    else docEl.append(box);
  }
}

let mermaidLoad = null;
let diagramCount = 0;
async function drawMermaid(root) {
  const pending = [...root.querySelectorAll('.md-mermaid')];
  if (!pending.length) return;
  mermaidLoad ??= new Promise((resolve, reject) => {
    const script = document.createElement('script');
    script.src = '/vendor/mermaid.min.js';
    script.onload = () => resolve(window.mermaid);
    script.onerror = () => {
      mermaidLoad = null;
      reject(new Error('could not load the diagram renderer'));
    };
    document.head.append(script);
  });
  let mermaid;
  try {
    mermaid = await mermaidLoad;
  } catch (e) {
    pending.forEach((b) => b.append(el('div', { class: 'md-error', text: e.message })));
    return;
  }
  mermaid.initialize({ startOnLoad: false, securityLevel: 'strict', theme: isDark() ? 'dark' : 'default' });
  for (const box of pending) {
    try {
      const { svg } = await mermaid.render(`planos-mermaid-${++diagramCount}`, box.dataset.source);
      const drawn = new DOMParser().parseFromString(svg, 'image/svg+xml').documentElement;
      if (drawn.nodeName !== 'svg') throw new Error('the diagram did not render');
      box.replaceChildren(document.importNode(drawn, true));
    } catch (e) {
      box.append(el('div', { class: 'md-error', text: `Diagram error: ${e?.message || e}` }));
    }
  }
  layout();
}

function renderOutline() {
  const toc = $('toc');
  const heads = [...$('doc').querySelectorAll('h1, h2, h3')].filter((h) => !h.classList.contains('replies-src') && !h.classList.contains('dup-title'));
  const counts = new Map();
  for (const c of state.session?.comments || []) {
    if (c.state === 'resolved') continue;
    const h = heads.filter((x) => Number(x.dataset.s) <= c.start_line).pop();
    if (h) counts.set(h, (counts.get(h) || 0) + 1);
  }
  toc.replaceChildren(...heads.map((h) => {
    const a = el('button', { class: `toc-item${h.tagName === 'H3' ? ' sub' : ''}`, onclick: () => jumpTo(h) },
      el('span', { class: 'toc-name', text: h.textContent }),
      counts.get(h) ? el('span', { class: 'tab-badge', text: String(counts.get(h)) }) : null);
    a._heading = h;
    return a;
  }));
  markCurrentSection();
}

// A clicked section stays current until you scroll yourself: the last
// sections of a plan can't scroll up to the top, so position alone can't tell.
let pinnedHeading = null;

function jumpTo(h) {
  pinnedHeading = h;
  markCurrentSection();
  h.scrollIntoView({ behavior: 'smooth', block: 'start' });
}

function unpinSection() {
  if (pinnedHeading) {
    pinnedHeading = null;
    markCurrentSection();
  }
}
window.addEventListener('wheel', unpinSection, { passive: true });
window.addEventListener('touchstart', unpinSection, { passive: true });
document.addEventListener('keydown', (e) => {
  if (['ArrowUp', 'ArrowDown', 'PageUp', 'PageDown', 'Home', 'End', ' '].includes(e.key)) unpinSection();
});
document.addEventListener('mousedown', (e) => {
  if (!e.target.closest('#toc')) unpinSection();
});

function markCurrentSection() {
  const items = [...$('toc').querySelectorAll('.toc-item')];
  let current = items.find((a) => a._heading === pinnedHeading);
  if (!current) {
    // At the bottom of the page, the last headings never reach the top line;
    // count any heading visible above the decision bar instead.
    const atBottom = window.innerHeight + window.scrollY >= document.documentElement.scrollHeight - 4;
    const bar = $('decide');
    const visibleBottom = bar.hidden ? window.innerHeight : bar.getBoundingClientRect().top;
    const line = atBottom ? visibleBottom - 20 : 140;
    current = items[0];
    for (const a of items) if (a._heading.getBoundingClientRect().top < line) current = a;
  }
  items.forEach((a) => a.setAttribute('aria-current', String(a === current)));
}

async function showRound(n) {
  if (n === null) {
    state.viewRound = null;
    state.tab = 'plan';
    refresh();
    return;
  }
  try {
    state.doc = await api('GET', `/api/s/${state.session.id}/round/${n}`);
    state.viewRound = n;
    state.tab = 'plan';
    state.composer = null;
    applySession({ session: state.session, doc: state.doc });
    window.scrollTo(0, 0);
  } catch (e) {
    toast(`Could not open round ${n}: ${e.message}`, true);
  }
}

// ---------- ranges and lines ----------

function blockAt(container, offset, atEnd) {
  const docEl = $('doc');
  let node = container;
  if (node.nodeType === Node.ELEMENT_NODE && node.childNodes.length) {
    node = node.childNodes[atEnd ? offset - 1 : offset] || node;
  }
  const e = node.nodeType === Node.ELEMENT_NODE ? node : node.parentElement;
  if (!e || !docEl.contains(e)) return null;
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
  const nested = b.querySelector(':scope > ul > li[data-s], :scope > ol > li[data-s]');
  if (nested && !nested.parentElement.contains(range.endContainer)) e = Math.min(e, Number(nested.dataset.s) - 1);
  return [s, Math.max(e, s)];
}

function lineLabel(s, e) {
  if (!s) return 'General';
  return e > s ? `Lines ${s}-${e}` : `Line ${s}`;
}

// A saved comment's range in the rendered page, from its lines and quote.
function rangeFor(c) {
  const docEl = $('doc');
  const blocks = [...docEl.querySelectorAll('[data-s]')].filter((b) => Number(b.dataset.s) <= c.end_line && Number(b.dataset.e) >= c.start_line && !b.classList.contains('replies-src'));
  if (!blocks.length) return null;
  const start = blocks[0];
  const walker = document.createTreeWalker(docEl, NodeFilter.SHOW_TEXT);
  const nodes = [];
  let text = '';
  let from = -1;
  for (let n = walker.nextNode(); n; n = walker.nextNode()) {
    if (from < 0 && start.contains(n)) from = text.length;
    nodes.push([n, text.length]);
    text += n.data;
  }
  const words = c.quote.trim().split(/\s+/).map((w) => w.replace(/[.*+?^${}()|[\]\\]/g, '\\$&'));
  const re = new RegExp(words.join('\\s*'), 'g');
  re.lastIndex = Math.max(0, from);
  const m = re.exec(text);
  if (!m) {
    const r = document.createRange();
    r.selectNodeContents(start);
    return r;
  }
  // Text node and offset for a position in `text`. A start belongs to the
  // node it begins; an end to the node it closes.
  const locate = (pos, isEnd) => {
    for (const [n, off] of nodes) {
      const len = n.data.length;
      if (isEnd ? pos > off && pos <= off + len : pos >= off && pos < off + len) return [n, pos - off];
    }
    const [n] = nodes[nodes.length - 1];
    return [n, n.data.length];
  };
  const r = document.createRange();
  const [sn, so] = locate(m.index, false);
  const [en, eo] = locate(m.index + m[0].length, true);
  r.setStart(sn, so);
  r.setEnd(en, eo);
  return r;
}

function selectionInDoc() {
  const docEl = $('doc');
  if (!docEl || state.viewRound !== null || state.tab !== 'plan') return null;
  const sel = window.getSelection();
  if (!sel.rangeCount || sel.isCollapsed) return null;
  const r = sel.getRangeAt(0);
  if (!docEl.contains(r.commonAncestorContainer) || !r.toString().trim()) return null;
  return r;
}

// ---------- comment rail ----------

const ranges = new Map(); // comment id -> Range

function renderRail() {
  const rail = $('rail');
  // Don't pull the rug out from under someone typing a reply.
  if (rail.contains(document.activeElement) && document.activeElement.tagName === 'TEXTAREA' && !document.activeElement.closest('.composer')) {
    state.railDirty = true;
    return;
  }
  state.railDirty = false;
  const s = state.session;
  ranges.clear();
  if (state.viewRound === null) for (const c of s.comments) if (!c.outdated) ranges.set(c.id, rangeFor(c));

  const live = state.viewRound === null;
  const comments = live ? s.comments : [];
  const anchored = comments.filter((c) => ranges.get(c.id));
  const loose = comments.filter((c) => !ranges.get(c.id));
  const entries = anchored.map((c) => ({ node: threadCard(c), range: ranges.get(c.id) }));
  if (state.composer) entries.push({ node: composerCard(), range: state.composer.range });
  entries.sort((a, b) => a.range.compareBoundaryPoints(Range.START_TO_START, b.range));

  const nodes = [];
  if (!comments.length && !state.composer) {
    nodes.push(hintCard());
  }
  for (const e of entries) {
    e.node._range = e.range;
    nodes.push(e.node);
  }
  if (loose.length) {
    nodes.push(el('div', 'rail-group', el('h3', { text: 'Not in this version' }), ...loose.map(threadCard)));
  }
  rail.replaceChildren(...nodes);
  paint();
  renderCounts();
  renderOutline();
  layout();
  const ta = rail.querySelector('.composer textarea');
  if (ta && document.activeElement !== ta) {
    grow(ta);
    ta.focus();
    ta.setSelectionRange(ta.value.length, ta.value.length);
  }
}

function hintCard() {
  const live = state.doc && !state.doc.draft;
  return el('div', 'hint',
    el('h3', { text: 'How to review' }),
    el('div', 'hint-row', el('span', { text: 'Select text, then press' }), el('kbd', { text: 'C' })),
    el('div', 'hint-row', el('span', { text: 'Comment on a whole block' }), el('kbd', { text: '+' })),
    el('div', 'hint-row', el('span', { text: 'Suggest new wording' }), el('kbd', { text: 'S' })),
    el('div', 'hint-row', el('span', { text: 'Save a comment' }), el('kbd', { text: '⌘ ↵' })),
    live ? null : el('p', { class: 'help hint-note', text: 'You can comment on the draft now. Comments go out when Claude presents the plan, or with your next message.' }));
}

const STATE_PILL = { draft: ['Not sent', 'past'], open: ['Open', 'waiting'], resolved: ['Resolved', 'current'] };

function threadCard(c) {
  const [label, kind] = STATE_PILL[c.state];
  const card = el('div', `card thread${c.id === state.activeId ? ' active' : ''}${c.state === 'resolved' ? ' resolved' : ''}`);
  card.dataset.id = c.id;
  const tools = el('div', 'card-tools');
  if (c.state === 'draft') {
    tools.append(
      el('button', { class: 'link-btn', text: 'Edit', onclick: (e) => { e.stopPropagation(); editDraft(c); } }),
      el('button', { class: 'link-btn danger', text: 'Delete', onclick: (e) => { e.stopPropagation(); deleteComment(c); } }));
  } else if (c.state === 'open') {
    tools.append(el('button', { class: 'link-btn', text: 'Resolve', onclick: (e) => { e.stopPropagation(); patchComment(c, { state: 'resolved' }); } }));
  } else {
    tools.append(el('button', { class: 'link-btn', text: 'Reopen', onclick: (e) => { e.stopPropagation(); patchComment(c, { state: 'open' }); } }));
  }
  card.append(el('div', 'card-head',
    el('span', { class: 'cid', text: c.state === 'draft' ? 'new' : c.id }),
    el('span', { class: 'pill line', text: lineLabel(c.start_line, c.end_line) }),
    el('span', { class: `pill ${kind}`, text: label }),
    c.outdated ? el('span', { class: 'pill bad', text: 'Text changed' }) : null,
    tools));
  if (c.state === 'resolved') {
    card.append(el('div', { class: 'note-text clamp', text: c.messages[0]?.text || '' }));
  } else {
    card.append(el('div', { class: 'quote', text: c.quote }));
    if (c.suggestion) card.append(el('div', 'suggestion', el('span', { class: 'suggestion-label', text: 'Suggested wording' }), el('div', { text: c.suggestion })));
    for (const m of c.messages) {
      if (!m.text) continue;
      card.append(el('div', `msg ${m.author}`,
        el('div', 'msg-head', el('span', { class: `avatar ${m.author}`, text: m.author === 'claude' ? 'C' : 'Y' }), el('span', { class: 'who', text: m.author === 'claude' ? 'Claude' : 'You' }), el('span', { class: 'help', text: `round ${Math.max(1, m.round)}` })),
        el('div', { class: 'note-text', text: m.text })));
    }
    if (c.state === 'open') card.append(replyBox(c));
  }
  card.addEventListener('click', (e) => {
    if (e.target.closest('textarea, button')) return;
    setActive(c.id);
    const r = ranges.get(c.id);
    if (r) revealRange(r);
  });
  return card;
}

function replyBox(c) {
  const last = c.messages[c.messages.length - 1];
  const awaiting = last?.author === 'you' && !c.reply_draft;
  const ta = el('textarea', { rows: 1, placeholder: awaiting ? 'Add to your comment…' : 'Reply to Claude…' });
  ta.value = c.reply_draft || '';
  let timer = 0;
  const save = () => patchComment(c, { reply_draft: ta.value }, true);
  ta.addEventListener('input', () => {
    grow(ta);
    clearTimeout(timer);
    timer = setTimeout(save, 600);
  });
  ta.addEventListener('blur', () => {
    clearTimeout(timer);
    if ((ta.value || '') !== (c.reply_draft || '')) save();
    if (state.railDirty) setTimeout(renderRail, 0);
  });
  ta.addEventListener('keydown', (e) => {
    if (e.key === 'Escape') ta.blur();
  });
  return el('div', 'reply',
    ta,
    c.reply_draft ? el('div', { class: 'help', text: 'Saved. Goes out with your next decision.' }) : null);
}

function composerCard() {
  const d = state.composer;
  const card = el('div', 'card composer');
  const ta = el('textarea', { rows: 2, placeholder: d.suggest ? 'Why this wording? (optional)' : 'What should Claude change here?' });
  ta.value = d.text || '';
  ta.addEventListener('input', () => {
    d.text = ta.value;
    grow(ta);
    layout();
    renderCounts();
  });
  const keys = (e) => {
    if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) {
      e.preventDefault();
      saveComposer();
    } else if (e.key === 'Escape') {
      e.preventDefault();
      closeComposer();
    }
  };
  ta.addEventListener('keydown', keys);
  const kindSeg = el('div', 'segmented small',
    el('button', { 'aria-pressed': String(!d.suggest), text: 'Comment', onclick: () => { d.suggest = false; renderRail(); } }),
    el('button', { 'aria-pressed': String(!!d.suggest), text: 'Suggest edit', onclick: () => { d.suggest = true; d.replacement ??= d.quote; renderRail(); } }));
  card.append(el('div', 'card-head', el('span', { class: 'pill line', text: lineLabel(d.start_line, d.end_line) }), el('span', 'spacer'), d.editingId ? null : kindSeg));
  card.append(el('div', { class: 'quote', text: d.quote }));
  if (d.suggest) {
    const rep = el('textarea', { class: 'replacement', rows: 2, 'aria-label': 'Suggested wording' });
    rep.value = d.replacement ?? d.quote;
    rep.addEventListener('input', () => {
      d.replacement = rep.value;
      grow(rep);
      layout();
    });
    rep.addEventListener('keydown', keys);
    card.append(el('div', 'suggestion editing', el('span', { class: 'suggestion-label', text: 'Replace with' }), rep));
    setTimeout(() => grow(rep), 0);
  }
  card.append(ta);
  const tip = el('span', 'help', el('kbd', { text: '⌘ ↵' }), ' to save');
  card.append(el('div', 'card-actions', tip,
    el('button', { class: 'btn ghost', text: 'Cancel', onclick: closeComposer }),
    el('button', { class: 'btn primary', text: d.editingId ? 'Save' : 'Comment', onclick: saveComposer })));
  return card;
}

function grow(ta) {
  ta.style.height = 'auto';
  ta.style.height = `${ta.scrollHeight}px`;
}

function paint() {
  if (!canHighlight) return;
  const s = state.session;
  const open = [];
  const active = [];
  for (const c of s?.comments || []) {
    const r = ranges.get(c.id);
    if (!r || c.state === 'resolved') continue;
    (c.id === state.activeId ? active : open).push(r);
  }
  CSS.highlights.set('planos-comment', new Highlight(...open));
  CSS.highlights.set('planos-active', new Highlight(...active));
  CSS.highlights.set('planos-draft', new Highlight(...(state.composer ? [state.composer.range] : [])));
}

// Cards sit beside the text they point at, pushed down when they would overlap.
function layout() {
  const rail = $('rail');
  if (!rail || rail.offsetParent === null) return;
  const cards = [...rail.children].filter((c) => c._range);
  if (stacked.matches) {
    rail.querySelectorAll('.card').forEach((c) => (c.style.top = ''));
    rail.style.minHeight = '';
    return;
  }
  const railTop = rail.getBoundingClientRect().top;
  let bottom = 0;
  // Static items (hint, outdated group) flow first.
  for (const n of rail.children) if (!n._range) bottom = Math.max(bottom, n.offsetTop + n.offsetHeight);
  for (const card of cards) {
    const rects = card._range.getClientRects();
    const anchor = (rects.length ? rects[0] : card._range.getBoundingClientRect()).top - railTop;
    const top = Math.max(anchor - 12, bottom + 10);
    card.style.top = `${top}px`;
    bottom = top + card.offsetHeight;
  }
  rail.style.minHeight = `${bottom}px`;
}

function setActive(id) {
  state.activeId = id;
  $('rail').querySelectorAll('.thread').forEach((c) => c.classList.toggle('active', c.dataset.id === id));
  paint();
}

function revealRange(range) {
  const r = range.getBoundingClientRect();
  if (r.top < 60 || r.bottom > window.innerHeight - 140) window.scrollBy({ top: r.top - window.innerHeight / 3, behavior: 'smooth' });
}

function sendable() {
  const s = state.session;
  if (!s) return 0;
  return s.comments.filter((c) => c.state === 'draft' || c.reply_draft).length;
}

function renderCounts() {
  const n = sendable();
  const count = $('count');
  count.hidden = n === 0;
  count.textContent = n;
  const composerText = state.composer && (state.composer.text || '').trim();
  $('changes').disabled = n === 0 && !general.value.trim() && !composerText;
  disarmApprove();
}

// ---------- comment actions ----------

function startComposer(range, span, suggest) {
  if (state.viewRound !== null) return;
  const [s, e] = span || lineSpan(range);
  const quote = range.toString().replace(/\s+/g, ' ').trim();
  if (!quote) return;
  state.composer = { range, start_line: s, end_line: e, quote, text: '', suggest: !!suggest, replacement: suggest ? quote : null, editingId: null };
  state.activeId = null;
  renderRail();
}

function editDraft(c) {
  const range = ranges.get(c.id);
  if (!range) return;
  state.composer = { range, start_line: c.start_line, end_line: c.end_line, quote: c.quote, text: c.messages[0]?.text || '', suggest: !!c.suggestion, replacement: c.suggestion, editingId: c.id };
  renderRail();
}

function closeComposer() {
  state.composer = null;
  renderRail();
}

async function saveComposer() {
  const d = state.composer;
  if (!d) return;
  const text = (d.text || '').trim();
  const suggestion = d.suggest ? (d.replacement || '').trim() : null;
  if (!text && !suggestion) {
    $('rail').querySelector('.composer textarea')?.focus();
    return;
  }
  try {
    if (d.editingId) {
      await api('PATCH', `/api/s/${state.session.id}/comments/${d.editingId}`, { text, suggestion: suggestion ?? '' });
    } else {
      await api('POST', `/api/s/${state.session.id}/comments`, { start_line: d.start_line, end_line: d.end_line, quote: d.quote, text, suggestion });
    }
    state.composer = null;
    refresh();
  } catch (e) {
    toast(`Could not save the comment: ${e.message}`, true);
  }
}

async function patchComment(c, body, quiet) {
  try {
    await api('PATCH', `/api/s/${state.session.id}/comments/${c.id}`, body);
    if (!quiet) refresh();
  } catch (e) {
    toast(`Could not update ${c.id}: ${e.message}`, true);
  }
}

async function deleteComment(c) {
  try {
    await api('DELETE', `/api/s/${state.session.id}/comments/${c.id}`);
    refresh();
  } catch (e) {
    toast(`Could not delete the comment: ${e.message}`, true);
  }
}

// ---------- Claude's questions ----------

function renderQuestion() {
  const slot = $('question-slot');
  const p = state.session.pending;
  if (p?.kind !== 'question' || state.viewRound !== null) {
    slot.replaceChildren();
    return;
  }
  if (slot.dataset.rid === String(p.id)) return; // keep what you have typed
  slot.dataset.rid = String(p.id);
  const questions = p.tool_input.questions || [];
  const form = el('form', 'question-card');
  form.append(el('div', 'question-title', el('span', { class: 'pill waiting', text: 'Claude is asking' }), el('span', { class: 'help', text: 'Your answers go straight back to Claude.' })));
  questions.forEach((q, i) => {
    const name = `q${i}`;
    const opts = (q.options || []).map((o) =>
      el('label', 'option',
        el('input', { type: q.multiSelect ? 'checkbox' : 'radio', name, value: o.label }),
        el('span', 'option-body', el('span', { class: 'option-label', text: o.label }), o.description ? el('span', { class: 'option-desc', text: o.description }) : null)));
    const other = el('input', { type: 'text', class: 'other', name: `${name}-other`, placeholder: 'Other (type your own answer)' });
    form.append(el('fieldset', 'question',
      el('legend', {}, q.header ? el('span', { class: 'pill past', text: q.header }) : null, el('span', { class: 'question-text', text: q.question })),
      el('div', 'options', ...opts),
      other));
  });
  const send = el('button', { class: 'btn primary', type: 'submit', text: 'Send answers' });
  form.append(el('div', 'card-actions',
    el('span', 'spacer'),
    el('button', { class: 'btn ghost', type: 'button', text: 'Answer in terminal', onclick: () => decide({ decision: 'terminal' }) }),
    send));
  form.addEventListener('submit', (e) => {
    e.preventDefault();
    const answers = {};
    for (const [i, q] of questions.entries()) {
      const picked = [...form.querySelectorAll(`input[name="q${i}"]:checked`)].map((x) => x.value);
      const other = form.querySelector(`input[name="q${i}-other"]`).value.trim();
      if (other) picked.push(other);
      if (!picked.length) {
        toast(`Answer: ${q.question}`, true);
        return;
      }
      answers[q.question] = picked.join(', ');
    }
    decide({ decision: 'answers', answers });
  });
  slot.replaceChildren(form);
}

// ---------- decision bar ----------

function renderDecide() {
  const s = state.session;
  const bar = $('decide');
  const review = s.pending?.kind === 'review';
  const live = state.viewRound === null && state.tab === 'plan';
  const canQueue = !s.pending && live && state.doc;
  bar.hidden = !(live && (review || canQueue));
  if (bar.hidden) return;
  $('terminal').hidden = !review;
  $('approve-group').hidden = !review;
  $('changes').firstChild.textContent = review ? 'Request changes ' : 'Send with my next message ';
  $('decide-note').textContent = review ? '' : 'Claude is not waiting for a review.';
  for (const b of document.querySelectorAll('#approve-group [data-mode]')) b.setAttribute('aria-pressed', String(b.dataset.mode === state.mode));
  renderCounts();
}

document.querySelectorAll('#approve-group [data-mode]').forEach((b) =>
  b.addEventListener('click', () => {
    state.mode = b.dataset.mode;
    storageSet('planos-mode', state.mode);
    renderDecide();
  }));

function disarmApprove() {
  if (state.approveArmed) clearTimeout(state.approveArmed);
  state.approveArmed = null;
  $('approve').textContent = 'Approve plan';
  $('approve').classList.remove('warn');
}

async function decide(body) {
  const buttons = document.querySelectorAll('#decide button, .question-card button');
  buttons.forEach((b) => (b.disabled = true));
  try {
    const res = await api('POST', `/api/s/${state.session.id}/decide`, body);
    general.value = '';
    grow(general);
    $('question-slot').dataset.rid = '';
    toast({
      approve: 'Plan approved. Claude is starting the work.',
      changes: 'Sent to Claude. It will revise the plan.',
      queued: 'Saved. It goes to Claude with your next message.',
      answers: 'Answers sent to Claude.',
      terminal: 'Back to the terminal.',
    }[res.sent] || 'Sent.');
    if (res.sent === 'approve') state.tab = 'execution';
    refresh();
  } catch (e) {
    toast(e.message, true);
  } finally {
    buttons.forEach((b) => (b.disabled = false));
    renderCounts();
  }
}

$('approve').addEventListener('click', () => {
  const n = sendable();
  if (n && !state.approveArmed) {
    $('approve').textContent = `Approve and drop ${n} unsent?`;
    $('approve').classList.add('warn');
    state.approveArmed = setTimeout(disarmApprove, 4000);
    return;
  }
  decide({ decision: 'approve', mode: state.mode });
});

$('changes').addEventListener('click', async () => {
  if (state.composer && (state.composer.text || '').trim()) await saveComposer();
  decide({ decision: 'changes', general: general.value });
});

$('terminal').addEventListener('click', () => decide({ decision: 'terminal' }));

general.addEventListener('input', () => {
  grow(general);
  renderCounts();
});

// ---------- other tabs ----------

function renderTabBody() {
  const s = state.session;
  const body = $('tab-body');
  if (state.tab === 'execution') body.replaceChildren(...executionView(s));
  else if (state.tab === 'check') body.replaceChildren(...checkView(s));
  else body.replaceChildren(...historyView(s));
}

const TODO_ICON = { completed: ['✓', 'current'], in_progress: ['●', 'waiting'], pending: ['○', 'past'] };

function executionView(s) {
  if (!s.approved) {
    return [el('div', 'empty', el('h2', { text: 'Nothing approved yet' }), el('p', { text: 'Once you approve the plan, Claude\'s task list and every file it touches show up here as it works.' }))];
  }
  const mode = { default: 'manual', acceptEdits: 'accept edits', auto: 'auto' }[s.approved.mode] || s.approved.mode;
  const done = s.todos.filter((t) => t.status === 'completed').length;
  const out = [el('div', 'section-head', el('h2', { text: 'Tasks' }), el('span', { class: 'help', text: `Round ${s.approved.round} approved at ${clock(s.approved.at)} in ${mode} mode` }))];
  if (s.todos.length) {
    out.push(el('div', 'meter', el('span', { style: `width:${Math.round((done / s.todos.length) * 100)}%` })));
    out.push(el('div', 'list', ...s.todos.map((t) => {
      const [icon, kind] = TODO_ICON[t.status] || ['○', 'past'];
      return el('div', `row todo ${t.status}`, el('span', { class: `todo-icon ${kind}`, text: icon }), el('span', { class: 'row-text', text: t.text }));
    })));
  } else {
    out.push(el('p', { class: 'help', text: 'Claude has not made a task list yet.' }));
  }
  out.push(el('div', 'section-head', el('h2', { text: 'Activity' }), el('span', { class: 'help', text: `${s.activity.length} tool calls` })));
  if (s.activity.length) {
    out.push(el('div', 'list activity', ...s.activity.slice().reverse().slice(0, 120).map((a) =>
      el('div', 'row', el('span', { class: 'act-time', text: clock(a.at) }), el('span', { class: 'pill past', text: a.tool }), el('span', { class: 'act-text', text: tildePath(a.summary) })))));
  } else {
    out.push(el('p', { class: 'help', text: 'No tool calls yet.' }));
  }
  return out;
}

const CHECK_PILL = { done: 'current', changed: 'waiting', partial: 'waiting', skipped: 'bad' };

function checkView(s) {
  if (!s.report) {
    const why = s.status === 'executing'
      ? (s.report_requested ? 'PlanOS asked Claude for a plan check. It appears here when Claude answers.' : 'When the work looks finished, PlanOS asks Claude to check each step of the approved plan.')
      : 'Approve a plan first. When the work is finished, Claude checks each step here.';
    return [el('div', 'empty', el('h2', { text: 'No plan check yet' }), el('p', { text: why }))];
  }
  const counts = {};
  for (const i of s.report.items) counts[i.status] = (counts[i.status] || 0) + 1;
  return [
    el('div', 'stats four', ...['done', 'changed', 'partial', 'skipped'].map((k) => stat(counts[k] || 0, k[0].toUpperCase() + k.slice(1), k === 'skipped' && counts[k] > 0))),
    el('div', 'list check', ...s.report.items.map((i) => el('div', 'row', el('span', { class: `pill ${CHECK_PILL[i.status] || 'past'}`, text: i.status }), el('span', { class: 'row-text', text: i.text })))),
    el('p', { class: 'help', text: `Checked at ${clock(s.report.at)}` }),
  ];
}

function historyView(s) {
  if (!s.rounds.length) return [el('div', 'empty', el('h2', { text: 'No rounds yet' }), el('p', { text: 'Each time Claude presents the plan, it is saved here as a round.' }))];
  const sentIn = (n) => s.comments.filter((c) => c.messages.some((m) => m.author === 'you' && m.round === n)).length;
  return [el('div', 'list', ...s.rounds.slice().reverse().map((r) =>
    el('button', { class: 'row history-row', onclick: () => showRound(r.n === s.rounds.length ? null : r.n) },
      el('div', 'row-top', el('span', { class: 'row-text', text: `Round ${r.n}` }), r.n === s.rounds.length ? el('span', { class: 'pill current', text: 'Latest' }) : null),
      el('div', 'meta',
        el('span', { text: clock(r.at) }),
        el('span', { text: r.n === 1 ? 'first version' : `${r.changed_lines.length} lines changed` }),
        el('span', { text: `${sentIn(r.n)} comment${sentIn(r.n) === 1 ? '' : 's'} on it` })))))];
}

// ---------- selection button, keys and block gutter ----------

function placeSelButton() {
  const r = selectionInDoc();
  if (!r || state.composer) {
    selbtn.hidden = true;
    return;
  }
  const rects = [...r.getClientRects()].filter((x) => x.width > 1 && x.height > 1);
  const last = rects.length ? rects[rects.length - 1] : r.getBoundingClientRect();
  selbtn.hidden = false;
  const w = selbtn.offsetWidth;
  const h = selbtn.offsetHeight;
  const box = $('doc').getBoundingClientRect();
  const bar = $('decide');
  const floor = (bar.hidden ? window.innerHeight : bar.getBoundingClientRect().top) - 8;
  const left = Math.min(Math.max(last.right - w, box.left + 8), box.right - w - 8);
  let top = last.bottom + 8;
  if (top + h > floor) top = (rects[0] || last).top - h - 8;
  if (top < 8 || last.bottom < 0 || last.top > floor) {
    selbtn.hidden = true;
    return;
  }
  selbtn.style.left = `${left}px`;
  selbtn.style.top = `${top}px`;
}

let selFrame = 0;
function showSelButton() {
  cancelAnimationFrame(selFrame);
  selFrame = requestAnimationFrame(placeSelButton);
}

function commentOnSelection(suggest) {
  const r = selectionInDoc();
  selbtn.hidden = true;
  if (!r) return false;
  const range = r.cloneRange();
  window.getSelection().removeAllRanges();
  startComposer(range, null, suggest);
  return true;
}

let dragging = false;
document.addEventListener('mousedown', (e) => {
  if (selbtn.contains(e.target)) return;
  dragging = true;
  selbtn.hidden = true;
});
document.addEventListener('mouseup', () => {
  dragging = false;
  showSelButton();
});
document.addEventListener('selectionchange', () => {
  if (!selectionInDoc()) selbtn.hidden = true;
  else if (!dragging) showSelButton();
});
document.addEventListener('keydown', (e) => {
  const typing = e.target.closest && e.target.closest('textarea, input');
  if (typing || e.metaKey || e.ctrlKey || e.altKey) return;
  const k = e.key.toLowerCase();
  if ((k === 'c' || k === 's') && commentOnSelection(k === 's')) e.preventDefault();
});
selbtn.addEventListener('mousedown', (e) => e.preventDefault());
selbtn.addEventListener('click', () => commentOnSelection(false));

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

function bindDocEvents(docEl) {
  docEl.addEventListener('mouseover', (e) => {
    if (state.viewRound !== null) return;
    const block = e.target.closest && e.target.closest('[data-s]');
    if (!block || !docEl.contains(block) || block.classList.contains('replies-src')) return;
    const d = docEl.getBoundingClientRect();
    const line = firstLineRect(block);
    gutter._block = block;
    gutter.hidden = false;
    gutter.style.left = `${d.left + 12}px`;
    gutter.style.top = `${line.top + line.height / 2 - 12}px`;
  });
  docEl.addEventListener('mouseleave', (e) => {
    if (e.relatedTarget !== gutter && !gutter.contains(e.relatedTarget)) gutter.hidden = true;
  });
  docEl.addEventListener('click', (e) => {
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
    let hit = null;
    for (const [id, r] of ranges) {
      try {
        if (r && r.isPointInRange(node, offset)) hit = id;
      } catch {
        /* range from another render */
      }
    }
    setActive(hit);
  });
  new ResizeObserver(layout).observe(docEl);
}

gutter.addEventListener('mouseenter', () => setHovered(gutter._block));
gutter.addEventListener('mouseleave', (e) => {
  setHovered(null);
  if (!$('doc')?.contains(e.relatedTarget)) gutter.hidden = true;
});
gutter.addEventListener('click', () => {
  const block = gutter._block;
  if (!block) return;
  const range = document.createRange();
  range.selectNodeContents(block);
  gutter.hidden = true;
  setHovered(null);
  startComposer(range, [Number(block.dataset.s), Number(block.dataset.e)]);
});

window.addEventListener('scroll', () => {
  if (!selbtn.hidden) showSelButton();
  gutter.hidden = true;
  setHovered(null);
  if (state.route.view === 'session' && state.tab === 'plan') markCurrentSection();
}, { passive: true });
window.addEventListener('resize', layout);
stacked.addEventListener('change', layout);

// ---------- start ----------

state.route = parseRoute();
poll();
