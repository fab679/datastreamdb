//! DQL abstract syntax tree. Port of the `GraphQuery` family in `parser.go`.

#![warn(missing_docs)]

use std::collections::HashMap;

use datastreamdb_proto::{FacetParams, Order, SchemaRequest};

use super::math::MathTree;

/// Function name constants.
pub const UID_FUNC: &str = "uid";
/// `val` function name.
pub const VALUE_FUNC: &str = "val";
/// `type` function name.
pub const TYPE_FUNC: &str = "type";
/// `len` function name.
pub const LEN_FUNC: &str = "len";
/// `count` function name.
pub const COUNT_FUNC: &str = "count";
/// `uid_in` function name.
pub const UID_IN_FUNC: &str = "uid_in";
/// `similar_to` function name.
pub const SIMILAR_TO_FUNC: &str = "similar_to";

/// Any variable kind.
pub const ANY_VAR: i32 = 0;
/// UID variable kind.
pub const UID_VAR: i32 = 1;
/// Value variable kind.
pub const VALUE_VAR: i32 = 2;
/// List variable kind.
pub const LIST_VAR: i32 = 3;

/// Parsed query tree. Converted to the executor plan by `datastreamdb-query`.
#[derive(Clone, Debug, Default, PartialEq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "AST mirror of Go's GraphQuery"
)]
pub struct GraphQuery {
    /// Root UIDs (`uid(...)` at root).
    pub uid: Vec<u64>,
    /// Predicate name.
    pub attr: String,
    /// Language tags.
    pub langs: Vec<String>,
    /// Block alias.
    pub alias: String,
    /// `count(...)` block.
    pub is_count: bool,
    /// Internal (aggregator/expand/val) block.
    pub is_internal: bool,
    /// `@groupby` block.
    pub is_groupby: bool,
    /// Defined variable (`var as ...`).
    pub var: String,
    /// Variables this block needs.
    pub needs_var: Vec<VarContext>,
    /// Root function.
    pub func: Option<Function>,
    /// Expand target (`_all_` or a variable/type list).
    pub expand: String,
    /// Directive arguments.
    pub args: HashMap<String, String>,
    /// Sort instructions.
    pub order: Vec<Order>,
    /// Child blocks.
    pub children: Vec<GraphQuery>,
    /// `@filter` tree.
    pub filter: Option<Box<FilterTree>>,
    /// `math(...)` expression.
    pub math_exp: Option<MathTree>,
    /// `@normalize`.
    pub normalize: bool,
    /// `@recurse`.
    pub recurse: bool,
    /// `@recurse` arguments.
    pub recurse_args: RecurseArgs,
    /// Shortest-path arguments.
    pub shortest_path_args: ShortestPathArgs,
    /// `@cascade` predicates (`__all__` for bare cascade).
    pub cascade: Vec<String>,
    /// `@ignorereflex`.
    pub ignore_reflex: bool,
    /// `@facets` selection.
    pub facets: Option<FacetParams>,
    /// `@facets` filter.
    pub facets_filter: Option<Box<FilterTree>>,
    /// `@groupby` attributes.
    pub groupby_attrs: Vec<GroupByAttr>,
    /// Facet variable mappings (facet name to variable).
    pub facet_var: HashMap<String, String>,
    /// Facet orderings.
    pub facets_order: Vec<FacetOrder>,
    /// Allowed predicates (ACL-filtered queries).
    pub allowed_preds: Vec<String>,
    /// Fragment reference name (empty unless this is a spread).
    pub(crate) fragment: String,
    /// Block without a starting function (aggregation-only).
    pub is_empty: bool,
}

impl GraphQuery {
    /// Empty query block (rewriters fill fields explicitly).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether this node is a fragment reference/spread.
    #[must_use]
    pub fn is_fragment(&self) -> bool {
        !self.fragment.is_empty()
    }

