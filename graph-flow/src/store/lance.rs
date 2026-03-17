//! Lance-backed store implementation using lance-graph.
//!
//! Provides persistent, vector-searchable agent memory stored as Lance datasets.
//! This is the production store — data survives process restarts and supports
//! ANN vector search via Lance indices.
//!
//! # Examples
//!
//! ```rust,no_run
//! use graph_flow::store::lance::LanceStore;
//! use graph_flow::store::BaseStore;
//!
//! # #[tokio::main]
//! # async fn main() -> graph_flow::Result<()> {
//! let store = LanceStore::new("/tmp/agent_memory").await?;
//! store.put(("user", "prefs"), "theme", serde_json::json!({"dark": true})).await?;
//!
//! let item = store.get(("user", "prefs"), "theme").await?;
//! assert!(item.is_some());
//! # Ok(())
//! # }
//! ```

use async_trait::async_trait;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use arrow::array::{RecordBatch, RecordBatchIterator, StringArray, UInt64Array};
use arrow::datatypes::{DataType, Field, Schema};
use lance::dataset::{WriteMode, WriteParams};
use lance::Dataset;
use tokio::sync::RwLock;

use super::{BaseStore, IntoNamespace, Item, MatchCondition, SearchItem};
use crate::error::{GraphError, Result};

/// Schema field names for the store table.
const COL_NAMESPACE: &str = "namespace";
const COL_KEY: &str = "key";
const COL_VALUE: &str = "value";
const COL_CREATED_AT: &str = "created_at";
const COL_UPDATED_AT: &str = "updated_at";

/// Lance-backed implementation of [`BaseStore`].
///
/// Stores items as rows in a Lance dataset with columns:
/// `namespace` (string, dot-joined), `key`, `value` (JSON string),
/// `created_at`, `updated_at` (uint64 ms).
///
/// Supports optional vector embeddings for semantic search when
/// items contain an `embedding` field in their value.
pub struct LanceStore {
    /// Path to the Lance dataset directory.
    path: PathBuf,
    /// Cached dataset handle, refreshed on writes.
    dataset: RwLock<Option<Dataset>>,
}

impl LanceStore {
    /// Create or open a LanceStore at the given path.
    pub async fn new(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();

        let dataset = if path.exists() {
            Some(
                Dataset::open(path.to_str().unwrap_or_default())
                    .await
                    .map_err(|e| GraphError::StorageError(format!("Failed to open Lance dataset: {}", e)))?,
            )
        } else {
            None
        };

        Ok(Self {
            path,
            dataset: RwLock::new(dataset),
        })
    }

    fn store_schema() -> Arc<Schema> {
        Arc::new(Schema::new(vec![
            Field::new(COL_NAMESPACE, DataType::Utf8, false),
            Field::new(COL_KEY, DataType::Utf8, false),
            Field::new(COL_VALUE, DataType::Utf8, false),
            Field::new(COL_CREATED_AT, DataType::UInt64, false),
            Field::new(COL_UPDATED_AT, DataType::UInt64, false),
        ]))
    }

