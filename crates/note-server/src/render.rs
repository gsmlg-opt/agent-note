use std::sync::OnceLock;

use pulldown_cmark::{CodeBlockKind, CowStr, Event, Options, Parser, Tag, TagEnd};
use syntect::highlighting::ThemeSet;
use syntect::parsing::SyntaxSet;

/// Render Markdown to HTML: syntect-highlighted code blocks, mermaid-rendered diagrams, and
/// markdown raw HTML preserved for imported documents that use anchor tags.
pub fn render_markdown_html(md: &str) -> String {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);

    let mut events: Vec<Event> = Vec::new();
    let mut in_code = false;
    let mut code_lang = String::new();
    let mut code_buf = String::new();

    for ev in Parser::new_ext(md, options) {
        match ev {
            Event::Start(Tag::CodeBlock(kind)) => {
                in_code = true;
                code_buf.clear();
                code_lang = match kind {
                    CodeBlockKind::Fenced(info) => {
                        info.split_whitespace().next().unwrap_or("").to_string()
                    }
                    CodeBlockKind::Indented => String::new(),
                };
            }
            Event::End(TagEnd::CodeBlock) => {
                in_code = false;
                let html = if code_lang.eq_ignore_ascii_case("mermaid") {
                    render_mermaid(&code_buf)
                } else {
                    highlight_code(&code_buf, &code_lang)
                };
                events.push(Event::Html(CowStr::from(html)));
            }
            Event::Text(t) if in_code => code_buf.push_str(&t),
            other => events.push(other),
        }
    }

    let mut out = String::new();
    pulldown_cmark::html::push_html(&mut out, events.into_iter());
    out
}

