//! Subgraph-to-JSON rendering. Split from `output.rs`: the `SubGraph`
//! render methods plus the `process_node_uids`/`to_json` entry points.
//!
//! Port of the traversal half of the upstream Go `query/outputnode.go`.

#![warn(missing_docs)]

use datastreamdb_types::{TypeId, Val};

use crate::errors::{QueryError, Result};
use crate::groupby::GroupResults;
use crate::output::{Encoder, JsonNode, MAX_ENCODED_SIZE};
use crate::subgraph::SubGraph;
use crate::vars::convert_with_best_effort;

impl SubGraph {
    /// Renders group-by results under `field`.
    fn add_groupby(node: &mut JsonNode, res: &GroupResults, field: &str) {
        if res.group.is_empty() {
            return;
        }
        let mut group = Encoder::new_node(field);
        for grp in &res.group {
            let mut item = Encoder::new_node("@groupby");
            for key in &grp.keys {
                Encoder::add_value(&mut item, &key.attr, &key.key);
            }
            for agg in &grp.aggregates {
                Encoder::add_value(&mut item, &agg.attr, &agg.key);
            }
            Encoder::add_list_child(&mut group, item);
        }
        Encoder::add_list_child(node, group);
    }

    /// Renders empty-block aggregations.
    fn add_aggregations(&self, node: &mut JsonNode) -> Result<()> {
        for child in &self.children {
            let agg_val = child.params.uid_to_val.get(0).cloned().unwrap_or(Val {
                tid: TypeId::Default,
                value: datastreamdb_types::Value::Null,
            });
            let missing = child.params.uid_to_val.get(0).is_none();
            if missing && child.params.needs_var.is_empty() {
                return Err(QueryError::Invalid(
                    "Only aggregated variables allowed within empty block.".to_owned(),
                ));
            }
            if child.params.normalize && child.params.alias.is_empty() {
                continue;
            }
            let field = child.agg_with_var_field_name();
            let mut item = Encoder::new_node(&self.params.alias);
            Encoder::add_value(&mut item, &field, &agg_val);
            Encoder::add_list_child(node, item);
        }
        if Encoder::is_empty(node) {
            Encoder::add_list_child(node, Encoder::new_node(&self.params.alias));
        }
        Ok(())
    }

    /// Renders `count(uid)` children.
    fn handle_count_uid_nodes(&self, node: &mut JsonNode, count: usize) -> bool {
        let mut added = false;
        let field_id = self.field_name().to_owned();
        for child in &self.children {
            let is_count = child.attr == "uid" && child.params.do_count && child.is_internal();
            let norm_without_alias = child.params.alias.is_empty() && child.params.normalize;
            if is_count && !norm_without_alias {
                added = true;
                let val = Val {
                    tid: TypeId::Int,
                    value: datastreamdb_types::Value::Int(i64::try_from(count).unwrap_or(i64::MAX)),
                };
                let field = if child.params.alias.is_empty() {
                    "count".to_owned()
                } else {
                    child.params.alias.clone()
                };
                let mut item = Encoder::new_node(&field_id);
                Encoder::add_value(&mut item, &field, &val);
                Encoder::add_list_child(node, item);
            }
        }
        added
    }

    /// Renders a `count(...)` value.
    fn add_count(&self, count: u64, node: &mut JsonNode) {
        if self.params.normalize && self.params.alias.is_empty() {
            return;
        }
        let val = Val {
            tid: TypeId::Int,
            value: datastreamdb_types::Value::Int(i64::try_from(count).unwrap_or(i64::MAX)),
        };
        let field = if self.params.alias.is_empty() {
            format!("count({})", self.attr)
        } else {
            self.params.alias.clone()
        };
        Encoder::add_value(node, &field, &val);
    }

    /// Aggregation output field name.
    pub(crate) fn agg_with_var_field_name(&self) -> String {
        if !self.params.alias.is_empty() {
            return self.params.alias.clone();
        }
        let mut field = format!("val({})", self.params.var);
        if let Some(needs) = self.params.needs_var.first() {
            field = format!("val({})", needs.name);
            if let Some(func) = &self.src_func {
                field = format!("{}({field})", func.name);
            }
        }
        field
    }

    /// Renders an internal (variable) node.
    fn add_internal_node(&self, uid: u64, node: &mut JsonNode) {
        let Some(val) = self.params.uid_to_val.get(uid) else {
            return;
        };
        if matches!(val.value, datastreamdb_types::Value::Null) {
            return;
        }
        let field = self.agg_with_var_field_name();
        Encoder::add_value(node, &field, val);
    }

