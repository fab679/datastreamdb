//! Schema-change index rebuilds. Port of `IndexRebuild` from
//! `posting/index.go` (tokenizer diffing, prefix drops, data/index/cross
//! rebuilds).
//!
//! Delta from Go: the two-phase temp-Badger `rebuilder.Run` becomes a
//! single scan-and-commit loop over the store (no temp database, no stream
//! framework). Vector specs are rejected with
//! [`PostingError::Unsupported`]. Callers install the new schema before
//! [`IndexRebuild::build_indexes`] and the interim query schema from
//! [`IndexRebuild::query_schema`] while a rebuild runs.

#![warn(missing_docs)]

use datastreamdb_proto::{DirectedEdge, EdgeOp, SchemaUpdate};
use datastreamdb_tok::tok::get_tokenizer;
use datastreamdb_types::{TypeId, Val, Value};

use crate::engine::PostingEngine;
use crate::errors::{PostingError, Result};
use crate::index::IndexMutationInfo;
use crate::mem::IterateDiskArgs;

/// Index maintenance decision for one schema transition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum IndexOp {
    /// Leave the index alone.
    Noop,
    /// Delete the index.
    Delete,
    /// Delete and rebuild the index.
    Rebuild,
}

/// Tokenizer diff for one rebuild.
#[derive(Clone, Debug)]
struct TokenizerDiff {
    /// Decision.
    op: IndexOp,
    /// Tokenizers whose entries drop.
    to_delete: Vec<String>,
    /// Tokenizers to rebuild.
    to_rebuild: Vec<String>,
}

/// Schema transition requiring index maintenance.
#[derive(Clone, Debug)]
pub struct IndexRebuild {
    /// Namespaced attribute.
    pub attr: String,
    /// Rebuild read timestamp.
    pub start_ts: u64,
    /// Previous schema (`None` for new predicates).
    pub old_schema: Option<SchemaUpdate>,
    /// New schema.
    pub current_schema: SchemaUpdate,
}

impl IndexRebuild {
    /// Rejects vector specs (deferred milestone).
    fn check_no_vectors(&self) -> Result<()> {
        let old_vectors = self
            .old_schema
            .as_ref()
            .is_some_and(|schema| !schema.index_specs.is_empty());
        if old_vectors || !self.current_schema.index_specs.is_empty() {
            return Err(PostingError::Unsupported(
                "vector index rebuild (vector milestone)".to_owned(),
            ));
        }
        Ok(())
    }

    /// Tokenizer diff between the old and new schema.
    fn tok_diff(&self) -> TokenizerDiff {
        let old = self.old_schema.clone().unwrap_or_default();
        let curr_indexed =
            self.current_schema.directive == datastreamdb_proto::Directive::Index as i32;
        let prev_indexed = old.directive == datastreamdb_proto::Directive::Index as i32;
        if !curr_indexed && !prev_indexed {
            return TokenizerDiff {
                op: IndexOp::Noop,
                to_delete: Vec::new(),
                to_rebuild: Vec::new(),
            };
        }
        if !curr_indexed {
            return TokenizerDiff {
                op: IndexOp::Delete,
                to_delete: old.tokenizer,
                to_rebuild: Vec::new(),
            };
        }
        if self.current_schema.value_type != old.value_type {
            return TokenizerDiff {
                op: IndexOp::Rebuild,
                to_delete: old.tokenizer.clone(),
                to_rebuild: self.current_schema.tokenizer.clone(),
            };
        }
        let mut to_delete = Vec::new();
        let mut to_rebuild = Vec::new();
        for tok in &old.tokenizer {
            if !self.current_schema.tokenizer.contains(tok) {
                to_delete.push(tok.clone());
            }
        }
        for tok in &self.current_schema.tokenizer {
            if !old.tokenizer.contains(tok) {
                to_rebuild.push(tok.clone());
            }
        }
        if to_delete.is_empty() && to_rebuild.is_empty() {
            return TokenizerDiff {
                op: IndexOp::Noop,
                to_delete,
                to_rebuild,
            };
        }
        TokenizerDiff {
            op: IndexOp::Rebuild,
            to_delete,
            to_rebuild,
        }
    }

