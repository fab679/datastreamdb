//! Vector-index surface: factories, indexes, filters. Port of `tok/index/*`,
//! `tok/index_factory.go`, `tok/constraints`, and the index helpers.
//!
//! M2 ships the traits, options plumbing, and an in-memory stub factory; the
//! persistent HNSW engine lands in the vector milestone (see `TODO.md`).

#![warn(missing_docs)]

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use super::options::{AllowedOptions, Options};
use super::tok::{IDENT_VFLOAT, TokError, Tokenizer};

// --- constraints (port of tok/constraints) ---

/// Signed integer constraint.
pub trait Signed: Copy {}
/// Unsigned integer constraint.
pub trait Unsigned: Copy {}
/// Integer constraint.
pub trait Integer: Copy {}
/// Float constraint (`f32`/`f64`).
pub trait Float: Copy + PartialOrd {}

macro_rules! impl_constraints {
    (signed: $($t:ty),*; unsigned: $($u:ty),*; float: $($f:ty),*) => {
        $(impl Signed for $t {})*
        $(impl Unsigned for $u {})*
        $(impl Integer for $t {})*
        $(impl Integer for $u {})*
        $(impl Float for $f {})*
    };
}

impl_constraints!(signed: i8, i16, i32, i64, isize; unsigned: u8, u16, u32, u64, usize; float: f32, f64);

/// Decodes LE bytes into floats, ignoring a trailing partial word.
/// Safe port of `BytesAsFloatArray` (Go aliases memory unsafely).
#[must_use]
pub fn bytes_as_floats(bytes: &[u8], float_bits: usize) -> Vec<f64> {
    match float_bits {
        32 => bytes
            .chunks_exact(4)
            .map(|w| f64::from(f32::from_le_bytes([w[0], w[1], w[2], w[3]])))
            .collect(),
        _ => bytes
            .chunks_exact(8)
            .map(|w| f64::from_le_bytes([w[0], w[1], w[2], w[3], w[4], w[5], w[6], w[7]]))
            .collect(),
    }
}

/// Decodes one LE float. Port of `BytesToFloat` (panics on bad width, like Go).
///
/// # Panics
///
/// Panics on unknown widths or short input (mirrors Go).
#[must_use]
pub fn bytes_to_float(bytes: &[u8], float_bits: usize) -> f64 {
    match float_bits {
        32 => f64::from(f32::from_le_bytes(bytes[..4].try_into().expect("4 bytes"))),
        64 => f64::from_le_bytes(bytes[..8].try_into().expect("8 bytes")),
        _ => panic!("Invalid floatBits"),
    }
}

// --- index types (port of tok/index) ---

/// Key/value mutation record (mirrors `index.KeyValue`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyValue {
    /// Entity UID.
    pub entity: u64,
    /// Attribute name.
    pub attr: String,
    /// Value bytes.
    pub value: Vec<u8>,
}

/// Filter keeping search results (`true` keeps). Port of `SearchFilter`.
pub type SearchFilter<T> = Arc<dyn Fn(&[T], &[T], u64) -> bool + Send + Sync>;

/// Accepts every result. Port of `AcceptAll`.
#[must_use]
pub fn accept_all<T>() -> SearchFilter<T> {
    Arc::new(|_, _, _| true)
}

/// Rejects every result. Port of `AcceptNone`.
#[must_use]
pub fn accept_none<T>() -> SearchFilter<T> {
    Arc::new(|_, _, _| false)
}

/// Extended search result. Port of `SearchPathResult`.
#[derive(Clone, Debug, Default)]
pub struct SearchPathResult {
    /// Nearest neighbors in sorted order (post-filter).
    pub neighbors: Vec<u64>,
    /// Path from search start to the closest neighbor.
    pub path: Vec<u64>,
    /// Named counters captured during search.
    pub metrics: HashMap<String, u64>,
}

/// Per-call search tuning. Port of `VectorIndexOptions`.
#[derive(Clone, Default)]
pub struct VectorIndexOptions<T> {
    /// Search-breadth override (`> 0` applies).
    pub ef_override: usize,
    /// Metric-domain distance ceiling.
    pub distance_threshold: Option<f64>,
    /// Result filter (defaults to accept-all).
    pub filter: Option<SearchFilter<T>>,
}

