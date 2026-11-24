//! Query execution graph. Port of `SubGraph`, `params`, `varValue`,
//! `mathTree`, and the AST-to-graph translation from `query/query.go`
//! (`newGraph`, `treeCopy`, `toSubGraph`, `fill`).
//!
//! Delta from Go: the graph is single-threaded (`Box` links, sequential
//! traversal) instead of goroutine-per-child with channels.

#![warn(missing_docs)]

use std::collections::{HashMap, HashSet};

use datastreamdb_dql::{
    Arg as DqlArg, FacetOrder, FilterTree as DqlFilterTree, Function as DqlFunction, GraphQuery,
    GroupByAttr, LIST_VAR, MathTree as DqlMathTree, RecurseArgs, ShortestPathArgs, UID_VAR,
    VALUE_VAR,
};
use datastreamdb_proto::{
    FacetsList, FilterFunction, FilterTree as ProtoFilterTree, LangList, List as ProtoList, Order,
    ValueList,
};
use datastreamdb_types::{ShardedMap, Val};

use crate::errors::{QueryError, Result};

/// Cascade directive arguments.
#[derive(Clone, Debug, Default)]
pub struct CascadeArgs {
    /// Cascaded fields (`__all__` for bare `@cascade`).
    pub fields: Vec<String>,
    /// Deferred `first` (applied after cascading).
    pub first: i32,
    /// Deferred `offset` (applied after cascading).
    pub offset: i32,
}

/// Value of one query variable.
#[derive(Clone, Debug, Default)]
pub struct VarValue {
    /// UID list (UID variables).
    pub uids: Option<ProtoList>,
    /// UID to value map.
    pub vals: ShardedMap,
    /// Subgraph path from the root to the definition.
    pub path: Vec<*const SubGraph>,
    /// Value rows (`expand(val(x))` support).
    pub str_list: Vec<ValueList>,
}

/// Math expression node (owned port of `mathTree`).
#[derive(Clone, Debug)]
pub struct MathNode {
    /// Operator or function name.
    pub op: String,
    /// Variable leaf.
    pub var: String,
    /// Constant leaf.
    pub constant: Val,
    /// Evaluated values.
    pub val: ShardedMap,
    /// Child expressions.
    pub child: Vec<MathNode>,
}

/// Query function (port of query `Function`, distinct from `dql.Function`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Function {
    /// Function name.
    pub name: String,
    /// Arguments.
    pub args: Vec<DqlArg>,
    /// `gt(count(...), ...)` form.
    pub is_count: bool,
    /// `eq(val(...), ...)` form.
    pub is_value_var: bool,
    /// `eq(len(...), ...)` form.
    pub is_len_var: bool,
}

impl Default for MathNode {
    fn default() -> Self {
        Self {
            op: String::new(),
            var: String::new(),
            constant: Val {
                tid: datastreamdb_types::TypeId::Default,
                value: datastreamdb_types::Value::Null,
            },
            val: ShardedMap::new(),
            child: Vec::new(),
        }
    }
}

