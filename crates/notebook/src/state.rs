//! Notebook state management.
//!
//! Wraps the reactive `Runtime` with cell ordering, human-readable ID
//! mapping, and save/load/export operations. This is the single source of
//! truth that all MCP tools operate on.
//!
//! Language detection: when `lang` is omitted or set to `"auto"`, the runtime's
//! `detect_language` module infers the language from the code's first tokens.

use notebook_publish::{Block, Document, OutputFormat};
use notebook_runtime::cell::{Cell, CellLanguage};
use notebook_runtime::detect::detect_language;
use notebook_runtime::executor::Runtime;
use notebook_runtime::{CellId, CellStatus};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Notebook state — owns the runtime and tracks cell ordering.
pub struct NotebookState {
    /// The reactive execution runtime.
    pub runtime: Runtime,
    /// Ordered list of short cell IDs (insertion order).
    cell_order: Vec<String>,
    /// Short ID → internal UUID.
    id_map: HashMap<String, CellId>,
    /// Internal UUID → short ID.
    reverse_map: HashMap<CellId, String>,
    /// Monotonic counter for generating short IDs.
    counter: u64,
}

// ---------------------------------------------------------------------------
// Public response types (serialized into MCP tool outputs)
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct ExecuteResult {
    pub cell_id: String,
    pub status: String,
    pub output: String,
    pub output_mime: Option<String>,
    pub timing_ms: u64,
    pub detected_lang: Option<String>,
    pub downstream_rerun: Vec<String>,
}

#[derive(Serialize)]
pub struct CellInfo {
    pub cell_id: String,
    pub code: String,
    pub lang: String,
    pub status: String,
    pub output: String,
    pub output_mime: Option<String>,
    pub refs: Vec<String>,
    pub defs: Vec<String>,
    pub last_run_ms: u64,
}

#[derive(Serialize)]
pub struct CellSummary {
    pub cell_id: String,
    pub lang: String,
    pub status: String,
    pub defs: Vec<String>,
    pub refs: Vec<String>,
}

#[derive(Serialize)]
pub struct CreateResult {
    pub cell_id: String,
    pub detected_lang: Option<String>,
}

#[derive(Serialize)]
pub struct UpdateResult {
    pub cell_id: String,
    pub status: String,
    pub downstream_rerun: Vec<String>,
}

#[derive(Serialize)]
pub struct DeleteResult {
    pub stale_cells: Vec<String>,
}

#[derive(Serialize)]
pub struct DagNode {
    pub cell_id: String,
    pub defs: Vec<String>,
    pub refs: Vec<String>,
    pub status: String,
}

#[derive(Serialize)]
pub struct DagEdge {
    pub from: String,
    pub to: String,
    pub via: String,
}

#[derive(Serialize)]
pub struct DagInfo {
    pub nodes: Vec<DagNode>,
    pub edges: Vec<DagEdge>,
}

#[derive(Serialize)]
pub struct SaveResult {
    pub saved: bool,
    pub cells: usize,
}

#[derive(Serialize)]
pub struct LoadResult {
    pub loaded: bool,
    pub cells: usize,
}

#[derive(Serialize)]
pub struct ExportResult {
    pub exported: bool,
    pub path: String,
}

#[derive(Serialize)]
pub struct DetectResult {
    pub detected: String,
    pub confidence: &'static str,
}

// ---------------------------------------------------------------------------
// Serialization format for save/load
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize)]
struct NotebookFile {
    cells: Vec<CellRecord>,
}

#[derive(Serialize, Deserialize)]
struct CellRecord {
    id: String,
    code: String,
    lang: String,
    defs: Vec<String>,
    refs: Vec<String>,
    output: Option<String>,
    output_mime: Option<String>,
}

// ---------------------------------------------------------------------------
// Implementation
// ---------------------------------------------------------------------------

impl NotebookState {
    pub fn new() -> Self {
        Self {
            runtime: Runtime::new(),
            cell_order: Vec::new(),
            id_map: HashMap::new(),
            reverse_map: HashMap::new(),
            counter: 0,
        }
    }