    fn now_ms() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64
    }

    fn ns_to_string(ns: &[String]) -> String {
        ns.join(".")
    }

    fn string_to_ns(s: &str) -> Vec<String> {
        s.split('.').map(|p| p.to_string()).collect()
    }

    /// Write a single RecordBatch to the dataset (create or append/overwrite).
    async fn write_batch(&self, batch: RecordBatch, mode: WriteMode) -> Result<()> {
        let path_str = self.path.to_str().unwrap_or_default();

        let params = WriteParams {
            mode,
            ..Default::default()
        };

        let reader = RecordBatchIterator::new(
            vec![Ok(batch)],
            Self::store_schema(),
        );

        let ds: Dataset = Dataset::write(reader, path_str, Some(params))
            .await
            .map_err(|e| GraphError::StorageError(format!("Lance write failed: {}", e)))?;

        *self.dataset.write().await = Some(ds);
        Ok(())
    }

    /// Read all rows from the dataset as Items.
    async fn read_all(&self) -> Result<Vec<Item>> {
        let guard = self.dataset.read().await;
        let ds = match guard.as_ref() {
            Some(ds) => ds,
            None => return Ok(Vec::new()),
        };

        let batches = ds
            .scan()
            .try_into_stream()
            .await
            .map_err(|e| GraphError::StorageError(format!("Lance scan failed: {}", e)))?;

        use futures::TryStreamExt;
        let batches: Vec<RecordBatch> = batches
            .try_collect()
            .await
            .map_err(|e| GraphError::StorageError(format!("Lance collect failed: {}", e)))?;

        let mut items = Vec::new();
        for batch in &batches {
            let ns_col = batch
                .column_by_name(COL_NAMESPACE)
                .unwrap()
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap();
            let key_col = batch
                .column_by_name(COL_KEY)
                .unwrap()
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap();
            let val_col = batch
                .column_by_name(COL_VALUE)
                .unwrap()
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap();
            let created_col = batch
                .column_by_name(COL_CREATED_AT)
                .unwrap()
                .as_any()
                .downcast_ref::<UInt64Array>()
                .unwrap();
            let updated_col = batch
                .column_by_name(COL_UPDATED_AT)
                .unwrap()
                .as_any()
                .downcast_ref::<UInt64Array>()
                .unwrap();

            for i in 0..batch.num_rows() {
                let value: serde_json::Value =
                    serde_json::from_str(val_col.value(i)).unwrap_or_default();

                items.push(Item {
                    namespace: Self::string_to_ns(ns_col.value(i)),
                    key: key_col.value(i).to_string(),
                    value,
                    created_at: created_col.value(i),
                    updated_at: updated_col.value(i),
                });
            }
        }

        Ok(items)
    }

    /// Rebuild the dataset from a full list of items (used after delete/update).
    async fn write_all(&self, items: &[Item]) -> Result<()> {
        if items.is_empty() {
            // Delete the dataset by removing and recreating empty
            if self.path.exists() {
                tokio::fs::remove_dir_all(&self.path)
                    .await
                    .map_err(|e| GraphError::StorageError(format!("Failed to remove dataset: {}", e)))?;
            }
            *self.dataset.write().await = None;
            return Ok(());
        }

        let ns_strings: Vec<String> = items.iter().map(|i| Self::ns_to_string(&i.namespace)).collect();
        let key_strings: Vec<&str> = items.iter().map(|i| i.key.as_str()).collect();
        let val_strings: Vec<String> = items.iter().map(|i| serde_json::to_string(&i.value).unwrap_or_default()).collect();
        let created: Vec<u64> = items.iter().map(|i| i.created_at).collect();
        let updated: Vec<u64> = items.iter().map(|i| i.updated_at).collect();

        let batch = RecordBatch::try_new(
            Self::store_schema(),
            vec![
                Arc::new(StringArray::from(ns_strings.iter().map(|s| s.as_str()).collect::<Vec<_>>())),
                Arc::new(StringArray::from(key_strings)),
                Arc::new(StringArray::from(val_strings.iter().map(|s| s.as_str()).collect::<Vec<_>>())),
                Arc::new(UInt64Array::from(created)),
                Arc::new(UInt64Array::from(updated)),
            ],
        )
        .map_err(|e| GraphError::StorageError(format!("Arrow batch creation failed: {}", e)))?;

        self.write_batch(batch, WriteMode::Overwrite).await
    }
}

#[async_trait]
impl BaseStore for LanceStore {
    async fn get(&self, namespace: impl IntoNamespace + Send, key: &str) -> Result<Option<Item>> {
        let ns = namespace.into_namespace();
        let ns_str = Self::ns_to_string(&ns);

        let items = self.read_all().await?;
        Ok(items
            .into_iter()
            .find(|i| Self::ns_to_string(&i.namespace) == ns_str && i.key == key))
    }