    /// Count-index decision.
    fn count_op(&self) -> IndexOp {
        let old = self.old_schema.clone().unwrap_or_default();
        if self.current_schema.count == old.count {
            return IndexOp::Noop;
        }
        if self.current_schema.count {
            IndexOp::Rebuild
        } else {
            IndexOp::Delete
        }
    }

    /// Reverse-edge decision.
    fn reverse_op(&self) -> IndexOp {
        let old = self.old_schema.clone().unwrap_or_default();
        let curr = self.current_schema.directive == datastreamdb_proto::Directive::Reverse as i32;
        let prev = old.directive == datastreamdb_proto::Directive::Reverse as i32;
        if curr == prev {
            return IndexOp::Noop;
        }
        if curr {
            IndexOp::Rebuild
        } else {
            IndexOp::Delete
        }
    }

    /// Whether any tokenizer, reverse, or count index rebuilds.
    #[must_use]
    pub fn need_index_rebuild(&self) -> bool {
        self.tok_diff().op == IndexOp::Rebuild
            || self.reverse_op() == IndexOp::Rebuild
            || self.count_op() == IndexOp::Rebuild
    }

    /// Schema servable while the rebuild runs (deleting tokenizers minus
    /// the ones being rebuilt, no rebuilding count/reverse).
    #[must_use]
    pub fn query_schema(&self) -> SchemaUpdate {
        let diff = self.tok_diff();
        let old = self.old_schema.clone().unwrap_or_default();
        let mut query = self.current_schema.clone();
        query.tokenizer = old
            .tokenizer
            .iter()
            .filter(|tok| !diff.to_delete.contains(tok))
            .cloned()
            .collect();
        if self.count_op() == IndexOp::Rebuild {
            query.count = false;
        }
        if self.reverse_op() == IndexOp::Rebuild {
            query.directive = datastreamdb_proto::Directive::None as i32;
        }
        query
    }

    /// Whether scalar-to-list data rewrites apply (errors on list-to-scalar).
    ///
    /// # Errors
    ///
    /// Returns [`PostingError::Corrupt`] when narrowing list to scalar.
    pub fn needs_list_type_rebuild(&self) -> Result<bool> {
        let Some(old) = &self.old_schema else {
            return Ok(false);
        };
        if self.current_schema.list && !old.list {
            return Ok(true);
        }
        if old.list && !self.current_schema.list {
            return Err(PostingError::Corrupt(format!(
                "type can't be changed from list to scalar for attr: [{}] \
                 without dropping it first",
                datastreamdb_x::parse_attr(&self.current_schema.predicate)
            )));
        }
        Ok(false)
    }

    /// Drops every index prefix the transition deletes or rebuilds.
    ///
    /// # Errors
    ///
    /// Returns [`PostingError`] on storage failure.
    pub fn drop_indexes(&self, engine: &std::sync::Arc<PostingEngine>) -> Result<()> {
        self.check_no_vectors()?;
        let mut prefixes = Vec::new();
        let diff = self.tok_diff();
        if diff.op != IndexOp::Noop {
            for tok in diff.to_delete.iter().chain(diff.to_rebuild.iter()) {
                prefixes.extend(tokenizer_prefixes(&self.attr, tok, false)?);
                if tok == "exact" {
                    prefixes.extend(tokenizer_prefixes(&self.attr, tok, true)?);
                }
            }
        }
        if self.reverse_op() != IndexOp::Noop {
            let pk = parsed(&self.attr);
            prefixes.push(pk.reverse_prefix());
            let mut split = pk.reverse_prefix();
            split[0] = datastreamdb_x::BYTE_SPLIT;
            prefixes.push(split);
        }
        if self.count_op() != IndexOp::Noop {
            let pk = parsed(&self.attr);
            prefixes.push(pk.count_prefix(false));
            prefixes.push(pk.count_prefix(true));
            let mut split = pk.count_prefix(false);
            split[0] = datastreamdb_x::BYTE_SPLIT;
            prefixes.push(split);
            let mut split = pk.count_prefix(true);
            split[0] = datastreamdb_x::BYTE_SPLIT;
            prefixes.push(split);
        }
        crate::index::delete_prefixes(engine, &prefixes)
    }