/// Execution parameters of one subgraph node.
#[derive(Clone, Debug, Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "exact port of Go params struct"
)]
pub struct Params {
    /// Block alias.
    pub alias: String,
    /// Value of the `first` argument.
    pub count: i32,
    /// Value of the `offset` argument.
    pub offset: i32,
    /// Value of the `after` argument.
    pub after_uid: u64,
    /// `count(...)` instead of values.
    pub do_count: bool,
    /// Return UIDs (debug mode).
    pub get_uid: bool,
    /// Sort instructions.
    pub order: Vec<Order>,
    /// Preferred languages.
    pub langs: Vec<String>,
    /// Requested facets and aliases.
    pub facet: Option<datastreamdb_proto::FacetParams>,
    /// Facet sort instructions.
    pub facets_order: Vec<FacetOrder>,
    /// Defined variable.
    pub var: String,
    /// Facet variable aliases.
    pub facet_var: HashMap<String, String>,
    /// Required variables.
    pub needs_var: Vec<datastreamdb_dql::VarContext>,
    /// Variables passed down to children.
    pub parent_vars: HashMap<String, VarValue>,
    /// Cross-block value variables.
    pub uid_to_val: ShardedMap,
    /// `@normalize`.
    pub normalize: bool,
    /// `@recurse`.
    pub recurse: bool,
    /// Recurse arguments.
    pub recurse_args: RecurseArgs,
    /// Cascade arguments.
    pub cascade: CascadeArgs,
    /// `@ignorereflex`.
    pub ignore_reflex: bool,
    /// Shortest-path endpoint functions.
    pub shortest_path_args: ShortestPathArgs,
    /// Shortest-path source.
    pub from: u64,
    /// Shortest-path destination.
    pub to: u64,
    /// K-shortest route count.
    pub num_paths: i32,
    /// Max path weight.
    pub max_weight: f64,
    /// Min path weight.
    pub min_weight: f64,
    /// Priority-queue size cap.
    pub max_frontier_size: i64,
    /// Max exploration depth.
    pub explore_depth: Option<u64>,
    /// Internal (aggregator/var) subtree.
    pub is_internal: bool,
    /// Ignore this node's results.
    pub ignore_result: bool,
    /// `expand(...)` argument.
    pub expand: String,
    /// `@groupby` subtree.
    pub is_group_by: bool,
    /// Group-by attributes.
    pub groupby_attrs: Vec<GroupByAttr>,
    /// Reflex ancestor stack.
    pub parent_ids: Vec<u64>,
    /// Aggregation-only block.
    pub is_empty: bool,
    /// Expand every language value.
    pub expand_all: bool,
    /// Shortest-path results holder.
    pub shortest: bool,
    /// ACL-visible predicates.
    pub allowed_preds: Vec<String>,
}

/// One node of the execution graph.
#[derive(Clone, Debug, Default)]
pub struct SubGraph {
    /// Read timestamp.
    pub read_ts: u64,
    /// Cache selector.
    pub cache: i32,
    /// Attribute (empty for filter/aggregate nodes).
    pub attr: String,
    /// Attribute missing from schema.
    pub unknown_attr: bool,
    /// Execution parameters.
    pub params: Params,
    /// Count rows (one per source UID).
    pub counts: Vec<u32>,
    /// Value rows (one per source UID).
    pub value_matrix: Vec<ValueList>,
    /// UID rows (one per source UID).
    pub uid_matrix: Vec<ProtoList>,
    /// Facet rows (one per UID row).
    pub facets_matrix: Vec<FacetsList>,
    /// Expanded-value rows.
    pub expand_preds: Vec<ValueList>,
    /// Group-by results (one per UID list).
    pub groupby_res: Vec<crate::groupby::GroupResults>,
    /// Language rows (`expand_all` only).
    pub lang_tags: Vec<LangList>,
    /// Source UIDs.
    pub src_uids: Option<ProtoList>,
    /// Source function (root and filters only).
    pub src_func: Option<Function>,
    /// Filter operator (`and`/`or`/`not`).
    pub filter_op: String,
    /// Filter subgraphs.
    pub filters: Vec<Box<SubGraph>>,
    /// Facet filter tree.
    pub facets_filter: Option<ProtoFilterTree>,
    /// Math expression.
    pub math_exp: Option<MathNode>,
    /// Child subgraphs.
    pub children: Vec<Box<SubGraph>>,
    /// Destination UIDs after filtering.
    pub dest_uids: Option<ProtoList>,
    /// Whether the predicate is list-typed.
    pub list: bool,
    /// Path metadata (shortest path only).
    pub path_meta: Option<crate::shortest::PathMeta>,
}

impl SubGraph {
    /// Whether this subgraph is part of a group-by query.
    #[must_use]
    pub fn is_group_by(&self) -> bool {
        self.params.is_group_by
    }

    /// Whether this subgraph is marked internal.
    #[must_use]
    pub fn is_internal(&self) -> bool {
        self.params.is_internal
    }

    /// Output field name (alias wins over attribute).
    #[must_use]
    pub fn field_name(&self) -> &str {
        if self.params.alias.is_empty() {
            &self.attr
        } else {
            &self.params.alias
        }
    }

    /// Applies `set` to this node and every descendant.
    pub fn recurse(&mut self, set: &mut dyn FnMut(&mut SubGraph)) {
        set(self);
        for child in &mut self.children {
            child.recurse(set);
        }
        for filter in &mut self.filters {
            filter.recurse(set);
        }
    }

