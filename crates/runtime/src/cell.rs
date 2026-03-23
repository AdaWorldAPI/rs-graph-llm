//! Cell definition and management.
//!
//! A cell is a unit of code with:
//! - `defs`: variables it defines (writes)
//! - `refs`: variables it references (reads)
//! - `code`: source code string
//! - `language`: what language the code is in

use crate::{CellId, CellOutput, CellStatus};
use std::collections::HashSet;

/// The language of a cell's code.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CellLanguage {
    Python,
    Sql,
    Cypher,
    Gremlin,
    Sparql,
    R,
    Rust,
    Nars,
    Markdown,
}

/// Configuration for a cell.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct CellConfig {
    /// Whether the cell is disabled (won't execute).
    #[serde(default)]
    pub disabled: bool,
    /// Whether to hide the code in the UI.
    #[serde(default)]
    pub hide_code: bool,
}

/// A cell in the notebook.
#[derive(Debug, Clone)]
pub struct Cell {
    /// Unique identifier.
    pub id: CellId,
    /// Source code.
    pub code: String,
    /// Language of the code.
    pub language: CellLanguage,
    /// Variables this cell defines (writes).
    pub defs: HashSet<String>,
    /// Variables this cell references (reads).
    pub refs: HashSet<String>,
    /// Cell configuration.
    pub config: CellConfig,
    /// Current execution status.
    pub status: CellStatus,
    /// Last execution output.
    pub output: Option<CellOutput>,
    /// Console output (stdout/stderr).
    pub console: Vec<CellOutput>,
}

impl Cell {
    /// Create a new cell with the given code and language.
    pub fn new(code: impl Into<String>, language: CellLanguage) -> Self {
        Self {
            id: CellId::new_v4(),
            code: code.into(),
            language,
            defs: HashSet::new(),
            refs: HashSet::new(),
            config: CellConfig::default(),
            status: CellStatus::Idle,
            output: None,
            console: Vec::new(),
        }
    }

    /// Set the variables this cell defines.
    pub fn with_defs(mut self, defs: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.defs = defs.into_iter().map(Into::into).collect();
        self
    }

    /// Set the variables this cell references.
    pub fn with_refs(mut self, refs: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.refs = refs.into_iter().map(Into::into).collect();
        self
    }
}
