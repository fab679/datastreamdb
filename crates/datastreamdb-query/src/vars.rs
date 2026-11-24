//! Query variables. Port of the variable machinery from `query/query.go`:
//! var definition (`populateVarMap`/`updateVars`), consumption (`fillVars`),
//! aggregation (`valueVarAggregation`/`evalLevelAgg`), and level transforms.
//!
//! Ancestor paths use raw pointers (read-only, like Go's `[]*SubGraph`);
//! every path outlives its use within one request.

#![warn(missing_docs)]

use std::collections::{HashMap, HashSet};

use datastreamdb_dql::{ANY_VAR, LIST_VAR, UID_VAR, VALUE_VAR};
use datastreamdb_proto::List as ProtoList;
use datastreamdb_types::{ShardedMap, TypeId, Val, Value, convert};

use crate::aggregator::Aggregator;
use crate::errors::{QueryError, Result};
use crate::subgraph::{SubGraph, VarValue, is_inequality_fn};
use crate::var_levels::{
    eval_level_agg, facet_to_val, int_to_float, path_at_parent, path_at_parent_attr, transform_to,
};

/// Ancestor path (read-only views, like Go's `[]*SubGraph`).
pub type SgPath = Vec<*const SubGraph>;

/// Reads an ancestor path entry.
pub(crate) fn path_at(path: &SgPath, idx: usize) -> &SubGraph {
    // SAFETY: paths only ever point at live request subgraphs.
    unsafe { &*path[idx] }
}

/// Whether the function is an empty inequality over a variable.
#[must_use]
pub fn is_empty_ineq_fn_with_var(sg: &SubGraph) -> bool {
    sg.src_func.as_ref().is_some_and(|func| {
        is_inequality_fn(&func.name) && func.args.is_empty() && !sg.params.needs_var.is_empty()
    })
}

impl SubGraph {
    /// Fills this node's variables from completed blocks, recursively.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] on variable resolution failure.
    pub fn recursive_fill_vars(&mut self, done_vars: &HashMap<String, VarValue>) -> Result<()> {
        self.fill_vars(done_vars)?;
        for child in &mut self.children {
            child.recursive_fill_vars(done_vars)?;
        }
        for filter in &mut self.filters {
            filter.recursive_fill_vars(done_vars)?;
        }
        Ok(())
    }

    /// Fills shortest-path endpoints from UID variables.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] on variable resolution failure.
    pub fn fill_shortest_path_vars(&mut self, vars: &HashMap<String, VarValue>) -> Result<()> {
        let args = self.params.shortest_path_args.clone();
        if let Some(from) = args.from {
            if let Some(needs) = from.needs_var.first() {
                let uid = vars
                    .get(&needs.name)
                    .and_then(|var| var.uids.as_ref())
                    .and_then(|uids| uids.uids.first().copied())
                    .unwrap_or(0);
                self.params.from = uid;
                if uid == 0 {
                    self.dest_uids = Some(ProtoList { uids: Vec::new() });
                    return Ok(());
                }
            }
        }
        if let Some(to) = args.to {
            if let Some(needs) = to.needs_var.first() {
                let uid = vars
                    .get(&needs.name)
                    .and_then(|var| var.uids.as_ref())
                    .and_then(|uids| uids.uids.first().copied())
                    .unwrap_or(0);
                self.params.to = uid;
                if uid == 0 {
                    self.dest_uids = Some(ProtoList { uids: Vec::new() });
                }
            }
        }
        Ok(())
    }