    /// Rewrites scalar values into fingerprinted list values.
    ///
    /// # Errors
    ///
    /// Returns [`PostingError`] on storage failure.
    pub fn build_data(&self, engine: &std::sync::Arc<PostingEngine>) -> Result<()> {
        if !self.needs_list_type_rebuild()? {
            return Ok(());
        }
        let txn = engine.begin();
        let prefix = parsed(&self.attr).data_prefix();
        let mut args = IterateDiskArgs {
            prefix,
            read_ts: self.start_ts,
            reverse: false,
            start_key: Vec::new(),
            check_inclusion: None,
            function: &mut |list, _| {
                let mut scalar: Option<datastreamdb_proto::Posting> = None;
                list.iterate(self.start_ts, 0, &mut |p| {
                    if p.uid == u64::MAX {
                        scalar = Some(p.clone());
                    }
                    Ok(())
                })?;
                let Some(mpost) = scalar else {
                    return Ok(());
                };
                let data_list = txn.get(&list.key())?;
                let mut del = DirectedEdge {
                    value_id: mpost.uid,
                    attr: self.attr.clone(),
                    op: EdgeOp::Del as i32,
                    ..DirectedEdge::default()
                };
                crate::index::txn_add_mutation(&txn, &data_list, &mut del)?;
                let mut set = DirectedEdge {
                    attr: self.attr.clone(),
                    value: mpost.value.clone(),
                    value_type: mpost.val_type,
                    op: EdgeOp::Set as i32,
                    facets: mpost.facets.clone(),
                    ..DirectedEdge::default()
                };
                crate::index::txn_add_mutation(&txn, &data_list, &mut set)?;
                Ok(())
            },
        };
        engine.memory().iterate_disk(engine.store(), &mut args)?;
        engine.commit(&txn)?;
        Ok(())
    }

    /// Rebuilds token, reverse, and count indexes.
    ///
    /// # Errors
    ///
    /// Returns [`PostingError`] on storage/schema/tokenizer failure.
    pub fn build_indexes(&self, engine: &std::sync::Arc<PostingEngine>) -> Result<()> {
        self.check_no_vectors()?;
        self.rebuild_tok_index(engine)?;
        self.rebuild_reverse_edges(engine)?;
        self.rebuild_count_index(engine)
    }

    /// Runs drop, data rewrite, and index build in order.
    ///
    /// # Errors
    ///
    /// Returns [`PostingError`] on storage/schema/tokenizer failure.
    pub fn rebuild(&self, engine: &std::sync::Arc<PostingEngine>) -> Result<()> {
        self.drop_indexes(engine)?;
        self.build_data(engine)?;
        self.build_indexes(engine)
    }

    /// Rebuilds token indexes from data postings.
    fn rebuild_tok_index(&self, engine: &std::sync::Arc<PostingEngine>) -> Result<()> {
        let diff = self.tok_diff();
        if diff.op != IndexOp::Rebuild || diff.to_rebuild.is_empty() {
            return Ok(());
        }
        let mut tokenizers = Vec::new();
        for name in &diff.to_rebuild {
            tokenizers.push(
                get_tokenizer(name)
                    .ok_or_else(|| PostingError::Corrupt(format!("unknown tokenizer {name}")))?,
            );
        }
        let txn = engine.begin();
        let prefix = parsed(&self.attr).data_prefix();
        let mut args = IterateDiskArgs {
            prefix,
            read_ts: self.start_ts,
            reverse: false,
            start_key: Vec::new(),
            check_inclusion: None,
            function: &mut |list, pk| {
                list.iterate(self.start_ts, 0, &mut |p| {
                    let edge = DirectedEdge {
                        entity: pk.uid,
                        attr: self.attr.clone(),
                        lang: String::from_utf8_lossy(&p.lang_tag).into_owned(),
                        ..DirectedEdge::default()
                    };
                    let val = Val {
                        tid: TypeId::try_from(p.val_type).unwrap_or(TypeId::Default),
                        value: Value::Binary(p.value.clone()),
                    };
                    let mut info = IndexMutationInfo {
                        tokenizers: tokenizers.clone(),
                        edge,
                        val,
                        op: EdgeOp::Set,
                    };
                    txn.add_index_mutations(&mut info)?;
                    Ok(())
                })?;
                Ok(())
            },
        };
        engine.memory().iterate_disk(engine.store(), &mut args)?;
        engine.commit(&txn)?;
        Ok(())
    }