/// Storage transaction surface. Port of `index.Txn`.
pub trait Txn: Send + Sync {
    /// Start timestamp.
    fn start_ts(&self) -> u64;
    /// Point read.
    ///
    /// # Errors
    ///
    /// Returns [`IndexError`] on backend failure.
    fn get(&self, key: &[u8]) -> Result<Vec<u8>, IndexError>;
    /// Point read with the key lock held.
    ///
    /// # Errors
    ///
    /// Returns [`IndexError`] on backend failure.
    fn get_with_lock_held(&self, key: &[u8]) -> Result<Vec<u8>, IndexError>;
    /// Scans `prefix`, returning the match count.
    ///
    /// # Errors
    ///
    /// Returns [`IndexError`] on backend failure.
    fn find(&self, prefix: &[u8], filter: &dyn Fn(&[u8]) -> bool) -> Result<u64, IndexError>;
    /// Records a mutation.
    ///
    /// # Errors
    ///
    /// Returns [`IndexError`] on backend failure.
    fn add_mutation(&self, key: &[u8], edge: &KeyValue) -> Result<(), IndexError>;
    /// Records a mutation with the key lock held.
    ///
    /// # Errors
    ///
    /// Returns [`IndexError`] on backend failure.
    fn add_mutation_with_lock_held(&self, key: &[u8], edge: &KeyValue) -> Result<(), IndexError>;
    /// Locks a key.
    fn lock_key(&self, key: &[u8]);
    /// Unlocks a key.
    fn unlock_key(&self, key: &[u8]);
}

/// Read cache surface. Port of `index.CacheType`.
pub trait CacheType: Send + Sync {
    /// Point read.
    ///
    /// # Errors
    ///
    /// Returns [`IndexError`] on backend failure.
    fn get(&self, key: &[u8]) -> Result<Vec<u8>, IndexError>;
    /// Read timestamp.
    fn ts(&self) -> u64;
    /// Scans `prefix`, returning the match count.
    ///
    /// # Errors
    ///
    /// Returns [`IndexError`] on backend failure.
    fn find(&self, prefix: &[u8], filter: &dyn Fn(&[u8]) -> bool) -> Result<u64, IndexError>;
}

/// Index error.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum IndexError {
    /// Operation unsupported by this index.
    #[error("unsupported index operation: {0}")]
    Unsupported(String),
    /// Index or key missing.
    #[error("index not found: {0}")]
    NotFound(String),
    /// Duplicate entry.
    #[error("duplicate entry: {0}")]
    Duplicate(String),
    /// Backend failure.
    #[error("index backend: {0}")]
    Backend(String),
}

/// Vector index. Port of `index.VectorIndex` (+ optional search paths).
/// Lifetimes elided: implementations own their state.
pub trait VectorIndex<T: Float>: Send + Sync {
    /// Nearest UIDs for a query vector.
    ///
    /// # Errors
    ///
    /// Returns [`IndexError`] on backend failure.
    fn search(
        &self,
        query: &[T],
        max_results: usize,
        filter: &SearchFilter<T>,
    ) -> Result<Vec<u64>, IndexError>;
    /// Nearest UIDs for a stored query vector.
    ///
    /// # Errors
    ///
    /// Returns [`IndexError`] on backend failure.
    fn search_with_uid(
        &self,
        query_uid: u64,
        max_results: usize,
        filter: &SearchFilter<T>,
    ) -> Result<Vec<u64>, IndexError>;
    /// Nearest UIDs with per-call options.
    ///
    /// # Errors
    ///
    /// Returns [`IndexError`] on backend failure.
    fn search_with_options(
        &self,
        query: &[T],
        max_results: usize,
        opts: &VectorIndexOptions<T>,
    ) -> Result<Vec<u64>, IndexError>;
    /// Inserts a vector (errors on duplicate UIDs).
    ///
    /// # Errors
    ///
    /// Returns [`IndexError`] on duplicates or backend failure.
    fn insert(&self, uuid: u64, vec: Vec<T>) -> Result<Vec<KeyValue>, IndexError>;
    /// Search with path + metrics (may be unsupported).
    ///
    /// # Errors
    ///
    /// Returns [`IndexError`] when unsupported or on backend failure.
    fn search_with_path(
        &self,
        query: &[T],
        max_results: usize,
        filter: &SearchFilter<T>,
    ) -> Result<SearchPathResult, IndexError>;
}

