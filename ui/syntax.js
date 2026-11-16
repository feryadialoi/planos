'use strict';

// Small code highlighter for plan code blocks: comments, strings, numbers,
// keywords and types. Enough to make code scannable; not a full parser.
window.PlanSyntax = (() => {
  const KEYWORDS = new Set((
    'as async await break case catch class const continue crate def default defer del do dyn elif else enum export extends ' +
    'false fn for from func go if impl import in interface is let loop match mod move mut new nil None not null or package ' +
    'pass pub raise return select self Self static struct super switch then this throw trait true True False try type typeof ' +
    'unsafe use var void where while with yield fi done esac function local echo set unset ' +
    'SELECT FROM WHERE INSERT INTO VALUES UPDATE DELETE CREATE TABLE ALTER ADD COLUMN INDEX JOIN LEFT RIGHT ON AND OR NOT ' +
    'NULL PRIMARY KEY DEFAULT BEGIN COMMIT ROLLBACK GROUP BY ORDER LIMIT'
  ).split(/\s+/));
  const HASH_COMMENTS = new Set(['sh', 'bash', 'zsh', 'shell', 'python', 'py', 'yaml', 'yml', 'toml', 'ruby', 'rb', 'dockerfile', 'make', 'makefile', 'conf', 'ini']);
  const NONE = new Set(['', 'text', 'txt', 'plain', 'markdown', 'md', 'diff', 'mermaid']);

  function tokenizer(lang) {
    const comment = HASH_COMMENTS.has(lang)
      ? '#[^\\n]*'
      : lang === 'sql' ? '--[^\\n]*' : '\\/\\/[^\\n]*|\\/\\*[\\s\\S]*?\\*\\/';
    return new RegExp(
      `(${comment})|("(?:\\\\.|[^"\\\\])*"|'(?:\\\\.|[^'\\\\\\n])*'|\`(?:\\\\.|[^\`\\\\])*\`)|\\b(\\d[\\d_]*(?:\\.\\d+)?)\\b|\\b([A-Za-z_][A-Za-z0-9_]*)\\b`,
      'g',
    );
  }

  function span(cls, text) {
    const s = document.createElement('span');
    s.className = cls;
    s.textContent = text;
    return s;
  }

  // Replace the code element's text with highlighted spans.
  function highlight(code, lang) {
    lang = (lang || '').toLowerCase();
    if (NONE.has(lang)) return;
    const text = code.textContent;
    const re = tokenizer(lang);
    const out = document.createDocumentFragment();
    let last = 0;
    for (const m of text.matchAll(re)) {
      if (m.index > last) out.append(text.slice(last, m.index));
      if (m[1]) out.append(span('tok-comment', m[1]));
      else if (m[2]) out.append(span('tok-string', m[2]));
      else if (m[3]) out.append(span('tok-number', m[3]));
      else if (KEYWORDS.has(m[4])) out.append(span('tok-keyword', m[4]));
      else if (/^[A-Z][a-z0-9]/.test(m[4])) out.append(span('tok-type', m[4]));
      else out.append(m[4]);
      last = m.index + m[0].length;
    }
    if (last < text.length) out.append(text.slice(last));
    code.replaceChildren(out);
  }

  return { highlight };
})();