    /// Fills this node's variable references from completed blocks.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] on variable resolution failure.
    ///
    /// # Panics
    ///
    /// Panics if a UID variable lacks its list after the `is_some` check.
    pub fn fill_vars(&mut self, vars: &HashMap<String, VarValue>) -> Result<()> {
        if self.params.alias == "shortest" {
            self.fill_shortest_path_vars(vars)?;
        }

        let mut lists: Vec<ProtoList> = Vec::new();
        for needs in self.params.needs_var.clone() {
            let Some(found) = vars.get(&needs.name) else {
                continue;
            };
            if (needs.typ == ANY_VAR || needs.typ == LIST_VAR) && !found.str_list.is_empty() {
                self.expand_preds.clone_from(&found.str_list);
            } else if needs.typ == UID_VAR
                && self
                    .src_func
                    .as_ref()
                    .is_some_and(|func| func.name == "uid_in")
            {
                let mut args = Vec::new();
                if let Some(uids) = &found.uids {
                    for uid in &uids.uids {
                        args.push(datastreamdb_dql::Arg {
                            value: uid.to_string(),
                            ..datastreamdb_dql::Arg::default()
                        });
                    }
                }
                if let Some(func) = &mut self.src_func {
                    func.args = args;
                }
            } else if (needs.typ == ANY_VAR || needs.typ == UID_VAR) && found.uids.is_some() {
                lists.push(found.uids.clone().expect("uid list"));
            } else if needs.typ == ANY_VAR || needs.typ == VALUE_VAR {
                self.params.uid_to_val = found.vals.clone();
            } else if (needs.typ == ANY_VAR || needs.typ == UID_VAR) && !found.vals.is_empty() {
                let mut uids = Vec::with_capacity(found.vals.len());
                found.vals.iterate(|uid, _| {
                    uids.push(uid);
                    Ok::<(), QueryError>(())
                })?;
                uids.sort_unstable();
                lists.push(ProtoList { uids });
            } else if !found.vals.is_empty() || found.uids.is_some() {
                return Err(QueryError::Invalid(format!(
                    "Wrong variable type encountered for var({}) {}.",
                    needs.name, needs.typ
                )));
            } else {
                tracing::debug!("reached default case in fill_vars for var: {}", needs.name);
            }
        }
        self.replace_var_in_func()?;

        if self
            .dest_uids
            .as_ref()
            .is_some_and(|dest| !dest.uids.is_empty())
        {
            lists.push(self.dest_uids.clone().expect("dest uids"));
        }
        self.dest_uids = Some(datastreamdb_algo::uidlist::merge_sorted(&lists));
        Ok(())
    }

    /// Replaces `val(var)` arguments with the variable's values.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] on missing variables.
    pub fn replace_var_in_func(&mut self) -> Result<()> {
        let Some(src_func) = &self.src_func.clone() else {
            return Ok(());
        };
        let mut args = Vec::new();
        for arg in &src_func.args {
            if !arg.is_value_var {
                args.push(arg.clone());
                continue;
            }
            if self.params.uid_to_val.is_empty() {
                break;
            }
            let mut seen = std::collections::HashSet::new();
            self.params.uid_to_val.iterate(|_, val| {
                let text = match &convert(val, TypeId::String)?.value {
                    Value::String(text) => text.clone(),
                    _ => String::new(),
                };
                if seen.insert(text.clone()) {
                    args.push(datastreamdb_dql::Arg {
                        value: text,
                        ..datastreamdb_dql::Arg::default()
                    });
                }
                Ok::<(), QueryError>(())
            })?;
        }
        if let Some(func) = &mut self.src_func {
            func.args = args;
        }
        Ok(())
    }

    /// Evaluates an inequality over a value variable into destination UIDs.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] on type mismatch.
    pub fn apply_ineq_func(&mut self) -> Result<()> {
        if self.params.uid_to_val.is_empty() {
            return Ok(());
        }

        let mut typ = TypeId::Default;
        let mut stop = false;
        self.params.uid_to_val.iterate(|_, val| {
            if !stop {
                typ = val.tid;
                stop = true;
            }
            Ok::<(), QueryError>(())
        })?;
        let arg = self
            .src_func
            .as_ref()
            .and_then(|func| func.args.first().map(|arg| arg.value.clone()))
            .unwrap_or_default();
        let src = Val {
            tid: TypeId::String,
            value: Value::Binary(arg.as_bytes().to_vec()),
        };
        let bound = convert(&src, typ).map_err(|_| {
            QueryError::Invalid(format!(
                "Invalid argment {arg}. Comparing with different type"
            ))
        })?;

        let name = self
            .src_func
            .as_ref()
            .map(|func| func.name.clone())
            .unwrap_or_default();
        let mut dest = Vec::new();
        if let Some(src_uids) = &self.src_uids {
            for uid in &src_uids.uids {
                if let Some(current) = self.params.uid_to_val.get(*uid) {
                    if datastreamdb_types::compare_vals(&name, current, &bound).unwrap_or(false) {
                        dest.push(*uid);
                    }
                }
            }
        } else {
            self.params.uid_to_val.iterate(|uid, current| {
                if datastreamdb_types::compare_vals(&name, current, &bound).unwrap_or(false) {
                    dest.push(uid);
                }
                Ok::<(), QueryError>(())
            })?;
            dest.sort_unstable();
            self.uid_matrix = vec![ProtoList { uids: dest.clone() }];
        }
        self.dest_uids = Some(ProtoList { uids: dest });
        Ok(())
    }

