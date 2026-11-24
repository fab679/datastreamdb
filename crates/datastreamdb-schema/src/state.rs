//! Live schema state. Port of the in-memory parts of Go's `schema.go`.
//!
//! Disk sync (`Init`, `Load`, `Delete` with timestamps) waits for the store
//! milestones (see `TODO.md`).

#![warn(missing_docs)]

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};

use datastreamdb_proto::{Directive, SchemaUpdate, TypeUpdate, ValType};
use datastreamdb_tok::index::{
    FactoryCreateSpec, VEC_DEAD, VEC_ENTRY, VEC_KEYWORD, factory_create_spec,
};
use datastreamdb_tok::tok::{Tokenizer, get_tokenizer};
use datastreamdb_types::TypeId;

use super::parse::parse;
use crate::SchemaError;

/// Whether `datastreamdb.xid` is unique (mirrors Go's `IsUniqueDgraphXid`).
pub const IS_UNIQUE_DATASTREAMDB_XID: bool = true;

/// Live schema maps (one lock, like Go's single `RWMutex`).
#[derive(Debug, Default)]
struct Inner {
    /// Predicate to type information.
    predicate: HashMap<String, SchemaUpdate>,
    /// Type name to definition.
    types: HashMap<String, TypeUpdate>,
    /// Schema updates being applied in the background.
    mut_schema: HashMap<String, SchemaUpdate>,
}

/// Live schema state. An instance (not a global) so many isolated schemas
/// can live in one process.
#[derive(Debug, Default)]
pub struct SchemaState {
    inner: RwLock<Inner>,
}

impl SchemaState {
    /// Empty schema state.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, Inner> {
        self.inner.read().expect("schema lock")
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, Inner> {
        self.inner.write().expect("schema lock")
    }

    /// Clears all predicates, types, and background updates.
    pub fn delete_all(&self) {
        let mut inner = self.write();
        inner.predicate.clear();
        inner.types.clear();
        inner.mut_schema.clear();
    }

    /// Parses schema text and installs every predicate (test/bootstrap path).
    ///
    /// # Errors
    ///
    /// Returns [`SchemaError`] on malformed schema.
    pub fn parse_bytes(&self, bytes: &[u8]) -> Result<(), SchemaError> {
        let text = std::str::from_utf8(bytes).map_err(|e| SchemaError::message(e.to_string()))?;
        self.delete_all();
        let result = parse(text)?;
        for update in &result.preds {
            self.set(&update.predicate.clone(), update.clone());
        }
        Ok(())
    }

    /// Sets the in-memory schema for a predicate.
    pub fn set(&self, pred: &str, schema: SchemaUpdate) {
        self.write().predicate.insert(pred.to_owned(), schema);
        tracing::debug!("{}", log_update(&self.read().predicate[pred], pred));
    }

    /// Sets the background-mutation schema for a predicate.
    pub fn set_mut_schema(&self, pred: &str, schema: SchemaUpdate) {
        self.write().mut_schema.insert(pred.to_owned(), schema);
    }

    /// Deletes the background-mutation schema for a predicate.
    pub fn delete_mut_schema(&self, pred: &str) {
        self.write().mut_schema.remove(pred);
    }

    /// Deletes a predicate's schema (drop-predicate flows).
    pub fn delete(&self, pred: &str) {
        let mut inner = self.write();
        inner.predicate.remove(pred);
        inner.mut_schema.remove(pred);
    }

    /// Predicates with indexes currently building.
    #[must_use]
    pub fn indexing_predicates(&self) -> Vec<String> {
        self.read().mut_schema.keys().cloned().collect()
    }

    /// Sets the type definition for a type name.
    pub fn set_type(&self, name: &str, typ: TypeUpdate) {
        self.write().types.insert(name.to_owned(), typ);
        tracing::debug!("{}", log_type_update(&self.read().types[name], name));
    }

    /// Deletes a type definition (drop-type flows).
    pub fn delete_type(&self, name: &str) {
        self.write().types.remove(name);
    }

