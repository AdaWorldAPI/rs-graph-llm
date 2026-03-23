//! Typed state support for graph-flow.
//!
//! LangGraph uses TypedDict for state. graph-flow uses `Context` (HashMap<String, Value>).
//! This module adds an optional typed state layer via generics, while remaining
//! backward compatible with the existing `Context` API.
//!
//! # Examples
//!
//! ```rust
//! use graph_flow::typed_context::{TypedContext, State};
//! use serde::{Serialize, Deserialize};
//!
//! #[derive(Debug, Clone, Default, Serialize, Deserialize)]
//! struct AgentState {
//!     query: String,
//!     results: Vec<String>,
//!     iteration: usize,
//! }
//!
//! impl State for AgentState {}
//!
//! # #[tokio::main]
//! # async fn main() {
//! let ctx = TypedContext::new(AgentState {
//!     query: "rust langgraph".to_string(),
//!     results: vec![],
//!     iteration: 0,
//! });
//!
//! // Access typed state
//! {
//!     let state = ctx.state();
//!     assert_eq!(state.query, "rust langgraph");
//! }
//!
//! // Mutate typed state
//! ctx.update_state(|s| {
//!     s.iteration += 1;
//!     s.results.push("result1".to_string());
//! });
//!
//! {
//!     let state = ctx.state();
//!     assert_eq!(state.iteration, 1);
//!     assert_eq!(state.results.len(), 1);
//! }
//!
//! // Still access the underlying Context for untyped data
//! ctx.context().set_sync("extra_key", "extra_value".to_string());
//! let val: Option<String> = ctx.context().get_sync("extra_key");
//! assert_eq!(val, Some("extra_value".to_string()));
//! # }
//! ```

use async_trait::async_trait;
use serde::{de::DeserializeOwned, Serialize};
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};

use crate::context::Context;
use crate::error::Result;
use crate::task::TaskResult;

/// Marker trait for typed state structs.
///
/// Any struct that is Send + Sync + Serialize + DeserializeOwned + Clone can be used
/// as typed state in a `TypedContext`.
pub trait State: Send + Sync + Serialize + DeserializeOwned + Clone + 'static {}

/// A context wrapper that provides both typed state and untyped key-value storage.
///
/// `TypedContext<S>` wraps the standard `Context` and adds a typed state `S`.
/// The typed state provides compile-time guarantees about the shape of your
/// workflow state, while the underlying `Context` is still available for
/// dynamic data.
///
/// # Examples
///
/// ```rust
/// use graph_flow::typed_context::{TypedContext, State};
/// use serde::{Serialize, Deserialize};
///
/// #[derive(Debug, Clone, Default, Serialize, Deserialize)]
/// struct MyState {
///     counter: i32,
///     message: String,
/// }
///
/// impl State for MyState {}
///
/// let ctx = TypedContext::new(MyState {
///     counter: 0,
///     message: "hello".to_string(),
/// });
///
/// // Read state
/// assert_eq!(ctx.state().counter, 0);
///
/// // Update state
/// ctx.update_state(|s| s.counter += 1);
/// assert_eq!(ctx.state().counter, 1);
/// ```
#[derive(Clone)]
pub struct TypedContext<S: State> {
    inner: Context,
    state: Arc<RwLock<S>>,
}

impl<S: State> TypedContext<S> {
    /// Create a new TypedContext with initial state.
    pub fn new(initial_state: S) -> Self {
        Self {
            inner: Context::new(),
            state: Arc::new(RwLock::new(initial_state)),
        }
    }

    /// Create a new TypedContext with initial state and an existing Context.
    pub fn with_context(initial_state: S, context: Context) -> Self {
        Self {
            inner: context,
            state: Arc::new(RwLock::new(initial_state)),
        }
    }

