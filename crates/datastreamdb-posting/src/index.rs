//! Index maintenance on the write path. Port of `AddMutationWithIndex`
//! and its helpers from `posting/index.go`: token-index, reverse, and count
//! edges plus the drop flows.
//!
//! Vector (`factorySpecs`) branches are deferred to the vector milestone and
//! report [`PostingError::Unsupported`]. Schema rebuild (`IndexRebuild`)
//! follows in `rebuild.rs`.

#![warn(missing_docs)]

use std::sync::Arc;

use datastreamdb_proto::{DirectedEdge, EdgeOp, Posting, ValType};
use datastreamdb_schema::SchemaState;
use datastreamdb_store::WriteBatch;
use datastreamdb_tok::tok::Tokenizer;
use datastreamdb_types::{TypeId, Val, Value};

use crate::engine::PostingEngine;
use crate::errors::{PostingError, Result};
use crate::iter::get_posting_and_length_no_sort_inner;
use crate::list::{List, OP_DEL, STAR_ALL, fingerprint_edge, new_posting};
use crate::read::value_to_types_val;
use crate::txn::Txn;

/// Token-index mutation request for one value edge.
pub struct IndexMutationInfo {
    /// Tokenizers (defaults from schema when empty).
    pub tokenizers: Vec<Arc<dyn Tokenizer>>,
    /// Original UID-to-value edge.
    pub edge: DirectedEdge,
    /// Value to tokenize.
    pub val: Val,
    /// Index operation.
    pub op: EdgeOp,
}

/// Count-index update from `countBefore` to `countAfter`.
#[derive(Clone, Debug, Default)]
pub struct CountParams {
    /// Attribute.
    pub attr: String,
    /// Count before the mutation.
    pub count_before: i64,
    /// Count after the mutation.
    pub count_after: i64,
    /// Entity UID.
    pub entity: u64,
    /// Reverse count index.
    pub reverse: bool,
}

/// Typed edge operation.
fn edge_op(edge: &DirectedEdge) -> Result<EdgeOp> {
    EdgeOp::try_from(edge.op).map_err(|_| PostingError::Corrupt(format!("bad edge op {}", edge.op)))
}

/// Applies one edge to a list with abort/conflict handling.
pub(crate) fn txn_add_mutation(txn: &Txn, list: &List, edge: &mut DirectedEdge) -> Result<()> {
    if txn.should_abort() {
        return Err(PostingError::Conflict);
    }
    let schema = txn.engine().schema().clone();
    let conflict = list.add_mutation(txn.start_ts(), edge, &schema)?;
    txn.add_conflict_key(conflict);
    Ok(())
}

/// Tokenizes one value for the schema tokenizers (Go `indexTokens`).
fn index_tokens(
    schema: &SchemaState,
    attr: &str,
    lang: &str,
    val: &Val,
    tokenizers: &[Arc<dyn Tokenizer>],
) -> Result<Vec<Vec<u8>>> {
    let schema_type = schema.type_of(attr).map_err(|_| {
        PostingError::Corrupt(format!("cannot index attribute {attr} of type object"))
    })?;
    if !schema_type.is_scalar() {
        return Err(PostingError::Corrupt(format!(
            "cannot index attribute {attr} of type object"
        )));
    }
    if !schema.is_indexed(attr, false) {
        return Err(PostingError::Corrupt(format!(
            "attribute {attr} is not indexed"
        )));
    }
    let converted = datastreamdb_types::convert(val, schema_type)?;
    let mut tokens = Vec::new();
    for tokenizer in tokenizers {
        let lang_tok = datastreamdb_tok::text::tokenizer_for_lang(tokenizer.name(), lang)
            .unwrap_or_else(|| tokenizer.clone());
        tokens.extend(datastreamdb_tok::tok::build_tokens(
            &converted,
            lang_tok.as_ref(),
        )?);
    }
    Ok(tokens)
}

/// Whether a count edge needs writing (rewrites `SET` to `OVR` on hits).
fn should_add_count_edge(found: bool, edge: &mut DirectedEdge) -> bool {
    if found {
        if edge_op(edge).is_ok_and(|op| op != EdgeOp::Del) {
            edge.op = EdgeOp::Ovr as i32;
        }
        return true;
    }
    edge_op(edge).is_ok_and(|op| op != EdgeOp::Del)
}