    /// Rebuilds reverse edges from forward postings.
    fn rebuild_reverse_edges(&self, engine: &std::sync::Arc<PostingEngine>) -> Result<()> {
        if self.reverse_op() != IndexOp::Rebuild {
            return Ok(());
        }
        let txn = engine.begin();
        let prefix = parsed(&self.attr).data_prefix();
        let mut args = IterateDiskArgs {
            prefix,
            read_ts: self.start_ts,
            reverse: false,
            start_key: Vec::new(),
            check_inclusion: None,
            function: &mut |list, pk| {
                list.iterate(self.start_ts, 0, &mut |p| {
                    let edge = DirectedEdge {
                        entity: pk.uid,
                        value_id: p.uid,
                        attr: self.attr.clone(),
                        op: EdgeOp::Set as i32,
                        facets: p.facets.clone(),
                        ..DirectedEdge::default()
                    };
                    txn.add_reverse_mutation(&edge)?;
                    Ok(())
                })?;
                Ok(())
            },
        };
        engine.memory().iterate_disk(engine.store(), &mut args)?;
        engine.commit(&txn)?;
        Ok(())
    }

    /// Rebuilds count indexes over forward then reverse lists.
    fn rebuild_count_index(&self, engine: &std::sync::Arc<PostingEngine>) -> Result<()> {
        if self.count_op() != IndexOp::Rebuild {
            return Ok(());
        }
        for reverse in [false, true] {
            let txn = engine.begin();
            let pk = parsed(&self.attr);
            let prefix = if reverse {
                pk.reverse_prefix()
            } else {
                pk.data_prefix()
            };
            let mut args = IterateDiskArgs {
                prefix,
                read_ts: self.start_ts,
                reverse: false,
                start_key: Vec::new(),
                check_inclusion: None,
                function: &mut |list, pk| {
                    let size = list.length(self.start_ts, 0);
                    if size < 0 {
                        return Ok(());
                    }
                    let mut edge = DirectedEdge {
                        value_id: pk.uid,
                        attr: self.attr.clone(),
                        op: EdgeOp::Set as i32,
                        ..DirectedEdge::default()
                    };
                    let count = u32::try_from(size).unwrap_or(u32::MAX);
                    txn.add_count_mutation(&mut edge, count, reverse)?;
                    Ok(())
                },
            };
            engine.memory().iterate_disk(engine.store(), &mut args)?;
            engine.commit(&txn)?;
        }
        Ok(())
    }
}

/// Parsed key scaffold for one attribute.
fn parsed(attr: &str) -> datastreamdb_x::ParsedKey {
    datastreamdb_x::parse(&datastreamdb_x::data_key(attr, 1)).expect("data key parses")
}

/// Index prefixes (normal plus split-part) for one tokenizer.
fn tokenizer_prefixes(attr: &str, name: &str, lang: bool) -> Result<Vec<Vec<u8>>> {
    let mut prefix = parsed(attr).index_prefix();
    let id = if lang {
        datastreamdb_tok::text::tokenizer_for_lang(name, "en")
            .map(|tok| tok.identifier())
            .ok_or_else(|| PostingError::Corrupt(format!("unknown tokenizer {name}")))?
    } else {
        get_tokenizer(name)
            .map(|tok| tok.identifier())
            .ok_or_else(|| PostingError::Corrupt(format!("unknown tokenizer {name}")))?
    };
    prefix.push(id);
    let mut split = parsed(attr).index_prefix();
    split[0] = datastreamdb_x::BYTE_SPLIT;
    split.push(id);
    Ok(vec![prefix, split])
}