    /// Get read access to the typed state.
    pub fn state(&self) -> RwLockReadGuard<'_, S> {
        self.state.read().expect("state lock poisoned")
    }

    /// Get write access to the typed state.
    pub fn state_mut(&self) -> RwLockWriteGuard<'_, S> {
        self.state.write().expect("state lock poisoned")
    }

    /// Update the typed state with a closure.
    pub fn update_state<F>(&self, f: F)
    where
        F: FnOnce(&mut S),
    {
        let mut state = self.state.write().expect("state lock poisoned");
        f(&mut state);
    }

    /// Get a reference to the underlying Context.
    pub fn context(&self) -> &Context {
        &self.inner
    }

    /// Get a clone of the typed state.
    pub fn snapshot_state(&self) -> S {
        self.state.read().expect("state lock poisoned").clone()
    }

    /// Replace the entire typed state.
    pub fn replace_state(&self, new_state: S) {
        let mut state = self.state.write().expect("state lock poisoned");
        *state = new_state;
    }

    /// Serialize the typed state to the Context under a given key.
    ///
    /// This is useful for persisting the typed state alongside the Context.
    pub async fn sync_state_to_context(&self, key: &str) {
        let state = self.state.read().expect("state lock poisoned").clone();
        self.inner.set(key, state).await;
    }

    /// Deserialize the typed state from the Context under a given key.
    ///
    /// Returns true if successful, false if the key doesn't exist or deserialization fails.
    pub async fn sync_state_from_context(&self, key: &str) -> bool {
        if let Some(state) = self.inner.get::<S>(key).await {
            let mut current = self.state.write().expect("state lock poisoned");
            *current = state;
            true
        } else {
            false
        }
    }
}

impl<S: State + Default> TypedContext<S> {
    /// Create a `TypedContext` using `S::default()` as the initial state.
    ///
    /// This is a convenience builder for cases where the state struct
    /// derives `Default`.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use graph_flow::typed_context::{TypedContext, State};
    /// use serde::{Serialize, Deserialize};
    ///
    /// #[derive(Debug, Clone, Default, Serialize, Deserialize)]
    /// struct MyState { counter: i32 }
    /// impl State for MyState {}
    ///
    /// let ctx = TypedContext::<MyState>::default_state();
    /// assert_eq!(ctx.state().counter, 0);
    /// ```
    pub fn default_state() -> Self {
        Self::new(S::default())
    }
}

/// Builder for constructing a `TypedContext` step-by-step.
///
/// # Examples
///
/// ```rust
/// use graph_flow::typed_context::{TypedContextBuilder, State};
/// use serde::{Serialize, Deserialize};
///
/// #[derive(Debug, Clone, Default, Serialize, Deserialize)]
/// struct PipelineState {
///     query: String,
///     max_results: usize,
/// }
/// impl State for PipelineState {}
///
/// let ctx = TypedContextBuilder::new(PipelineState {
///         query: "hello".into(),
///         max_results: 10,
///     })
///     .context_value("extra", "metadata")
///     .build();
///
/// assert_eq!(ctx.state().query, "hello");
/// let extra: Option<String> = ctx.context().get_sync("extra");
/// assert_eq!(extra, Some("metadata".to_string()));
/// ```
pub struct TypedContextBuilder<S: State> {
    state: S,
    context: Context,
}

impl<S: State> TypedContextBuilder<S> {
    /// Start building a `TypedContext` with the given initial state.
    pub fn new(state: S) -> Self {
        Self {
            state,
            context: Context::new(),
        }
    }

    /// Set a key-value pair on the underlying `Context`.
    pub fn context_value(self, key: impl Into<String>, value: impl serde::Serialize) -> Self {
        self.context.set_sync(key, value);
        self
    }

    /// Use an existing `Context` as the underlying storage.
    pub fn with_context(mut self, context: Context) -> Self {
        self.context = context;
        self
    }

    /// Finish building and return the `TypedContext`.
    pub fn build(self) -> TypedContext<S> {
        TypedContext::with_context(self.state, self.context)
    }
}

