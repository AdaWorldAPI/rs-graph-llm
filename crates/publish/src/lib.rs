//! # notebook-publish
//!
//! Document publisher: notebook → PDF/HTML (quarto transcode).
//!
//! Pipeline:
//! 1. Parse notebook cells into document blocks
//! 2. Convert to Pandoc-compatible AST
//! 3. Render to output format (HTML, PDF via Pandoc subprocess)
//! 4. Custom extensions for graph visualization embedding

pub mod pandoc_ast;
pub mod markdown;
pub mod html;

/// A document ready for rendering.
#[derive(Debug)]
pub struct Document {
    /// Title from YAML frontmatter.
    pub title: Option<String>,
    /// Author from YAML frontmatter.
    pub author: Option<String>,
    /// Output format.
    pub format: OutputFormat,
    /// Document blocks (cells converted to document elements).
    pub blocks: Vec<Block>,
}

/// Output format for rendering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputFormat {
    Html,
    Pdf,
    Markdown,
}

/// A block in the document (paragraph, code, heading, etc.).
#[derive(Debug, Clone)]
pub enum Block {
    /// Markdown text block.
    Markdown(String),
    /// Code block with language and source.
    Code { language: String, source: String },
    /// Code output (rendered result).
    Output { mime_type: String, data: String },
    /// Heading with level (1-6) and text.
    Heading { level: u8, text: String },
    /// Graph visualization (vis.js data serialized as JSON).
    GraphVisualization { graph_json: String },
    /// Raw HTML block.
    RawHtml(String),
}

/// Render a document to the specified format.
pub fn render(doc: &Document) -> Result<String, RenderError> {
    match doc.format {
        OutputFormat::Html => html::render_html(doc),
        OutputFormat::Pdf => Err(RenderError::Unsupported(
            "PDF rendering requires Pandoc. Use render_with_pandoc().".into(),
        )),
        OutputFormat::Markdown => Ok(markdown::render_markdown(doc)),
    }
}

/// Render error.
#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    #[error("Render error: {0}")]
    Render(String),
    #[error("Unsupported: {0}")]
    Unsupported(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}
