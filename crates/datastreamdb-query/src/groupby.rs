//! Group-by aggregation. Port of `query/groupby.go`: dedup maps,
//! group formation, per-group aggregation, and grouped variables.

#![warn(missing_docs)]

use std::collections::HashMap;

use datastreamdb_proto::List as ProtoList;
use datastreamdb_types::{ShardedMap, TypeId, Val, Value, marshal};

use crate::aggregator::{Aggregator, convert_to};
use crate::errors::{QueryError, Result};
use crate::subgraph::{SubGraph, VarValue};
use crate::vars::SgPath;

/// One key or aggregate cell.
#[derive(Clone, Debug)]
pub struct GroupPair {
    /// Cell value.
    pub key: Val,
    /// Output attribute.
    pub attr: String,
}

/// One group: keys, aggregates, and member UIDs.
#[derive(Clone, Debug)]
pub struct Group {
    /// Group keys.
    pub keys: Vec<GroupPair>,
    /// Aggregates.
    pub aggregates: Vec<GroupPair>,
    /// Member UIDs.
    pub uids: Vec<u64>,
}

/// All groups of one node.
#[derive(Clone, Debug, Default)]
pub struct GroupResults {
    /// Groups.
    pub group: Vec<Group>,
}

/// One dedup bucket: member entities plus the representative key.
#[derive(Clone, Debug)]
struct GroupElements {
    /// Member entities.
    entities: Vec<u64>,
    /// Representative key.
    key: Val,
}

/// Dedup buckets of one attribute.
#[derive(Clone, Debug, Default)]
struct Uniq {
    /// Buckets by string key.
    elements: HashMap<String, GroupElements>,
    /// Attribute.
    attr: String,
}

/// Per-attribute dedup state.
#[derive(Clone, Debug, Default)]
struct Dedup {
    /// Attribute buckets.
    groups: Vec<Uniq>,
}

impl Dedup {
    /// Bucket for `attr`, creating it on demand.
    fn group(&mut self, attr: &str) -> &mut Uniq {
        if let Some(pos) = self.groups.iter().rposition(|it| it.attr == attr) {
            return &mut self.groups[pos];
        }
        self.groups.push(Uniq {
            attr: attr.to_owned(),
            elements: HashMap::new(),
        });
        self.groups.last_mut().expect("pushed group")
    }

    /// Records a value for one entity.
    fn add_value(&mut self, attr: &str, value: Val, uid: u64) {
        let str_key = if value.tid == TypeId::Uid {
            match value.value {
                Value::Uid(id) => id.to_string(),
                _ => return,
            }
        } else {
            match marshal(&value, TypeId::String) {
                Ok(text) => match text.value {
                    Value::String(text) => text,
                    _ => return,
                },
                Err(_) => return,
            }
        };
        let bucket = self.group(attr);
        bucket
            .elements
            .entry(str_key)
            .or_insert_with(|| GroupElements {
                key: value,
                entities: Vec::new(),
            })
            .entities
            .push(uid);
    }
}

impl Group {
    /// Aggregates one child into this group.
    fn aggregate_child(&mut self, child: &SubGraph) -> Result<()> {
        let mut field_name = child.params.alias.clone();
        if child.params.do_count {
            if child.attr != "uid" {
                return Err(QueryError::Invalid(
                    "Only uid predicate is allowed in count within groupby".to_owned(),
                ));
            }
            if field_name.is_empty() {
                "count".clone_into(&mut field_name);
            }
            self.aggregates.push(GroupPair {
                attr: field_name,
                key: Val {
                    tid: TypeId::Int,
                    value: Value::Int(i64::try_from(self.uids.len()).unwrap_or(i64::MAX)),
                },
            });
            return Ok(());
        }
        if child
            .src_func
            .as_ref()
            .is_some_and(|func| crate::aggregator::is_aggregator_fn(&func.name))
        {
            if field_name.is_empty() {
                field_name = format!(
                    "{}({})",
                    child.src_func.as_ref().expect("src func").name,
                    child.attr
                );
            }
            let final_val = aggregate_group(self, child)?;
            self.aggregates.push(GroupPair {
                attr: field_name,
                key: final_val,
            });
        }
        Ok(())
    }
}