/// Index factory. Port of `index.IndexFactory`.
pub trait IndexFactoryBase<T: Float>: Send + Sync {
    /// Factory name.
    fn name(&self) -> &'static str;
    /// Renders active options.
    fn options_string(&self, opts: &Options) -> String;
    /// Allowed options + parsers.
    fn allowed_options(&self) -> AllowedOptions;
    /// Creates an index (errors when the name exists).
    ///
    /// # Errors
    ///
    /// Returns [`IndexError`] when the name exists or on backend failure.
    fn create(
        &self,
        name: &str,
        opts: &Options,
        float_bits: usize,
    ) -> Result<Arc<dyn VectorIndex<T>>, IndexError>;
    /// Finds an index by name.
    ///
    /// # Errors
    ///
    /// Returns [`IndexError`] on backend failure.
    fn find(&self, name: &str) -> Result<Option<Arc<dyn VectorIndex<T>>>, IndexError>;
    /// Removes an index (missing names are a no-op).
    ///
    /// # Errors
    ///
    /// Returns [`IndexError`] on backend failure.
    fn remove(&self, name: &str) -> Result<(), IndexError>;
    /// Creates or replaces an index.
    ///
    /// # Errors
    ///
    /// Returns [`IndexError`] on backend failure.
    fn create_or_replace(
        &self,
        name: &str,
        opts: &Options,
        float_bits: usize,
    ) -> Result<Arc<dyn VectorIndex<T>>, IndexError>;
}

/// Tokenizer + factory bundle. Port of `tok.IndexFactory`.
pub trait IndexFactory: Tokenizer {
    /// Factory name.
    fn factory_name(&self) -> &'static str;
    /// Renders active options.
    fn options_string(&self, opts: &Options) -> String;
    /// Allowed options + parsers.
    fn allowed_options(&self) -> AllowedOptions;
    /// Creates an `f32` index.
    ///
    /// # Errors
    ///
    /// Returns [`IndexError`] when the name exists or on backend failure.
    fn create(
        &self,
        name: &str,
        opts: &Options,
        float_bits: usize,
    ) -> Result<Arc<dyn VectorIndex<f32>>, IndexError>;
    /// Finds an `f32` index.
    ///
    /// # Errors
    ///
    /// Returns [`IndexError`] on backend failure.
    fn find(&self, name: &str) -> Result<Option<Arc<dyn VectorIndex<f32>>>, IndexError>;
    /// Removes an index.
    ///
    /// # Errors
    ///
    /// Returns [`IndexError`] on backend failure.
    fn remove(&self, name: &str) -> Result<(), IndexError>;
    /// Creates or replaces an index.
    ///
    /// # Errors
    ///
    /// Returns [`IndexError`] on backend failure.
    fn create_or_replace(
        &self,
        name: &str,
        opts: &Options,
        float_bits: usize,
    ) -> Result<Arc<dyn VectorIndex<f32>>, IndexError>;
}

/// Everything needed to create an index. Port of `FactoryCreateSpec`.
pub struct FactoryCreateSpec {
    factory: Arc<dyn IndexFactory>,
    opts: Options,
}

impl FactoryCreateSpec {
    /// Display name (factory + options rendering).
    #[must_use]
    pub fn name(&self) -> String {
        format!(
            "{}{}",
            self.factory.factory_name(),
            self.factory.options_string(&self.opts)
        )
    }

    /// Creates (or replaces) the index.
    ///
    /// # Errors
    ///
    /// Returns [`IndexError`] on backend failure.
    pub fn create_index(&self, name: &str) -> Result<Arc<dyn VectorIndex<f32>>, IndexError> {
        self.factory.create_or_replace(name, &self.opts, 32)
    }
}

type FactoryRegistry = HashMap<String, Arc<dyn IndexFactory>>;

fn factories() -> &'static RwLock<FactoryRegistry> {
    static FACTORIES: std::sync::OnceLock<RwLock<FactoryRegistry>> = std::sync::OnceLock::new();
    FACTORIES.get_or_init(|| RwLock::new(HashMap::from([stub_factory_entry()])))
}

fn stub_factory_entry() -> (String, Arc<dyn IndexFactory>) {
    ("hnsw".to_owned(), Arc::new(HnswStubFactory))
}

/// Registers a factory (also usable as the `vfloat` tokenizer).
///
/// # Panics
///
/// Panics if the factory registry lock is poisoned.
pub fn register_index_factory(factory: Arc<dyn IndexFactory>) {
    factories()
        .write()
        .expect("factories")
        .insert(factory.factory_name().to_owned(), factory);
}

/// Looks up a factory by name. Port of `GetIndexFactory`.
///
/// # Panics
///
/// Panics if the factory registry lock is poisoned.
#[must_use]
pub fn get_index_factory(name: &str) -> Option<Arc<dyn IndexFactory>> {
    factories().read().expect("factories").get(name).cloned()
}

