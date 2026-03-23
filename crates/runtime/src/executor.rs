//! Cell execution engine.
//!
//! Executes cells by dispatching to the appropriate backend:
//! - Python/SQL: in-process (future: embedded Python via PyO3)
//! - Cypher/Gremlin/SPARQL/NARS: delegate to notebook-query crate
//! - R: delegate to notebook-kernel crate (IRkernel via Jupyter protocol)
//! - Rust: compile and run via evcxr or similar
//! - Markdown: render to HTML (no execution)

use crate::cell::CellLanguage;
use crate::dataflow::DataflowGraph;
use crate::{CellId, CellOutput, CellStatus, Notification};
use std::collections::HashMap;

/// Trait for cell executors. Each language has its own executor.
#[async_trait::async_trait]
pub trait CellExecutor: Send + Sync {
    /// Execute a cell and return its output.
    async fn execute(
        &self,
        code: &str,
        variables: &HashMap<String, serde_json::Value>,
    ) -> Result<ExecutionResult, ExecutionError>;
}

/// The result of executing a cell.
#[derive(Debug, Clone)]
pub struct ExecutionResult {
    /// The primary output (last expression value).
    pub output: Option<CellOutput>,
    /// Console output (stdout/stderr lines).
    pub console: Vec<CellOutput>,
    /// Variables defined by this execution.
    pub defs: HashMap<String, serde_json::Value>,
}

/// Error during cell execution.
#[derive(Debug, thiserror::Error)]
pub enum ExecutionError {
    #[error("Execution failed: {message}")]
    Failed {
        message: String,
        traceback: Vec<String>,
    },
    #[error("Cell was interrupted")]
    Interrupted,
    #[error("No executor for language: {language:?}")]
    UnsupportedLanguage { language: CellLanguage },
}

/// A built-in executor for Markdown cells (renders to HTML, no execution).
pub struct MarkdownExecutor;

#[async_trait::async_trait]
impl CellExecutor for MarkdownExecutor {
    async fn execute(
        &self,
        code: &str,
        _variables: &HashMap<String, serde_json::Value>,
    ) -> Result<ExecutionResult, ExecutionError> {
        // Markdown cells just pass through — the publisher renders them.
        Ok(ExecutionResult {
            output: Some(CellOutput {
                mime_type: "text/markdown".to_string(),
                data: code.to_string(),
            }),
            console: vec![],
            defs: HashMap::new(),
        })
    }
}

/// The runtime that manages cell execution and reactive updates.
pub struct Runtime {
    /// The dependency graph.
    pub graph: DataflowGraph,
    /// Registered executors per language.
    executors: HashMap<CellLanguage, Box<dyn CellExecutor>>,
    /// Current variable values (shared namespace).
    variables: HashMap<String, serde_json::Value>,
    /// Notification sender.
    notifications: Vec<Notification>,
}

impl Runtime {
    pub fn new() -> Self {
        let mut rt = Self {
            graph: DataflowGraph::new(),
            executors: HashMap::new(),
            variables: HashMap::new(),
            notifications: Vec::new(),
        };
        // Register the built-in Markdown executor.
        rt.register_executor(CellLanguage::Markdown, Box::new(MarkdownExecutor));
        rt
    }

    /// Register an executor for a language.
    pub fn register_executor(&mut self, language: CellLanguage, executor: Box<dyn CellExecutor>) {
        self.executors.insert(language, executor);
    }

    /// Check if an executor is registered for a language.
    pub fn has_executor(&self, language: &CellLanguage) -> bool {
        self.executors.contains_key(language)
    }

    /// Execute a cell and all its transitive dependents.
    pub async fn execute_cell(&mut self, cell_id: CellId) -> Result<Vec<Notification>, ExecutionError> {
        self.notifications.clear();

        // Get all cells that need to run (this cell + descendants)
        let to_run = self.graph.descendants(&[cell_id]);

        // Topological sort for execution order
        let sorted = self.graph.topological_sort(&to_run)
            .map_err(|e| ExecutionError::Failed {
                message: e.to_string(),
                traceback: vec![],
            })?;

        // Execute each cell in order
        for &run_id in &sorted {
            let cell = self.graph.get(run_id).ok_or_else(|| ExecutionError::Failed {
                message: format!("Cell {run_id} not found"),
                traceback: vec![],
            })?;

            // Skip disabled cells
            if cell.config.disabled {
                continue;
            }

            let language = cell.language.clone();
            let code = cell.code.clone();

            // Mark as running
            if let Some(cell) = self.graph.get_mut(run_id) {
                cell.status = CellStatus::Running;
            }
            self.notifications.push(Notification::CellOp {
                cell_id: run_id,
                output: None,
                console: vec![],
                status: CellStatus::Running,
                stale_inputs: None,
                timestamp: now(),
            });

            // Execute
            let executor = self.executors.get(&language)
                .ok_or(ExecutionError::UnsupportedLanguage { language })?;

            match executor.execute(&code, &self.variables).await {
                Ok(result) => {
                    // Update variables
                    for (name, value) in &result.defs {
                        self.variables.insert(name.clone(), value.clone());
                    }

                    // Update cell state
                    if let Some(cell) = self.graph.get_mut(run_id) {
                        cell.status = CellStatus::Idle;
                        cell.output = result.output.clone();
                        cell.console = result.console.clone();
                    }

                    self.notifications.push(Notification::CellOp {
                        cell_id: run_id,
                        output: result.output,
                        console: result.console,
                        status: CellStatus::Idle,
                        stale_inputs: Some(false),
                        timestamp: now(),
                    });
                }
                Err(e) => {
                    // Mark cell as idle with error
                    if let Some(cell) = self.graph.get_mut(run_id) {
                        cell.status = CellStatus::Idle;
                    }

                    let error_output = CellOutput {
                        mime_type: "application/vnd.marimo+error".to_string(),
                        data: e.to_string(),
                    };

                    self.notifications.push(Notification::CellOp {
                        cell_id: run_id,
                        output: Some(error_output),
                        console: vec![],
                        status: CellStatus::Idle,
                        stale_inputs: None,
                        timestamp: now(),
                    });

                    // Cancel descendants (don't run cells that depend on a failed cell)
                    break;
                }
            }
        }

        Ok(std::mem::take(&mut self.notifications))
    }

    /// Get the current value of a variable.
    pub fn get_variable(&self, name: &str) -> Option<&serde_json::Value> {
        self.variables.get(name)
    }
}

fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}