/// Aggregates a child's values over the group members.
fn aggregate_group(group: &Group, child: &SubGraph) -> Result<Val> {
    let mut agg = Aggregator::new(
        &child
            .src_func
            .as_ref()
            .map(|func| func.name.clone())
            .unwrap_or_default(),
    );
    for uid in &group.uids {
        let src = child.src_uids.as_ref().map_or(&[][..], |src| &src.uids);
        let Ok(idx) = src.binary_search(uid) else {
            continue;
        };
        let Some(row) = child.value_matrix.get(idx) else {
            continue;
        };
        if row.values.is_empty() {
            continue;
        }
        let Ok(val) = crate::vars::convert_with_best_effort(&row.values[0], &child.attr) else {
            continue;
        };
        agg.apply(&val)?;
    }
    agg.value()
}

impl GroupResults {
    /// Forms the cross product of attribute buckets.
    fn form_groups(&mut self, dedup: &Dedup, current: &[u64], pairs: &mut Vec<GroupPair>) {
        if dedup.groups.is_empty() || (!pairs.is_empty() && current.is_empty()) {
            return;
        }
        if pairs.len() == dedup.groups.len() {
            self.group.push(Group {
                keys: pairs.clone(),
                aggregates: Vec::new(),
                uids: current.to_vec(),
            });
            return;
        }
        for bucket in dedup.groups[pairs.len()].elements.values() {
            pairs.push(GroupPair {
                key: bucket.key.clone(),
                attr: dedup.groups[pairs.len()].attr.clone(),
            });
            let mut merged = Vec::new();
            if pairs.len() == 1 {
                merged.extend_from_slice(&bucket.entities);
            } else {
                datastreamdb_algo::uidlist::intersect_with(current, &bucket.entities, &mut merged);
            }
            let len = pairs.len();
            self.form_groups(dedup, &merged, pairs);
            pairs.truncate(len - 1);
        }
    }
}

/// Sorts groups deterministically.
fn group_less(left: &Group, right: &Group) -> bool {
    if left.uids.len() != right.uids.len() {
        return left.uids.len() < right.uids.len();
    }
    if left.keys.len() != right.keys.len() {
        return left.keys.len() < right.keys.len();
    }
    if left.aggregates.len() != right.aggregates.len() {
        return left.aggregates.len() < right.aggregates.len();
    }
    for (lkey, rkey) in left.keys.iter().zip(right.keys.iter()) {
        if let Ok(less) = datastreamdb_types::compare::less_than(&lkey.key, &rkey.key) {
            if less {
                return true;
            }
            if datastreamdb_types::compare::less_than(&rkey.key, &lkey.key).unwrap_or(false) {
                return false;
            }
        }
    }
    for (lagg, ragg) in left.aggregates.iter().zip(right.aggregates.iter()) {
        if let Ok(less) = datastreamdb_types::compare::less_than(&lagg.key, &ragg.key) {
            if less {
                return true;
            }
            if datastreamdb_types::compare::less_than(&ragg.key, &lagg.key).unwrap_or(false) {
                return false;
            }
        }
    }
    false
}

impl SubGraph {
    /// Forms groups over one UID list and aggregates them.
    fn form_result(&self, uids: &ProtoList) -> Result<GroupResults> {
        let mut dedup = Dedup::default();
        let mut res = GroupResults::default();

        for child in &self.children {
            if !child.params.ignore_result {
                continue;
            }
            let attr = if child.params.alias.is_empty() {
                child.attr.clone()
            } else {
                child.params.alias.clone()
            };
            if child
                .dest_uids
                .as_ref()
                .is_some_and(|dest| !dest.uids.is_empty())
            {
                for (i, row) in child.uid_matrix.iter().enumerate() {
                    let Some(src_uid) = child
                        .src_uids
                        .as_ref()
                        .and_then(|src| src.uids.get(i).copied())
                    else {
                        continue;
                    };
                    if uids.uids.binary_search(&src_uid).is_err() {
                        continue;
                    }
                    for uid in &row.uids {
                        dedup.add_value(
                            &attr,
                            Val {
                                tid: TypeId::Uid,
                                value: Value::Uid(*uid),
                            },
                            src_uid,
                        );
                    }
                }
            } else {
                for (i, row) in child.value_matrix.iter().enumerate() {
                    let Some(src_uid) = child
                        .src_uids
                        .as_ref()
                        .and_then(|src| src.uids.get(i).copied())
                    else {
                        continue;
                    };
                    if row.values.is_empty() || uids.uids.binary_search(&src_uid).is_err() {
                        continue;
                    }
                    let Ok(val) = convert_to(&row.values[0]) else {
                        continue;
                    };
                    dedup.add_value(&attr, val, src_uid);
                }
            }
        }

        res.form_groups(&dedup, &[], &mut Vec::new());

        for child in &self.children {
            if child.params.ignore_result {
                continue;
            }
            for group in &mut res.group {
                match group.aggregate_child(child) {
                    Ok(()) | Err(QueryError::EmptyAggregation) => {}
                    Err(err) => return Err(err),
                }
            }
        }
        res.group.sort_by(|left, right| {
            if group_less(left, right) {
                std::cmp::Ordering::Less
            } else if group_less(right, left) {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Equal
            }
        });
        Ok(res)
    }

