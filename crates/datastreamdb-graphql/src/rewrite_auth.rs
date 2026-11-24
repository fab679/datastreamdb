//! Auth query rewriting: static evaluation plus DQL auth queries. Port of
//! `authRewriter` (`addAuthQueries`, `rewriteAuthQueries`, `rewriteRuleNode`)
//! from `dgraph-main/graphql/resolve/query_rewriter.go`.

#![warn(missing_docs)]

use std::collections::HashMap;

use datastreamdb_dql::{FilterTree, Function, GraphQuery};

use crate::auth_rules::{AuthVars, RuleNode, RuleResult};
use crate::op::{BoundField, BoundSchema};
use crate::rewrite_vars::{VariableGenerator, plain_arg, type_func, var_arg};

/// Which operation's rules to enforce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AuthSelector {
    /// Query rules.
    Query,
    /// Password-query rules.
    Password,
    /// Add-mutation rules.
    Add,
    /// Update-mutation rules.
    Update,
    /// Delete-mutation rules.
    Delete,
}

/// Auth rewriting state for one operation.
#[allow(clippy::struct_excessive_bools, reason = "mirrors Go authRewriter")]
pub(crate) struct AuthRewriter<'a> {
    /// JWT auth variables.
    pub auth_variables: AuthVars,
    /// Whether auth queries (not user queries) are being written.
    pub is_writing_auth: bool,
    /// Whether the top level rewrites with a UID variable.
    pub filter_by_uid: bool,
    /// Rule selector.
    pub selector: AuthSelector,
    /// Shared variable generator.
    pub var_gen: &'a mut VariableGenerator,
    /// Current variable name.
    pub var_name: String,
    /// Parent variable name.
    pub parent_var_name: String,
    /// Whether any field in the hierarchy has auth rules.
    pub has_auth_rules: bool,
    /// Whether any field in the hierarchy has `@cascade`.
    pub has_cascade: bool,
    /// Bound schema.
    pub schema: &'a BoundSchema,
}