/// Builds a create-spec from `(name, options)` pairs. Port of
/// `GetFactoryCreateSpecFromSpec` (over plain pairs instead of protobuf).
///
/// # Errors
///
/// Returns [`IndexError`] for unknown factories or bad options.
pub fn factory_create_spec(
    name: &str,
    pairs: &[(&str, &str)],
) -> Result<FactoryCreateSpec, IndexError> {
    let factory =
        get_index_factory(name).ok_or_else(|| IndexError::NotFound(format!("factory {name}")))?;
    let mut opts = Options::new();
    factory
        .allowed_options()
        .populate(pairs, &mut opts)
        .map_err(|e| IndexError::Backend(e.to_string()))?;
    Ok(FactoryCreateSpec { factory, opts })
}

/// Suffix of the vector-entry sidecar predicate (port of `hnsw.VecEntry`).
pub const VEC_ENTRY: &str = "__vector_entry";
/// Infix of the vector-keyword sidecar predicate (port of `hnsw.VecKeyword`).
pub const VEC_KEYWORD: &str = "__vector_";
/// Suffix of the vector-dead sidecar predicate (port of `hnsw.VecDead`).
pub const VEC_DEAD: &str = "__vector_dead";

/// In-memory stub factory standing in for persistent HNSW until M5.
/// Accepts the documented HNSW options; indexes reject writes.
#[derive(Debug, Clone, Copy)]
pub struct HnswStubFactory;

impl Tokenizer for HnswStubFactory {
    fn name(&self) -> &'static str {
        "hnsw"
    }
    fn tokenizer_type(&self) -> &'static str {
        "float32vector"
    }
    fn tokens(&self, _value: &datastreamdb_types::Val) -> Result<Vec<Vec<u8>>, TokError> {
        Ok(vec![b"float".to_vec()])
    }
    fn identifier(&self) -> u8 {
        IDENT_VFLOAT
    }
    fn is_sortable(&self) -> bool {
        false
    }
    fn is_lossy(&self) -> bool {
        true
    }
}

impl IndexFactory for HnswStubFactory {
    fn factory_name(&self) -> &'static str {
        "hnsw"
    }
    fn options_string(&self, _opts: &Options) -> String {
        String::new()
    }
    fn allowed_options(&self) -> AllowedOptions {
        AllowedOptions::new()
            .add_int_option("maxLayers")
            .add_int_option("efConstruction")
            .add_int_option("maxElements")
            .add_string_option("metric")
    }
    fn create(
        &self,
        name: &str,
        _opts: &Options,
        _float_bits: usize,
    ) -> Result<Arc<dyn VectorIndex<f32>>, IndexError> {
        Err(IndexError::Unsupported(format!(
            "hnsw index {name} (vector milestone)"
        )))
    }
    fn find(&self, name: &str) -> Result<Option<Arc<dyn VectorIndex<f32>>>, IndexError> {
        Err(IndexError::Unsupported(format!(
            "hnsw index {name} (vector milestone)"
        )))
    }
    fn remove(&self, name: &str) -> Result<(), IndexError> {
        Err(IndexError::Unsupported(format!(
            "hnsw index {name} (vector milestone)"
        )))
    }
    fn create_or_replace(
        &self,
        name: &str,
        _opts: &Options,
        _float_bits: usize,
    ) -> Result<Arc<dyn VectorIndex<f32>>, IndexError> {
        Err(IndexError::Unsupported(format!(
            "hnsw index {name} (vector milestone)"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn factory_registry_flows() {
        let factory = get_index_factory("hnsw").expect("hnsw stub");
        assert_eq!(factory.factory_name(), "hnsw");
        assert!(
            factory
                .allowed_options()
                .option_list()
                .contains(&"metric".to_owned())
        );
        let spec = factory_create_spec("hnsw", &[("metric", "cosine")]).expect("spec");
        assert!(spec.name().starts_with("hnsw"));
        assert!(spec.create_index("v").is_err());
        assert!(get_index_factory("nope").is_none());
        assert!(factory_create_spec("nope", &[]).is_err());
        assert!(factory_create_spec("hnsw", &[("nope", "1")]).is_err());
    }

    #[test]
    #[allow(clippy::float_cmp, reason = "exact bit-preserving roundtrips")]
    fn float_bytes_helpers() {
        assert_eq!(bytes_as_floats(&1.5f32.to_le_bytes(), 32), [1.5]);
        assert_eq!(bytes_as_floats(&[1, 2, 3], 32), Vec::<f64>::new());
        assert_eq!(bytes_to_float(&2.5f64.to_le_bytes(), 64), 2.5);
    }
}
