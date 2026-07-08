use std::sync::OnceLock;

use pulldown_cmark::{CodeBlockKind, CowStr, Event, Options, Parser, Tag, TagEnd};
use syntect::highlighting::ThemeSet;
use syntect::parsing::SyntaxSet;

/// Render Markdown to sanitized HTML: syntect-highlighted code blocks, mermaid-rendered diagrams,
/// and raw HTML dropped so note content cannot inject arbitrary markup/scripts.
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
            Event::Html(_) | Event::InlineHtml(_) => {}
            other => events.push(other),
        }
    }

    let mut out = String::new();
    pulldown_cmark::html::push_html(&mut out, events.into_iter());
    out
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