    /// Gets the schema for a predicate (`write` selects the mutation schema).
    #[must_use]
    pub fn get(&self, pred: &str, write: bool) -> Option<SchemaUpdate> {
        let inner = self.read();
        if write {
            if let Some(schema) = inner.mut_schema.get(pred) {
                return Some(schema.clone());
            }
        }
        inner.predicate.get(pred).cloned()
    }

    /// Gets the type definition for a type name.
    #[must_use]
    pub fn get_type(&self, name: &str) -> Option<TypeUpdate> {
        self.read().types.get(name).cloned()
    }

    /// Returns the schema type of a predicate.
    ///
    /// # Errors
    ///
    /// Returns [`SchemaError`] when the predicate has no schema.
    pub fn type_of(&self, pred: &str) -> Result<TypeId, SchemaError> {
        let inner = self.read();
        if let Some(schema) = inner.predicate.get(pred) {
            if let Ok(t) = TypeId::try_from(schema.value_type) {
                return Ok(t);
            }
        }
        Err(SchemaError::message(format!(
            "Schema not defined for predicate: {pred}."
        )))
    }

    /// Whether the predicate is indexed.
    #[must_use]
    pub fn is_indexed(&self, pred: &str, write: bool) -> bool {
        let inner = self.read();
        if write {
            // TODO(Aman): we could return the query schema if it is a delete.
            if let Some(schema) = inner.mut_schema.get(pred) {
                if !schema.tokenizer.is_empty() || !schema.index_specs.is_empty() {
                    return true;
                }
            }
        }
        inner
            .predicate
            .get(pred)
            .is_some_and(|schema| !schema.tokenizer.is_empty() || !schema.index_specs.is_empty())
    }

    /// All predicate names.
    #[must_use]
    pub fn predicates(&self) -> Vec<String> {
        self.read().predicate.keys().cloned().collect()
    }

    /// All type names.
    #[must_use]
    pub fn types(&self) -> Vec<String> {
        self.read().types.keys().cloned().collect()
    }

    /// Tokenizers for a predicate (empty when the schema is missing).
    ///
    /// # Panics
    ///
    /// Panics when a stored tokenizer name is unregistered (Go asserts the
    /// same; names are validated at parse).
    #[must_use]
    pub fn tokenizers(&self, pred: &str, write: bool) -> Vec<Arc<dyn Tokenizer>> {
        let inner = self.read();
        let mut found: Option<&SchemaUpdate> = None;
        if write {
            found = inner.mut_schema.get(pred);
        }
        if found.is_none() {
            found = inner.predicate.get(pred);
        }
        let Some(schema) = found else {
            tracing::error!("Schema state not found for {pred}.");
            return Vec::new();
        };
        schema
            .tokenizer
            .iter()
            .map(|name| get_tokenizer(name).expect("tokenizer validated at parse"))
            .collect()
    }

    /// Index-factory create specs for a predicate.
    ///
    /// # Errors
    ///
    /// Returns [`SchemaError`] when the schema is missing or a spec is bad.
    pub fn factory_create_specs(
        &self,
        pred: &str,
        write: bool,
    ) -> Result<Vec<FactoryCreateSpec>, SchemaError> {
        let inner = self.read();
        let mut found: Option<&SchemaUpdate> = None;
        if write {
            found = inner.mut_schema.get(pred);
        }
        if found.is_none() {
            found = inner.predicate.get(pred);
        }
        let Some(schema) = found else {
            tracing::error!("Schema state not found for {pred}.");
            return Err(SchemaError::message(format!(
                "Schema state not found for {pred}."
            )));
        };
        schema
            .index_specs
            .iter()
            .map(|vs| {
                let pairs: Vec<(&str, &str)> = vs
                    .options
                    .iter()
                    .map(|pair| (pair.key.as_str(), pair.value.as_str()))
                    .collect();
                factory_create_spec(&vs.name, &pairs)
                    .map_err(|e| SchemaError::message(e.to_string()))
            })
            .collect()
    }

