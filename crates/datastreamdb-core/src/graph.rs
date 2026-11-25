//! Embedded graph engine: DQL, schema state, and (optionally) GraphQL.

use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
#[cfg(feature = "graphql")]
use std::{
    sync::{Weak, atomic::AtomicBool},
    time::{Duration, Instant},
};

use datastreamdb_posting::{
    IndexRebuild, PostingEngine,
    store::{PARTITION, read_chain},
};
use datastreamdb_query::{QueryEngine, request::uids_to_hex};
use datastreamdb_schema::{
    ParsedSchema, SchemaState, check_and_modify_pre_def_predicate, initial_schema, initial_types,
    is_pre_def_type_changed, parse_with_namespace,
};
use datastreamdb_store::{Backend, WriteBatch};
#[cfg(feature = "graphql")]
use parking_lot::Mutex;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

#[cfg(feature = "graphql")]
use datastreamdb_graphql::{
    op::{BoundSchema, OpKind},
    resolve::GraphQlEngine,
    schemagen::new_handler,
    subscription::rewrite_subscription_keyword,
};

use crate::CoreError;

/// Meta partition for embedded engine state (separate from graph data).
pub(crate) const META_PARTITION: &str = "_datastreamdb_meta";
const META_VERSION: &[u8] = b"1";
const SCHEMA_HISTORY_KEY: &[u8] = b"schema-history";
const SCHEMA_GENERATION_KEY: &[u8] = b"schema-generation";
#[cfg(feature = "graphql")]
const GRAPHQL_SDL_KEY: &[u8] = b"graphql-sdl";
const TS_CEILING_KEY: &[u8] = b"ts-ceiling";
const POSTING_CACHE_ENTRIES: usize = 64;
const TIMESTAMP_RESERVE: u64 = 1_000_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SchemaRecord {
    kind: String,
    text: String,
}

#[cfg(feature = "graphql")]
struct GraphQlRuntime {
    sdl: String,
    engine: Arc<GraphQlEngine>,
    generation: u64,
}

/// Result of polling a GraphQL subscription.
#[cfg(feature = "graphql")]
#[derive(Debug, Clone)]
pub enum SubscriptionEvent {
    /// A changed GraphQL response.
    Changed(serde_json::Value),
    /// No change since the previous response.
    Unchanged,
    /// The subscription ended (schema replacement or explicit close).
    Closed,
}

#[cfg(feature = "graphql")]
struct SubscriptionInner {
    engine: Arc<GraphQlEngine>,
    query: String,
    variables: serde_json::Value,
    headers: HashMap<String, String>,
    poll_interval: Duration,
    generation: u64,
    initial: serde_json::Value,
    pending_initial: AtomicBool,
    last_json: Mutex<String>,
    closed: AtomicBool,
}

/// One poll-based GraphQL subscription handle.
#[cfg(feature = "graphql")]
pub struct Subscription {
    id: u64,
    inner: Arc<SubscriptionInner>,
    engine: Weak<GraphEngine>,
}

#[cfg(feature = "graphql")]
#[derive(Default)]
struct SubscriptionRegistry {
    next_id: AtomicU64,
    items: Mutex<HashMap<u64, Arc<SubscriptionInner>>>,
}

#[cfg(feature = "graphql")]
impl SubscriptionRegistry {
    fn clear(&self) {
        self.items.lock().clear();
    }
}

/// One embedded graph engine over a shared backend.
pub(crate) struct GraphEngine {
    store: Arc<dyn Backend>,
    posting: Arc<PostingEngine>,
    query: Arc<QueryEngine>,
    history: RwLock<Vec<SchemaRecord>>,
    schema_generation: AtomicU64,
    ts_ceiling: AtomicU64,
    schema_gate: RwLock<()>,
    #[cfg(feature = "graphql")]
    graphql: RwLock<Option<Arc<GraphQlRuntime>>>,
    #[cfg(feature = "graphql")]
    graphql_generation: AtomicU64,
    #[cfg(feature = "graphql")]
    subscriptions: SubscriptionRegistry,
}