impl AuthRewriter<'_> {
    /// Whether auth queries are being written.
    pub(crate) fn writing_auth(&self) -> bool {
        self.is_writing_auth
    }

    /// Selects the rule node for a type under the current selector.
    pub(crate) fn select(&self, type_name: &str) -> Option<RuleNode> {
        let datastreamdb_name = self.schema.datastreamdb_name(type_name);
        let auth = self.schema.auth.get(&datastreamdb_name)?;
        let rules = auth.rules.as_ref()?;
        match self.selector {
            AuthSelector::Query => rules.query.clone(),
            AuthSelector::Password => rules.password.clone(),
            AuthSelector::Add => rules.add.clone(),
            AuthSelector::Update => rules.update.clone(),
            AuthSelector::Delete => rules.delete.clone(),
        }
    }

    /// Statically evaluates a type's rules (uncertain while writing auth).
    pub(crate) fn evaluate_static_rules(&self, type_name: &str) -> RuleResult {
        if self.is_writing_auth {
            return RuleResult::Uncertain;
        }
        self.select(type_name)
            .map_or(RuleResult::Uncertain, |node| {
                node.evaluate_static(&self.auth_variables)
            })
    }

    /// Rewrites a type's auth rules into queries plus a filter.
    pub(crate) fn rewrite_auth_queries(
        &mut self,
        type_name: &str,
    ) -> (Vec<GraphQuery>, Option<FilterTree>) {
        if self.is_writing_auth {
            return (Vec::new(), None);
        }
        let node = self.select(type_name);
        let saved_writing = self.is_writing_auth;
        self.is_writing_auth = true;
        let result = self.rewrite_rule_node(type_name, node.as_ref());
        self.is_writing_auth = saved_writing;
        result
    }

    /// Rewrites one rule node into queries plus a filter.
    fn rewrite_rule_node(
        &mut self,
        type_name: &str,
        node: Option<&RuleNode>,
    ) -> (Vec<GraphQuery>, Option<FilterTree>) {
        let Some(node) = node else {
            return (Vec::new(), None);
        };
        if !node.and.is_empty() {
            if node.evaluate_static(&self.auth_variables) == RuleResult::Negative {
                return (Vec::new(), None);
            }
            let (queries, filters) = self.rewrite_node_list(type_name, &node.and);
            return (queries, join_filters(filters, "and"));
        }
        if !node.or.is_empty() {
            let (queries, filters) = self.rewrite_node_list(type_name, &node.or);
            return (queries, join_filters(filters, "or"));
        }
        if let Some(not) = &node.not {
            let (queries, filter) = self.rewrite_rule_node(type_name, Some(not));
            let Some(filter) = filter else {
                return (queries, None);
            };
            return (
                queries,
                Some(FilterTree {
                    op: "not".to_owned(),
                    child: vec![filter],
                    func: None,
                }),
            );
        }
        if let Some(rule) = &node.rule {
            if node.evaluate_static(&self.auth_variables) == RuleResult::Negative {
                return (Vec::new(), None);
            }
            let vars_json: HashMap<String, serde_json::Value> = self.auth_variables.clone();
            let Ok(operation) = self.schema.parse_operation(rule, None, &vars_json) else {
                return (Vec::new(), None);
            };
            let Some(root) = operation.roots.into_iter().next() else {
                return (Vec::new(), None);
            };
            let var_name =
                self.var_gen
                    .next(self.schema, type_name, ("", ""), self.is_writing_auth);
            let rewritten = crate::rewrite_query::rewrite_as_query(self.schema, &root, self);
            let Some(mut first) = rewritten.into_iter().next() else {
                return (Vec::new(), None);
            };
            first.var.clone_from(&var_name);
            "var".clone_into(&mut first.attr);
            if first.cascade.is_empty() {
                first.cascade = vec!["__all__".to_owned()];
            }
            let filter = FilterTree {
                op: String::new(),
                child: Vec::new(),
                func: Some(Function {
                    name: "uid".to_owned(),
                    args: vec![var_arg(&var_name)],
                    ..Function::default()
                }),
            };
            return (vec![first], Some(filter));
        }
        (Vec::new(), None)
    }

    /// Rewrites a list of rule nodes, collecting queries and filters.
    fn rewrite_node_list(
        &mut self,
        type_name: &str,
        nodes: &[RuleNode],
    ) -> (Vec<GraphQuery>, Vec<FilterTree>) {
        let mut queries = Vec::new();
        let mut filters = Vec::new();
        for node in nodes {
            let (mut sub_queries, filter) = self.rewrite_rule_node(type_name, Some(node));
            queries.append(&mut sub_queries);
            if let Some(filter) = filter {
                filters.push(filter);
            }
        }
        (queries, filters)
    }

    /// Appends auth queries restricting a rewritten query to authorized nodes.
    #[allow(clippy::too_many_lines, reason = "exact port of Go addAuthQueries")]
    pub(crate) fn add_auth_queries(
        &mut self,
        type_name: &str,
        mut queries: Vec<GraphQuery>,
        rbac: RuleResult,
    ) -> Vec<GraphQuery> {
        if self.is_writing_auth {
            return queries;
        }
        let var_name = self
            .var_gen
            .next(self.schema, type_name, ("", ""), self.is_writing_auth);
        self.var_name = var_name;
        let (mut auth_queries, mut filter) = self.rewrite_auth_queries(type_name);

        if self.schema.is_interface(type_name) {
            let implementing = self.schema.implementing_types(type_name);
            let mut var_queries = Vec::new();
            let mut filters = Vec::new();
            let mut has_rules = false;
            for object in &implementing {
                let datastreamdb_name = self.schema.datastreamdb_name(object);
                if self
                    .schema
                    .auth
                    .get(&datastreamdb_name)
                    .is_some_and(|auth| auth.rules.is_some())
                {
                    has_rules = true;
                }
                let rbac = self.evaluate_static_rules(object);
                if rbac == RuleResult::Negative {
                    continue;
                }
                let query_var =
                    self.var_gen
                        .next(self.schema, object, ("", ""), self.is_writing_auth);
                let mut var_query = GraphQuery::new();
                "var".clone_into(&mut var_query.attr);
                var_query.var.clone_from(&query_var);
                var_query.func = Some(type_func(object));
                var_queries.push(var_query);
                let saved_var = std::mem::take(&mut self.var_name);
                self.var_name.clone_from(&query_var);
                let saved_writing = self.is_writing_auth;
                self.is_writing_auth = true;
                let node = self.select(object);
                let (obj_queries, obj_filter) = self.rewrite_rule_node(object, node.as_ref());
                self.is_writing_auth = saved_writing;
                self.var_name = saved_var;
                if obj_queries.is_empty() || rbac == RuleResult::Positive {
                    filters.push(FilterTree {
                        op: String::new(),
                        child: Vec::new(),
                        func: Some(Function {
                            name: "uid".to_owned(),
                            args: vec![var_arg(&query_var)],
                            ..Function::default()
                        }),
                    });
                } else {
                    var_queries.extend(obj_queries);
                    if let Some(obj_filter) = obj_filter {
                        filters.push(obj_filter);
                    }
                }
            }
            if has_rules && var_queries.is_empty() {
                let mut empty = GraphQuery::new();
                empty.attr = format!("{}()", queries[0].attr);
                return vec![empty];
            }
            auth_queries.extend(var_queries);
            if !filters.is_empty() {
                filter = Some(FilterTree {
                    op: "or".to_owned(),
                    child: filters,
                    func: None,
                });
            }
            if !has_rules {
                return queries;
            }
        }

        if auth_queries.is_empty() && !self.has_auth_rules {
            return queries;
        }
        if rbac != RuleResult::Uncertain {
            auth_queries.clear();
            filter = None;
        }
        let mut var_query = GraphQuery::new();
        var_query.var.clone_from(&self.var_name);
        "var".clone_into(&mut var_query.attr);
        var_query.func.clone_from(&queries[0].func);
        var_query.filter.clone_from(&queries[0].filter);
        let mut root_query = GraphQuery::new();
        root_query.var.clone_from(&self.parent_var_name);
        "var".clone_into(&mut root_query.attr);
        root_query.func = Some(Function {
            name: "uid".to_owned(),
            args: vec![var_arg(&self.var_name)],
            ..Function::default()
        });
        root_query.filter = filter.map(Box::new);
        queries[0].filter = None;
        if queries[0].cascade.is_empty() {
            root_query.args = std::mem::take(&mut queries[0].args);
            queries[0].args = HashMap::new();
            root_query.order = std::mem::take(&mut queries[0].order);
        }
        queries[0].func = Some(Function {
            name: "uid".to_owned(),
            args: vec![var_arg(&self.parent_var_name)],
            ..Function::default()
        });
        let mut out = queries;
        out.push(root_query);
        out.push(var_query);
        out.extend(auth_queries);
        out
    }

    /// Sets a `uid(varName)` root function for auth-scoped queries.
    pub(crate) fn add_variable_uid_func(&self, query: &mut GraphQuery) {
        let name = if self.var_name.is_empty() {
            self.parent_var_name.clone()
        } else {
            self.var_name.clone()
        };
        query.func = Some(Function {
            name: "uid".to_owned(),
            args: vec![var_arg(&name)],
            ..Function::default()
        });
    }
}

