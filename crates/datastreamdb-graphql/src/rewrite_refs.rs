//! Mutation reference fragments: ID links and displaced-edge deletes.
//! Split from `rewrite_object.rs`. Port of `asIDReference`, `addAdditionalDeletes`,
//! and `addDelete` from `dgraph-main/graphql/resolve/mutation_rewriter.go`.

#![warn(missing_docs)]

use datastreamdb_dql::{FilterTree, Function, GraphQuery};
use serde_json::{Map as JsonMap, Value as Json};

use crate::op::BoundSchema;
use crate::rewrite_object::{
    FragmentCheck, MutationFragment, SrcField, add_inverse_link, inverse_of,
};
use crate::rewrite_vars::VariableGenerator;

/// A field reference for delete building.
struct FieldRef {
    /// Field name.
    name: String,
    /// Field type name.
    type_name: String,
    /// Whether the field is a list.
    is_list: bool,
}

/// Builds a reference fragment to an existing UID.
pub(crate) fn as_id_reference(
    schema: &BoundSchema,
    val: &str,
    src_field: Option<&SrcField>,
    src_uid: &str,
    var_gen: &mut VariableGenerator,
    auth_vars: &crate::auth_rules::AuthVars,
    is_remove: bool,
) -> MutationFragment {
    let mut obj = JsonMap::new();
    obj.insert("uid".to_owned(), Json::String(val.to_owned()));
    add_inverse_link(schema, &mut obj, src_field, src_uid);
    let mut frag = MutationFragment {
        fragment: Json::Object(obj),
        ..MutationFragment::default()
    };
    if !is_remove {
        add_additional_deletes(
            schema, &mut frag, var_gen, auth_vars, src_field, src_uid, val,
        );
    }
    frag
}

/// Adds deletes for edges displaced by a relink.
fn add_additional_deletes(
    schema: &BoundSchema,
    frag: &mut MutationFragment,
    var_gen: &mut VariableGenerator,
    auth_vars: &crate::auth_rules::AuthVars,
    src_field: Option<&SrcField>,
    src_uid: &str,
    variable: &str,
) {
    let Some(src) = src_field else {
        return;
    };
    let Some((inv_name, _, _)) = inverse_of(schema, src) else {
        return;
    };
    let inv_type = schema
        .schema
        .type_def(&src.type_name)
        .and_then(|def| def.field(&inv_name))
        .map(|field| (field.ty.name().to_owned(), field.ty.is_list()));
    let Some((inv_type_name, inv_is_list)) = inv_type else {
        return;
    };
    let src_ref = FieldRef {
        name: src.name.clone(),
        type_name: src.type_name.clone(),
        is_list: src.is_list,
    };
    let inv_ref = FieldRef {
        name: inv_name,
        type_name: inv_type_name,
        is_list: inv_is_list,
    };
    add_delete(
        schema, frag, var_gen, auth_vars, variable, src_uid, &inv_ref, &src_ref,
    );
    add_delete(
        schema, frag, var_gen, auth_vars, src_uid, variable, &src_ref, &inv_ref,
    );
}

