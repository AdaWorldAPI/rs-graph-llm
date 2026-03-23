//! Dependency graph and topological sort.
//!
//! The dataflow graph tracks which cells depend on which variables.
//! When a cell's defs change, all cells that ref those defs are marked
//! for re-execution. Execution order is determined by topological sort.

use crate::cell::Cell;
use crate::CellId;
use std::collections::{HashMap, HashSet, VecDeque};

/// The dependency graph for all cells in a notebook.
#[derive(Debug, Default)]
pub struct DataflowGraph {
    /// All cells, indexed by ID.
    cells: HashMap<CellId, Cell>,
    /// Which cell defines each variable. (variable_name → cell_id)
    definitions: HashMap<String, CellId>,
    /// Edges: parent_cell → set of child cells that depend on it.
    children: HashMap<CellId, HashSet<CellId>>,
    /// Edges: child_cell → set of parent cells it depends on.
    parents: HashMap<CellId, HashSet<CellId>>,
}

impl DataflowGraph {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a cell in the graph. Rebuilds edges for this cell.
    pub fn register(&mut self, cell: Cell) {
        let cell_id = cell.id;

        // Remove old edges if cell was previously registered
        self.unregister(cell_id);

        // Register variable definitions
        for def in &cell.defs {
            self.definitions.insert(def.clone(), cell_id);
        }

        // Build edges: for each ref in this cell, find which cell defines it
        let mut parent_ids = HashSet::new();
        for ref_var in &cell.refs {
            if let Some(&parent_id) = self.definitions.get(ref_var) {
                if parent_id != cell_id {
                    parent_ids.insert(parent_id);
                    self.children.entry(parent_id).or_default().insert(cell_id);
                }
            }
        }
        self.parents.insert(cell_id, parent_ids);

        // Also update children edges for cells that ref our defs
        for def in &cell.defs {
            for (&other_id, other_cell) in &self.cells {
                if other_id != cell_id && other_cell.refs.contains(def) {
                    self.children.entry(cell_id).or_default().insert(other_id);
                    self.parents.entry(other_id).or_default().insert(cell_id);
                }
            }
        }

        self.cells.insert(cell_id, cell);
    }

    /// Remove a cell from the graph.
    pub fn unregister(&mut self, cell_id: CellId) {
        if let Some(old_cell) = self.cells.remove(&cell_id) {
            // Remove definitions
            for def in &old_cell.defs {
                if self.definitions.get(def) == Some(&cell_id) {
                    self.definitions.remove(def);
                }
            }
            // Remove edges
            if let Some(parent_ids) = self.parents.remove(&cell_id) {
                for parent_id in parent_ids {
                    if let Some(children) = self.children.get_mut(&parent_id) {
                        children.remove(&cell_id);
                    }
                }
            }
            if let Some(child_ids) = self.children.remove(&cell_id) {
                for child_id in child_ids {
                    if let Some(parents) = self.parents.get_mut(&child_id) {
                        parents.remove(&cell_id);
                    }
                }
            }
        }
    }

    /// Get all transitive descendants of the given cells (BFS).
    pub fn descendants(&self, cell_ids: &[CellId]) -> Vec<CellId> {
        let mut visited = HashSet::new();
        let mut queue: VecDeque<CellId> = cell_ids.iter().copied().collect();
        let mut result = Vec::new();

        while let Some(cell_id) = queue.pop_front() {
            if !visited.insert(cell_id) {
                continue;
            }
            result.push(cell_id);
            if let Some(children) = self.children.get(&cell_id) {
                for &child_id in children {
                    if !visited.contains(&child_id) {
                        queue.push_back(child_id);
                    }
                }
            }
        }
        result
    }