    /// Renders a `checkpwd(...)` verdict.
    fn add_check_pwd(&self, vals: &[datastreamdb_proto::TaskValue], node: &mut JsonNode) {
        let verdict = vals.first().is_some_and(|val| {
            !val.val.is_empty() && datastreamdb_task::context::task_value_to_bool(val)
        });
        let val = Val {
            tid: TypeId::Bool,
            value: datastreamdb_types::Value::Bool(verdict),
        };
        let field = if self.params.alias.is_empty() {
            format!("checkpwd({})", self.attr)
        } else {
            self.params.alias.clone()
        };
        Encoder::add_value(node, &field, &val);
    }

    /// Renders one UID's children into `node`.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] on encoding failure.
    #[allow(clippy::too_many_lines, reason = "exact port of Go preTraverse")]
    pub fn pre_traverse(&mut self, enc: &Encoder, uid: u64, node: &mut JsonNode) -> Result<()> {
        if self.params.ignore_reflex {
            if self.params.parent_ids.contains(&uid) {
                return Ok(());
            }
            self.params.parent_ids.push(uid);
        }

        let mut invalid: Option<std::collections::HashSet<u64>> = None;
        // Borrow dance: iterate by index so children stay reborrowable.
        for child_idx in 0..self.children.len() {
            let skip = {
                let child = &self.children[child_idx];
                child.params.ignore_result
                    || (child.is_internal() && !child.params.expand.is_empty())
                    || (child.is_internal()
                        && child.params.normalize
                        && child.params.alias.is_empty())
            };
            if skip {
                let is_internal = self.children[child_idx].is_internal();
                let expand_empty = self.children[child_idx].params.expand.is_empty();
                let norm_alias = self.children[child_idx].params.normalize
                    && self.children[child_idx].params.alias.is_empty();
                if is_internal && expand_empty && !norm_alias {
                    self.children[child_idx].add_internal_node(uid, node);
                }
                continue;
            }
            if self.children[child_idx].uid_matrix.is_empty() {
                continue;
            }
            {
                let child = &self.children[child_idx];
                if !child.facets_matrix.is_empty()
                    && child.facets_matrix.len() != child.uid_matrix.len()
                {
                    return Err(QueryError::Invalid(format!(
                        "Length of facetsMatrix and uidMatrix mismatch: {} vs {}",
                        child.facets_matrix.len(),
                        child.uid_matrix.len()
                    )));
                }
            }

            let src_uids = self.children[child_idx]
                .src_uids
                .clone()
                .unwrap_or_default()
                .uids;
            let Some(idx) = src_uids.iter().position(|candidate| *candidate == uid) else {
                continue;
            };

            if self.children[child_idx].params.is_group_by {
                let child = &self.children[child_idx];
                if child.groupby_res.len() <= idx {
                    return Err(QueryError::Invalid(format!(
                        "Unexpected length while adding Groupby. Idx: [{idx}], len: [{}]",
                        child.groupby_res.len()
                    )));
                }
                let res = child.groupby_res[idx].clone();
                let field = child.field_name().to_owned();
                Self::add_groupby(node, &res, &field);
                continue;
            }

            let field_name = self.children[child_idx].field_name().to_owned();
            let has_counts = !self.children[child_idx].counts.is_empty();
            let is_checkpwd = self.children[child_idx]
                .src_func
                .as_ref()
                .is_some_and(|func| func.name == "checkpwd");
            let has_uids = self.children[child_idx]
                .uid_matrix
                .get(idx)
                .is_some_and(|row| !row.uids.is_empty());

            if has_counts {
                let count = u64::from(self.children[child_idx].counts[idx]);
                self.children[child_idx].add_count(count, node);
            } else if is_checkpwd {
                let vals = self.children[child_idx]
                    .value_matrix
                    .get(idx)
                    .map(|row| row.values.clone())
                    .unwrap_or_default();
                self.children[child_idx].add_check_pwd(&vals, node);
            } else if has_uids {
                self.pre_traverse_uids(enc, node, child_idx, idx, &mut invalid)?;
            } else {
                self.pre_traverse_value(enc, node, child_idx, idx, uid, &field_name)?;
            }
        }

        if self.params.ignore_reflex && !self.params.parent_ids.is_empty() {
            self.params.parent_ids.pop();
        }

        if (self.params.get_uid && !Encoder::is_empty(node)) || self.params.shortest {
            Encoder::set_uid(node, uid, "uid");
        }
        if let Some(meta) = &self.path_meta {
            let weight = Val {
                tid: TypeId::Float,
                value: datastreamdb_types::Value::Float(meta.weight),
            };
            Encoder::add_value(node, "_weight_", &weight);
        }
        Ok(())
    }