pub fn render_embedded_markdown_document(content_html: &str) -> String {
    format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="color-scheme" content="light">
<title>Note content</title>
<style>
:root {{
  --color-primary: #0065ff;
  --color-surface: #ffffff;
  --color-surface-container: #f5f6fa;
  --color-on-surface: #2f2e3f;
  --color-on-surface-variant: #5a607f;
  --color-outline: #d7dbec;
}}
* {{ box-sizing: border-box; }}
html {{ background: var(--color-surface); }}
body {{
  margin: 0;
  background: var(--color-surface);
  color: var(--color-on-surface);
  font-family: "PingFang SC", -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Arial, sans-serif;
  font-size: 16px;
  line-height: 1.6;
}}
.markdown-body {{
  width: min(100%, 960px);
  margin: 0 auto;
  padding: 24px;
  overflow-wrap: anywhere;
}}
.markdown-body > :first-child {{ margin-top: 0; }}
.markdown-body > :last-child {{ margin-bottom: 0; }}
.markdown-body h1,
.markdown-body h2,
.markdown-body h3,
.markdown-body h4,
.markdown-body h5,
.markdown-body h6 {{
  margin: 1.5rem 0 1rem;
  color: #191919;
  font-weight: 600;
  line-height: 1.25;
}}
.markdown-body h1 {{ font-size: 2rem; }}
.markdown-body h2 {{ font-size: 1.5rem; }}
.markdown-body h3 {{ font-size: 1.25rem; }}
.markdown-body h1,
.markdown-body h2 {{ padding-bottom: 0.3em; border-bottom: 1px solid var(--color-outline); }}
.markdown-body p,
.markdown-body ul,
.markdown-body ol,
.markdown-body pre,
.markdown-body table,
.markdown-body blockquote {{ margin: 0 0 1rem; }}
.markdown-body ul,
.markdown-body ol {{ padding-left: 2rem; }}
.markdown-body li + li {{ margin-top: 0.25rem; }}
.markdown-body a {{ color: var(--color-primary); text-decoration: none; }}
.markdown-body a:hover {{ text-decoration: underline; }}
.markdown-body a:focus-visible {{ outline: 2px solid var(--color-primary); outline-offset: 2px; }}
.markdown-body code,
.markdown-body pre {{
  font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
  font-size: 0.875rem;
}}
.markdown-body :not(pre) > code {{
  padding: 0.15em 0.35em;
  border-radius: 4px;
  background: var(--color-surface-container);
}}
.markdown-body pre {{
  max-width: 100%;
  padding: 1rem;
  overflow-x: auto;
  border: 1px solid var(--color-outline);
  border-radius: 6px;
  background: var(--color-surface-container);
  line-height: 1.45;
}}
.markdown-body pre code {{ padding: 0; background: transparent; }}
.markdown-body .dm-code-block {{
  margin: 0 0 1rem;
  overflow: hidden;
  border: 1px solid var(--color-outline);
  border-radius: 6px;
  background: #101720;
}}
.markdown-body .dm-code-block-header {{
  min-height: 2rem;
  padding: 0.4rem 0.75rem;
  border-bottom: 1px solid #2d3748;
  background: #17202c;
  color: #d7dee9;
  font-size: 0.75rem;
  font-weight: 600;
  text-transform: uppercase;
}}
.markdown-body .dm-code-block pre {{
  margin: 0;
  border: 0;
  border-radius: 0;
  background: transparent;
  color: #d7dee9;
}}
.markdown-body .dm-token-keyword {{ color: #ffb454; font-weight: 600; }}
.markdown-body .dm-token-function {{ color: #82aaff; }}
.markdown-body .dm-token-type {{ color: #c792ea; }}
.markdown-body .dm-token-builtin {{ color: #7dd3fc; }}
.markdown-body .dm-token-string {{ color: #a5d977; }}
.markdown-body .dm-token-number {{ color: #f78c6c; }}
.markdown-body .dm-token-literal {{ color: #ff8fa3; }}
.markdown-body .dm-token-comment {{ color: #7a869a; font-style: italic; }}
.markdown-body blockquote {{
  margin-left: 0;
  padding-left: 1rem;
  border-left: 4px solid var(--color-outline);
  color: var(--color-on-surface-variant);
}}
.markdown-body table {{
  display: block;
  width: 100%;
  overflow-x: auto;
  border-collapse: collapse;
}}
.markdown-body th,
.markdown-body td {{
  padding: 0.5rem 0.75rem;
  border: 1px solid var(--color-outline);
  text-align: left;
  vertical-align: top;
}}
.markdown-body th,
.markdown-body tr:nth-child(even) {{ background: var(--color-surface-container); }}
.markdown-body img,
.markdown-body svg {{ max-width: 100%; height: auto; }}
.markdown-body hr {{
  height: 1px;
  margin: 1.5rem 0;
  border: 0;
  background: var(--color-outline);
}}
.markdown-body .mermaid-diagram {{ max-width: 100%; overflow-x: auto; }}
.markdown-body .mermaid-error {{ color: #b42318; }}
@media (max-width: 640px) {{
  .markdown-body {{ padding: 16px; }}
}}
</style>
</head>
<body>
<main class="markdown-body">{content_html}</main>
</body>
</html>
"#
    )
}

fn highlight_code(code: &str, lang: &str) -> String {
    static SYNTAXES: OnceLock<SyntaxSet> = OnceLock::new();
    static THEMES: OnceLock<ThemeSet> = OnceLock::new();
    let syntaxes = SYNTAXES.get_or_init(SyntaxSet::load_defaults_newlines);
    let themes = THEMES.get_or_init(ThemeSet::load_defaults);
    let theme = &themes.themes["InspiredGitHub"];
    let syntax = if lang.is_empty() {
        syntaxes.find_syntax_plain_text()
    } else {
        syntaxes
            .find_syntax_by_token(lang)
            .unwrap_or_else(|| syntaxes.find_syntax_plain_text())
    };
    syntect::html::highlighted_html_for_string(code, syntaxes, syntax, theme)
        .unwrap_or_else(|_| format!("<pre><code>{}</code></pre>", escape_html(code)))
}

fn render_mermaid(code: &str) -> String {
    match mermaid_rs_renderer::render(code) {
        Ok(svg) => format!("<div class=\"mermaid-diagram\">{svg}</div>"),
        Err(e) => format!(
            "<pre class=\"mermaid-error\"><code>{}</code></pre>",
            escape_html(&format!("mermaid render error: {e}\n\n{code}"))
        ),
    }
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::{render_embedded_markdown_document, render_markdown_html};

    #[test]
    fn preserves_raw_html_anchors() {
        let html = render_markdown_html("<a id=\"abstract\"></a>\n\n## Abstract");

        assert!(html.contains("<a id=\"abstract\"></a>"));
        assert!(!html.contains("&lt;a id=&quot;abstract&quot;&gt;"));
    }

    #[test]
    fn embedded_document_contains_inline_markdown_styles() {
        let document = render_embedded_markdown_document("<h1>Embedded</h1>");

        assert!(document.starts_with("<!doctype html>"));
        assert!(document.contains("<style>"));
        assert!(document.contains(".markdown-body table"));
        assert!(document.contains("<main class=\"markdown-body\"><h1>Embedded</h1></main>"));
    }
}