    /// Vector index names for a predicate.
    #[must_use]
    pub fn vector_indexes(&self, pred: &str, write: bool) -> Vec<String> {
        match self.factory_create_specs(pred, write) {
            Ok(specs) => specs.iter().map(FactoryCreateSpec::name).collect(),
            Err(err) => {
                tracing::error!("Vector Tokenizers error for {pred}: {err}");
                Vec::new()
            }
        }
    }

    /// Tokenizer names for a predicate.
    #[must_use]
    pub fn tokenizer_names(&self, pred: &str, write: bool) -> Vec<String> {
        self.tokenizers(pred, write)
            .iter()
            .map(|t| t.name().to_owned())
            .collect()
    }

    /// Whether the predicate uses the tokenizer id.
    #[must_use]
    pub fn has_tokenizer(&self, id: u8, pred: &str, write: bool) -> bool {
        self.tokenizers(pred, write)
            .iter()
            .any(|t| t.identifier() == id)
    }

    /// Whether the predicate has a reverse edge.
    #[must_use]
    pub fn is_reversed(&self, pred: &str, write: bool) -> bool {
        let inner = self.read();
        if write {
            if let Some(schema) = inner.mut_schema.get(pred) {
                if schema.directive == Directive::Reverse as i32 {
                    return true;
                }
            }
        }
        inner
            .predicate
            .get(pred)
            .is_some_and(|schema| schema.directive == Directive::Reverse as i32)
    }

    /// Whether a count index is maintained for the predicate.
    #[must_use]
    pub fn has_count(&self, pred: &str, write: bool) -> bool {
        let inner = self.read();
        if write {
            if let Some(schema) = inner.mut_schema.get(pred) {
                if schema.count {
                    return true;
                }
            }
        }
        inner.predicate.get(pred).is_some_and(|schema| schema.count)
    }

    /// Predicates to delete for a drop (vector sidecars included).
    #[must_use]
    pub fn predicates_to_delete(&self, pred: &str) -> Vec<String> {
        let inner = self.read();
        let Some(schema) = inner.predicate.get(pred) else {
            return Vec::new();
        };
        if schema.value_type == ValType::Vfloat as i32 && !schema.index_specs.is_empty() {
            return vec![
                pred.to_owned(),
                format!("{pred}{VEC_ENTRY}"),
                format!("{pred}{VEC_KEYWORD}"),
                format!("{pred}{VEC_DEAD}"),
            ];
        }
        vec![pred.to_owned()]
    }

    /// Whether the predicate is a list.
    #[must_use]
    pub fn is_list(&self, pred: &str) -> bool {
        self.read()
            .predicate
            .get(pred)
            .is_some_and(|schema| schema.list)
    }

    /// Whether the predicate supports upserts.
    #[must_use]
    pub fn has_upsert(&self, pred: &str) -> bool {
        self.read()
            .predicate
            .get(pred)
            .is_some_and(|schema| schema.upsert)
    }

    /// Whether the predicate is language-tagged.
    #[must_use]
    pub fn has_lang(&self, pred: &str) -> bool {
        self.read()
            .predicate
            .get(pred)
            .is_some_and(|schema| schema.lang)
    }

    /// Whether the predicate skips conflict detection.
    #[must_use]
    pub fn has_no_conflict(&self, pred: &str) -> bool {
        self.read()
            .predicate
            .get(pred)
            .is_some_and(|schema| schema.no_conflict)
    }

    /// Whether any background indexing is in progress.
    #[must_use]
    pub fn indexing_in_progress(&self) -> bool {
        !self.read().mut_schema.is_empty()
    }

    /// Active namespaces from the current types.
    #[must_use]
    pub fn namespaces(&self) -> HashSet<u64> {
        self.read()
            .types
            .keys()
            .map(|typ| datastreamdb_x::parse_namespace(typ))
            .collect()
    }