    /// Groups over the whole matrix to assign grouped variables.
    #[allow(clippy::too_many_lines, reason = "exact port of Go fillGroupedVars")]
    fn fill_grouped_vars(
        &mut self,
        done_vars: &mut HashMap<String, VarValue>,
        path: &SgPath,
    ) -> Result<()> {
        if !self
            .children
            .iter()
            .any(|child| !child.params.var.is_empty())
        {
            return Ok(());
        }

        let mut path_node: Option<*const SubGraph> = None;
        let mut dedup = Dedup::default();
        for child in &self.children {
            if !child.params.ignore_result {
                continue;
            }
            let attr = if child.params.alias.is_empty() {
                child.attr.clone()
            } else {
                child.params.alias.clone()
            };
            if child
                .dest_uids
                .as_ref()
                .is_some_and(|dest| !dest.uids.is_empty())
            {
                for (i, row) in child.uid_matrix.iter().enumerate() {
                    let Some(src_uid) = child
                        .src_uids
                        .as_ref()
                        .and_then(|src| src.uids.get(i).copied())
                    else {
                        continue;
                    };
                    for uid in &row.uids {
                        dedup.add_value(
                            &attr,
                            Val {
                                tid: TypeId::Uid,
                                value: Value::Uid(*uid),
                            },
                            src_uid,
                        );
                    }
                }
                path_node = Some(std::ptr::from_ref::<SubGraph>(child.as_ref()));
            } else {
                for (i, row) in child.value_matrix.iter().enumerate() {
                    let Some(src_uid) = child
                        .src_uids
                        .as_ref()
                        .and_then(|src| src.uids.get(i).copied())
                    else {
                        continue;
                    };
                    if row.values.is_empty() {
                        continue;
                    }
                    let Ok(val) = convert_to(&row.values[0]) else {
                        continue;
                    };
                    dedup.add_value(&attr, val, src_uid);
                }
            }
        }

        let mut res = GroupResults::default();
        res.form_groups(&dedup, &[], &mut Vec::new());

        for child in &self.children {
            for group in &mut res.group {
                match group.aggregate_child(child) {
                    Ok(()) | Err(QueryError::EmptyAggregation) => {}
                    Err(err) => return Err(err),
                }
            }
            if child.params.var.is_empty() {
                continue;
            }
            let var = child.params.var.clone();
            let mut temp = ShardedMap::new();
            for group in &res.group {
                if group.keys.is_empty() {
                    continue;
                }
                if group.keys.len() > 1 {
                    return Err(QueryError::Invalid(format!(
                        "Expected one UID for var in groupby but got: {}",
                        group.keys.len()
                    )));
                }
                let Value::Uid(uid) = group.keys[0].key.value else {
                    return Err(QueryError::Invalid(
                        "Vars can be assigned only when grouped by UID attribute".to_owned(),
                    ));
                };
                if let Some(last) = group.aggregates.last() {
                    temp.set(uid, last.key.clone());
                }
            }
            let mut var_path = path.clone();
            if let Some(node) = path_node {
                var_path.push(node);
            }
            done_vars.insert(
                var,
                VarValue {
                    vals: temp,
                    path: var_path,
                    ..VarValue::default()
                },
            );
        }
        Ok(())
    }

    /// Runs group-by over every UID list, then assigns grouped variables.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] on aggregation or grouping failure.
    pub fn process_group_by(
        &mut self,
        done_vars: &mut HashMap<String, VarValue>,
        path: &SgPath,
    ) -> Result<()> {
        for row in self.uid_matrix.clone() {
            let res = self.form_result(&row)?;
            self.groupby_res.push(res);
        }
        self.fill_grouped_vars(done_vars, path)?;
        self.children.clear();
        Ok(())
    }
}