    /// Debug print of the tree (port of `DebugPrint`, via tracing).
    pub fn debug_print(&self, prefix: &str) {
        tracing::info!(
            "{}[{:x} {:?} {:?}]\n",
            prefix,
            self.uid_hash(),
            self.attr,
            self.alias
        );
        for child in &self.children {
            child.debug_print(&format!("{prefix}|->"));
        }
    }

    /// FNV-ish hash of UIDs for debug output (Go prints `%x` of the slice).
    fn uid_hash(&self) -> u64 {
        self.uid
            .iter()
            .fold(0u64, |h, u| h.wrapping_mul(31).wrapping_add(*u))
    }

    /// Expands fragment spreads against the fragment map (cycle-checked).
    pub(crate) fn expand_fragments(
        &mut self,
        fmap: &HashMap<String, FragmentNode>,
    ) -> Result<(), crate::DqlError> {
        // Go tracks DFS state on the shared nodes; we thread explicit
        // visiting/done sets with identical cycle and reuse semantics.
        let mut visiting = std::collections::HashSet::new();
        let mut done: HashMap<String, Vec<GraphQuery>> = HashMap::new();
        self.expand_inner(fmap, &mut visiting, &mut done)
    }

    fn expand_inner(
        &mut self,
        fmap: &HashMap<String, FragmentNode>,
        visiting: &mut std::collections::HashSet<String>,
        done: &mut HashMap<String, Vec<GraphQuery>>,
    ) -> Result<(), crate::DqlError> {
        // Copy children to preserve order while replacing spreads.
        let mut expanded = Vec::with_capacity(self.children.len());
        for mut child in std::mem::take(&mut self.children) {
            if child.is_fragment() {
                let name = child.fragment.clone();
                if visiting.contains(&name) {
                    return Err(crate::DqlError::message(format!("Cycle detected: {name}")));
                }
                if let Some(cached) = done.get(&name) {
                    expanded.extend(cached.clone());
                    continue;
                }
                let Some(node) = fmap.get(&name) else {
                    return Err(crate::DqlError::message(format!(
                        "Missing fragment: {name}"
                    )));
                };
                visiting.insert(name.clone());
                let mut body = node.gq.clone();
                body.expand_inner(fmap, visiting, done)?;
                visiting.remove(&name);
                done.insert(name, body.children.clone());
                expanded.extend(body.children);
            } else {
                child.expand_inner(fmap, visiting, done)?;
                expanded.push(child);
            }
        }
        self.children = expanded;
        Ok(())
    }
}

/// Fragment definition node.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct FragmentNode {
    /// Fragment name.
    pub name: String,
    /// Fragment body.
    pub gq: GraphQuery,
}

/// `@recurse` arguments.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RecurseArgs {
    /// Maximum depth.
    pub depth: u64,
    /// Whether loops are allowed.
    pub allow_loop: bool,
    pub(crate) var_map: HashMap<String, String>,
}

/// Shortest-path arguments.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ShortestPathArgs {
    /// `from` endpoint (UID or `uid()` function).
    pub from: Option<Function>,
    /// `to` endpoint (UID or `uid()` function).
    pub to: Option<Function>,
}

/// One `@groupby` attribute.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GroupByAttr {
    /// Attribute name.
    pub attr: String,
    /// Alias.
    pub alias: String,
    /// Language tags.
    pub langs: Vec<String>,
}

/// Ordering for one facet key.
#[derive(Clone, Debug, PartialEq)]
pub struct FacetOrder {
    /// Facet key.
    pub key: String,
    /// Descending order.
    pub desc: bool,
}

/// Variable needed to complete a query.
#[derive(Clone, Debug, PartialEq)]
pub struct VarContext {
    /// Variable name.
    pub name: String,
    /// Variable kind (`UID_VAR` / `VALUE_VAR` / ...).
    pub typ: i32,
}

/// Filter tree: `op` + children on inner nodes, `func` on leaves.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FilterTree {
    /// Operator (`and` / `or` / `not`).
    pub op: String,
    /// Child filters.
    pub child: Vec<FilterTree>,
    /// Leaf function.
    pub func: Option<Function>,
}

