//! Pandoc AST types in Rust.
//!
//! Subset of Pandoc's JSON AST format, sufficient for notebook rendering.
//! Reference: https://hackage.haskell.org/package/pandoc-types

use serde::{Deserialize, Serialize};

/// A Pandoc document.
#[derive(Debug, Serialize, Deserialize)]
pub struct Pandoc {
    #[serde(rename = "pandoc-api-version")]
    pub api_version: Vec<i32>,
    pub meta: serde_json::Value,
    pub blocks: Vec<PandocBlock>,
}

/// A block element in the Pandoc AST.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "t", content = "c")]
pub enum PandocBlock {
    /// Plain text (no paragraph wrapping).
    Plain(Vec<PandocInline>),
    /// Paragraph.
    Para(Vec<PandocInline>),
    /// Code block: (Attr, String).
    CodeBlock(Attr, String),
    /// Header: (Int, Attr, [Inline]).
    Header(i32, Attr, Vec<PandocInline>),
    /// Horizontal rule.
    HorizontalRule,
    /// Raw block: (Format, String).
    RawBlock(String, String),
    /// Div: (Attr, [Block]).
    Div(Attr, Vec<PandocBlock>),
}

/// An inline element in the Pandoc AST.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "t", content = "c")]
pub enum PandocInline {
    /// Plain text.
    Str(String),
    /// Emphasized text.
    Emph(Vec<PandocInline>),
    /// Strong text.
    Strong(Vec<PandocInline>),
    /// Inline code.
    Code(Attr, String),
    /// Whitespace.
    Space,
    /// Soft line break.
    SoftBreak,
    /// Hard line break.
    LineBreak,
    /// Raw inline: (Format, String).
    RawInline(String, String),
    /// Link: (Attr, [Inline], Target).
    Link(Attr, Vec<PandocInline>, Target),
}

/// Attributes: (id, classes, key-value pairs).
pub type Attr = (String, Vec<String>, Vec<(String, String)>);

/// Link target: (url, title).
pub type Target = (String, String);
