//! Long-term memory store for agents.
//!
//! Maps to LangGraph's `langgraph.store` module. Provides namespace-scoped
//! key-value storage with optional vector search.
//!
//! # Examples
//!
//! ```rust
//! use graph_flow::store::{InMemoryStore, BaseStore, Item};
//!
//! # #[tokio::main]
//! # async fn main() -> graph_flow::Result<()> {
//! let store = InMemoryStore::new();
//! store.put(("user", "prefs"), "theme", serde_json::json!({"dark": true})).await?;
//!
//! let item = store.get(("user", "prefs"), "theme").await?;
//! assert!(item.is_some());
//!
//! let items = store.search(("user", "prefs"), None, 10).await?;
//! assert_eq!(items.len(), 1);
//! # Ok(())
//! # }
//! ```

pub mod memory;
#[cfg(feature = "lance-store")]
pub mod lance;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::Result;

/// A stored item in the memory store.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Item {
    /// The namespace this item belongs to.
    pub namespace: Vec<String>,
    /// The unique key within the namespace.
    pub key: String,
    /// The stored value.
    pub value: serde_json::Value,
    /// When the item was created (Unix timestamp ms).
    pub created_at: u64,
    /// When the item was last updated (Unix timestamp ms).
    pub updated_at: u64,
}

impl Item {
    /// Create a new Item.
    pub fn new(namespace: Vec<String>, key: String, value: serde_json::Value) -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        Self {
            namespace,
            key,
            value,
            created_at: now,
            updated_at: now,
        }
    }
}

/// A search result item with optional relevance score.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchItem {
    /// The stored item.
    pub item: Item,
    /// Relevance score (0.0–1.0) if vector search was used.
    pub score: Option<f64>,
}

/// Filter condition for search operations.
#[derive(Debug, Clone)]
pub struct MatchCondition {
    /// JSON path to match against (e.g. "metadata.source").
    pub path: String,
    /// Value to match.
    pub value: serde_json::Value,
}

impl MatchCondition {
    pub fn new(path: impl Into<String>, value: serde_json::Value) -> Self {
        Self {
            path: path.into(),
            value,
        }
    }
}

/// Trait for namespace tuples — converts various tuple forms to `Vec<String>`.
pub trait IntoNamespace {
    fn into_namespace(self) -> Vec<String>;
}

impl IntoNamespace for Vec<String> {
    fn into_namespace(self) -> Vec<String> {
        self
    }
}

impl IntoNamespace for &[&str] {
    fn into_namespace(self) -> Vec<String> {
        self.iter().map(|s| s.to_string()).collect()
    }
}

impl IntoNamespace for (&str,) {
    fn into_namespace(self) -> Vec<String> {
        vec![self.0.to_string()]
    }
}

impl IntoNamespace for (&str, &str) {
    fn into_namespace(self) -> Vec<String> {
        vec![self.0.to_string(), self.1.to_string()]
    }
}

impl IntoNamespace for (&str, &str, &str) {
    fn into_namespace(self) -> Vec<String> {
        vec![self.0.to_string(), self.1.to_string(), self.2.to_string()]
    }
}

/// Core trait for long-term memory storage.
///
/// Maps to LangGraph's `BaseStore`. Provides namespace-scoped CRUD with
/// optional vector search support.
#[async_trait]
pub trait BaseStore: Send + Sync {
    /// Get a single item by namespace and key.
    async fn get(&self, namespace: impl IntoNamespace + Send, key: &str) -> Result<Option<Item>>;

    /// Search for items within a namespace.
    ///
    /// - `filter`: optional match conditions
    /// - `limit`: max items to return
    async fn search(
        &self,
        namespace: impl IntoNamespace + Send,
        filter: Option<Vec<MatchCondition>>,
        limit: usize,
    ) -> Result<Vec<SearchItem>>;

    /// Store or update an item.
    async fn put(
        &self,
        namespace: impl IntoNamespace + Send,
        key: &str,
        value: serde_json::Value,
    ) -> Result<()>;

    /// Delete an item.
    async fn delete(&self, namespace: impl IntoNamespace + Send, key: &str) -> Result<()>;

    /// List all namespaces, optionally filtered by prefix.
    async fn list_namespaces(
        &self,
        prefix: Option<Vec<String>>,
    ) -> Result<Vec<Vec<String>>>;
}

pub use memory::InMemoryStore;
#[cfg(feature = "lance-store")]
pub use lance::LanceStore;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_item_creation() {
        let item = Item::new(
            vec!["user".into(), "prefs".into()],
            "theme".into(),
            serde_json::json!({"dark": true}),
        );
        assert_eq!(item.namespace, vec!["user", "prefs"]);
        assert_eq!(item.key, "theme");
        assert!(item.created_at > 0);
    }

    #[test]
    fn test_into_namespace() {
        let ns: Vec<String> = ("a", "b").into_namespace();
        assert_eq!(ns, vec!["a", "b"]);

        let ns: Vec<String> = ("a", "b", "c").into_namespace();
        assert_eq!(ns, vec!["a", "b", "c"]);
    }

    #[test]
    fn test_match_condition() {
        let cond = MatchCondition::new("metadata.source", serde_json::json!("web"));
        assert_eq!(cond.path, "metadata.source");
    }
}
