//! Markdown to HTML, with every commentable block tagged by the plan lines it
//! came from (`data-s` / `data-e`, 1-based, inclusive). The UI reads these to
//! tell Claude which lines a comment points at.

use pulldown_cmark::{CodeBlockKind, CowStr, Event, Options, Parser, Tag, TagEnd, html};

pub fn render(md: &str) -> String {
    let lines = LineIndex::new(md);
    let opts = Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_GFM;

    let mut events: Vec<Event> = Vec::new();
    for (event, range) in Parser::new_ext(md, opts).into_offset_iter() {
        let attr = || {
            let (s, e) = lines.span(range.start, range.end);
            format!(r#" data-s="{s}" data-e="{e}""#)
        };
        match event {
            Event::Start(Tag::Paragraph) => events.push(raw(format!("<p{}>", attr()))),
            Event::End(TagEnd::Paragraph) => events.push(raw("</p>\n".into())),
            Event::Start(Tag::Heading { level, .. }) => {
                events.push(raw(format!("<{level}{}>", attr())))
            }
            Event::End(TagEnd::Heading(level)) => events.push(raw(format!("</{level}>\n"))),
            Event::Start(Tag::Item) => events.push(raw(format!("<li{}>", attr()))),
            Event::End(TagEnd::Item) => events.push(raw("</li>\n".into())),
            Event::Start(Tag::CodeBlock(kind)) => {
                let lang = match &kind {
                    CodeBlockKind::Fenced(info) => info.split_whitespace().next().unwrap_or(""),
                    CodeBlockKind::Indented => "",
                };
                let class = if lang.is_empty() {
                    String::new()
                } else {
                    format!(r#" class="language-{}""#, escape(lang))
                };
                events.push(raw(format!("<pre{}><code{class}>", attr())));
            }
            Event::End(TagEnd::CodeBlock) => events.push(raw("</code></pre>\n".into())),
            // Tables keep the writer's own events (it tracks column alignment),
            // so the line range goes on a wrapper.
            Event::Start(Tag::Table(_)) => {
                events.push(raw(format!(r#"<div class="table"{}>"#, attr())));
                events.push(event);
            }
            Event::End(TagEnd::Table) => {
                events.push(event);
                events.push(raw("</div>\n".into()));
            }
            // A plan is untrusted text: raw HTML in it is shown, never executed.
            Event::Html(s) | Event::InlineHtml(s) => events.push(Event::Text(s)),
            other => events.push(other),
        }
    }

    let mut out = String::with_capacity(md.len() * 2);
    html::push_html(&mut out, events.into_iter());
    out
}

/// First markdown heading in the plan, used as the page title.
pub fn title(md: &str) -> Option<String> {
    md.lines()
        .map(str::trim_start)
        .find(|l| l.starts_with('#'))
        .map(|l| l.trim_start_matches('#').trim().to_string())
        .filter(|t| !t.is_empty())
}

fn raw(s: String) -> Event<'static> {
    Event::Html(CowStr::from(s))
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

struct LineIndex {
    starts: Vec<usize>,
}

impl LineIndex {
    fn new(text: &str) -> Self {
        let mut starts = vec![0];
        starts.extend(text.match_indices('\n').map(|(i, _)| i + 1));
        Self { starts }
    }

    /// 1-based line holding byte `offset`.
    fn line(&self, offset: usize) -> usize {
        self.starts.partition_point(|&s| s <= offset)
    }

    /// Inclusive 1-based line span of the byte range `start..end`. The last
    /// byte decides the end line, so a range ending in `\n` stays on its line.
    fn span(&self, start: usize, end: usize) -> (usize, usize) {
        (self.line(start), self.line(end.saturating_sub(1).max(start)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attr(html: &str, tag: &str) -> Vec<String> {
        let open = format!("<{tag} ");
        html.match_indices(&open)
            .map(|(i, _)| {
                let rest = &html[i..];
                rest[..rest.find('>').unwrap()].to_string()
            })
            .collect()
    }

    #[test]
    fn headings_and_paragraphs_carry_their_lines() {
        let md = "# Title\n\nFirst para\nwraps here.\n\n## Step\n\nSecond.\n";
        let html = render(md);
        assert_eq!(attr(&html, "h1"), [r#"<h1 data-s="1" data-e="1""#]);
        assert_eq!(attr(&html, "h2"), [r#"<h2 data-s="6" data-e="6""#]);
        assert_eq!(
            attr(&html, "p"),
            [r#"<p data-s="3" data-e="4""#, r#"<p data-s="8" data-e="8""#]
        );
    }

    #[test]
    fn list_items_get_their_own_lines() {
        let md = "1. one\n2. two\n   - nested\n3. three\n";
        let html = render(md);
        assert_eq!(
            attr(&html, "li"),
            [
                r#"<li data-s="1" data-e="1""#,
                r#"<li data-s="2" data-e="3""#,
                r#"<li data-s="3" data-e="3""#,
                r#"<li data-s="4" data-e="4""#,
            ]
        );
    }

    #[test]
    fn code_blocks_and_tables() {
        let md = "Intro\n\n```rust\nfn a() {}\n```\n\n| a | b |\n|---|---|\n| 1 | 2 |\n";
        let html = render(md);
        assert!(html.contains(r#"<pre data-s="3" data-e="5"><code class="language-rust">fn a() {}"#));
        assert!(html.contains(r#"<div class="table" data-s="7" data-e="9"><table>"#));
    }

    #[test]
    fn raw_html_is_escaped_not_executed() {
        let html = render("Hi <script>alert(1)</script>\n\n<img src=x onerror=alert(1)>\n");
        assert!(!html.contains("<script>"));
        assert!(!html.contains("<img"));
        assert!(html.contains("&lt;script&gt;"));
    }

    #[test]
    fn task_lists_render() {
        let html = render("- [ ] todo\n- [x] done\n");
        assert!(html.contains(r#"type="checkbox""#));
    }

    #[test]
    fn title_is_first_heading() {
        assert_eq!(title("intro\n# Plan: ship it\n## x").as_deref(), Some("Plan: ship it"));
        assert_eq!(title("no heading"), None);
    }
}