/// A task trait that enforces typed state at task boundaries.
///
/// `TypedTask<S>` is a higher-level alternative to `Task` that works with
/// `TypedContext<S>` instead of raw `Context`. It provides compile-time
/// guarantees that the task operates on the correct state type.
///
/// Every `TypedTask<S>` automatically implements `Task`, so it can be
/// used anywhere a `Task` is expected. The bridge layer serializes the
/// typed state to/from the underlying `Context` under a well-known key.
///
/// # Examples
///
/// ```rust
/// use graph_flow::typed_context::{TypedTask, TypedContext, State};
/// use graph_flow::{TaskResult, NextAction};
/// use async_trait::async_trait;
/// use serde::{Serialize, Deserialize};
///
/// #[derive(Debug, Clone, Default, Serialize, Deserialize)]
/// struct CounterState { count: i32 }
/// impl State for CounterState {}
///
/// struct IncrementTask;
///
/// #[async_trait]
/// impl TypedTask<CounterState> for IncrementTask {
///     fn id(&self) -> &str { "increment" }
///
///     async fn run_typed(
///         &self,
///         ctx: &TypedContext<CounterState>,
///     ) -> graph_flow::Result<TaskResult> {
///         ctx.update_state(|s| s.count += 1);
///         Ok(TaskResult::new(
///             Some(format!("count={}", ctx.state().count)),
///             NextAction::Continue,
///         ))
///     }
/// }
/// ```
#[async_trait]
pub trait TypedTask<S: State + Default>: Send + Sync {
    /// Unique identifier for this typed task.
    fn id(&self) -> &str;

    /// Context keys this task expects to find (for validation).
    fn input_keys(&self) -> &[&str] {
        &[]
    }

    /// Context keys this task will write (for documentation / validation).
    fn output_keys(&self) -> &[&str] {
        &[]
    }

    /// Execute the task with typed state.
    async fn run_typed(&self, ctx: &TypedContext<S>) -> Result<TaskResult>;
}

/// The well-known context key used to serialize/deserialize typed state.
pub const TYPED_STATE_KEY: &str = "__typed_state";

/// Adapter that wraps a `TypedTask<S>` so it can be used as a `Task`.
///
/// On entry it deserializes the typed state from `Context` (or uses
/// `S::default()`). On exit it serializes the typed state back to `Context`.
pub struct TypedTaskAdapter<S: State + Default, T: TypedTask<S>> {
    inner: T,
    _marker: std::marker::PhantomData<S>,
}

impl<S: State + Default, T: TypedTask<S>> TypedTaskAdapter<S, T> {
    /// Wrap a `TypedTask` so it implements `Task`.
    pub fn new(task: T) -> Self {
        Self {
            inner: task,
            _marker: std::marker::PhantomData,
        }
    }
}

#[async_trait]
impl<S: State + Default, T: TypedTask<S>> crate::task::Task for TypedTaskAdapter<S, T> {
    fn id(&self) -> &str {
        self.inner.id()
    }

    fn input_keys(&self) -> &[&str] {
        self.inner.input_keys()
    }

    fn output_keys(&self) -> &[&str] {
        self.inner.output_keys()
    }