/// Joins filters with an operator (flattening singletons).
fn join_filters(mut filters: Vec<FilterTree>, op: &str) -> Option<FilterTree> {
    match filters.len() {
        0 => None,
        1 => filters.into_iter().next(),
        _ => Some(FilterTree {
            op: op.to_owned(),
            child: std::mem::take(&mut filters),
            func: None,
        }),
    }
}

/// Whether any field in the hierarchy has auth rules.
pub(crate) fn has_auth_rules(
    schema: &BoundSchema,
    field: &BoundField,
    selector: AuthSelector,
) -> bool {
    let mut var_gen = VariableGenerator::new();
    let rewriter = AuthRewriter {
        auth_variables: HashMap::new(),
        is_writing_auth: false,
        filter_by_uid: false,
        selector,
        var_gen: &mut var_gen,
        var_name: String::new(),
        parent_var_name: String::new(),
        has_auth_rules: false,
        has_cascade: false,
        schema,
    };
    has_auth_rules_inner(&rewriter, field)
}

/// Recursive auth-rule probe.
fn has_auth_rules_inner(rewriter: &AuthRewriter<'_>, field: &BoundField) -> bool {
    if rewriter.select(&constructed_for(field)).is_some() {
        return true;
    }
    field
        .children
        .iter()
        .any(|child| has_auth_rules_inner(rewriter, child))
}