    /// Deletes predicate and type info for a namespace.
    pub fn delete_preds_for_ns(&self, del_ns: u64) {
        let mut inner = self.write();
        let doomed: Vec<String> = inner
            .predicate
            .keys()
            .filter(|pred| datastreamdb_x::parse_namespace(pred) == del_ns)
            .cloned()
            .collect();
        for pred in &doomed {
            inner.predicate.remove(pred);
            inner.mut_schema.remove(pred);
        }
        inner
            .types
            .retain(|typ, _| datastreamdb_x::parse_namespace(typ) != del_ns);
    }
}

fn log_update(schema: &SchemaUpdate, pred: &str) -> String {
    let typ = TypeId::try_from(schema.value_type)
        .map(|t| t.to_string())
        .unwrap_or_default();
    let typ = if schema.list { format!("[{typ}]") } else { typ };
    format!(
        "Setting schema for attr {pred}: {typ}, tokenizer: {:?}, directive: {}, count: {}",
        schema.tokenizer, schema.directive, schema.count
    )
}

fn log_type_update(typ: &TypeUpdate, name: &str) -> String {
    format!("Setting type definition for type {name}: {typ:?}")
}

/// Schema updates inserted at startup for `namespace`.
#[must_use]
pub fn initial_schema(namespace: u64, acl_enabled: bool) -> Vec<SchemaUpdate> {
    initial_schema_internal(namespace, false, acl_enabled)
}

/// All schema updates regardless of options (bulk loading).
#[must_use]
pub fn complete_initial_schema(namespace: u64) -> Vec<SchemaUpdate> {
    initial_schema_internal(namespace, true, false)
}

fn initial_schema_internal(namespace: u64, all: bool, acl_enabled: bool) -> Vec<SchemaUpdate> {
    let mut schema = vec![
        SchemaUpdate {
            predicate: "datastreamdb.type".into(),
            value_type: ValType::String as i32,
            directive: Directive::Index as i32,
            tokenizer: vec!["exact".into()],
            list: true,
            ..SchemaUpdate::default()
        },
        SchemaUpdate {
            predicate: "datastreamdb.drop.op".into(),
            value_type: ValType::String as i32,
            ..SchemaUpdate::default()
        },
        SchemaUpdate {
            predicate: "datastreamdb.graphql.schema".into(),
            value_type: ValType::String as i32,
            ..SchemaUpdate::default()
        },
        SchemaUpdate {
            predicate: "datastreamdb.graphql.xid".into(),
            value_type: ValType::String as i32,
            directive: Directive::Index as i32,
            tokenizer: vec!["exact".into()],
            upsert: true,
            ..SchemaUpdate::default()
        },
        SchemaUpdate {
            predicate: "datastreamdb.graphql.p_query".into(),
            value_type: ValType::String as i32,
            directive: Directive::Index as i32,
            tokenizer: vec!["sha256".into()],
            ..SchemaUpdate::default()
        },
    ];
    if namespace == datastreamdb_x::ROOT_NAMESPACE {
        schema.extend([
            SchemaUpdate {
                predicate: "datastreamdb.namespace.name".into(),
                value_type: ValType::String as i32,
                directive: Directive::Index as i32,
                tokenizer: vec!["exact".into()],
                unique: true,
                upsert: true,
                ..SchemaUpdate::default()
            },
            SchemaUpdate {
                predicate: "datastreamdb.namespace.id".into(),
                value_type: ValType::Int as i32,
                directive: Directive::Index as i32,
                tokenizer: vec!["int".into()],
                unique: true,
                upsert: true,
                ..SchemaUpdate::default()
            },
        ]);
    }
    if all || acl_enabled {
        schema.extend(acl_schema());
    }
    for update in &mut schema {
        update.predicate = datastreamdb_x::namespace_attr(namespace, &update.predicate.clone());
    }
    schema
}

/// Type updates inserted at startup for `namespace`.
#[must_use]
pub fn initial_types(namespace: u64, acl_enabled: bool) -> Vec<TypeUpdate> {
    initial_types_internal(namespace, false, acl_enabled)
}

/// All type updates regardless of options.
#[must_use]
pub fn complete_initial_types(namespace: u64) -> Vec<TypeUpdate> {
    initial_types_internal(namespace, true, false)
}

