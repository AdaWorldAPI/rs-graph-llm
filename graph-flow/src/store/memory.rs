//! In-memory store implementation.

use async_trait::async_trait;
use dashmap::DashMap;
use std::sync::Arc;

use super::{BaseStore, IntoNamespace, Item, MatchCondition, SearchItem};
use crate::error::Result;

/// In-memory implementation of [`BaseStore`].
///
/// Suitable for development, testing, and short-lived agents.
/// Data is lost when the process exits.
///
/// # Examples
///
/// ```rust
/// use graph_flow::store::{InMemoryStore, BaseStore};
///
/// # #[tokio::main]
/// # async fn main() -> graph_flow::Result<()> {
/// let store = InMemoryStore::new();
/// store.put(("user", "data"), "key1", serde_json::json!({"x": 1})).await?;
///
/// let item = store.get(("user", "data"), "key1").await?;
/// assert!(item.is_some());
/// assert_eq!(item.unwrap().value["x"], 1);
/// # Ok(())
/// # }
/// ```
pub struct InMemoryStore {
    /// Key format: "ns1.ns2.ns3:key"
    data: Arc<DashMap<String, Item>>,
}

impl InMemoryStore {
    pub fn new() -> Self {
        Self {
            data: Arc::new(DashMap::new()),
        }
    }

    fn make_key(namespace: &[String], key: &str) -> String {
        format!("{}:{}", namespace.join("."), key)
    }

    fn ns_prefix(namespace: &[String]) -> String {
        format!("{}:", namespace.join("."))
    }

    fn matches_filter(item: &Item, conditions: &[MatchCondition]) -> bool {
        for cond in conditions {
            let parts: Vec<&str> = cond.path.split('.').collect();
            let mut current = &item.value;
            let mut found = false;
            for part in &parts {
                if let Some(next) = current.get(part) {
                    current = next;
                    found = true;
                } else {
                    found = false;
                    break;
                }
            }
            if !found || current != &cond.value {
                return false;
            }
        }
        true
    }
}

impl Default for InMemoryStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl BaseStore for InMemoryStore {
    async fn get(&self, namespace: impl IntoNamespace + Send, key: &str) -> Result<Option<Item>> {
        let ns = namespace.into_namespace();
        let k = Self::make_key(&ns, key);
        Ok(self.data.get(&k).map(|entry| entry.value().clone()))
    }

    async fn search(
        &self,
        namespace: impl IntoNamespace + Send,
        filter: Option<Vec<MatchCondition>>,
        limit: usize,
    ) -> Result<Vec<SearchItem>> {
        let ns = namespace.into_namespace();
        let prefix = Self::ns_prefix(&ns);
        let conditions = filter.unwrap_or_default();

        let mut results = Vec::new();
        for entry in self.data.iter() {
            if entry.key().starts_with(&prefix) {
                let item = entry.value();
                if conditions.is_empty() || Self::matches_filter(item, &conditions) {
                    results.push(SearchItem {
                        item: item.clone(),
                        score: None,
                    });
                    if results.len() >= limit {
                        break;
                    }
                }
            }
        }
        Ok(results)
    }

    async fn put(
        &self,
        namespace: impl IntoNamespace + Send,
        key: &str,
        value: serde_json::Value,
    ) -> Result<()> {
        let ns = namespace.into_namespace();
        let k = Self::make_key(&ns, key);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        self.data
            .entry(k)
            .and_modify(|item| {
                item.value = value.clone();
                item.updated_at = now;
            })
            .or_insert_with(|| Item {
                namespace: ns.clone(),
                key: key.to_string(),
                value,
                created_at: now,
                updated_at: now,
            });
        Ok(())
    }

    async fn delete(&self, namespace: impl IntoNamespace + Send, key: &str) -> Result<()> {
        let ns = namespace.into_namespace();
        let k = Self::make_key(&ns, key);
        self.data.remove(&k);
        Ok(())
    }

    async fn list_namespaces(
        &self,
        prefix: Option<Vec<String>>,
    ) -> Result<Vec<Vec<String>>> {
        let prefix_str = prefix
            .as_ref()
            .map(|p| format!("{}.", p.join(".")))
            .unwrap_or_default();

        let mut namespaces = std::collections::HashSet::new();
        for entry in self.data.iter() {
            let item = entry.value();
            let ns_str = item.namespace.join(".");
            if prefix_str.is_empty() || ns_str.starts_with(&prefix_str) || ns_str == prefix_str.trim_end_matches('.') {
                namespaces.insert(item.namespace.clone());
            }
        }
        Ok(namespaces.into_iter().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_put_and_get() {
        let store = InMemoryStore::new();
        store
            .put(("user", "prefs"), "theme", serde_json::json!({"dark": true}))
            .await
            .unwrap();

        let item = store.get(("user", "prefs"), "theme").await.unwrap();
        assert!(item.is_some());
        let item = item.unwrap();
        assert_eq!(item.value["dark"], true);
        assert_eq!(item.key, "theme");
    }

    #[tokio::test]
    async fn test_search() {
        let store = InMemoryStore::new();
        store
            .put(("docs",), "a", serde_json::json!({"type": "pdf", "title": "A"}))
            .await
            .unwrap();
        store
            .put(("docs",), "b", serde_json::json!({"type": "txt", "title": "B"}))
            .await
            .unwrap();

        // Search all
        let items = store.search(("docs",), None, 10).await.unwrap();
        assert_eq!(items.len(), 2);

        // Search with filter
        let items = store
            .search(
                ("docs",),
                Some(vec![MatchCondition::new("type", serde_json::json!("pdf"))]),
                10,
            )
            .await
            .unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].item.value["title"], "A");
    }

    #[tokio::test]
    async fn test_delete() {
        let store = InMemoryStore::new();
        store
            .put(("ns",), "k", serde_json::json!("v"))
            .await
            .unwrap();
        assert!(store.get(("ns",), "k").await.unwrap().is_some());

        store.delete(("ns",), "k").await.unwrap();
        assert!(store.get(("ns",), "k").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_list_namespaces() {
        let store = InMemoryStore::new();
        store.put(("a", "b"), "k1", serde_json::json!(1)).await.unwrap();
        store.put(("a", "c"), "k2", serde_json::json!(2)).await.unwrap();
        store.put(("x",), "k3", serde_json::json!(3)).await.unwrap();

        let all = store.list_namespaces(None).await.unwrap();
        assert_eq!(all.len(), 3);

        let filtered = store.list_namespaces(Some(vec!["a".into()])).await.unwrap();
        assert_eq!(filtered.len(), 2);
    }

    #[tokio::test]
    async fn test_update_existing() {
        let store = InMemoryStore::new();
        store
            .put(("ns",), "k", serde_json::json!({"v": 1}))
            .await
            .unwrap();

        let item1 = store.get(("ns",), "k").await.unwrap().unwrap();
        assert_eq!(item1.value["v"], 1);

        store
            .put(("ns",), "k", serde_json::json!({"v": 2}))
            .await
            .unwrap();

        let item2 = store.get(("ns",), "k").await.unwrap().unwrap();
        assert_eq!(item2.value["v"], 2);
        assert_eq!(item2.created_at, item1.created_at);
    }
}