    /// Rebuilds destination UIDs from the UID rows (order-aware).
    pub fn update_dest_uids(&mut self) {
        let dest = self.dest_uids.clone().unwrap_or_default();
        let mut included = vec![false; dest.uids.len()];
        for row in &self.uid_matrix {
            for uid in &row.uids {
                if let Ok(idx) = dest.uids.binary_search(uid) {
                    included[idx] = true;
                }
            }
        }
        if let Some(dest) = &mut self.dest_uids {
            let mut kept = Vec::new();
            for (idx, uid) in dest.uids.iter().enumerate() {
                if included[idx] {
                    kept.push(*uid);
                }
            }
            dest.uids = kept;
        }
    }

    /// Whether two subgraphs address the same data (filter merge check).
    #[must_use]
    pub fn is_similar(&self, other: &SubGraph) -> bool {
        if self.attr != other.attr {
            return false;
        }
        if self.params.langs != other.params.langs {
            return false;
        }
        if self.params.do_count {
            return other.params.do_count;
        }
        if other.params.do_count {
            return false;
        }
        if let Some(src_func) = &self.src_func {
            return other
                .src_func
                .as_ref()
                .is_some_and(|other| src_func.name == other.name);
        }
        if !self.filters.is_empty() {
            return !other.filters.is_empty() && self.filters.len() == other.filters.len();
        }
        true
    }
}

/// Query latency breakdown.
#[derive(Clone, Debug, Default)]
pub struct Latency {
    /// Parsing time.
    pub parsing_ns: u64,
    /// Task processing time.
    pub processing_ns: u64,
    /// JSON encoding time.
    pub json_ns: u64,
}

/// Whether the operator is an inequality.
#[must_use]
pub fn is_inequality_fn(name: &str) -> bool {
    matches!(name, "eq" | "le" | "ge" | "gt" | "lt" | "between")
}

/// Whether `uid(...)` carries no variable.
#[must_use]
pub fn is_uid_fn_without_var(func: Option<&DqlFunction>) -> bool {
    func.is_some_and(|func| func.name == "uid" && func.needs_var.is_empty())
}

/// Whether the directive argument is valid here.
#[must_use]
pub fn is_valid_arg(arg: &str) -> bool {
    matches!(
        arg,
        "numpaths"
            | "from"
            | "to"
            | "orderasc"
            | "orderdesc"
            | "first"
            | "offset"
            | "after"
            | "depth"
            | "minweight"
            | "maxweight"
            | "maxfrontiersize"
    )
}

/// Whether the function name is supported.
#[must_use]
pub fn is_valid_func_name(name: &str) -> bool {
    matches!(
        name,
        "anyofterms"
            | "allofterms"
            | "val"
            | "regexp"
            | "anyoftext"
            | "alloftext"
            | "ngram"
            | "has"
            | "uid"
            | "uid_in"
            | "anyof"
            | "allof"
            | "type"
            | "match"
            | "similar_to"
    ) || is_inequality_fn(name)
        || datastreamdb_types::geofilter::is_geo_func(name)
}

/// Copies a math expression into an owned node.
pub fn math_copy(src: &DqlMathTree) -> MathNode {
    MathNode {
        op: src.op.clone(),
        var: src.var.clone(),
        constant: src.constant.clone(),
        val: ShardedMap::new(),
        child: src.child.iter().map(math_copy).collect(),
    }
}

/// Creates the source function plus the `type` alias rewrite.
fn create_src_function(sg: &mut SubGraph, func: &DqlFunction) {
    sg.src_func = Some(Function {
        name: func.name.clone(),
        args: func.args.clone(),
        is_count: func.is_count,
        is_value_var: func.is_value_var,
        is_len_var: func.is_len_var,
    });
    if func.name == "type" {
        "datastreamdb.type".clone_into(&mut sg.attr);
        if let Some(src_func) = &mut sg.src_func {
            "eq".clone_into(&mut src_func.name);
            src_func.is_count = false;
            src_func.is_value_var = false;
            src_func.is_len_var = false;
        }
        return;
    }
    if !func.lang.is_empty() {
        sg.params.langs.push(func.lang.clone());
    }
}