/// Adds one displaced-edge delete plus its lookup query.
#[allow(clippy::too_many_lines, reason = "exact port of Go addDelete")]
#[allow(clippy::too_many_arguments, reason = "exact port of Go addDelete")]
fn add_delete(
    schema: &BoundSchema,
    frag: &mut MutationFragment,
    var_gen: &mut VariableGenerator,
    auth_vars: &crate::auth_rules::AuthVars,
    qry_var: &str,
    exclude_var: &str,
    qry_field: &FieldRef,
    del_field: &FieldRef,
) {
    if qry_field.is_list || qry_var.starts_with("_:") {
        return;
    }
    let qry_var = qry_var
        .strip_prefix("uid(")
        .and_then(|inner| inner.strip_suffix(')'))
        .unwrap_or(qry_var);
    let target_var = var_gen.next(schema, &qry_field.type_name, ("", ""), false);
    let del_pred = schema
        .predicate(&qry_field.type_name, &del_field.name)
        .unwrap_or(&del_field.name)
        .to_owned();
    let mut child = GraphQuery::new();
    child.var.clone_from(&target_var);
    schema
        .predicate(&del_field.type_name, &qry_field.name)
        .unwrap_or(&qry_field.name)
        .clone_into(&mut child.attr);
    let mut qry = GraphQuery::new();
    "var".clone_into(&mut qry.attr);
    qry.func = Some(Function {
        name: "uid".to_owned(),
        args: vec![crate::rewrite_vars::var_arg(qry_var)],
        ..Function::default()
    });
    qry.children = vec![child];
    let mut exclude = exclude_var.to_owned();
    if let Some(inner) = exclude_var
        .strip_prefix("uid(")
        .and_then(|inner| inner.strip_suffix(')'))
    {
        inner.clone_into(&mut exclude);
    }
    if !exclude_var.starts_with("_:") {
        qry.children[0].filter = Some(Box::new(FilterTree {
            op: "not".to_owned(),
            child: vec![FilterTree {
                op: String::new(),
                child: Vec::new(),
                func: Some(Function {
                    name: "uid".to_owned(),
                    args: vec![crate::rewrite_vars::var_arg(&exclude)],
                    ..Function::default()
                }),
            }],
            func: None,
        }));
    }
    frag.queries.push(qry);
    let del_uid = if qry_var.parse::<u64>().is_ok() || qry_var.starts_with("0x") {
        qry_var.to_owned()
    } else {
        format!("uid({qry_var})")
    };
    let link = Json::Object(JsonMap::from_iter([(
        "uid".to_owned(),
        Json::String(del_uid),
    )]));
    let mut del = JsonMap::new();
    del.insert("uid".to_owned(), Json::String(format!("uid({target_var})")));
    if del_field.is_list {
        del.insert(del_pred, Json::Array(vec![link]));
    } else {
        del.insert(del_pred, link);
    }
    frag.deletes.push(Json::Object(del));

    let mut auth = crate::rewrite_auth::AuthRewriter {
        auth_variables: auth_vars.clone(),
        is_writing_auth: false,
        filter_by_uid: false,
        selector: crate::rewrite_auth::AuthSelector::Update,
        var_gen,
        var_name: target_var.clone(),
        parent_var_name: format!("{}Root", qry_field.type_name),
        has_auth_rules: false,
        has_cascade: false,
        schema,
    };
    auth.has_auth_rules = auth.select(&qry_field.type_name).is_some();
    let (auth_queries, auth_filter) = auth.rewrite_auth_queries(&qry_field.type_name);
    if auth_queries.is_empty() {
        return;
    }
    let mut target_block = GraphQuery::new();
    target_block.attr.clone_from(&target_var);
    target_block.func = Some(Function {
        name: "uid".to_owned(),
        args: vec![crate::rewrite_vars::var_arg(&target_var)],
        ..Function::default()
    });
    let mut uid_child = GraphQuery::new();
    "uid".clone_into(&mut uid_child.attr);
    target_block.children = vec![uid_child];
    let mut auth_block = GraphQuery::new();
    auth_block.attr = format!("{target_var}.auth");
    auth_block.func = Some(Function {
        name: "uid".to_owned(),
        args: vec![crate::rewrite_vars::var_arg(&target_var)],
        ..Function::default()
    });
    auth_block.filter = auth_filter.map(Box::new);
    let mut auth_uid_child = GraphQuery::new();
    "uid".clone_into(&mut auth_uid_child.attr);
    auth_block.children = vec![auth_uid_child];
    frag.queries.push(target_block);
    frag.queries.push(auth_block);
    frag.queries.extend(auth_queries);
    let check = std::mem::take(&mut frag.check);
    frag.check = FragmentCheck::Auth {
        query: target_var,
        inner: Box::new(check),
    };
}
