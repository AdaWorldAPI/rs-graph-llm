//! HTML rendering from document blocks.

use crate::{Block, Document, RenderError};

/// Render a document to standalone HTML.
pub fn render_html(doc: &Document) -> Result<String, RenderError> {
    let mut body = String::new();

    for block in &doc.blocks {
        match block {
            Block::Markdown(text) => {
                // Convert markdown to HTML using pulldown-cmark
                let parser = pulldown_cmark::Parser::new(text);
                let mut html = String::new();
                pulldown_cmark::html::push_html(&mut html, parser);
                body.push_str(&html);
            }
            Block::Code { language, source } => {
                body.push_str(&format!(
                    "<pre><code class=\"language-{language}\">{}</code></pre>\n",
                    html_escape(source)
                ));
            }
            Block::Output { mime_type, data } => {
                if mime_type == "text/html" {
                    body.push_str(data);
                } else if mime_type.starts_with("text/") {
                    body.push_str(&format!("<pre class=\"output\">{}</pre>\n", html_escape(data)));
                } else if mime_type.starts_with("image/") {
                    body.push_str(&format!(
                        "<img src=\"data:{mime_type};base64,{data}\" />\n"
                    ));
                }
            }
            Block::Heading { level, text } => {
                body.push_str(&format!("<h{level}>{}</h{level}>\n", html_escape(text)));
            }
            Block::GraphVisualization { graph_json } => {
                body.push_str(&format!(
                    r#"<div class="graph-viz" id="graph-{id}"></div>
<script>
  (function() {{
    var data = {graph_json};
    var container = document.getElementById('graph-{id}');
    var network = new vis.Network(container, data, {{}});
  }})();
</script>
"#,
                    id = uuid::Uuid::new_v4().to_string().replace('-', ""),
                ));
            }
            Block::RawHtml(html) => {
                body.push_str(html);
                body.push('\n');
            }
        }
    }

    let title = doc.title.as_deref().unwrap_or("Notebook");

    Ok(format!(
        r#"<!DOCTYPE html>
<html>
<head>
  <meta charset="utf-8">
  <title>{title}</title>
  <script src="https://unpkg.com/vis-network/standalone/umd/vis-network.min.js"></script>
  <style>
    body {{ font-family: system-ui, sans-serif; max-width: 800px; margin: 0 auto; padding: 2rem; }}
    pre {{ background: #f5f5f5; padding: 1rem; border-radius: 4px; overflow-x: auto; }}
    .output {{ background: #fff; border-left: 3px solid #4CAF50; }}
    .graph-viz {{ width: 100%; height: 400px; border: 1px solid #ddd; }}
  </style>
</head>
<body>
{body}
</body>
</html>"#
    ))
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