    /// Appends empty rows so traversal stays aligned.
    pub fn append_dummy_values(&mut self) {
        let Some(src_uids) = &self.src_uids else {
            return;
        };
        if src_uids.uids.is_empty() {
            return;
        }
        for _ in &src_uids.uids {
            self.uid_matrix.push(ProtoList::default());
            self.value_matrix
                .push(datastreamdb_proto::ValueList::default());
        }
    }

    /// Intersects UID rows with destination UIDs (order-aware).
    pub fn update_uid_matrix(&mut self) {
        self.update_facet_matrix();
        let dest = self.dest_uids.clone().unwrap_or_default();
        for row in &mut self.uid_matrix {
            if !self.params.order.is_empty() || !self.params.facets_order.is_empty() {
                datastreamdb_algo::uidlist::apply_filter(row, |uid, _| {
                    dest.uids.binary_search(&uid).is_ok()
                });
            } else {
                let mut merged = Vec::new();
                datastreamdb_algo::uidlist::intersect_with(
                    &row.uids.clone(),
                    &dest.uids,
                    &mut merged,
                );
                row.uids = merged;
            }
        }
    }

    /// Drops facets of filtered-out UIDs.
    pub fn update_facet_matrix(&mut self) {
        if self.facets_matrix.len() != self.uid_matrix.len() {
            return;
        }
        let dest = self.dest_uids.clone().unwrap_or_default();
        for (lidx, row) in self.uid_matrix.clone().iter().enumerate() {
            if row.uids.is_empty() {
                continue;
            }
            let kept: Vec<datastreamdb_proto::Facets> = row
                .uids
                .iter()
                .enumerate()
                .filter(|(_, uid)| dest.uids.binary_search(uid).is_ok())
                .filter_map(|(idx, _)| self.facets_matrix[lidx].facets_list.get(idx).cloned())
                .collect();
            self.facets_matrix[lidx].facets_list = kept;
        }
    }

    /// Stores this subtree's variables plus cascade filtering.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] on aggregation failure.
    pub fn populate_var_map(
        &mut self,
        done_vars: &mut HashMap<String, VarValue>,
        sg_path: &mut SgPath,
    ) -> Result<()> {
        if self.dest_uids.is_none() || self.is_group_by() {
            return Ok(());
        }

        let mut cascade_args = HashSet::new();
        for pred in &self.params.cascade.fields {
            cascade_args.insert(pred.clone());
        }
        let cascade_all = cascade_args.contains("__all__");

        if self.params.alias != "shortest" {
            if !self.filters.is_empty() {
                self.update_uid_matrix();
            }

            for idx in 0..self.children.len() {
                sg_path.push(std::ptr::from_ref::<SubGraph>(self));
                let result = self.children[idx].populate_var_map(done_vars, sg_path);
                sg_path.pop();
                result?;
                if self.children[idx].params.cascade.fields.is_empty() {
                    continue;
                }
                self.children[idx].update_uid_matrix();

                let (first, offset) = (
                    self.children[idx].params.cascade.first,
                    self.children[idx].params.cascade.offset,
                );
                if first != 0 || offset != 0 {
                    for row in &mut self.children[idx].uid_matrix {
                        let (start, end) = page_range(first, offset, row.uids.len());
                        row.uids = row.uids[start..end.min(row.uids.len())].to_vec();
                    }
                }
            }

            if !self.params.cascade.fields.is_empty() {
                let dest = self.dest_uids.clone().unwrap_or_default().uids;
                let mut out = Vec::with_capacity(dest.len());
                for (i, uid) in dest.iter().enumerate() {
                    let mut exclude = false;
                    for child in &self.children {
                        if child.attr == "uid" {
                            continue;
                        }
                        let empty_values = child
                            .value_matrix
                            .get(i)
                            .is_none_or(|row| row.values.is_empty());
                        let empty_counts = child.counts.get(i).is_none();
                        let empty_uids = child
                            .uid_matrix
                            .get(i)
                            .is_none_or(|row| row.uids.is_empty());
                        if (cascade_all || cascade_args.contains(&child.attr))
                            && !child.is_internal()
                            && empty_values
                            && empty_counts
                            && empty_uids
                        {
                            exclude = true;
                            break;
                        }
                    }
                    if !exclude {
                        out.push(*uid);
                    }
                }
                self.dest_uids = Some(ProtoList { uids: out });
            }
        }

        self.update_vars(done_vars, sg_path)
    }