/// Copies a parsed filter into an execution subtree.
///
/// # Errors
///
/// Returns [`QueryError`] on invalid filter trees.
pub fn filter_copy(sg: &mut SubGraph, tree: &DqlFilterTree) -> Result<()> {
    if tree.op.is_empty() {
        let func = tree
            .func
            .as_ref()
            .ok_or_else(|| QueryError::Invalid("filter without operator or function".to_owned()))?;
        sg.attr.clone_from(&func.attr);
        if func.name != "uid" && !is_valid_func_name(&func.name) {
            return Err(QueryError::Invalid(format!(
                "Invalid function name: {}",
                func.name
            )));
        }
        if is_uid_fn_without_var(Some(func)) {
            sg.src_func = Some(Function {
                name: func.name.clone(),
                ..Function::default()
            });
            sg.params.uid_to_val = ShardedMap::new();
            if !func.uid.is_empty() {
                let mut uids = func.uid.clone();
                uids.sort_unstable();
                sg.populate(&uids);
            }
        } else {
            if func.attr == "uid" {
                return Err(QueryError::Invalid("Argument cannot be \"uid\"".to_owned()));
            }
            create_src_function(sg, func);
            sg.params.needs_var.extend(func.needs_var.iter().cloned());
        }
    } else {
        sg.filter_op.clone_from(&tree.op);
    }
    for child in &tree.child {
        let mut child_sg = SubGraph::default();
        filter_copy(&mut child_sg, child)?;
        sg.filters.push(Box::new(child_sg));
    }
    Ok(())
}

/// Dedup key for sibling blocks.
fn unique_key(gchild: &GraphQuery) -> String {
    let mut key = gchild.attr.clone();
    if gchild.func.is_some() {
        use std::fmt::Write as _;
        let _ = write!(key, "{:?}", gchild.func);
    }
    if gchild.attr == "val" {
        if !gchild.var.is_empty() && gchild.var != "val" {
            key = format!("val({})", gchild.var);
        } else if let Some(needs) = gchild.needs_var.first() {
            key = format!("val({})", needs.name);
        }
        if let Some(func) = &gchild.func {
            key += &func.name;
        }
    }
    if gchild.is_count {
        key += "count";
    }
    if !gchild.langs.is_empty() {
        use std::fmt::Write as _;
        let _ = write!(key, "{:?}", gchild.langs);
    }
    if gchild.math_exp.is_some() {
        key = format!("val({})", gchild.var);
    }
    if gchild.is_groupby {
        key += "groupby";
    }
    key
}