/// Count after a mutation (Go `countAfterMutation`).
fn count_after_mutation(count_before: i64, found: bool, op: EdgeOp, one_uid: bool) -> i64 {
    if one_uid {
        if op == EdgeOp::Set {
            return 1;
        }
        if op == EdgeOp::Del && found {
            return 0;
        }
        return count_before;
    }
    if !found && op != EdgeOp::Del {
        return count_before + 1;
    }
    if found && op == EdgeOp::Del {
        return count_before - 1;
    }
    count_before
}

impl Txn {
    /// Maintains token-index edges for one value (scalar path only).
    ///
    /// # Errors
    ///
    /// Returns [`PostingError`] on storage/schema/tokenizer failure, or
    /// [`PostingError::Unsupported`] for vector-indexed predicates.
    pub fn add_index_mutations(&self, info: &mut IndexMutationInfo) -> Result<()> {
        let schema = self.engine().schema().clone();
        if info.tokenizers.is_empty() {
            info.tokenizers = schema.tokenizers(&info.edge.attr, false);
        }
        if !schema.vector_indexes(&info.edge.attr, false).is_empty() {
            return Err(PostingError::Unsupported(
                "vector indexes (vector milestone)".to_owned(),
            ));
        }

        let attr = info.edge.attr.clone();
        let uid = info.edge.entity;
        if uid == 0 {
            return Err(PostingError::Corrupt("invalid UID with value 0".to_owned()));
        }

        let tokens = index_tokens(&schema, &attr, &info.edge.lang, &info.val, &info.tokenizers)?;
        let mut edge = DirectedEdge {
            value_id: uid,
            attr,
            op: info.op as i32,
            ..DirectedEdge::default()
        };
        for token in &tokens {
            self.add_index_mutation(&mut edge, token)?;
        }
        Ok(())
    }

    /// Adds one token-to-UID index edge.
    ///
    /// # Errors
    ///
    /// Returns [`PostingError`] on storage failure.
    pub fn add_index_mutation(&self, edge: &mut DirectedEdge, token: &[u8]) -> Result<()> {
        let key = datastreamdb_x::index_key(&edge.attr, token);
        let plist = self.cache().get_from_delta(&key)?;
        txn_add_mutation(self, &plist, edge)
    }

    /// Adds a reverse edge, maintaining counts when indexed.
    ///
    /// # Errors
    ///
    /// Returns [`PostingError`] on storage failure.
    pub fn add_reverse_mutation_helper(
        &self,
        plist: &List,
        has_count_index: bool,
        edge: &mut DirectedEdge,
    ) -> Result<CountParams> {
        if self.should_abort() {
            return Err(PostingError::Conflict);
        }
        let schema = self.engine().schema().clone();
        let mut inner = plist.inner.write();
        let mut count_before = 0i64;
        let mut found = false;
        let mut curr_post: Option<Posting> = None;
        if has_count_index {
            let (count, was_found, post) = get_posting_and_length_no_sort_inner(
                &inner,
                plist.store.as_ref(),
                &schema,
                self.start_ts(),
                0,
                edge.value_id,
            )?;
            count_before = count;
            found = was_found;
            curr_post = post;
        }

        if !has_count_index
            || should_add_count_edge(
                found && curr_post.as_ref().is_some_and(|p| p.op != OP_DEL),
                edge,
            )
        {
            let conflict = List::add_mutation_inner(&mut inner, self.start_ts(), edge, &schema)?;
            self.add_conflict_key(conflict);
        }

        if has_count_index {
            let pk = datastreamdb_x::parse(&inner.key)?;
            let one_uid = !schema.is_list(&edge.attr) && !pk.is_reverse();
            let count_after = count_after_mutation(count_before, found, edge_op(edge)?, one_uid);
            return Ok(CountParams {
                attr: edge.attr.clone(),
                count_before,
                count_after,
                entity: edge.entity,
                reverse: true,
            });
        }
        Ok(CountParams::default())
    }

    /// Adds a bare reverse edge (rebuild path, no counts).
    ///
    /// # Errors
    ///
    /// Returns [`PostingError`] on storage failure.
    pub fn add_reverse_mutation(&self, edge: &DirectedEdge) -> Result<()> {
        let key = datastreamdb_x::reverse_key(&edge.attr, edge.value_id);
        let plist = self.get_from_delta(&key)?;
        let mut swapped = DirectedEdge {
            entity: edge.value_id,
            value_id: edge.entity,
            attr: edge.attr.clone(),
            op: edge.op,
            facets: edge.facets.clone(),
            ..DirectedEdge::default()
        };
        txn_add_mutation(self, &plist, &mut swapped)
    }