    /// Records this node's variable definitions.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] on aggregation failure.
    pub fn update_vars(
        &mut self,
        done_vars: &mut HashMap<String, VarValue>,
        sg_path: &SgPath,
    ) -> Result<()> {
        if self.params.var.is_empty() && self.params.facet_var.is_empty() {
            return Ok(());
        }
        let path_copy = sg_path.clone();
        self.populate_uid_val_var(done_vars, &path_copy)?;
        self.populate_facet_vars(done_vars, &path_copy)
    }

    /// Records UID/value/count variable definitions.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] on aggregation failure.
    #[allow(clippy::too_many_lines, reason = "exact port of Go populateUIDValVar")]
    pub fn populate_uid_val_var(
        &mut self,
        done_vars: &mut HashMap<String, VarValue>,
        sg_path: &SgPath,
    ) -> Result<()> {
        if self.params.var.is_empty() {
            return Ok(());
        }

        if !self.counts.is_empty() {
            let mut vals = ShardedMap::new();
            if let Some(src_uids) = &self.src_uids {
                for (idx, uid) in src_uids.uids.iter().enumerate() {
                    vals.set(
                        *uid,
                        Val {
                            tid: TypeId::Int,
                            value: Value::Int(i64::from(self.counts[idx])),
                        },
                    );
                }
            }
            done_vars.insert(
                self.params.var.clone(),
                VarValue {
                    vals,
                    path: sg_path.clone(),
                    str_list: self.value_matrix.clone(),
                    ..VarValue::default()
                },
            );
        } else if self.params.do_count && self.attr == "uid" && self.is_internal() {
            let mut vals = ShardedMap::new();
            let len = self.src_uids.as_ref().map_or(0, |src| src.uids.len());
            vals.set(
                u64::MAX,
                Val {
                    tid: TypeId::Int,
                    value: Value::Int(i64::try_from(len).unwrap_or(i64::MAX)),
                },
            );
            done_vars.insert(
                self.params.var.clone(),
                VarValue {
                    vals,
                    path: sg_path.clone(),
                    str_list: self.value_matrix.clone(),
                    ..VarValue::default()
                },
            );
        } else if self
            .dest_uids
            .as_ref()
            .is_some_and(|dest| !dest.uids.is_empty())
            || (self.attr == "uid" && self.src_uids.is_some())
        {
            let uids = if self.attr == "uid" {
                self.src_uids.clone()
            } else {
                self.dest_uids.clone()
            };
            if let Some(existing) = done_vars.get(&self.params.var) {
                let merged = datastreamdb_algo::uidlist::merge_sorted(&[
                    existing.uids.clone().unwrap_or_default(),
                    uids.clone().unwrap_or_default(),
                ]);
                let mut updated = existing.clone();
                updated.uids = Some(merged);
                done_vars.insert(self.params.var.clone(), updated);
            } else {
                done_vars.insert(
                    self.params.var.clone(),
                    VarValue {
                        uids,
                        path: sg_path.clone(),
                        vals: ShardedMap::new(),
                        str_list: self.value_matrix.clone(),
                    },
                );
            }
        } else if !self.value_matrix.is_empty() && self.src_uids.is_some() && !sg_path.is_empty() {
            let mut entry = done_vars
                .get(&self.params.var)
                .cloned()
                .unwrap_or(VarValue {
                    vals: ShardedMap::new(),
                    path: sg_path.clone(),
                    str_list: self.value_matrix.clone(),
                    ..VarValue::default()
                });
            if let Some(src_uids) = &self.src_uids {
                for (idx, uid) in src_uids.uids.iter().enumerate() {
                    let Some(row) = self.value_matrix.get(idx) else {
                        continue;
                    };
                    if row.values.len() > 1 {
                        return Err(QueryError::Invalid(
                            "Value variables not supported for predicate with list type."
                                .to_owned(),
                        ));
                    }
                    if row.values.is_empty() {
                        continue;
                    }
                    if let Ok(val) = convert_with_best_effort(&row.values[0], &self.attr) {
                        entry.vals.set(*uid, val);
                    }
                }
            }
            done_vars.insert(self.params.var.clone(), entry);
        } else {
            if done_vars.contains_key(&self.params.var) {
                return Ok(());
            }
            done_vars.insert(
                self.params.var.clone(),
                VarValue {
                    path: sg_path.clone(),
                    vals: ShardedMap::new(),
                    str_list: self.value_matrix.clone(),
                    ..VarValue::default()
                },
            );
        }
        Ok(())
    }