/// Copies parsed children into execution subtrees (dedup by `unique_key`).
///
/// # Errors
///
/// Returns [`QueryError`] on invalid child blocks.
///
/// # Panics
///
/// Panics if an aggregator child lacks its function node.
#[allow(clippy::too_many_lines, reason = "exact port of Go treeCopy")]
pub fn tree_copy(gq: &GraphQuery, sg: &mut SubGraph) -> Result<()> {
    let mut attrs_seen = HashSet::new();
    for gchild in &gq.children {
        if sg.params.alias == "shortest" && !gchild.expand.is_empty() {
            return Err(QueryError::Invalid(
                "expand() not allowed inside shortest".to_owned(),
            ));
        }

        let key = if gchild.alias.is_empty() {
            unique_key(gchild)
        } else {
            gchild.alias.clone()
        };
        if !attrs_seen.insert(key.clone()) {
            return Err(QueryError::Invalid(format!(
                "{key} not allowed multiple times in same sub-query."
            )));
        }

        let mut args = Params {
            alias: gchild.alias.clone(),
            expand: gchild.expand.clone(),
            facet: gchild.facets.clone(),
            facets_order: gchild.facets_order.clone(),
            facet_var: gchild.facet_var.clone(),
            get_uid: sg.params.get_uid,
            ignore_reflex: sg.params.ignore_reflex,
            langs: gchild.langs.clone(),
            needs_var: gchild.needs_var.clone(),
            normalize: gchild.normalize || sg.params.normalize,
            order: gchild.order.clone(),
            var: gchild.var.clone(),
            groupby_attrs: gchild.groupby_attrs.clone(),
            is_group_by: gchild.is_groupby,
            is_internal: gchild.is_internal,
            cascade: CascadeArgs::default(),
            ..Params::default()
        };

        if !sg.params.cascade.fields.is_empty() {
            args.cascade
                .fields
                .extend(sg.params.cascade.fields.iter().cloned());
        }
        if !gchild.cascade.is_empty() {
            args.cascade.fields.clone_from(&gchild.cascade);
        }
        // Own the child so cascade pagination can strip its args (like Go
        // mutating `gq.Args`) before `fill_args` and the recursion below.
        let mut owned = gchild.clone();
        if !args.cascade.fields.is_empty() {
            add_cascade_pagination_arguments(&mut args, &mut owned);
        }

        if gchild.is_count {
            if !gchild.children.is_empty() {
                return Err(QueryError::Invalid(
                    "Node with count cannot have child attributes".to_owned(),
                ));
            }
            args.do_count = true;
        }

        for arg in owned.args.keys() {
            if !is_valid_arg(arg) {
                return Err(QueryError::Invalid(format!("Invalid argument: {arg}")));
            }
        }
        fill_args(&mut args, &owned)?;

        if !args.order.is_empty() && !args.facets_order.is_empty() {
            return Err(QueryError::Invalid(
                "Cannot specify order at both args and facets".to_owned(),
            ));
        }

        let mut dst = SubGraph {
            attr: gchild.attr.clone(),
            params: args,
            ..SubGraph::default()
        };
        if let Some(math_exp) = &gchild.math_exp {
            dst.math_exp = Some(math_copy(math_exp));
        }

        if gchild
            .func
            .as_ref()
            .is_some_and(|func| func.is_aggregator() || func.is_password_verifier())
        {
            let func = gchild.func.as_ref().expect("function");
            if !gchild.children.is_empty() {
                return Err(QueryError::Invalid(format!(
                    "Node with {:?} cant have child attr",
                    func.name
                )));
            }
            if gchild.filter.is_some() {
                return Err(QueryError::Invalid(format!(
                    "Node with {:?} cant have filter, please place the filter on the upper level",
                    func.name
                )));
            }
            if func.attr == "uid" {
                return Err(QueryError::Invalid("Argument cannot be \"uid\"".to_owned()));
            }
            create_src_function(&mut dst, func);
        }

        if let Some(filter) = &gchild.filter {
            let mut dst_filter = SubGraph::default();
            filter_copy(&mut dst_filter, filter)?;
            dst.filters.push(Box::new(dst_filter));
        }

        if let Some(facets_filter) = &gchild.facets_filter {
            dst.facets_filter = Some(to_facets_filter(facets_filter)?);
        }

        sg.children.push(Box::new(dst));
        let last = sg.children.len() - 1;
        tree_copy(&owned, &mut sg.children[last])?;
    }
    Ok(())
}

/// Moves pagination arguments into the cascade scope.
fn add_cascade_pagination_arguments(args: &mut Params, gq: &mut GraphQuery) {
    // Go ignores parse errors and deletes the keys either way.
    args.cascade.first = gq
        .args
        .remove("first")
        .and_then(|first| first.parse().ok())
        .unwrap_or(0);
    args.cascade.offset = gq
        .args
        .remove("offset")
        .and_then(|offset| offset.parse().ok())
        .unwrap_or(0);
}