    /// Adds a reverse edge plus count maintenance, fanning out single-UID
    /// replacements first.
    ///
    /// # Errors
    ///
    /// Returns [`PostingError`] on storage failure.
    pub fn add_reverse_and_count_mutation(&self, edge: &DirectedEdge) -> Result<()> {
        let schema = self.engine().schema().clone();
        let key = datastreamdb_x::reverse_key(&edge.attr, edge.value_id);
        let has_count_index = schema.has_count(&edge.attr, false);
        let plist = if has_count_index {
            self.get(&key)?
        } else {
            self.get_from_delta(&key)?
        };

        let pred = schema.get(&edge.attr, false);
        let op = edge_op(edge)?;
        let is_single_uid_update = pred.as_ref().is_some_and(|pred| {
            !pred.list
                && pred.value_type == ValType::Uid as i32
                && op != EdgeOp::Del
                && edge.value_id != 0
        });
        if is_single_uid_update {
            let data_key = datastreamdb_x::data_key(&edge.attr, edge.entity);
            let data_list = if has_count_index {
                self.get(&data_key)?
            } else {
                self.get_from_delta(&data_key)?
            };
            let mut result = Ok(());
            data_list.iterate(self.start_ts(), 0, &mut |p| {
                let del_edge = DirectedEdge {
                    entity: edge.entity,
                    value_id: p.uid,
                    attr: edge.attr.clone(),
                    op: EdgeOp::Del as i32,
                    ..DirectedEdge::default()
                };
                if let Err(err) = self.add_reverse_and_count_mutation(&del_edge) {
                    result = Err(err);
                    return Err(PostingError::StopIteration);
                }
                Ok(())
            })?;
            result?;
        }

        let mut swapped = DirectedEdge {
            entity: edge.value_id,
            value_id: edge.entity,
            attr: edge.attr.clone(),
            op: edge.op,
            facets: edge.facets.clone(),
            ..DirectedEdge::default()
        };
        let cp = self.add_reverse_mutation_helper(&plist, has_count_index, &mut swapped)?;
        if has_count_index && cp.count_after != cp.count_before {
            self.update_count(&cp)?;
        }
        Ok(())
    }

    /// Adds one count-bucket edge.
    ///
    /// # Errors
    ///
    /// Returns [`PostingError`] on storage failure.
    pub fn add_count_mutation(
        &self,
        edge: &mut DirectedEdge,
        count: u32,
        reverse: bool,
    ) -> Result<()> {
        let key = datastreamdb_x::count_key(&edge.attr, count, reverse);
        let plist = self.cache().get_from_delta(&key)?;
        txn_add_mutation(self, &plist, edge)
    }

    /// Moves an entity between count buckets.
    ///
    /// # Errors
    ///
    /// Returns [`PostingError`] on storage failure.
    pub fn update_count(&self, params: &CountParams) -> Result<()> {
        let mut edge = DirectedEdge {
            value_id: params.entity,
            attr: params.attr.clone(),
            op: EdgeOp::Del as i32,
            ..DirectedEdge::default()
        };
        if params.count_before > 0 {
            let count = u32::try_from(params.count_before).unwrap_or(u32::MAX);
            self.add_count_mutation(&mut edge, count, params.reverse)?;
        }
        if params.count_after > 0 {
            edge.op = EdgeOp::Set as i32;
            let count = u32::try_from(params.count_after).unwrap_or(u32::MAX);
            self.add_count_mutation(&mut edge, count, params.reverse)?;
        }
        Ok(())
    }