    async fn run(&self, context: Context) -> Result<TaskResult> {
        // Hydrate typed state from context (or default)
        let typed_ctx = TypedContext::<S>::default_state();

        // Try to load existing typed state from context
        let existing: Option<S> = context.get(TYPED_STATE_KEY).await;
        if let Some(state) = existing {
            typed_ctx.replace_state(state);
        }

        // Share the underlying context
        let typed_ctx = TypedContext::with_context(
            typed_ctx.snapshot_state(),
            context.clone(),
        );

        // Validate required input keys
        let input_keys = self.inner.input_keys();
        if !input_keys.is_empty() {
            context.require_keys(input_keys)?;
        }

        // Run the typed task
        let result = self.inner.run_typed(&typed_ctx).await?;

        // Persist typed state back to context
        typed_ctx.sync_state_to_context(TYPED_STATE_KEY).await;

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
    struct TestState {
        counter: i32,
        items: Vec<String>,
    }

    impl State for TestState {}

    #[test]
    fn test_typed_context_basic() {
        let ctx = TypedContext::new(TestState {
            counter: 0,
            items: vec![],
        });

        assert_eq!(ctx.state().counter, 0);
        assert!(ctx.state().items.is_empty());

        ctx.update_state(|s| {
            s.counter = 42;
            s.items.push("hello".to_string());
        });

        assert_eq!(ctx.state().counter, 42);
        assert_eq!(ctx.state().items, vec!["hello".to_string()]);
    }

    #[test]
    fn test_typed_context_with_context() {
        let context = Context::new();
        context.set_sync("key", "value".to_string());

        let ctx = TypedContext::with_context(
            TestState::default(),
            context,
        );

        let val: Option<String> = ctx.context().get_sync("key");
        assert_eq!(val, Some("value".to_string()));
    }

    #[test]
    fn test_snapshot_and_replace() {
        let ctx = TypedContext::new(TestState {
            counter: 10,
            items: vec!["a".to_string()],
        });

        let snap = ctx.snapshot_state();
        assert_eq!(snap.counter, 10);

        ctx.replace_state(TestState {
            counter: 99,
            items: vec![],
        });
        assert_eq!(ctx.state().counter, 99);

        // snapshot is independent
        assert_eq!(snap.counter, 10);
    }

    #[tokio::test]
    async fn test_sync_state_to_context() {
        let ctx = TypedContext::new(TestState {
            counter: 5,
            items: vec!["x".to_string()],
        });

        ctx.sync_state_to_context("typed_state").await;

        let loaded: Option<TestState> = ctx.context().get("typed_state").await;
        assert_eq!(
            loaded,
            Some(TestState {
                counter: 5,
                items: vec!["x".to_string()],
            })
        );
    }

    #[tokio::test]
    async fn test_sync_state_from_context() {
        let ctx = TypedContext::new(TestState::default());

        let target = TestState {
            counter: 77,
            items: vec!["loaded".to_string()],
        };
        ctx.context().set("state_key", target.clone()).await;

        assert!(ctx.sync_state_from_context("state_key").await);
        assert_eq!(ctx.state().counter, 77);
        assert_eq!(ctx.state().items, vec!["loaded".to_string()]);
    }

    #[tokio::test]
    async fn test_sync_state_from_context_missing() {
        let ctx = TypedContext::new(TestState::default());
        assert!(!ctx.sync_state_from_context("nonexistent").await);
        assert_eq!(ctx.state().counter, 0); // unchanged
    }

    #[test]
    fn test_clone() {
        let ctx = TypedContext::new(TestState {
            counter: 1,
            items: vec![],
        });

        let cloned = ctx.clone();
        ctx.update_state(|s| s.counter = 100);

        // Cloned shares the same Arc, so it sees the update
        assert_eq!(cloned.state().counter, 100);
    }

    // --- New tests for typed state schema parity ---

    #[test]
    fn test_default_state_builder() {
        let ctx = TypedContext::<TestState>::default_state();
        assert_eq!(ctx.state().counter, 0);
        assert!(ctx.state().items.is_empty());
    }

    #[test]
    fn test_typed_context_builder() {
        let ctx = TypedContextBuilder::new(TestState {
            counter: 42,
            items: vec!["a".to_string()],
        })
        .context_value("extra_key", "extra_value")
        .build();

        assert_eq!(ctx.state().counter, 42);
        let val: Option<String> = ctx.context().get_sync("extra_key");
        assert_eq!(val, Some("extra_value".to_string()));
    }

    #[test]
    fn test_typed_context_builder_with_context() {
        let existing = Context::new();
        existing.set_sync("pre_existing", 123);

        let ctx = TypedContextBuilder::new(TestState::default())
            .with_context(existing)
            .context_value("added", true)
            .build();

        let pre: Option<i32> = ctx.context().get_sync("pre_existing");
        assert_eq!(pre, Some(123));
        let added: Option<bool> = ctx.context().get_sync("added");
        assert_eq!(added, Some(true));
    }

    // --- Context validation tests ---

    #[test]
    fn test_validate_context_passes() {
        let ctx = Context::new();
        ctx.set_sync("name", "Alice");
        ctx.set_sync("age", 30);

        assert!(ctx.validate_context(&["name", "age"]).is_ok());
    }

    #[test]
    fn test_validate_context_fails_with_missing() {
        let ctx = Context::new();
        ctx.set_sync("name", "Alice");

        let result = ctx.validate_context(&["name", "age", "email"]);
        assert!(result.is_err());
        let msg = result.unwrap_err();
        assert!(msg.contains("age"), "error should mention 'age': {}", msg);
        assert!(msg.contains("email"), "error should mention 'email': {}", msg);
        assert!(!msg.contains("name"), "error should NOT mention 'name': {}", msg);
    }

    #[test]
    fn test_validate_context_empty_keys_passes() {
        let ctx = Context::new();
        assert!(ctx.validate_context(&[]).is_ok());
    }

    #[test]
    fn test_require_keys_returns_graph_error() {
        let ctx = Context::new();
        let result = ctx.require_keys(&["missing_key"]);
        assert!(result.is_err());
        match result.unwrap_err() {
            crate::error::GraphError::ContextError(msg) => {
                assert!(msg.contains("missing_key"));
            }
            other => panic!("Expected ContextError, got {:?}", other),
        }
    }

    #[test]
    fn test_context_keys() {
        let ctx = Context::new();
        ctx.set_sync("alpha", 1);
        ctx.set_sync("beta", 2);

        let mut keys = ctx.keys();
        keys.sort();
        assert_eq!(keys, vec!["alpha".to_string(), "beta".to_string()]);
    }

    // --- Task input_keys / output_keys tests ---

    use crate::task::{Task, NextAction, TaskResult};

    struct TaskWithSchema;

    #[async_trait]
    impl Task for TaskWithSchema {
        fn id(&self) -> &str {
            "task_with_schema"
        }

        fn input_keys(&self) -> &[&str] {
            &["query", "user_id"]
        }

        fn output_keys(&self) -> &[&str] {
            &["result", "confidence"]
        }

        async fn run(&self, context: Context) -> crate::error::Result<TaskResult> {
            context.require_keys(self.input_keys())?;
            let query: String = context.get("query").await.unwrap();
            context.set("result", format!("processed: {}", query)).await;
            context.set("confidence", 0.95f64).await;
            Ok(TaskResult::new(None, NextAction::Continue))
        }
    }

    struct TaskWithoutSchema;

    #[async_trait]
    impl Task for TaskWithoutSchema {
        fn id(&self) -> &str {
            "task_without_schema"
        }

        // Uses default empty input_keys/output_keys — backward compatible
        async fn run(&self, _context: Context) -> crate::error::Result<TaskResult> {
            Ok(TaskResult::new(None, NextAction::End))
        }
    }

    #[tokio::test]
    async fn test_task_input_keys_validation_passes() {
        let task = TaskWithSchema;
        let ctx = Context::new();
        ctx.set("query", "hello".to_string()).await;
        ctx.set("user_id", 42).await;

        let result = task.run(ctx).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_task_input_keys_validation_fails() {
        let task = TaskWithSchema;
        let ctx = Context::new();
        // Missing "query" and "user_id"

        let result = task.run(ctx).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_task_without_schema_backward_compatible() {
        let task = TaskWithoutSchema;
        assert!(task.input_keys().is_empty());
        assert!(task.output_keys().is_empty());

        let ctx = Context::new();
        let result = task.run(ctx).await;
        assert!(result.is_ok());
    }

    // --- TypedTask / TypedTaskAdapter tests ---

    #[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
    struct CounterState {
        count: i32,
    }
    impl State for CounterState {}

    struct IncrementTask;

    #[async_trait]
    impl TypedTask<CounterState> for IncrementTask {
        fn id(&self) -> &str {
            "increment"
        }

        async fn run_typed(&self, ctx: &TypedContext<CounterState>) -> crate::error::Result<TaskResult> {
            ctx.update_state(|s| s.count += 1);
            Ok(TaskResult::new(
                Some(format!("count={}", ctx.state().count)),
                NextAction::Continue,
            ))
        }
    }

    struct TypedTaskWithInputKeys;

    #[async_trait]
    impl TypedTask<CounterState> for TypedTaskWithInputKeys {
        fn id(&self) -> &str {
            "typed_with_inputs"
        }

        fn input_keys(&self) -> &[&str] {
            &["increment_by"]
        }

        async fn run_typed(&self, ctx: &TypedContext<CounterState>) -> crate::error::Result<TaskResult> {
            let by: i32 = ctx.context().get("increment_by").await.unwrap_or(1);
            ctx.update_state(|s| s.count += by);
            Ok(TaskResult::new(None, NextAction::Continue))
        }
    }

    #[tokio::test]
    async fn test_typed_task_adapter_basic() {
        let adapter = TypedTaskAdapter::new(IncrementTask);
        let ctx = Context::new();

        // First run — state starts from default (count=0), increments to 1
        let result = adapter.run(ctx.clone()).await.unwrap();
        assert_eq!(result.response, Some("count=1".to_string()));

        // State was persisted to context under TYPED_STATE_KEY
        let stored: Option<CounterState> = ctx.get(TYPED_STATE_KEY).await;
        assert_eq!(stored, Some(CounterState { count: 1 }));

        // Second run — picks up persisted state, increments to 2
        let result = adapter.run(ctx.clone()).await.unwrap();
        assert_eq!(result.response, Some("count=2".to_string()));

        let stored: Option<CounterState> = ctx.get(TYPED_STATE_KEY).await;
        assert_eq!(stored, Some(CounterState { count: 2 }));
    }

    #[tokio::test]
    async fn test_typed_task_adapter_validates_input_keys() {
        let adapter = TypedTaskAdapter::new(TypedTaskWithInputKeys);
        let ctx = Context::new();
        // Missing "increment_by" key — should fail validation

        let result = adapter.run(ctx).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_typed_task_adapter_input_keys_present() {
        let adapter = TypedTaskAdapter::new(TypedTaskWithInputKeys);
        let ctx = Context::new();
        ctx.set("increment_by", 5).await;

        let result = adapter.run(ctx.clone()).await;
        assert!(result.is_ok());

        let stored: Option<CounterState> = ctx.get(TYPED_STATE_KEY).await;
        assert_eq!(stored, Some(CounterState { count: 5 }));
    }

    #[tokio::test]
    async fn test_typed_vs_untyped_paths() {
        // Untyped path: plain Task with Context
        let untyped_ctx = Context::new();
        untyped_ctx.set("query", "hello".to_string()).await;
        untyped_ctx.set("user_id", 1).await;

        let untyped_task = TaskWithSchema;
        let result = untyped_task.run(untyped_ctx.clone()).await.unwrap();
        assert!(result.response.is_none());

        let res: Option<String> = untyped_ctx.get("result").await;
        assert_eq!(res, Some("processed: hello".to_string()));

        // Typed path: TypedTask with TypedContext via adapter
        let typed_ctx = Context::new();
        let adapter = TypedTaskAdapter::new(IncrementTask);
        let result = adapter.run(typed_ctx.clone()).await.unwrap();
        assert_eq!(result.response, Some("count=1".to_string()));

        // Both paths coexist — the typed state lives under TYPED_STATE_KEY
        // while untyped keys live as direct context entries
        let counter: Option<CounterState> = typed_ctx.get(TYPED_STATE_KEY).await;
        assert_eq!(counter.unwrap().count, 1);
    }
}