    /// Generate the next short cell ID.
    fn next_id(&mut self) -> String {
        self.counter += 1;
        format!("c_{:03}", self.counter)
    }

    /// Resolve a short ID to an internal UUID.
    fn resolve(&self, short_id: &str) -> Option<CellId> {
        self.id_map.get(short_id).copied()
    }

    /// Map a UUID back to its short ID.
    fn short_id(&self, uuid: CellId) -> String {
        self.reverse_map
            .get(&uuid)
            .cloned()
            .unwrap_or_else(|| uuid.to_string())
    }

    fn status_str(s: &CellStatus) -> &'static str {
        match s {
            CellStatus::Idle => "ok",
            CellStatus::Queued => "pending",
            CellStatus::Running => "running",
            CellStatus::Stale => "stale",
            CellStatus::Disabled => "disabled",
        }
    }

    fn cell_output_text(cell: &Cell) -> String {
        cell.output
            .as_ref()
            .map(|o| o.data.clone())
            .unwrap_or_default()
    }

    fn cell_output_mime(cell: &Cell) -> Option<String> {
        cell.output.as_ref().map(|o| o.mime_type.clone())
    }

    fn lang_str(lang: &CellLanguage) -> &'static str {
        match lang {
            CellLanguage::Python => "python",
            CellLanguage::Sql => "sql",
            CellLanguage::Cypher => "cypher",
            CellLanguage::Gremlin => "gremlin",
            CellLanguage::Sparql => "sparql",
            CellLanguage::R => "r",
            CellLanguage::Rust => "rust",
            CellLanguage::Nars => "nars",
            CellLanguage::Markdown => "markdown",
        }
    }

    /// Resolve language from string, with auto-detection support.
    /// If `lang` is `"auto"`, empty, or `None`, detect from code.
    fn resolve_language(lang: Option<&str>, code: &str) -> Result<(CellLanguage, Option<String>), String> {
        match lang {
            Some(l) if !l.is_empty() && l != "auto" => {
                let parsed = parse_language(l)?;
                Ok((parsed, None))
            }
            _ => {
                // Auto-detect from code.
                if let Some(detected) = detect_language(code) {
                    let name = match &detected {
                        CellLanguage::Python => "python",
                        CellLanguage::Sql => "sql",
                        CellLanguage::Cypher => "cypher",
                        CellLanguage::Gremlin => "gremlin",
                        CellLanguage::Sparql => "sparql",
                        CellLanguage::R => "r",
                        CellLanguage::Rust => "rust",
                        CellLanguage::Nars => "nars",
                        CellLanguage::Markdown => "markdown",
                    };
                    Ok((detected, Some(name.to_string())))
                } else {
                    // Default to Python if detection fails.
                    Ok((CellLanguage::Python, Some("python".to_string())))
                }
            }
        }
    }

    // -- Language detection ---------------------------------------------------

    /// Detect the language of a code string.
    pub fn detect_language(code: &str) -> DetectResult {
        if let Some(lang) = detect_language(code) {
            DetectResult {
                detected: Self::lang_str(&lang).to_string(),
                confidence: "high",
            }
        } else {
            DetectResult {
                detected: "python".to_string(),
                confidence: "low",
            }
        }
    }

    // -- Cell CRUD -----------------------------------------------------------

    /// Create a new cell (does not execute).
    /// If `lang` is `"auto"` or `None`, auto-detect from code.
    pub fn create_cell(
        &mut self,
        code: &str,
        lang: Option<&str>,
        after: Option<&str>,
    ) -> Result<CreateResult, String> {
        let (language, detected) = Self::resolve_language(lang, code)?;
        let short = self.next_id();
        let cell = Cell::new(code, language);
        let uuid = cell.id;

        self.runtime.graph.register(cell);
        self.id_map.insert(short.clone(), uuid);
        self.reverse_map.insert(uuid, short.clone());

        // Insert at the right position in the order.
        if let Some(after_id) = after {
            if let Some(pos) = self.cell_order.iter().position(|id| id == after_id) {
                self.cell_order.insert(pos + 1, short.clone());
            } else {
                self.cell_order.push(short.clone());
            }
        } else {
            self.cell_order.push(short.clone());
        }

        Ok(CreateResult {
            cell_id: short,
            detected_lang: detected,
        })
    }

    /// Get a cell's full state.
    pub fn get_cell(&self, short_id: &str) -> Result<CellInfo, String> {
        let uuid = self.resolve(short_id).ok_or("Cell not found")?;
        let cell = self
            .runtime
            .graph
            .get(uuid)
            .ok_or("Cell not found in graph")?;

        Ok(CellInfo {
            cell_id: short_id.to_string(),
            code: cell.code.clone(),
            lang: Self::lang_str(&cell.language).to_string(),
            status: Self::status_str(&cell.status).to_string(),
            output: Self::cell_output_text(cell),
            output_mime: Self::cell_output_mime(cell),
            refs: cell.refs.iter().cloned().collect(),
            defs: cell.defs.iter().cloned().collect(),
            last_run_ms: 0,
        })
    }

    /// List all cells in order.
    pub fn list_cells(&self) -> Vec<CellSummary> {
        self.cell_order
            .iter()
            .filter_map(|short| {
                let uuid = self.resolve(short)?;
                let cell = self.runtime.graph.get(uuid)?;
                Some(CellSummary {
                    cell_id: short.clone(),
                    lang: Self::lang_str(&cell.language).to_string(),
                    status: Self::status_str(&cell.status).to_string(),
                    defs: cell.defs.iter().cloned().collect(),
                    refs: cell.refs.iter().cloned().collect(),
                })
            })
            .collect()
    }

    /// Update a cell's code and trigger reactive re-execution.
    pub async fn update_cell(
        &mut self,
        short_id: &str,
        code: &str,
    ) -> Result<UpdateResult, String> {
        let uuid = self.resolve(short_id).ok_or("Cell not found")?;

        // Update the code in the graph.
        let language = {
            let cell = self
                .runtime
                .graph
                .get_mut(uuid)
                .ok_or("Cell not found in graph")?;
            cell.code = code.to_string();
            cell.status = CellStatus::Stale;
            cell.language.clone()
        };

        // Re-register to rebuild edges (defs/refs may have changed).
        let cell = self.runtime.graph.get(uuid).unwrap().clone();
        self.runtime.graph.register(Cell {
            id: uuid,
            code: code.to_string(),
            language,
            defs: cell.defs,
            refs: cell.refs,
            config: cell.config,
            status: CellStatus::Idle,
            output: cell.output,
            console: cell.console,
        });

        // Execute the cell and its downstream dependents.
        let downstream = self.execute_and_collect(uuid).await;

        Ok(UpdateResult {
            cell_id: short_id.to_string(),
            status: self
                .runtime
                .graph
                .get(uuid)
                .map(|c| Self::status_str(&c.status))
                .unwrap_or("ok")
                .to_string(),
            downstream_rerun: downstream,
        })
    }

    /// Delete a cell and mark downstream cells as stale.
    pub fn delete_cell(&mut self, short_id: &str) -> Result<DeleteResult, String> {
        let uuid = self.resolve(short_id).ok_or("Cell not found")?;

        // Find downstream cells before removing.
        let descendants = self.runtime.graph.descendants(&[uuid]);
        let stale: Vec<String> = descendants
            .iter()
            .filter(|&&id| id != uuid)
            .filter_map(|&id| {
                // Mark as stale.
                if let Some(cell) = self.runtime.graph.get_mut(id) {
                    cell.status = CellStatus::Stale;
                }
                Some(self.short_id(id))
            })
            .collect();

        // Remove the cell.
        self.runtime.graph.unregister(uuid);
        self.id_map.remove(short_id);
        self.reverse_map.remove(&uuid);
        self.cell_order.retain(|id| id != short_id);

        Ok(DeleteResult { stale_cells: stale })
    }

    // -- Execution -----------------------------------------------------------

    /// Execute a cell (create if needed).
    /// If `lang` is `"auto"`, empty, or `None`, auto-detect from code.
    pub async fn execute_cell(
        &mut self,
        code: &str,
        lang: Option<&str>,
        cell_id: Option<&str>,
    ) -> Result<ExecuteResult, String> {
        let start = std::time::Instant::now();

        let (resolved_lang, detected) = Self::resolve_language(lang, code)?;
        let lang_str = Self::lang_str(&resolved_lang);

        let short_id = if let Some(id) = cell_id {
            // Ensure the cell exists; update code if it does.
            if self.resolve(id).is_some() {
                let cell_uuid = self.resolve(id).unwrap();
                if let Some(cell) = self.runtime.graph.get_mut(cell_uuid) {
                    cell.code = code.to_string();
                }
                id.to_string()
            } else {
                // Create with the given ID.
                let cell = Cell::new(code, resolved_lang.clone());
                let uuid = cell.id;
                self.runtime.graph.register(cell);
                self.id_map.insert(id.to_string(), uuid);
                self.reverse_map.insert(uuid, id.to_string());
                self.cell_order.push(id.to_string());
                id.to_string()
            }
        } else {
            // Auto-generate.
            let result = self.create_cell(code, Some(lang_str), None)?;
            result.cell_id
        };

        let uuid = self.resolve(&short_id).unwrap();
        let downstream = self.execute_and_collect(uuid).await;
        let elapsed = start.elapsed().as_millis() as u64;

        let (output, output_mime) = self
            .runtime
            .graph
            .get(uuid)
            .map(|c| (Self::cell_output_text(c), Self::cell_output_mime(c)))
            .unwrap_or_default();

        let status = self
            .runtime
            .graph
            .get(uuid)
            .map(|c| Self::status_str(&c.status))
            .unwrap_or("error")
            .to_string();

        Ok(ExecuteResult {
            cell_id: short_id,
            status,
            output,
            output_mime,
            timing_ms: elapsed,
            detected_lang: detected,
            downstream_rerun: downstream,
        })
    }

    /// Run runtime.execute_cell and return the list of downstream cell short IDs.
    async fn execute_and_collect(&mut self, uuid: CellId) -> Vec<String> {
        match self.runtime.execute_cell(uuid).await {
            Ok(notifications) => {
                // Collect IDs of cells that were re-run (excluding the trigger cell).
                notifications
                    .iter()
                    .filter_map(|n| match n {
                        notebook_runtime::Notification::CellOp {
                            cell_id, status, ..
                        } if *cell_id != uuid && *status == CellStatus::Idle => {
                            Some(self.short_id(*cell_id))
                        }
                        _ => None,
                    })
                    .collect()
            }
            Err(e) => {
                // Mark the cell with error output.
                if let Some(cell) = self.runtime.graph.get_mut(uuid) {
                    cell.output = Some(notebook_runtime::CellOutput {
                        mime_type: "text/plain".to_string(),
                        data: format!("Error: {e}"),
                    });
                    cell.status = CellStatus::Idle;
                }
                vec![]
            }
        }
    }

    // -- DAG -----------------------------------------------------------------

    pub fn dag(&self) -> DagInfo {
        let cell_ids = self.runtime.graph.cell_ids();
        let mut nodes = Vec::new();
        let mut edges = Vec::new();

        for &uuid in &cell_ids {
            if let Some(cell) = self.runtime.graph.get(uuid) {
                let short = self.short_id(uuid);
                nodes.push(DagNode {
                    cell_id: short.clone(),
                    defs: cell.defs.iter().cloned().collect(),
                    refs: cell.refs.iter().cloned().collect(),
                    status: Self::status_str(&cell.status).to_string(),
                });

                // Build edges: for each ref, find who defines it.
                for ref_var in &cell.refs {
                    if let Some(parent_uuid) = self.runtime.graph.who_defines(ref_var) {
                        edges.push(DagEdge {
                            from: self.short_id(parent_uuid),
                            to: short.clone(),
                            via: ref_var.clone(),
                        });
                    }
                }
            }
        }

        DagInfo { nodes, edges }
    }

    // -- Save / Load ---------------------------------------------------------

    pub fn save(&self, path: &str) -> Result<SaveResult, String> {
        let records: Vec<CellRecord> = self
            .cell_order
            .iter()
            .filter_map(|short| {
                let uuid = self.resolve(short)?;
                let cell = self.runtime.graph.get(uuid)?;
                Some(CellRecord {
                    id: short.clone(),
                    code: cell.code.clone(),
                    lang: Self::lang_str(&cell.language).to_string(),
                    defs: cell.defs.iter().cloned().collect(),
                    refs: cell.refs.iter().cloned().collect(),
                    output: cell.output.as_ref().map(|o| o.data.clone()),
                    output_mime: cell.output.as_ref().map(|o| o.mime_type.clone()),
                })
            })
            .collect();

        let count = records.len();
        let file = NotebookFile { cells: records };
        let json = serde_json::to_string_pretty(&file).map_err(|e| e.to_string())?;
        std::fs::write(path, json).map_err(|e| e.to_string())?;

        Ok(SaveResult {
            saved: true,
            cells: count,
        })
    }

    pub fn load(&mut self, path: &str) -> Result<LoadResult, String> {
        let json = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let file: NotebookFile = serde_json::from_str(&json).map_err(|e| e.to_string())?;

        // Clear current state.
        let old_ids: Vec<CellId> = self.id_map.values().copied().collect();
        for uuid in old_ids {
            self.runtime.graph.unregister(uuid);
        }
        self.id_map.clear();
        self.reverse_map.clear();
        self.cell_order.clear();
        self.counter = 0;

        for record in &file.cells {
            let language = parse_language(&record.lang)?;
            let mut cell = Cell::new(&record.code, language)
                .with_defs(record.defs.iter().cloned())
                .with_refs(record.refs.iter().cloned());

            if let (Some(data), Some(mime)) = (&record.output, &record.output_mime) {
                cell.output = Some(notebook_runtime::CellOutput {
                    mime_type: mime.clone(),
                    data: data.clone(),
                });
            }

            let uuid = cell.id;
            self.runtime.graph.register(cell);
            self.id_map.insert(record.id.clone(), uuid);
            self.reverse_map.insert(uuid, record.id.clone());
            self.cell_order.push(record.id.clone());

            // Track the counter so new IDs don't collide.
            if let Some(num) = record.id.strip_prefix("c_").and_then(|s| s.parse::<u64>().ok()) {
                if num >= self.counter {
                    self.counter = num;
                }
            }
        }

        Ok(LoadResult {
            loaded: true,
            cells: file.cells.len(),
        })
    }

    // -- Export ---------------------------------------------------------------

    pub fn export(&self, format: &str, path: &str) -> Result<ExportResult, String> {
        let output_format = match format {
            "html" => OutputFormat::Html,
            "pdf" => OutputFormat::Pdf,
            "markdown" | "md" => OutputFormat::Markdown,
            _ => return Err(format!("Unsupported format: {format}")),
        };

        let mut blocks = Vec::new();

        for short in &self.cell_order {
            let uuid = match self.resolve(short) {
                Some(u) => u,
                None => continue,
            };
            let cell = match self.runtime.graph.get(uuid) {
                Some(c) => c,
                None => continue,
            };

            // Add the code block.
            let lang_str = Self::lang_str(&cell.language);
            if cell.language == CellLanguage::Markdown {
                blocks.push(Block::Markdown(cell.code.clone()));
            } else {
                blocks.push(Block::Code {
                    language: lang_str.to_string(),
                    source: cell.code.clone(),
                });
            }

            // Add output if present.
            if let Some(output) = &cell.output {
                // If output is JSON with graph data, render as graph visualization.
                if output.mime_type == "application/json" {
                    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&output.data) {
                        if parsed.get("nodes").is_some() && parsed.get("edges").is_some() {
                            blocks.push(Block::GraphVisualization {
                                graph_json: output.data.clone(),
                            });
                            continue;
                        }
                    }
                }
                blocks.push(Block::Output {
                    mime_type: output.mime_type.clone(),
                    data: output.data.clone(),
                });
            }
        }

        let doc = Document {
            title: None,
            author: None,
            format: output_format,
            blocks,
        };

        let rendered = notebook_publish::render(&doc).map_err(|e| e.to_string())?;
        std::fs::write(path, &rendered).map_err(|e| e.to_string())?;

        Ok(ExportResult {
            exported: true,
            path: path.to_string(),
        })
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