    /// Topological sort of the given cell IDs.
    /// Returns cells in execution order (parents before children).
    pub fn topological_sort(&self, cell_ids: &[CellId]) -> Result<Vec<CellId>, CycleError> {
        let cell_set: HashSet<CellId> = cell_ids.iter().copied().collect();
        let mut in_degree: HashMap<CellId, usize> = HashMap::new();
        let mut adj: HashMap<CellId, Vec<CellId>> = HashMap::new();

        // Build subgraph
        for &cell_id in &cell_set {
            in_degree.entry(cell_id).or_insert(0);
            if let Some(children) = self.children.get(&cell_id) {
                for &child_id in children {
                    if cell_set.contains(&child_id) {
                        adj.entry(cell_id).or_default().push(child_id);
                        *in_degree.entry(child_id).or_insert(0) += 1;
                    }
                }
            }
        }

        // Kahn's algorithm
        let mut queue: VecDeque<CellId> = in_degree
            .iter()
            .filter(|(_, &deg)| deg == 0)
            .map(|(&id, _)| id)
            .collect();
        let mut sorted = Vec::with_capacity(cell_ids.len());

        while let Some(cell_id) = queue.pop_front() {
            sorted.push(cell_id);
            if let Some(neighbors) = adj.get(&cell_id) {
                for &next in neighbors {
                    if let Some(deg) = in_degree.get_mut(&next) {
                        *deg -= 1;
                        if *deg == 0 {
                            queue.push_back(next);
                        }
                    }
                }
            }
        }

        if sorted.len() != cell_set.len() {
            return Err(CycleError {
                cells: cell_set.difference(&sorted.iter().copied().collect()).copied().collect(),
            });
        }

        Ok(sorted)
    }

    /// Get a reference to a cell by ID.
    pub fn get(&self, cell_id: CellId) -> Option<&Cell> {
        self.cells.get(&cell_id)
    }

    /// Get a mutable reference to a cell by ID.
    pub fn get_mut(&mut self, cell_id: CellId) -> Option<&mut Cell> {
        self.cells.get_mut(&cell_id)
    }

    /// Get all cell IDs.
    pub fn cell_ids(&self) -> Vec<CellId> {
        self.cells.keys().copied().collect()
    }

    /// Check if a variable is defined by any cell.
    pub fn who_defines(&self, variable: &str) -> Option<CellId> {
        self.definitions.get(variable).copied()
    }
}

/// Error when the dependency graph contains a cycle.
#[derive(Debug, thiserror::Error)]
#[error("Dependency cycle detected involving cells: {cells:?}")]
pub struct CycleError {
    pub cells: Vec<CellId>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cell::CellLanguage;

    #[test]
    fn test_simple_dependency() {
        let mut graph = DataflowGraph::new();

        let cell_a = Cell::new("x = 1", CellLanguage::Python)
            .with_defs(["x"]);
        let cell_b = Cell::new("y = x + 1", CellLanguage::Python)
            .with_defs(["y"])
            .with_refs(["x"]);

        let a_id = cell_a.id;
        let b_id = cell_b.id;

        graph.register(cell_a);
        graph.register(cell_b);

        // b depends on a
        let descendants = graph.descendants(&[a_id]);
        assert!(descendants.contains(&b_id));

        // Topological sort: a before b
        let sorted = graph.topological_sort(&[a_id, b_id]).unwrap();
        assert_eq!(sorted[0], a_id);
        assert_eq!(sorted[1], b_id);
    }

    #[test]
    fn test_diamond_dependency() {
        let mut graph = DataflowGraph::new();

        let a = Cell::new("x = 1", CellLanguage::Python).with_defs(["x"]);
        let b = Cell::new("y = x", CellLanguage::Python).with_defs(["y"]).with_refs(["x"]);
        let c = Cell::new("z = x", CellLanguage::Python).with_defs(["z"]).with_refs(["x"]);
        let d = Cell::new("w = y + z", CellLanguage::Python).with_defs(["w"]).with_refs(["y", "z"]);

        let (a_id, b_id, c_id, d_id) = (a.id, b.id, c.id, d.id);

        graph.register(a);
        graph.register(b);
        graph.register(c);
        graph.register(d);

        // All are descendants of a
        let desc = graph.descendants(&[a_id]);
        assert_eq!(desc.len(), 4);

        // d comes after b and c in sort
        let sorted = graph.topological_sort(&[a_id, b_id, c_id, d_id]).unwrap();
        assert_eq!(sorted[0], a_id);
        assert_eq!(*sorted.last().unwrap(), d_id);
    }
}