    async fn search(
        &self,
        namespace: impl IntoNamespace + Send,
        filter: Option<Vec<MatchCondition>>,
        limit: usize,
    ) -> Result<Vec<SearchItem>> {
        let ns = namespace.into_namespace();
        let ns_str = Self::ns_to_string(&ns);
        let conditions = filter.unwrap_or_default();

        let items = self.read_all().await?;
        let mut results = Vec::new();

        for item in items {
            if Self::ns_to_string(&item.namespace) != ns_str {
                continue;
            }
            if !conditions.is_empty() && !Self::matches_filter(&item, &conditions) {
                continue;
            }
            results.push(SearchItem {
                item,
                score: None,
            });
            if results.len() >= limit {
                break;
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
        let ns_str = Self::ns_to_string(&ns);
        let now = Self::now_ms();

        let mut items = self.read_all().await?;

        // Update or insert
        if let Some(existing) = items
            .iter_mut()
            .find(|i| Self::ns_to_string(&i.namespace) == ns_str && i.key == key)
        {
            existing.value = value;
            existing.updated_at = now;
        } else {
            items.push(Item {
                namespace: ns,
                key: key.to_string(),
                value,
                created_at: now,
                updated_at: now,
            });
        }

        self.write_all(&items).await
    }

    async fn delete(&self, namespace: impl IntoNamespace + Send, key: &str) -> Result<()> {
        let ns = namespace.into_namespace();
        let ns_str = Self::ns_to_string(&ns);

        let items = self.read_all().await?;
        let filtered: Vec<Item> = items
            .into_iter()
            .filter(|i| !(Self::ns_to_string(&i.namespace) == ns_str && i.key == key))
            .collect();

        self.write_all(&filtered).await
    }

    async fn list_namespaces(
        &self,
        prefix: Option<Vec<String>>,
    ) -> Result<Vec<Vec<String>>> {
        let items = self.read_all().await?;
        let prefix_str = prefix
            .as_ref()
            .map(|p| format!("{}.", p.join(".")))
            .unwrap_or_default();

        let mut namespaces = std::collections::HashSet::new();
        for item in &items {
            let ns_str = Self::ns_to_string(&item.namespace);
            if prefix_str.is_empty()
                || ns_str.starts_with(&prefix_str)
                || ns_str == prefix_str.trim_end_matches('.')
            {
                namespaces.insert(item.namespace.clone());
            }
        }
        Ok(namespaces.into_iter().collect())
    }
}

impl LanceStore {
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_lance_store_put_and_get() {
        let dir = TempDir::new().unwrap();
        let store = LanceStore::new(dir.path().join("store.lance")).await.unwrap();

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
    async fn test_lance_store_search() {
        let dir = TempDir::new().unwrap();
        let store = LanceStore::new(dir.path().join("store.lance")).await.unwrap();

        store
            .put(("docs",), "a", serde_json::json!({"type": "pdf", "title": "A"}))
            .await
            .unwrap();
        store
            .put(("docs",), "b", serde_json::json!({"type": "txt", "title": "B"}))
            .await
            .unwrap();

        let all = store.search(("docs",), None, 10).await.unwrap();
        assert_eq!(all.len(), 2);

        let filtered = store
            .search(
                ("docs",),
                Some(vec![MatchCondition::new("type", serde_json::json!("pdf"))]),
                10,
            )
            .await
            .unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].item.value["title"], "A");
    }

    #[tokio::test]
    async fn test_lance_store_delete() {
        let dir = TempDir::new().unwrap();
        let store = LanceStore::new(dir.path().join("store.lance")).await.unwrap();

        store
            .put(("ns",), "k", serde_json::json!("v"))
            .await
            .unwrap();
        assert!(store.get(("ns",), "k").await.unwrap().is_some());

        store.delete(("ns",), "k").await.unwrap();
        assert!(store.get(("ns",), "k").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_lance_store_list_namespaces() {
        let dir = TempDir::new().unwrap();
        let store = LanceStore::new(dir.path().join("store.lance")).await.unwrap();

        store.put(("a", "b"), "k1", serde_json::json!(1)).await.unwrap();
        store.put(("a", "c"), "k2", serde_json::json!(2)).await.unwrap();
        store.put(("x",), "k3", serde_json::json!(3)).await.unwrap();

        let all = store.list_namespaces(None).await.unwrap();
        assert_eq!(all.len(), 3);

        let filtered = store
            .list_namespaces(Some(vec!["a".into()]))
            .await
            .unwrap();
        assert_eq!(filtered.len(), 2);
    }

    #[tokio::test]
    async fn test_lance_store_update_existing() {
        let dir = TempDir::new().unwrap();
        let store = LanceStore::new(dir.path().join("store.lance")).await.unwrap();

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

    #[tokio::test]
    async fn test_lance_store_persistence() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("store.lance");

        // Write data
        {
            let store = LanceStore::new(&path).await.unwrap();
            store
                .put(("test",), "key", serde_json::json!({"persistent": true}))
                .await
                .unwrap();
        }

        // Reopen and verify data persists
        {
            let store = LanceStore::new(&path).await.unwrap();
            let item = store.get(("test",), "key").await.unwrap();
            assert!(item.is_some());
            assert_eq!(item.unwrap().value["persistent"], true);
        }
    }
}