pub fn parse_language(lang: &str) -> Result<CellLanguage, String> {
    match lang.to_lowercase().as_str() {
        "python" => Ok(CellLanguage::Python),
        "sql" => Ok(CellLanguage::Sql),
        "cypher" => Ok(CellLanguage::Cypher),
        "gremlin" => Ok(CellLanguage::Gremlin),
        "sparql" => Ok(CellLanguage::Sparql),
        "r" => Ok(CellLanguage::R),
        "rust" => Ok(CellLanguage::Rust),
        "nars" | "narsese" => Ok(CellLanguage::Nars),
        "markdown" | "md" => Ok(CellLanguage::Markdown),
        other => Err(format!("Unsupported language: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_and_list() {
        let mut state = NotebookState::new();
        let r1 = state.create_cell("x = 1", Some("python"), None).unwrap();
        let r2 = state.create_cell("y = x", Some("python"), Some(&r1.cell_id)).unwrap();
        assert_eq!(r1.cell_id, "c_001");
        assert_eq!(r2.cell_id, "c_002");

        let cells = state.list_cells();
        assert_eq!(cells.len(), 2);
        assert_eq!(cells[0].cell_id, "c_001");
        assert_eq!(cells[1].cell_id, "c_002");
    }

    #[test]
    fn delete_cell_removes_from_order() {
        let mut state = NotebookState::new();
        state.create_cell("a", Some("rust"), None).unwrap();
        let r2 = state.create_cell("b", Some("rust"), None).unwrap();
        state.create_cell("c", Some("rust"), None).unwrap();

        state.delete_cell(&r2.cell_id).unwrap();
        let cells = state.list_cells();
        assert_eq!(cells.len(), 2);
        assert_eq!(cells[0].cell_id, "c_001");
        assert_eq!(cells[1].cell_id, "c_003");
    }

    #[test]
    fn dag_empty() {
        let state = NotebookState::new();
        let dag = state.dag();
        assert!(dag.nodes.is_empty());
        assert!(dag.edges.is_empty());
    }

    #[test]
    fn auto_detect_gremlin() {
        let mut state = NotebookState::new();
        let r = state.create_cell("g.V().hasLabel('person')", None, None).unwrap();
        assert_eq!(r.detected_lang, Some("gremlin".to_string()));
    }

    #[test]
    fn auto_detect_cypher() {
        let mut state = NotebookState::new();
        let r = state.create_cell("MATCH (n:Person) RETURN n", None, None).unwrap();
        assert_eq!(r.detected_lang, Some("cypher".to_string()));
    }

    #[test]
    fn auto_detect_sparql() {
        let mut state = NotebookState::new();
        let r = state.create_cell("SELECT ?s ?p ?o WHERE { ?s ?p ?o }", None, None).unwrap();
        assert_eq!(r.detected_lang, Some("sparql".to_string()));
    }

    #[test]
    fn auto_detect_r() {
        let mut state = NotebookState::new();
        let r = state.create_cell("paths %>% filter(weight > 0.8)", None, None).unwrap();
        assert_eq!(r.detected_lang, Some("r".to_string()));
    }

    #[test]
    fn auto_detect_markdown() {
        let mut state = NotebookState::new();
        let r = state.create_cell("# Hello World", None, None).unwrap();
        assert_eq!(r.detected_lang, Some("markdown".to_string()));
    }

    #[test]
    fn explicit_lang_overrides_detect() {
        let mut state = NotebookState::new();
        let r = state.create_cell("# Hello World", Some("python"), None).unwrap();
        assert_eq!(r.detected_lang, None); // No detection when explicitly set
    }
}