impl std::fmt::Debug for GraphEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GraphEngine")
            .field("backend", &self.store.name())
            .field("ts_ceiling", &self.ts_ceiling.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl GraphEngine {
    pub(crate) fn open(store: Arc<dyn Backend>) -> Result<Arc<Self>, CoreError> {
        store
            .put(META_PARTITION, b"meta-version", META_VERSION)
            .map_err(CoreError::Store)?;

        let schema = Arc::new(SchemaState::new());
        for update in initial_schema(0, false) {
            schema.set(&update.predicate.clone(), update);
        }
        for update in initial_types(0, false) {
            schema.set_type(&update.type_name.clone(), update);
        }

        let posting = PostingEngine::open(store.clone(), schema, POSTING_CACHE_ENTRIES);
        let query = QueryEngine::open(posting.clone()).map_err(query_err)?;

        #[allow(unused_mut)]
        let mut history = load_history(&store)?;
        let stored_generation = load_generation(&store)?;
        let generation = hash_history(&history);
        let rebuild = stored_generation != Some(generation);
        for record in &history {
            apply_schema_text(&posting, &record.text, rebuild)?;
        }
        if rebuild && !history.is_empty() {
            persist_history(&store, &history, generation)?;
        }

        // The oracle is in-memory; always take the higher of the persisted
        // reservation and the actual posting-chain maximum so a crash between
        // a mutation and its ceiling write cannot hide data on reopen.
        let ceiling = load_u64(&store, TS_CEILING_KEY)?
            .unwrap_or(0)
            .max(scan_max_ts(&store)?);
        posting.oracle().advance_to(ceiling);

        #[cfg(feature = "graphql")]
        let graphql = build_loaded_graphql(&store, &posting, &query, &mut history)?;
        #[cfg(feature = "graphql")]
        let generation = hash_history(&history);

        Ok(Arc::new(Self {
            store,
            posting,
            query,
            history: RwLock::new(history),
            schema_generation: AtomicU64::new(generation),
            ts_ceiling: AtomicU64::new(ceiling),
            schema_gate: RwLock::new(()),
            #[cfg(feature = "graphql")]
            graphql: RwLock::new(graphql),
            #[cfg(feature = "graphql")]
            graphql_generation: AtomicU64::new(1),
            #[cfg(feature = "graphql")]
            subscriptions: SubscriptionRegistry::default(),
        }))
    }

    pub(crate) fn dql_query_json(&self, text: &str) -> Result<String, CoreError> {
        let _gate = self.schema_gate.read();
        let bytes = self
            .query
            .query_json(0, text, self.posting.read_ts())
            .map_err(query_err)?;
        String::from_utf8(bytes).map_err(|err| CoreError::Query(err.to_string()))
    }

    pub(crate) fn dql_query(&self, text: &str) -> Result<serde_json::Value, CoreError> {
        let json = self.dql_query_json(text)?;
        serde_json::from_str(&json).map_err(|err| CoreError::Query(err.to_string()))
    }

    pub(crate) fn dql_mutate(&self, text: &str) -> Result<HashMap<String, u64>, CoreError> {
        let _gate = self.schema_gate.read();
        self.reserve_timestamp()?;
        self.query.mutate(0, text).map_err(query_err)
    }

    pub(crate) fn dql_mutate_json(&self, text: &str) -> Result<String, CoreError> {
        let assigned = self.dql_mutate(text)?;
        serde_json::to_string(&uids_to_hex(&assigned))
            .map_err(|err| CoreError::Query(err.to_string()))
    }

    pub(crate) fn dql_alter(&self, text: &str) -> Result<(), CoreError> {
        let _gate = self.schema_gate.write();
        self.reserve_timestamp()?;
        let parsed = parse_with_namespace(text, 0).map_err(schema_err)?;
        apply_parsed_schema(&self.posting, &parsed, true)?;
        self.append_history("dql", text)
    }

    #[cfg(feature = "graphql")]
    pub(crate) fn set_graphql_schema(&self, sdl: &str) -> Result<(), CoreError> {
        let _gate = self.schema_gate.write();
        self.reserve_timestamp()?;
        let handler = new_handler(sdl, false).map_err(gql_err)?;
        let dg_schema = handler.dg_schema().to_owned();
        let bound = BoundSchema::bind(&handler).map_err(gql_err)?;
        let engine = GraphQlEngine::new(bound, self.query.clone(), 0).map_err(gql_err)?;
        let parsed = parse_with_namespace(&dg_schema, 0).map_err(schema_err)?;
        apply_parsed_schema(&self.posting, &parsed, true)?;
        let generation = self.graphql_generation.fetch_add(1, Ordering::SeqCst) + 1;
        *self.graphql.write() = Some(Arc::new(GraphQlRuntime {
            sdl: sdl.to_owned(),
            engine: Arc::new(engine),
            generation,
        }));
        self.subscriptions.clear();
        self.append_history_with_sdl("graphql", &dg_schema, sdl)
    }

    #[cfg(feature = "graphql")]
    pub(crate) fn graphql_schema(&self) -> Option<String> {
        self.graphql
            .read()
            .as_ref()
            .map(|runtime| runtime.sdl.clone())
    }

    #[cfg(feature = "graphql")]
    pub(crate) fn graphql(
        &self,
        query: &str,
        operation: Option<&str>,
        variables: serde_json::Value,
        headers: &HashMap<String, String>,
    ) -> Result<serde_json::Value, CoreError> {
        let _gate = self.schema_gate.read();
        self.reserve_timestamp()?;
        let runtime = self.graphql_runtime()?;
        let response = futures::executor::block_on(
            runtime
                .engine
                .execute_named(query, operation, variables, headers),
        );
        response_value(&response)
    }

    #[cfg(feature = "graphql")]
    pub(crate) async fn graphql_async(
        &self,
        query: &str,
        operation: Option<&str>,
        variables: serde_json::Value,
        headers: &HashMap<String, String>,
    ) -> Result<serde_json::Value, CoreError> {
        self.reserve_timestamp()?;
        let runtime = {
            let _gate = self.schema_gate.read();
            self.graphql_runtime()?
        };
        let response = runtime
            .engine
            .execute_named(query, operation, variables, headers)
            .await;
        response_value(&response)
    }

    #[cfg(feature = "graphql")]
    pub(crate) fn graphql_subscribe(
        self: &Arc<Self>,
        query: &str,
        variables: serde_json::Value,
        headers: &HashMap<String, String>,
        poll_interval: Duration,
    ) -> Result<Subscription, CoreError> {
        let _gate = self.schema_gate.read();
        let runtime = self.graphql_runtime()?;
        let vars = json_vars(&variables);
        let operation = runtime
            .engine
            .shared()
            .bound
            .parse_operation(query, None, &vars)
            .map_err(gql_err)?;
        if operation.kind != OpKind::Subscription || operation.roots.len() != 1 {
            return Err(CoreError::GraphQl(
                "subscription must have exactly one root field".to_owned(),
            ));
        }
        let executable = rewrite_subscription_keyword(query);
        let response = futures::executor::block_on(runtime.engine.execute_named(
            &executable,
            None,
            variables.clone(),
            headers,
        ));
        if !response.errors.is_empty() {
            return Err(CoreError::GraphQl(
                response
                    .errors
                    .iter()
                    .map(|err| err.message.clone())
                    .collect::<Vec<_>>()
                    .join("; "),
            ));
        }
        let initial = response_value(&response)?;
        let last_json =
            serde_json::to_string(&response).map_err(|err| CoreError::GraphQl(err.to_string()))?;
        let id = self.subscriptions.next_id.fetch_add(1, Ordering::SeqCst) + 1;
        let inner = Arc::new(SubscriptionInner {
            engine: runtime.engine.clone(),
            query: executable,
            variables,
            headers: headers.clone(),
            poll_interval,
            generation: runtime.generation,
            initial: initial.clone(),
            pending_initial: AtomicBool::new(true),
            last_json: Mutex::new(last_json),
            closed: AtomicBool::new(false),
        });
        self.subscriptions.items.lock().insert(id, inner.clone());
        Ok(Subscription {
            id,
            inner,
            engine: Arc::downgrade(self),
        })
    }

    #[cfg(feature = "graphql")]
    pub(crate) fn graphql_next(
        self: &Arc<Self>,
        id: u64,
        wait: Duration,
    ) -> Result<SubscriptionEvent, CoreError> {
        let inner = self
            .subscriptions
            .items
            .lock()
            .get(&id)
            .cloned()
            .ok_or_else(|| CoreError::BadRequest(format!("unknown subscription {id}")))?;
        Subscription {
            id,
            inner,
            engine: Arc::downgrade(self),
        }
        .next_timeout(wait)
    }

    #[cfg(feature = "graphql")]
    pub(crate) fn graphql_unsubscribe(self: &Arc<Self>, id: u64) -> bool {
        let removed = self.subscriptions.items.lock().remove(&id);
        if let Some(inner) = removed {
            inner.closed.store(true, Ordering::SeqCst);
            true
        } else {
            false
        }
    }

    #[cfg(feature = "graphql")]
    fn graphql_runtime(&self) -> Result<Arc<GraphQlRuntime>, CoreError> {
        self.graphql
            .read()
            .clone()
            .ok_or_else(|| CoreError::GraphQl("GraphQL schema is not configured".to_owned()))
    }

    fn append_history(&self, kind: &str, text: &str) -> Result<(), CoreError> {
        let mut history = self.history.write();
        history.push(SchemaRecord {
            kind: kind.to_owned(),
            text: text.to_owned(),
        });
        let generation = hash_history(&history);
        persist_history(&self.store, &history, generation)?;
        self.schema_generation.store(generation, Ordering::Relaxed);
        Ok(())
    }

    #[cfg(feature = "graphql")]
    fn append_history_with_sdl(&self, kind: &str, text: &str, sdl: &str) -> Result<(), CoreError> {
        let mut history = self.history.write();
        history.push(SchemaRecord {
            kind: kind.to_owned(),
            text: text.to_owned(),
        });
        let generation = hash_history(&history);
        let json =
            serde_json::to_vec(&*history).map_err(|err| CoreError::Schema(err.to_string()))?;
        let mut batch = WriteBatch::new();
        batch.put(META_PARTITION, SCHEMA_HISTORY_KEY, &json);
        batch.put(
            META_PARTITION,
            SCHEMA_GENERATION_KEY,
            &generation.to_be_bytes(),
        );
        batch.put(META_PARTITION, GRAPHQL_SDL_KEY, sdl.as_bytes());
        self.store.write_batch(&batch).map_err(CoreError::Store)?;
        self.schema_generation.store(generation, Ordering::Relaxed);
        Ok(())
    }

    fn reserve_timestamp(&self) -> Result<(), CoreError> {
        let current = self.posting.oracle().max_assigned();
        let ceiling = self.ts_ceiling.load(Ordering::Relaxed);
        if current.saturating_add(1) >= ceiling {
            let next = current.saturating_add(TIMESTAMP_RESERVE);
            self.store
                .put(META_PARTITION, TS_CEILING_KEY, &next.to_be_bytes())
                .map_err(CoreError::Store)?;
            self.posting.oracle().advance_to(next);
            self.ts_ceiling.store(next, Ordering::Relaxed);
        }
        Ok(())
    }
}

#[cfg(feature = "graphql")]
impl Subscription {
    /// Subscription id used by the envelope/binding API.
    #[must_use]
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Initial response captured at subscribe time.
    #[must_use]
    pub fn initial(&self) -> serde_json::Value {
        self.inner.initial.clone()
    }

    /// Polls immediately for a changed response.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::GraphQl`] when re-execution fails.
    pub fn next(&self) -> Result<SubscriptionEvent, CoreError> {
        futures::executor::block_on(self.next_async())
    }

    /// Polls until a change, `timeout`, or closure.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::GraphQl`] when re-execution fails.
    pub fn next_timeout(&self, timeout: Duration) -> Result<SubscriptionEvent, CoreError> {
        let deadline = Instant::now() + timeout;
        loop {
            let event = self.next()?;
            if !matches!(event, SubscriptionEvent::Unchanged) || Instant::now() >= deadline {
                return Ok(event);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            let sleep = self.inner.poll_interval.min(remaining);
            if sleep.is_zero() {
                return Ok(SubscriptionEvent::Unchanged);
            }
            std::thread::sleep(sleep);
        }
    }

    /// Async poll for a changed response.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::GraphQl`] when re-execution fails.
    pub async fn next_async(&self) -> Result<SubscriptionEvent, CoreError> {
        let Some(engine) = self.engine.upgrade() else {
            return Ok(SubscriptionEvent::Closed);
        };
        if self.inner.closed.load(Ordering::SeqCst)
            || self.inner.generation != engine.graphql_generation.load(Ordering::SeqCst)
        {
            return Ok(SubscriptionEvent::Closed);
        }
        if self.inner.pending_initial.swap(false, Ordering::SeqCst) {
            return Ok(SubscriptionEvent::Changed(self.inner.initial.clone()));
        }
        let response = self
            .inner
            .engine
            .execute_named(
                &self.inner.query,
                None,
                self.inner.variables.clone(),
                &self.inner.headers,
            )
            .await;
        let json =
            serde_json::to_string(&response).map_err(|err| CoreError::GraphQl(err.to_string()))?;
        let mut last = self.inner.last_json.lock();
        if json == *last {
            return Ok(SubscriptionEvent::Unchanged);
        }
        *last = json;
        Ok(SubscriptionEvent::Changed(response_value(&response)?))
    }

    /// Async stream of changes, polling with the configured interval.
    pub fn stream(
        self,
    ) -> impl futures::Stream<Item = Result<SubscriptionEvent, CoreError>> + Send + 'static {
        futures::stream::unfold(self, |sub| async move {
            loop {
                match sub.next_async().await {
                    Ok(SubscriptionEvent::Closed) => return None,
                    Ok(SubscriptionEvent::Unchanged) => {
                        tokio::time::sleep(sub.inner.poll_interval).await;
                    }
                    Ok(event) => return Some((Ok(event), sub)),
                    Err(err) => return Some((Err(err), sub)),
                }
            }
        })
    }

    /// Returns the id for the id-based envelope/binding API.
    ///
    /// The subscription remains registered until [`Subscription::close`] or
    /// [`crate::Instance::graphql_unsubscribe`] is called.
    #[must_use]
    pub fn into_id(self) -> u64 {
        self.id
    }

    /// Closes this subscription and unregisters it.
    pub fn close(&self) {
        if !self.inner.closed.swap(true, Ordering::SeqCst) {
            if let Some(engine) = self.engine.upgrade() {
                engine.subscriptions.items.lock().remove(&self.id);
            }
        }
    }
}

#[cfg(feature = "graphql")]
fn build_loaded_graphql(
    store: &Arc<dyn Backend>,
    posting: &Arc<PostingEngine>,
    query: &Arc<QueryEngine>,
    history: &mut Vec<SchemaRecord>,
) -> Result<Option<Arc<GraphQlRuntime>>, CoreError> {
    let Some(sdl) = load_string(store, GRAPHQL_SDL_KEY)? else {
        return Ok(None);
    };
    let handler = new_handler(&sdl, false).map_err(gql_err)?;
    let dg_schema = handler.dg_schema().to_owned();
    if !history.iter().any(|record| record.kind == "graphql") {
        apply_schema_text(posting, &dg_schema, true)?;
        history.push(SchemaRecord {
            kind: "graphql".to_owned(),
            text: dg_schema,
        });
        let generation = hash_history(history);
        persist_history(store, history, generation)?;
    }
    let bound = BoundSchema::bind(&handler).map_err(gql_err)?;
    let engine = GraphQlEngine::new(bound, query.clone(), 0).map_err(gql_err)?;
    Ok(Some(Arc::new(GraphQlRuntime {
        sdl,
        engine: Arc::new(engine),
        generation: 1,
    })))
}

#[cfg(feature = "graphql")]
fn json_vars(value: &serde_json::Value) -> HashMap<String, serde_json::Value> {
    value
        .as_object()
        .map(|map| {
            map.iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(feature = "graphql")]
fn response_value<T: Serialize>(response: &T) -> Result<serde_json::Value, CoreError> {
    serde_json::to_value(response).map_err(|err| CoreError::GraphQl(err.to_string()))
}

fn apply_schema_text(
    posting: &Arc<PostingEngine>,
    text: &str,
    rebuild: bool,
) -> Result<(), CoreError> {
    let parsed = parse_with_namespace(text, 0).map_err(schema_err)?;
    apply_parsed_schema(posting, &parsed, rebuild)
}

fn apply_parsed_schema(
    posting: &Arc<PostingEngine>,
    parsed: &ParsedSchema,
    rebuild: bool,
) -> Result<(), CoreError> {
    let start_ts = posting.oracle().max_assigned();
    for update in &parsed.preds {
        let mut update = update.clone();
        if check_and_modify_pre_def_predicate(&mut update) {
            return Err(CoreError::Schema(format!(
                "cannot alter pre-defined predicate {}",
                update.predicate
            )));
        }
        let old = posting.schema().get(&update.predicate, false);
        posting
            .schema()
            .set(&update.predicate.clone(), update.clone());
        if rebuild && old.as_ref() != Some(&update) {
            IndexRebuild {
                attr: update.predicate.clone(),
                start_ts,
                old_schema: old,
                current_schema: update,
            }
            .rebuild(posting)
            .map_err(posting_err)?;
        }
    }
    for update in &parsed.types {
        if is_pre_def_type_changed(update) {
            return Err(CoreError::Schema(format!(
                "cannot alter pre-defined type {}",
                update.type_name
            )));
        }
        posting
            .schema()
            .set_type(&update.type_name.clone(), update.clone());
    }
    Ok(())
}

fn load_history(store: &Arc<dyn Backend>) -> Result<Vec<SchemaRecord>, CoreError> {
    let Some(bytes) = store
        .get(META_PARTITION, SCHEMA_HISTORY_KEY)
        .map_err(CoreError::Store)?
    else {
        return Ok(Vec::new());
    };
    serde_json::from_slice(&bytes).map_err(|err| CoreError::Schema(err.to_string()))
}

fn persist_history(
    store: &Arc<dyn Backend>,
    history: &[SchemaRecord],
    generation: u64,
) -> Result<(), CoreError> {
    let json = serde_json::to_vec(history).map_err(|err| CoreError::Schema(err.to_string()))?;
    let mut batch = WriteBatch::new();
    batch.put(META_PARTITION, SCHEMA_HISTORY_KEY, &json);
    batch.put(
        META_PARTITION,
        SCHEMA_GENERATION_KEY,
        &generation.to_be_bytes(),
    );
    store.write_batch(&batch).map_err(CoreError::Store)
}

fn load_generation(store: &Arc<dyn Backend>) -> Result<Option<u64>, CoreError> {
    load_u64(store, SCHEMA_GENERATION_KEY)
}

#[cfg(feature = "graphql")]
fn load_string(store: &Arc<dyn Backend>, key: &[u8]) -> Result<Option<String>, CoreError> {
    let Some(bytes) = store.get(META_PARTITION, key).map_err(CoreError::Store)? else {
        return Ok(None);
    };
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|err| CoreError::BadRequest(err.to_string()))
}

fn load_u64(store: &Arc<dyn Backend>, key: &[u8]) -> Result<Option<u64>, CoreError> {
    let Some(bytes) = store.get(META_PARTITION, key).map_err(CoreError::Store)? else {
        return Ok(None);
    };
    let array: [u8; 8] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| CoreError::BadRequest(format!("invalid meta value for {key:?}")))?;
    Ok(Some(u64::from_be_bytes(array)))
}

fn scan_max_ts(store: &Arc<dyn Backend>) -> Result<u64, CoreError> {
    let pairs = store
        .scan_prefix(PARTITION, b"")
        .map_err(CoreError::Store)?;
    let mut max_ts = 0;
    for (key, _) in pairs {
        if let Some(chain) = read_chain(store.as_ref(), &key).map_err(posting_err)? {
            max_ts = max_ts.max(chain.max_ts());
        }
    }
    Ok(max_ts)
}

fn hash_history(history: &[SchemaRecord]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for record in history {
        for byte in record.kind.as_bytes().iter().chain(record.text.as_bytes()) {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x100_0000_01b3);
        }
    }
    hash
}

#[allow(clippy::needless_pass_by_value, reason = "map_err adapter")]
fn query_err(err: datastreamdb_query::QueryError) -> CoreError {
    CoreError::Query(err.to_string())
}

#[allow(clippy::needless_pass_by_value, reason = "map_err adapter")]
fn schema_err(err: datastreamdb_schema::SchemaError) -> CoreError {
    CoreError::Schema(err.to_string())
}

#[allow(clippy::needless_pass_by_value, reason = "map_err adapter")]
fn posting_err(err: datastreamdb_posting::PostingError) -> CoreError {
    CoreError::Posting(err.to_string())
}

#[cfg(feature = "graphql")]
fn gql_err(err: impl std::fmt::Display) -> CoreError {
    CoreError::GraphQl(err.to_string())
}