    /// Core mutation helper: resolves the old posting, applies the edge,
    /// and reports the count delta.
    ///
    /// # Errors
    ///
    /// Returns [`PostingError`] on storage/schema failure.
    #[allow(clippy::too_many_lines)]
    pub fn add_mutation_helper(
        &self,
        list: &List,
        do_update_index: bool,
        has_count_index: bool,
        edge: &mut DirectedEdge,
    ) -> Result<(Val, bool, CountParams)> {
        if self.should_abort() {
            return Err(PostingError::Conflict);
        }
        let schema = self.engine().schema().clone();
        let mut inner = list.inner.write();

        let uid = if edge.value_type == ValType::Uid as i32 {
            edge.value_id
        } else {
            fingerprint_edge(edge, schema.is_list(&edge.attr))
        };

        let is_scalar = !schema.is_list(&edge.attr);
        let op = edge_op(edge)?;
        let del_non_list =
            is_scalar && op == EdgeOp::Del && edge.value.as_slice() != STAR_ALL.as_bytes();

        let mut count_before = 0i64;
        let mut curr_post: Option<Posting> = None;
        let mut found = false;
        if has_count_index {
            let (count, was_found, post) = get_posting_and_length_no_sort_inner(
                &inner,
                list.store.as_ref(),
                &schema,
                self.start_ts(),
                0,
                uid,
            )?;
            count_before = count;
            found = was_found;
            curr_post = post;
        } else if do_update_index || del_non_list {
            let fp = fingerprint_edge(edge, schema.is_list(&edge.attr));
            let (was_found, post) =
                crate::read::find_posting(&inner, list.store.as_ref(), self.start_ts(), fp)?;
            found = was_found;
            curr_post = post;
        }

        if del_non_list {
            let new_post = new_posting(edge)?;
            let matches = found
                && curr_post.as_ref().is_some_and(|curr| {
                    curr.value == new_post.value && curr.val_type == new_post.val_type
                });
            if !matches {
                return Ok((empty_val(), false, CountParams::default()));
            }
        }

        if !has_count_index
            || should_add_count_edge(
                found && curr_post.as_ref().is_some_and(|p| p.op != OP_DEL),
                edge,
            )
        {
            let conflict = List::add_mutation_inner(&mut inner, self.start_ts(), edge, &schema)?;
            self.add_conflict_key(conflict);
        }

        let mut val = empty_val();
        if found && do_update_index {
            if let Some(curr) = &curr_post {
                val = value_to_types_val(curr);
            }
        }

        if has_count_index {
            let pk = datastreamdb_x::parse(&inner.key)?;
            let one_uid = is_scalar && !pk.is_reverse();
            let count_after = count_after_mutation(count_before, found, edge_op(edge)?, one_uid);
            return Ok((
                val,
                found,
                CountParams {
                    attr: edge.attr.clone(),
                    count_before,
                    count_after,
                    entity: edge.entity,
                    reverse: false,
                },
            ));
        }
        Ok((val, found, CountParams::default()))
    }
}

impl List {
    /// Adds an edge plus reverse/count/index maintenance.
    ///
    /// # Errors
    ///
    /// Returns [`PostingError`] on storage/schema/tokenizer failure.
    pub fn add_mutation_with_index(&self, edge: &mut DirectedEdge, txn: &Txn) -> Result<()> {
        if edge.attr.is_empty() {
            return Err(PostingError::Corrupt(format!(
                "predicate cannot be empty for edge with subject: [{}], object: [{}]",
                edge.entity, edge.value_id
            )));
        }
        if edge_op(edge)? == EdgeOp::Del && edge.value.as_slice() == STAR_ALL.as_bytes() {
            return self.handle_delete_all(edge, txn);
        }

        let schema = txn.engine().schema().clone();
        let do_update_index = schema.is_indexed(&edge.attr, false);
        let has_count_index = schema.has_count(&edge.attr, false);

        if edge.value_id != 0 && schema.is_reversed(&edge.attr, false) {
            txn.add_reverse_and_count_mutation(edge)?;
        }

        let (mut val, found, cp) =
            txn.add_mutation_helper(self, do_update_index, has_count_index, edge)?;
        if has_count_index && cp.count_after != cp.count_before {
            txn.update_count(&cp)?;
        }
        if do_update_index {
            if found {
                let mut info = IndexMutationInfo {
                    tokenizers: schema.tokenizers(&edge.attr, false),
                    edge: edge.clone(),
                    val,
                    op: EdgeOp::Del,
                };
                txn.add_index_mutations(&mut info)?;
            }
            if edge_op(edge)? != EdgeOp::Del {
                val = Val {
                    tid: TypeId::try_from(edge.value_type).unwrap_or(TypeId::Default),
                    value: Value::Binary(edge.value.clone()),
                };
                let mut info = IndexMutationInfo {
                    tokenizers: schema.tokenizers(&edge.attr, false),
                    edge: edge.clone(),
                    val,
                    op: EdgeOp::Set,
                };
                txn.add_index_mutations(&mut info)?;
            }
        }
        Ok(())
    }