    /// Renders UID-bearing children (entity recursion + facets).
    #[allow(clippy::too_many_lines)]
    fn pre_traverse_uids(
        &mut self,
        enc: &Encoder,
        node: &mut JsonNode,
        child_idx: usize,
        idx: usize,
        invalid: &mut Option<std::collections::HashSet<u64>>,
    ) -> Result<()> {
        let field_name = self.children[child_idx].field_name().to_owned();
        let facets_list = if self.children[child_idx].params.facet.is_some() {
            self.children[child_idx]
                .facets_matrix
                .get(idx)
                .map(|list| list.facets_list.clone())
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        if self.params.ignore_reflex {
            let parent_ids = self.params.parent_ids.clone();
            self.children[child_idx].params.parent_ids = parent_ids;
        }
        enc.add_size(field_name.len() as u64);
        let rows = self.children[child_idx]
            .uid_matrix
            .get(idx)
            .map(|row| row.uids.clone())
            .unwrap_or_default();
        for (child_pos, child_uid) in rows.iter().enumerate() {
            if field_name.is_empty() || invalid.as_ref().is_some_and(|set| set.contains(child_uid))
            {
                continue;
            }
            let mut item = Encoder::new_node(&field_name);
            let mut child =
                std::mem::replace(&mut self.children[child_idx], Box::new(SubGraph::default()));
            let result = child.pre_traverse(enc, *child_uid, &mut item);
            // Restore before handling the result (keeps the tree intact).
            self.children[child_idx] = child;
            match result {
                Err(QueryError::InvalidCascade) => {
                    invalid
                        .get_or_insert_with(std::collections::HashSet::new)
                        .insert(*child_uid);
                    continue;
                }
                Err(err) => return Err(err),
                Ok(()) => {}
            }

            if Encoder::is_empty(&item) {
                continue;
            }
            if self.params.get_uid {
                Encoder::set_uid(&mut item, *child_uid, "uid");
            }
            if self.children[child_idx].params.facet.is_some() {
                if let Some(facets) = facets_list.get(child_pos) {
                    Encoder::attach_facets(
                        &mut item,
                        &field_name,
                        false,
                        &facets.facets,
                        child_pos,
                    )?;
                }
            }
            if self.children[child_idx].params.normalize {
                Encoder::fix_order(&mut item);
                for flat in Encoder::normalize(&item)? {
                    let mut flat_node = Encoder::new_node(&field_name);
                    flat_node.visited = true;
                    flat_node.children = flat;
                    Encoder::add_list_child(node, flat_node);
                }
                continue;
            }
            if self.children[child_idx].list {
                Encoder::add_list_child(node, item);
            } else {
                Encoder::add_map_child(node, item);
            }
        }

        let _ = self.children[child_idx].handle_count_uid_nodes(node, rows.len());
        Ok(())
    }

    /// Renders value children (scalars, languages, facets).
    #[allow(clippy::too_many_lines)]
    fn pre_traverse_value(
        &mut self,
        enc: &Encoder,
        node: &mut JsonNode,
        child_idx: usize,
        idx: usize,
        uid: u64,
        field_name: &str,
    ) -> Result<()> {
        let mut field_name = field_name.to_owned();
        {
            let child = &self.children[child_idx];
            if child.params.alias.is_empty()
                && !child.params.langs.is_empty()
                && child.params.langs[0] != "*"
            {
                field_name.push('@');
                field_name.push_str(&child.params.langs.join(":"));
            }
        }
        enc.add_size(field_name.len() as u64);

        if self.children[child_idx].attr == "uid" {
            Encoder::set_uid(node, uid, &field_name);
            return Ok(());
        }

        if self.children[child_idx]
            .facets_matrix
            .get(idx)
            .is_some_and(|list| !list.facets_list.is_empty())
        {
            let lists = self.children[child_idx].facets_matrix[idx]
                .facets_list
                .clone();
            let is_list = self.children[child_idx].list;
            for (i, facets) in lists.iter().enumerate() {
                Encoder::attach_facets(node, &field_name, is_list, &facets.facets, i)?;
            }
        }

        let values = self.children[child_idx]
            .value_matrix
            .get(idx)
            .map(|row| row.values.clone())
            .unwrap_or_default();
        let langs = self.children[child_idx].params.langs.clone();
        let expand_all = self.children[child_idx].params.expand_all;
        let is_list = self.children[child_idx].list;
        let lang_tags = self.children[child_idx].lang_tags.clone();
        let normalize = self.children[child_idx].params.normalize;
        let alias = self.children[child_idx].params.alias.clone();
        for (i, cell) in values.iter().enumerate() {
            let val = convert_with_best_effort(cell, &self.children[child_idx].attr)?;
            if expand_all && !lang_tags.get(idx).is_none_or(|tags| tags.lang.is_empty()) {
                let tags = &lang_tags[idx].lang;
                if i >= tags.len() {
                    return Err(QueryError::Invalid(
                        "pb.error: all lang tags should be either present or absent".to_owned(),
                    ));
                }
                let mut tagged = field_name.clone();
                if !tags[i].is_empty() && tags[i] != "*" {
                    tagged.push('@');
                    tagged.push_str(&tags[i]);
                }
                Encoder::add_list_value(node, &tagged, &val, is_list && tags[i].is_empty());
                continue;
            }
            let as_list = is_list && langs.is_empty();
            if !normalize {
                Encoder::add_list_value(node, &field_name, &val, as_list);
                continue;
            }
            if !alias.is_empty() {
                Encoder::add_list_value(node, &field_name, &val, as_list);
            }
        }
        Ok(())
    }
}

/// Renders the root blocks into DQL JSON.
///
/// # Errors
///
/// Returns [`QueryError`] on encoding failure.
pub fn process_node_uids(node: &mut JsonNode, enc: &Encoder, sg: &mut SubGraph) -> Result<()> {
    if sg.params.is_empty {
        return sg.add_aggregations(node);
    }
    enc.add_size(sg.params.alias.len() as u64);
    if sg.uid_matrix.is_empty() {
        Encoder::add_list_child(node, Encoder::new_node(&sg.params.alias.clone()));
        return Ok(());
    }

    let mut has_child = sg.handle_count_uid_nodes(
        node,
        sg.dest_uids.as_ref().map_or(0, |dest| dest.uids.len()),
    );
    if sg.params.is_group_by {
        let Some(res) = sg.groupby_res.first().cloned() else {
            return Err(QueryError::Invalid(
                "Expected GroupbyRes to have length > 0.".to_owned(),
            ));
        };
        let alias = sg.params.alias.clone();
        SubGraph::add_groupby(node, &res, &alias);
        return Ok(());
    }

    let rows = sg.uid_matrix[0].uids.clone();
    let dest = sg.dest_uids.clone().unwrap_or_default().uids;
    for uid in rows {
        if dest.binary_search(&uid).is_err() {
            continue;
        }
        let mut item = Encoder::new_node(&sg.params.alias.clone());
        match sg.pre_traverse(enc, uid, &mut item) {
            Err(QueryError::InvalidCascade) => continue,
            Err(err) => return Err(err),
            Ok(()) => {}
        }
        if Encoder::is_empty(&item) {
            continue;
        }
        has_child = true;
        if !sg.params.normalize {
            Encoder::add_list_child(node, item);
            continue;
        }
        Encoder::fix_order(&mut item);
        for flat in Encoder::normalize(&item)? {
            let mut flat_node = Encoder::new_node(&sg.params.alias.clone());
            flat_node.visited = true;
            flat_node.children = flat;
            Encoder::add_list_child(node, flat_node);
        }
    }

    if !has_child {
        Encoder::add_list_child(node, Encoder::new_node(&sg.params.alias.clone()));
    }
    Ok(())
}

/// Renders executed root blocks into DQL JSON bytes.
///
/// # Errors
///
/// Returns [`QueryError`] on encoding failure.
pub fn to_json(roots: &mut [SubGraph]) -> Result<Vec<u8>> {
    let mut enc = Encoder::new();
    let mut root = Encoder::new_node("_root_");
    for sg in roots.iter_mut() {
        process_node_uids(&mut root, &enc, sg)?;
    }
    Encoder::fix_order(&mut root);
    if root.children.is_empty() {
        return Ok(b"{}".to_vec());
    }
    enc.encode(&root)?;
    if enc.buf.len() as u64 > MAX_ENCODED_SIZE {
        return Err(QueryError::Invalid(format!(
            "while writing to buffer. Encoded response size: {} is bigger than threshold: {}",
            enc.buf.len(),
            MAX_ENCODED_SIZE
        )));
    }
    Ok(std::mem::take(&mut enc.buf))
}

impl Encoder {
    /// Adds to the size counter.
    fn add_size(&self, bytes: u64) {
        self.cur_size.set(self.cur_size.get() + bytes);
    }
}