/// One function argument.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Arg {
    /// Argument value.
    pub value: String,
    /// Whether the argument is `val(...)`.
    pub is_value_var: bool,
    /// Whether the argument is a `$...` DQL variable.
    pub is_dql_var: bool,
}

/// DQL function (`eq`, `uid`, `has`, ...).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Function {
    /// Attribute the function applies to.
    pub attr: String,
    /// Language tag.
    pub lang: String,
    /// Function name (lowercased).
    pub name: String,
    /// Arguments.
    pub args: Vec<Arg>,
    /// UIDs (`uid(...)` function).
    pub uid: Vec<u64>,
    /// Variables the function needs.
    pub needs_var: Vec<VarContext>,
    /// `gt(count(...), ...)` form.
    pub is_count: bool,
    /// `eq(val(...), ...)` form.
    pub is_value_var: bool,
    /// `eq(len(...), ...)` form.
    pub is_len_var: bool,
}

impl Function {
    /// Whether the function is an aggregation (`min`/`max`/`sum`/`avg`).
    #[must_use]
    pub fn is_aggregator(&self) -> bool {
        is_aggregator(&self.name)
    }

    /// Whether the function is the `checkpwd` verifier.
    #[must_use]
    pub fn is_password_verifier(&self) -> bool {
        self.name == "checkpwd"
    }
}

/// Variables defined and needed by one query block.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Vars {
    /// Defined variable names.
    pub defines: Vec<String>,
    /// Needed variable names.
    pub needs: Vec<String>,
}

/// Parse result: query trees, variable use, and schema request.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ParseResult {
    /// Top-level query blocks.
    pub query: Vec<GraphQuery>,
    /// Variable use per block.
    pub query_vars: Vec<Vars>,
    /// Schema block, if any.
    pub schema: Option<SchemaRequest>,
}

/// Parse input: query text plus `$...` variable values.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Request {
    /// Query text.
    pub text: String,
    /// DQL variable values by `$name`.
    pub variables: HashMap<String, String>,
}

/// Whether the function name is an aggregation.
#[must_use]
pub fn is_aggregator(name: &str) -> bool {
    matches!(name, "min" | "max" | "sum" | "avg")
}

/// Whether the function name is `expand`.
#[must_use]
pub fn is_expand_func(name: &str) -> bool {
    name == "expand"
}

/// Whether the function name is `math`.
#[must_use]
pub fn is_math_block(name: &str) -> bool {
    name == "math"
}

/// Whether the function name is a geo function.
#[must_use]
pub fn is_geo_func(name: &str) -> bool {
    matches!(name, "near" | "contains" | "within" | "intersects")
}

/// Whether the function name is an inequality (`eq`/`le`/`ge`/`gt`/`lt`/`between`).
#[must_use]
pub fn is_inequality_fn(name: &str) -> bool {
    matches!(name, "eq" | "le" | "ge" | "gt" | "lt" | "between")
}

/// Filter operator precedence (`not` > `and` > `or`).
pub fn filter_op_precedence(op: &str) -> i32 {
    match op {
        "not" => 3,
        "and" => 2,
        "or" => 1,
        _ => 0,
    }
}

/// Math operator precedence (port of `mathOpPrecedence`).
pub fn math_op_precedence(op: &str) -> i32 {
    match op {
        "u-" => 500,
        "floor" => 105,
        "ceil" => 104,
        "since" => 103,
        "exp" => 100,
        "ln" => 99,
        "sqrt" => 98,
        "cond" => 90,
        "pow" => 89,
        "logbase" => 88,
        "max" => 85,
        "min" => 84,
        "/" | "*" => 50,
        "dot" => 49,
        "%" => 48,
        "-" => 47,
        "+" => 46,
        "<" => 10,
        ">" => 9,
        "<=" => 8,
        ">=" => 7,
        "==" => 6,
        "!=" => 5,
        _ => 0,
    }
}