/// Schema updates for the ACL predicates.
fn acl_schema() -> Vec<SchemaUpdate> {
    vec![
        SchemaUpdate {
            predicate: "datastreamdb.xid".into(),
            value_type: ValType::String as i32,
            directive: Directive::Index as i32,
            tokenizer: vec!["exact".into()],
            upsert: true,
            unique: IS_UNIQUE_DATASTREAMDB_XID,
            ..SchemaUpdate::default()
        },
        SchemaUpdate {
            predicate: "datastreamdb.password".into(),
            value_type: ValType::Password as i32,
            ..SchemaUpdate::default()
        },
        SchemaUpdate {
            predicate: "datastreamdb.user.group".into(),
            value_type: ValType::Uid as i32,
            directive: Directive::Reverse as i32,
            list: true,
            ..SchemaUpdate::default()
        },
        SchemaUpdate {
            predicate: "datastreamdb.acl.rule".into(),
            value_type: ValType::Uid as i32,
            list: true,
            ..SchemaUpdate::default()
        },
        SchemaUpdate {
            predicate: "datastreamdb.rule.predicate".into(),
            value_type: ValType::String as i32,
            directive: Directive::Index as i32,
            tokenizer: vec!["exact".into()],
            upsert: true,
            ..SchemaUpdate::default()
        },
        SchemaUpdate {
            predicate: "datastreamdb.rule.permission".into(),
            value_type: ValType::Int as i32,
            ..SchemaUpdate::default()
        },
    ]
}

fn field(predicate: &str, value_type: ValType) -> SchemaUpdate {
    SchemaUpdate {
        predicate: predicate.into(),
        value_type: value_type as i32,
        ..SchemaUpdate::default()
    }
}

// NOTE: new types here must also land in x's pre-defined type map.
fn initial_types_internal(namespace: u64, all: bool, acl_enabled: bool) -> Vec<TypeUpdate> {
    let mut types = vec![
        TypeUpdate {
            type_name: "datastreamdb.graphql".into(),
            fields: vec![
                field("datastreamdb.graphql.schema", ValType::String),
                field("datastreamdb.graphql.xid", ValType::String),
            ],
        },
        TypeUpdate {
            type_name: "datastreamdb.graphql.persisted_query".into(),
            fields: vec![field("datastreamdb.graphql.p_query", ValType::String)],
        },
    ];
    if namespace == datastreamdb_x::ROOT_NAMESPACE {
        types.push(TypeUpdate {
            type_name: "datastreamdb.namespace".into(),
            fields: vec![
                field("datastreamdb.namespace.name", ValType::String),
                field("datastreamdb.namespace.id", ValType::Int),
            ],
        });
    }
    if all || acl_enabled {
        types.extend([
            TypeUpdate {
                type_name: "datastreamdb.type.User".into(),
                fields: vec![
                    field("datastreamdb.xid", ValType::String),
                    field("datastreamdb.password", ValType::Password),
                    field("datastreamdb.user.group", ValType::Uid),
                ],
            },
            TypeUpdate {
                type_name: "datastreamdb.type.Group".into(),
                fields: vec![
                    field("datastreamdb.xid", ValType::String),
                    field("datastreamdb.acl.rule", ValType::Uid),
                ],
            },
            TypeUpdate {
                type_name: "datastreamdb.type.Rule".into(),
                fields: vec![
                    field("datastreamdb.rule.predicate", ValType::String),
                    field("datastreamdb.rule.permission", ValType::Int),
                ],
            },
        ]);
    }
    for typ in &mut types {
        typ.type_name = datastreamdb_x::namespace_attr(namespace, &typ.type_name.clone());
        for f in &mut typ.fields {
            f.predicate = datastreamdb_x::namespace_attr(namespace, &f.predicate.clone());
        }
    }
    types
}