/// Whether any field in the hierarchy has `@cascade`.
pub(crate) fn has_cascade_directive(field: &BoundField) -> bool {
    if field.cascade.is_some() {
        return true;
    }
    field.children.iter().any(has_cascade_directive)
}

/// Type a field's auth rules are constructed for (aggregates unwrap).
pub(crate) fn constructed_for(field: &BoundField) -> String {
    if is_aggregate_field(field) {
        return field
            .type_name()
            .strip_suffix("AggregateResult")
            .unwrap_or_else(|| field.type_name())
            .to_owned();
    }
    field.type_name().to_owned()
}

/// Whether the field is an aggregate field (`XAggregate: YAggregateResult`).
pub(crate) fn is_aggregate_field(field: &BoundField) -> bool {
    field.name.ends_with("Aggregate") && field.type_name().ends_with("AggregateResult")
}

/// Parent/selection auth query pair. Port of Go `commonAuthQueryVars`.
pub(crate) struct CommonAuthQueryVars {
    /// Parent query collecting child UIDs.
    pub parent_qry: GraphQuery,
    /// Selection query aggregating filters and auth.
    pub selection_qry: GraphQuery,
}

/// Builds parent/selection auth queries for a nested field.
pub(crate) fn build_common_auth_queries(
    schema: &BoundSchema,
    field: &BoundField,
    var_name: &str,
    parent_var_name: &str,
    parent_selection: &str,
) -> CommonAuthQueryVars {
    let pred = if is_aggregate_field(field) {
        let base = field.name.strip_suffix("Aggregate").unwrap_or(&field.name);
        schema.predicate(&field.object, base).unwrap_or(base)
    } else {
        schema
            .predicate(&field.object, &field.name)
            .unwrap_or(&field.name)
    };
    let mut child = GraphQuery::new();
    pred.clone_into(&mut child.attr);
    var_name.clone_into(&mut child.var);
    let mut parent_qry = GraphQuery::new();
    "var".clone_into(&mut parent_qry.attr);
    parent_qry.func = Some(Function {
        name: "uid".to_owned(),
        args: vec![var_arg(parent_selection)],
        ..Function::default()
    });
    parent_qry.children = vec![child];
    let mut selection_qry = GraphQuery::new();
    parent_var_name.clone_into(&mut selection_qry.var);
    "var".clone_into(&mut selection_qry.attr);
    selection_qry.func = Some(Function {
        name: "uid".to_owned(),
        args: vec![var_arg(var_name)],
        ..Function::default()
    });
    CommonAuthQueryVars {
        parent_qry,
        selection_qry,
    }
}

/// Builds a `uid(var)` leaf filter.
pub(crate) fn uid_var_filter(var_name: &str) -> FilterTree {
    FilterTree {
        op: String::new(),
        child: Vec::new(),
        func: Some(Function {
            name: "uid".to_owned(),
            args: vec![var_arg(var_name)],
            ..Function::default()
        }),
    }
}

/// Builds an `eq(val(pwd), 1)` leaf filter.
pub(crate) fn eq_val_filter() -> FilterTree {
    FilterTree {
        op: String::new(),
        child: Vec::new(),
        func: Some(Function {
            name: "eq".to_owned(),
            args: vec![plain_arg("val(pwd)"), plain_arg("1")],
            ..Function::default()
        }),
    }
}