    /// Deletes every posting plus reverse/index/count maintenance.
    ///
    /// # Errors
    ///
    /// Returns [`PostingError`] on storage/schema/tokenizer failure.
    fn handle_delete_all(&self, edge: &mut DirectedEdge, txn: &Txn) -> Result<()> {
        let schema = txn.engine().schema().clone();
        let is_reversed = schema.is_reversed(&edge.attr, false);
        let is_indexed = schema.is_indexed(&edge.attr, false);
        let has_count = schema.has_count(&edge.attr, false);
        let mut plen = 0i64;
        let mut result = Ok(());
        self.iterate(txn.start_ts(), 0, &mut |p| {
            plen += 1;
            if is_reversed {
                let del_edge = DirectedEdge {
                    entity: edge.entity,
                    value_id: p.uid,
                    attr: edge.attr.clone(),
                    op: edge.op,
                    ..DirectedEdge::default()
                };
                if let Err(err) = txn.add_reverse_and_count_mutation(&del_edge) {
                    result = Err(err);
                    return Err(PostingError::StopIteration);
                }
            } else if is_indexed {
                let val = Val {
                    tid: TypeId::try_from(p.val_type).unwrap_or(TypeId::Default),
                    value: Value::Binary(p.value.clone()),
                };
                let mut info = IndexMutationInfo {
                    tokenizers: schema.tokenizers(&edge.attr, false),
                    edge: edge.clone(),
                    val,
                    op: EdgeOp::Del,
                };
                if let Err(err) = txn.add_index_mutations(&mut info) {
                    result = Err(err);
                    return Err(PostingError::StopIteration);
                }
            }
            Ok(())
        })?;
        result?;
        if has_count {
            txn.update_count(&CountParams {
                attr: edge.attr.clone(),
                count_before: plen,
                count_after: 0,
                entity: edge.entity,
                reverse: false,
            })?;
        }
        txn_add_mutation(txn, self, edge)
    }
}

/// Empty scalar value.
fn empty_val() -> Val {
    Val {
        tid: TypeId::Default,
        value: Value::Binary(Vec::new()),
    }
}

/// Deletes every key under each prefix (drop flows).
pub(crate) fn delete_prefixes(engine: &PostingEngine, prefixes: &[Vec<u8>]) -> Result<()> {
    use crate::store::PARTITION;
    let mut batch = WriteBatch::new();
    let mut keys = Vec::new();
    for prefix in prefixes {
        for (key, _) in engine.store().scan_prefix(PARTITION, prefix)? {
            keys.push(key);
        }
    }
    for key in &keys {
        batch.delete(PARTITION, key);
        engine.memory().del(key);
    }
    engine.store().write_batch(&batch)?;
    Ok(())
}

impl PostingEngine {
    /// Drops every posting and clears the cache.
    ///
    /// # Errors
    ///
    /// Returns [`PostingError`] on storage failure.
    pub fn delete_all(&self) -> Result<()> {
        self.memory().clear();
        delete_prefixes(self, &[Vec::new()])
    }

    /// Drops every posting of a namespace (schema and types intact).
    ///
    /// # Errors
    ///
    /// Returns [`PostingError`] on storage failure.
    pub fn delete_data(&self, ns: u64) -> Result<()> {
        self.memory().clear();
        delete_prefixes(self, &[datastreamdb_x::data_prefix(ns)])
    }

    /// Drops one predicate (schema, postings, and indexes).
    ///
    /// # Errors
    ///
    /// Returns [`PostingError`] on storage failure.
    pub fn delete_predicate(&self, attr: &str) -> Result<()> {
        for pred in self.schema().predicates_to_delete(attr) {
            self.schema().delete(&pred);
            delete_prefixes(self, &[datastreamdb_x::predicate_prefix(&pred)])?;
        }
        Ok(())
    }

    /// Drops a namespace (postings plus namespace schema). Delta from Go:
    /// Badger's namespace ban has no embedded equivalent; callers must not
    /// reuse the namespace afterwards.
    ///
    /// # Errors
    ///
    /// Returns [`PostingError`] on storage failure.
    pub fn delete_namespace(&self, ns: u64) -> Result<()> {
        self.memory().clear();
        self.schema().delete_preds_for_ns(ns);
        delete_prefixes(self, &[datastreamdb_x::data_prefix(ns)])
    }
}