/// Whether the pre-defined predicate update differs from the initial one.
/// May set `Unique` on `datastreamdb.xid` upgrades. `false` for non-pre-defined.
#[must_use]
pub fn check_and_modify_pre_def_predicate(update: &mut SchemaUpdate) -> bool {
    if !datastreamdb_x::is_pre_defined_predicate(&update.predicate) {
        return false;
    }
    let ns = datastreamdb_x::parse_namespace(&update.predicate);
    for original in complete_initial_schema(ns) {
        if original.predicate != update.predicate {
            continue;
        }
        // Only Unique may change on datastreamdb.xid; upgrade older clusters.
        if update.predicate == datastreamdb_x::namespace_attr(ns, "datastreamdb.xid")
            && !update.unique
            && is_datastreamdb_xid_change_valid(&original, update)
        {
            update.unique = true;
            return false;
        }
        return original != *update;
    }
    true
}

/// Whether the only change is the `Unique` flag.
fn is_datastreamdb_xid_change_valid(original: &SchemaUpdate, update: &SchemaUpdate) -> bool {
    let changed = compare_schema_updates(original, update);
    changed.len() == 1 && changed[0] == "Unique"
}

/// Field names (Go order) differing between two updates.
fn compare_schema_updates(original: &SchemaUpdate, update: &SchemaUpdate) -> Vec<&'static str> {
    let mut changed = Vec::new();
    if original.predicate != update.predicate {
        changed.push("Predicate");
    }
    if original.value_type != update.value_type {
        changed.push("ValueType");
    }
    if original.directive != update.directive {
        changed.push("Directive");
    }
    if original.tokenizer != update.tokenizer {
        changed.push("Tokenizer");
    }
    if original.count != update.count {
        changed.push("Count");
    }
    if original.list != update.list {
        changed.push("List");
    }
    if original.upsert != update.upsert {
        changed.push("Upsert");
    }
    if original.lang != update.lang {
        changed.push("Lang");
    }
    if original.non_nullable != update.non_nullable {
        changed.push("NonNullable");
    }
    if original.non_nullable_list != update.non_nullable_list {
        changed.push("NonNullableList");
    }
    if original.object_type_name != update.object_type_name {
        changed.push("ObjectTypeName");
    }
    if original.no_conflict != update.no_conflict {
        changed.push("NoConflict");
    }
    if original.unique != update.unique {
        changed.push("Unique");
    }
    if original.index_specs != update.index_specs {
        changed.push("IndexSpecs");
    }
    changed
}

/// Whether the pre-defined type update differs from the initial one.
/// `false` for non-pre-defined types.
#[must_use]
pub fn is_pre_def_type_changed(update: &TypeUpdate) -> bool {
    if !datastreamdb_x::is_pre_defined_type(&update.type_name) {
        return false;
    }
    for original in complete_initial_types(datastreamdb_x::parse_namespace(&update.type_name)) {
        if original.type_name != update.type_name {
            continue;
        }
        if original.fields.len() != update.fields.len() {
            return true;
        }
        for (field, other) in original.fields.iter().zip(update.fields.iter()) {
            if field.predicate != other.predicate {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Port of Go's `TestCompareSchemaUpdates` (in-module, like Go).
    #[test]
    fn compare_updates_table() {
        let base = SchemaUpdate {
            predicate: "name".into(),
            value_type: ValType::String as i32,
            count: true,
            ..SchemaUpdate::default()
        };
        // No changes.
        assert!(compare_schema_updates(&base, &base).is_empty());
        // Predicate changed.
        let mut update = base.clone();
        update.predicate = "age".into();
        assert_eq!(compare_schema_updates(&base, &update), ["Predicate"]);
        // Multiple fields changed.
        update.count = false;
        assert_eq!(
            compare_schema_updates(&base, &update),
            ["Predicate", "Count"]
        );
        // Unchanged and changed fields.
        let base = SchemaUpdate {
            predicate: "name".into(),
            count: true,
            ..SchemaUpdate::default()
        };
        let mut update = base.clone();
        update.upsert = true;
        assert_eq!(compare_schema_updates(&base, &update), ["Upsert"]);
    }
}
