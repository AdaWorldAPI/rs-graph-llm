//! Query result types and conversions.

use crate::{GraphData, GraphNode, GraphEdge};

/// Convert a list of node/edge JSON records into vis.js-compatible GraphData.
pub fn to_graph_data(
    nodes: Vec<GraphNode>,
    edges: Vec<GraphEdge>,
) -> GraphData {
    GraphData { nodes, edges }
}
