//! Markdown rendering from document blocks.

use crate::{Block, Document};

/// Render a document to Markdown.
pub fn render_markdown(doc: &Document) -> String {
    let mut out = String::new();

    // YAML frontmatter
    if doc.title.is_some() || doc.author.is_some() {
        out.push_str("---\n");
        if let Some(title) = &doc.title {
            out.push_str(&format!("title: \"{title}\"\n"));
        }
        if let Some(author) = &doc.author {
            out.push_str(&format!("author: \"{author}\"\n"));
        }
        out.push_str("---\n\n");
    }

    for block in &doc.blocks {
        match block {
            Block::Markdown(text) => {
                out.push_str(text);
                out.push_str("\n\n");
            }
            Block::Code { language, source } => {
                out.push_str(&format!("```{language}\n{source}\n```\n\n"));
            }
            Block::Output { mime_type, data } => {
                if mime_type.starts_with("text/") {
                    out.push_str(&format!("```\n{data}\n```\n\n"));
                }
            }
            Block::Heading { level, text } => {
                let hashes = "#".repeat(*level as usize);
                out.push_str(&format!("{hashes} {text}\n\n"));
            }
            Block::GraphVisualization { graph_json: _ } => {
                out.push_str("*[Graph visualization]*\n\n");
            }
            Block::RawHtml(html) => {
                out.push_str(html);
                out.push_str("\n\n");
            }
        }
    }

    out
}