/// Fills numeric directive arguments.
fn fill_args(args: &mut Params, gq: &GraphQuery) -> Result<()> {
    if let Some(offset) = gq.args.get("offset") {
        args.offset = parse_int_arg(offset)
            .map_err(|err| QueryError::Invalid(format!("invalid offset argument: {err}")))?;
    }
    if let Some(after) = gq.args.get("after") {
        args.after_uid = parse_uint_arg(after)
            .map_err(|err| QueryError::Invalid(format!("invalid after argument: {err}")))?;
    }

    if args.alias == "shortest" {
        if let Some(depth) = gq.args.get("depth") {
            args.explore_depth =
                Some(parse_uint_arg(depth).map_err(|err| {
                    QueryError::Invalid(format!("invalid depth argument: {err}"))
                })?);
        }
        if let Some(paths) = gq.args.get("numpaths") {
            let num_paths = parse_uint_arg(paths)
                .map_err(|err| QueryError::Invalid(format!("invalid numpaths argument: {err}")))?;
            if num_paths > i64::MAX as u64 {
                return Err(QueryError::Invalid(format!(
                    "numpaths value {num_paths} exceeds maximum allowed value {}",
                    i64::MAX
                )));
            }
            args.num_paths = i32::try_from(num_paths).unwrap_or(i32::MAX);
        }
        args.max_weight = match gq.args.get("maxweight") {
            Some(weight) => weight.parse().map_err(|_| {
                QueryError::Invalid(format!("invalid maxweight argument: {weight}"))
            })?,
            None => f64::MAX,
        };
        args.min_weight = match gq.args.get("minweight") {
            Some(weight) => weight.parse().map_err(|_| {
                QueryError::Invalid(format!("invalid minweight argument: {weight}"))
            })?,
            None => -f64::MAX,
        };
        args.max_frontier_size = match gq.args.get("maxfrontiersize") {
            Some(size) => size.parse().map_err(|_| {
                QueryError::Invalid(format!("invalid maxfrontiersize argument: {size}"))
            })?,
            None => i64::MAX,
        };

        if gq.shortest_path_args.from.is_none() || gq.shortest_path_args.to.is_none() {
            return Err(QueryError::Invalid(
                "from/to can't be nil for shortest path".to_owned(),
            ));
        }
        if let Some(from) = gq
            .shortest_path_args
            .from
            .as_ref()
            .and_then(|from| from.uid.first())
        {
            args.from = *from;
        }
        if let Some(to) = gq
            .shortest_path_args
            .to
            .as_ref()
            .and_then(|to| to.uid.first())
        {
            args.to = *to;
        }
    }

    if let Some(first) = gq.args.get("first") {
        args.count = parse_int_arg(first)
            .map_err(|err| QueryError::Invalid(format!("invalid first argument: {err}")))?;
    }
    Ok(())
}

/// Parses a base-0 i32 directive argument (Go `ParseInt(s, 0, 32)`).
fn parse_int_arg(text: &str) -> std::result::Result<i32, String> {
    parse_int_auto(text)
        .and_then(|value| i32::try_from(value).map_err(|_| format!("out of range: {text}")))
}

/// Parses a base-0 u64 directive argument.
fn parse_uint_arg(text: &str) -> std::result::Result<u64, String> {
    if let Some(hex) = text.strip_prefix("0x") {
        return u64::from_str_radix(hex, 16).map_err(|_| format!("invalid number: {text}"));
    }
    if text.len() > 1 && text.starts_with('0') {
        return u64::from_str_radix(text, 8).map_err(|_| format!("invalid number: {text}"));
    }
    text.parse().map_err(|_| format!("invalid number: {text}"))
}

/// Parses a base-0 i64 (Go `ParseInt(s, 0, 64)`).
fn parse_int_auto(text: &str) -> std::result::Result<i64, String> {
    let bad = || format!("invalid number: {text}");
    let (negative, digits) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text.strip_prefix('+').unwrap_or(text)),
    };
    if let Some(hex) = digits.strip_prefix("0x") {
        let magnitude = i64::from_str_radix(hex, 16).map_err(|_| bad())?;
        return Ok(if negative { -magnitude } else { magnitude });
    }
    if digits.len() > 1 && digits.starts_with('0') {
        let magnitude = i64::from_str_radix(digits, 8).map_err(|_| bad())?;
        return Ok(if negative { -magnitude } else { magnitude });
    }
    text.parse().map_err(|_| bad())
}

/// Converts a parsed block into an execution subtree.
///
/// # Errors
///
/// Returns [`QueryError`] on invalid blocks.
pub fn to_sub_graph(gq: &GraphQuery, debug: bool) -> Result<SubGraph> {
    let mut sg = new_graph(gq, debug)?;
    tree_copy(gq, &mut sg)?;
    Ok(sg)
}