    /// Records facet variable definitions (summing int/float repeats).
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] on aggregation failure.
    ///
    /// # Panics
    ///
    /// Panics if a UID seen in the first pass vanished from the map.
    pub fn populate_facet_vars(
        &mut self,
        done_vars: &mut HashMap<String, VarValue>,
        sg_path: &SgPath,
    ) -> Result<()> {
        if self.params.facet_var.is_empty() || self.params.facet.is_none() {
            return Ok(());
        }

        let mut path = sg_path.clone();
        path.push(std::ptr::from_ref::<SubGraph>(self));
        if let Some(facet) = &self.params.facet {
            for param in &facet.param {
                if let Some(fvar) = self.params.facet_var.get(&param.key) {
                    done_vars.insert(
                        fvar.clone(),
                        VarValue {
                            vals: ShardedMap::new(),
                            path: path.clone(),
                            ..VarValue::default()
                        },
                    );
                }
            }
        }

        if self.facets_matrix.is_empty() {
            return Ok(());
        }

        for (i, row) in self.uid_matrix.clone().iter().enumerate() {
            for (j, uid) in row.uids.iter().enumerate() {
                let Some(facets) = self
                    .facets_matrix
                    .get(i)
                    .and_then(|list| list.facets_list.get(j))
                else {
                    continue;
                };
                for facet in facets.facets.clone() {
                    let Some(fvar) = self.params.facet_var.get(&facet.key).cloned() else {
                        continue;
                    };
                    let entry = done_vars.entry(fvar.clone()).or_insert(VarValue {
                        vals: ShardedMap::new(),
                        path: path.clone(),
                        ..VarValue::default()
                    });
                    if entry.vals.get(*uid).is_none() {
                        entry.vals.set(*uid, facet_to_val(&facet)?);
                    } else {
                        let current = entry.vals.get(*uid).expect("facet var").clone();
                        let next = facet_to_val(&facet)?;
                        if next.tid != TypeId::Int && next.tid != TypeId::Float {
                            return Err(QueryError::Invalid(
                                "Repeated id with non int/float value for facet var encountered."
                                    .to_owned(),
                            ));
                        }
                        let mut agg = Aggregator::new("sum");
                        agg.apply(&current)?;
                        agg.apply(&next)?;
                        if let Ok(val) = agg.value() {
                            entry.vals.set(*uid, val);
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Aggregates value variables bottom-up.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] on aggregation failure.
    pub fn populate_post_aggregation(
        &mut self,
        done_vars: &mut HashMap<String, VarValue>,
        path: &mut SgPath,
        parent: Option<*const SubGraph>,
    ) -> Result<()> {
        let this = std::ptr::from_ref::<SubGraph>(self);
        for idx in 0..self.children.len() {
            path.push(this);
            let result = self.children[idx].populate_post_aggregation(done_vars, path, Some(this));
            path.pop();
            result?;
        }
        self.value_var_aggregation(done_vars, path, parent)
    }

    /// Aggregates one internal/group-by/empty node.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] on math or aggregation failure.
    ///
    /// # Panics
    ///
    /// Panics if the math expression vanished after the `is_some` check.
    #[allow(
        clippy::too_many_lines,
        reason = "exact port of Go valueVarAggregation"
    )]
    pub fn value_var_aggregation(
        &mut self,
        done_vars: &mut HashMap<String, VarValue>,
        path: &SgPath,
        parent: Option<*const SubGraph>,
    ) -> Result<()> {
        if !self.is_internal() && !self.is_group_by() && !self.params.is_empty {
            return Ok(());
        }
        if self.params.is_empty && parent.is_none() {
            return Ok(());
        }

        if self.is_group_by() {
            return self.process_group_by(done_vars, path);
        }
        if self
            .src_func
            .as_ref()
            .is_some_and(|func| crate::aggregator::is_aggregator_fn(&func.name))
            && parent.is_some_and(|p| !path_at_parent(p).is_group_by())
        {
            let mp = eval_level_agg(done_vars, self, parent.map(path_at_parent))?;
            if !self.params.var.is_empty() {
                let mut entry = done_vars.get(&self.params.var).cloned().unwrap_or_default();
                entry.vals = mp.clone();
                done_vars.insert(self.params.var.clone(), entry);
            }
            self.params.uid_to_val = mp;
        } else if self.math_exp.is_some() {
            self.transform_vars(done_vars, path)?;
            crate::math::eval_math_tree(self.math_exp.as_mut().expect("math"))?;
            let val_len = self.math_exp.as_ref().expect("math").val.len();
            if val_len != 0 {
                let mut is_int = false;
                let mut is_float = false;
                self.math_exp
                    .as_ref()
                    .expect("math")
                    .val
                    .iterate(|_, val| {
                        if val.tid == TypeId::Float {
                            is_float = true;
                        }
                        if val.tid == TypeId::Int {
                            is_int = true;
                        }
                        Ok::<(), QueryError>(())
                    })?;
                if is_int && is_float {
                    let updates: Vec<(u64, Val)> = {
                        let mut updates = Vec::new();
                        self.math_exp
                            .as_ref()
                            .expect("math")
                            .val
                            .iterate(|uid, val| {
                                if val.tid == TypeId::Int {
                                    if let Value::Int(num) = val.value {
                                        let widened = int_to_float(num);
                                        updates.push((
                                            uid,
                                            Val {
                                                tid: TypeId::Float,
                                                value: Value::Float(widened),
                                            },
                                        ));
                                    }
                                }
                                Ok::<(), QueryError>(())
                            })?;
                        updates
                    };
                    for (uid, val) in updates {
                        self.math_exp.as_mut().expect("math").val.set(uid, val);
                    }
                }
                let mut entry = done_vars.get(&self.params.var).cloned().unwrap_or_default();
                entry
                    .vals
                    .clone_from(&self.math_exp.as_ref().expect("math").val);
                entry.path.clone_from(path);
                done_vars.insert(self.params.var.clone(), entry);
                self.params.uid_to_val = self.math_exp.as_ref().expect("math").val.clone();
            } else if !matches!(
                self.math_exp.as_ref().expect("math").constant.value,
                Value::Null
            ) {
                let mut mp = ShardedMap::new();
                let range: &[u64] = self
                    .src_uids
                    .as_ref()
                    .map(|src| src.uids.as_slice())
                    .or_else(|| {
                        if parent.is_none() {
                            self.dest_uids.as_ref().map(|dest| dest.uids.as_slice())
                        } else {
                            None
                        }
                    })
                    .unwrap_or(&[]);
                if self.src_uids.is_none() && parent.is_some() {
                    let mut entry = done_vars.get(&self.params.var).cloned().unwrap_or_default();
                    entry
                        .vals
                        .set(0, self.math_exp.as_ref().expect("math").constant.clone());
                    done_vars.insert(self.params.var.clone(), entry.clone());
                    self.params.uid_to_val = entry.vals;
                    return Ok(());
                }
                for uid in range {
                    mp.set(*uid, self.math_exp.as_ref().expect("math").constant.clone());
                }
                let mut entry = done_vars.get(&self.params.var).cloned().unwrap_or_default();
                entry.vals = mp.clone();
                done_vars.insert(self.params.var.clone(), entry);
                self.params.uid_to_val = mp;
            } else {
                tracing::debug!("Warning: Math expression is using unassigned values or constants");
            }
        } else if !self.params.needs_var.is_empty() {
            let name = self.params.needs_var[0].name.clone();
            if let Some(src) = done_vars.get(&name) {
                self.params.uid_to_val = src.vals.clone();
            }
        } else if self.attr == "uid" && self.params.do_count {
            // Computed while rendering.
        } else {
            let parent_attr = parent.map_or("", path_at_parent_attr);
            return Err(QueryError::Invalid(format!(
                "Unhandled pb.node <{}> with parent <{}>",
                self.attr, parent_attr
            )));
        }
        Ok(())
    }

    /// Brings math variables onto one level.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] on transform failure.
    ///
    /// # Panics
    ///
    /// Panics if the constant entry vanished after the `is_some` check.
    pub fn transform_vars(
        &mut self,
        done_vars: &HashMap<String, VarValue>,
        path: &SgPath,
    ) -> Result<()> {
        let Some(math_exp) = &mut self.math_exp else {
            return Ok(());
        };
        math_exp.each_var_node(&mut |node| {
            let current = done_vars.get(&node.var).cloned().unwrap_or_default();
            let new_map = transform_to(&current, path)?;
            if new_map.len() == 1 && new_map.get(u64::MAX).is_some() {
                node.constant = new_map.get(u64::MAX).expect("const").clone();
            } else {
                node.val = new_map;
            }
            Ok(())
        })
    }
}

/// Converts a task value with schema-appropriate typing.
///
/// # Errors
///
/// Returns [`QueryError`] on conversion failure.
pub fn convert_with_best_effort(tv: &datastreamdb_proto::TaskValue, attr: &str) -> Result<Val> {
    let tid = TypeId::try_from(tv.val_type).unwrap_or(TypeId::Default);
    if !tid.is_scalar() {
        return Err(QueryError::Invalid(format!(
            "Leaf predicate:'{attr}' must be a scalar."
        )));
    }
    convert(
        &Val {
            tid: TypeId::Binary,
            value: Value::Binary(tv.val.clone()),
        },
        tid,
    )
    .map_err(|err| {
        QueryError::Invalid(format!(
            "error interpreting appropriate type for {attr}: {err}"
        ))
    })
}

/// Pagination window (port of `x.PageRange`).
#[must_use]
pub fn page_range(count: i32, offset: i32, len: usize) -> (usize, usize) {
    let offset = usize::try_from(offset.max(0)).unwrap_or(0);
    if offset >= len {
        return (len, len);
    }
    if count <= 0 {
        return (offset, len);
    }
    let count = usize::try_from(count.max(0)).unwrap_or(0);
    (offset, offset.saturating_add(count).min(len))
}

/// Deep-copies filters plus scalar fields (like Go's struct copy).
pub fn copy_filters_recurse(dst: &mut SubGraph, src: &SubGraph) {
    let children = std::mem::take(&mut dst.children);
    let filters = std::mem::take(&mut dst.filters);
    *dst = src.clone();
    dst.children = children;
    dst.children.clear();
    dst.filters = filters;
    dst.filters.clear();
    for filter in &src.filters {
        let mut fresh = SubGraph::default();
        copy_filters_recurse(&mut fresh, filter);
        dst.filters.push(Box::new(fresh));
    }
}