/// Builds the root execution node for one block.
///
/// # Errors
///
/// Returns [`QueryError`] on invalid arguments.
pub fn new_graph(gq: &GraphQuery, debug: bool) -> Result<SubGraph> {
    let mut args = Params {
        alias: gq.alias.clone(),
        cascade: CascadeArgs {
            fields: gq.cascade.clone(),
            ..CascadeArgs::default()
        },
        get_uid: debug,
        ignore_reflex: gq.ignore_reflex,
        is_empty: gq.is_empty,
        langs: gq.langs.clone(),
        needs_var: gq.needs_var.clone(),
        normalize: gq.normalize,
        order: gq.order.clone(),
        parent_vars: HashMap::new(),
        recurse: gq.recurse,
        recurse_args: gq.recurse_args.clone(),
        shortest_path_args: gq.shortest_path_args.clone(),
        var: gq.var.clone(),
        groupby_attrs: gq.groupby_attrs.clone(),
        is_group_by: gq.is_groupby,
        allowed_preds: gq.allowed_preds.clone(),
        ..Params::default()
    };

    let mut gq = gq.clone();
    if !args.cascade.fields.is_empty() {
        add_cascade_pagination_arguments(&mut args, &mut gq);
    }

    for arg in gq.args.keys() {
        if !is_valid_arg(arg) {
            return Err(QueryError::Invalid(format!("Invalid argument: {arg}")));
        }
    }
    fill_args(&mut args, &gq)
        .map_err(|err| QueryError::Invalid(format!("while filling args: {err}")))?;

    let mut sg = SubGraph {
        params: args,
        ..SubGraph::default()
    };

    if let Some(func) = &gq.func {
        if func.attr != "uid" {
            sg.attr.clone_from(&func.attr);
        } else if func.uid.is_empty() {
            return Err(QueryError::Invalid("Argument cannot be \"uid\"".to_owned()));
        }
        if !is_valid_func_name(&func.name) {
            return Err(QueryError::Invalid(format!(
                "Invalid function name: {}",
                func.name
            )));
        }
        create_src_function(&mut sg, func);
    }

    if is_uid_fn_without_var(gq.func.as_ref()) && !gq.uid.is_empty() {
        sg.populate(&gq.uid);
    }

    if let Some(filter) = &gq.filter {
        let mut sgf = SubGraph::default();
        filter_copy(&mut sgf, filter)
            .map_err(|err| QueryError::Invalid(format!("while copying filter: {err}")))?;
        sg.filters.push(Box::new(sgf));
    }
    if let Some(facets_filter) = &gq.facets_filter {
        sg.facets_filter = Some(to_facets_filter(facets_filter).map_err(|err| {
            QueryError::Invalid(format!("while converting to facets filter: {err}"))
        })?);
    }
    Ok(sg)
}

/// Converts a parsed facet filter into its wire form.
///
/// # Errors
///
/// Returns [`QueryError`] on invalid facet filters.
pub fn to_facets_filter(tree: &DqlFilterTree) -> Result<ProtoFilterTree> {
    if tree
        .func
        .as_ref()
        .is_some_and(|func| !func.needs_var.is_empty())
    {
        return Err(QueryError::Invalid(
            "Variables not supported in pb.FilterTree".to_owned(),
        ));
    }
    let mut out = ProtoFilterTree {
        op: tree.op.clone(),
        ..ProtoFilterTree::default()
    };
    for child in &tree.child {
        out.children.push(to_facets_filter(child)?);
    }
    if let Some(func) = &tree.func {
        out.func = Some(FilterFunction {
            key: func.attr.clone(),
            name: func.name.clone(),
            args: func.args.iter().map(|arg| arg.value.clone()).collect(),
        });
    }
    Ok(out)
}

impl SubGraph {
    /// Installs sorted source UIDs plus the single-row matrix.
    pub fn populate(&mut self, uids: &[u64]) {
        let mut sorted = uids.to_vec();
        sorted.sort_unstable();
        self.uid_matrix = vec![ProtoList {
            uids: sorted.clone(),
        }];
        self.src_uids = Some(ProtoList { uids: sorted });
    }
}

/// Whether the variable kind needs UID lists.
#[must_use]
pub fn is_list_var(kind: i32) -> bool {
    kind == LIST_VAR
}

/// Whether the variable kind needs value lists.
#[must_use]
pub fn is_value_var(kind: i32) -> bool {
    kind == VALUE_VAR
}

/// Whether the variable kind needs UID sets.
#[must_use]
pub fn is_uid_var(kind: i32) -> bool {
    kind == UID_VAR
}
