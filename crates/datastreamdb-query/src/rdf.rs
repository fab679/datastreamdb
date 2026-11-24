//! RDF output. Port of `query/outputrdf.go`: N-Quad rendering of
//! executed subgraphs.

#![warn(missing_docs)]

use datastreamdb_proto::List as ProtoList;
use datastreamdb_types::{TypeId, Val};

use crate::errors::{QueryError, Result};
use crate::subgraph::SubGraph;
use crate::vars::convert_with_best_effort;

/// Renders executed root blocks as N-Quads.
///
/// # Errors
///
/// Returns [`QueryError`] on unsupported constructs.
pub fn to_rdf(roots: &[SubGraph]) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    for sg in roots {
        validate_subgraph_for_rdf(sg)?;
        for child in &sg.children {
            cast_to_rdf(&mut buf, child)?;
        }
    }
    Ok(buf)
}

/// Renders one subgraph and its descendants.
fn cast_to_rdf(buf: &mut Vec<u8>, sg: &SubGraph) -> Result<()> {
    validate_subgraph_for_rdf(sg)?;
    if sg.src_uids.is_some() {
        rdf_for_subgraph(buf, sg)?;
    }
    for child in &sg.children {
        cast_to_rdf(buf, child)?;
    }
    Ok(())
}

/// Renders one subgraph's rows.
fn rdf_for_subgraph(buf: &mut Vec<u8>, sg: &SubGraph) -> Result<()> {
    let non_null = sg.children.iter().any(|child| !child.uid_matrix.is_empty());
    if !sg.children.is_empty() && !non_null {
        return Ok(());
    }

    for (i, uid) in sg
        .src_uids
        .as_ref()
        .map_or(&[][..], |src| &src.uids)
        .iter()
        .enumerate()
    {
        if sg.params.ignore_result {
            continue;
        }
        if sg.is_internal() {
            if !sg.params.expand.is_empty() {
                continue;
            }
            let Some(val) = sg.params.uid_to_val.get(*uid) else {
                continue;
            };
            if matches!(val.value, datastreamdb_types::Value::Null) {
                continue;
            }
            let object = get_object_val(val)?;
            write_rdf(buf, *uid, sg.agg_with_var_field_name().as_bytes(), &object);
            continue;
        }
        if !sg.counts.is_empty() {
            rdf_for_count(buf, *uid, sg.counts[i], sg);
        } else if i < sg.uid_matrix.len()
            && !sg.uid_matrix[i].uids.is_empty()
            && !sg.children.is_empty()
        {
            rdf_for_uid_list(buf, *uid, &sg.uid_matrix[i], sg);
        } else if i < sg.value_matrix.len() {
            rdf_for_value_list(buf, *uid, &sg.value_matrix[i], sg.field_name());
        }
    }
    Ok(())
}

/// Writes `<subject> <predicate> object .`.
fn write_rdf(buf: &mut Vec<u8>, subject: u64, predicate: &[u8], object: &[u8]) {
    buf.extend_from_slice(format!("<{subject:#x}>").as_bytes());
    buf.push(b' ');
    buf.push(b'<');
    buf.extend_from_slice(predicate);
    buf.push(b'>');
    buf.push(b' ');
    buf.extend_from_slice(object);
    buf.extend_from_slice(b" .\n");
}

/// Renders a count row.
fn rdf_for_count(buf: &mut Vec<u8>, subject: u64, count: u32, sg: &SubGraph) {
    let mut field = sg.params.alias.clone();
    if field.is_empty() {
        field = format!("count({})", sg.attr);
    }
    write_rdf(
        buf,
        subject,
        field.as_bytes(),
        &quoted_number(count.to_string().as_bytes()),
    );
}

/// Renders UID rows (filtered members only).
fn rdf_for_uid_list(buf: &mut Vec<u8>, subject: u64, list: &ProtoList, sg: &SubGraph) {
    let dest = sg.dest_uids.as_ref().map_or(&[][..], |dest| &dest.uids);
    for uid in &list.uids {
        if dest.binary_search(uid).is_err() {
            continue;
        }
        write_rdf(
            buf,
            subject,
            sg.field_name().as_bytes(),
            format!("<{uid:#x}>").as_bytes(),
        );
    }
}

/// Renders value rows.
fn rdf_for_value_list(
    buf: &mut Vec<u8>,
    subject: u64,
    row: &datastreamdb_proto::ValueList,
    attr: &str,
) {
    for cell in &row.values {
        let Ok(val) = convert_with_best_effort(cell, attr) else {
            continue;
        };
        let Ok(object) = get_object_val(&val) else {
            continue;
        };
        write_rdf(buf, subject, attr.as_bytes(), &object);
    }
}

/// Renders one value as an RDF object.
fn get_object_val(val: &Val) -> Result<Vec<u8>> {
    let bytes = crate::output::val_to_bytes(val)?;
    match val.tid {
        TypeId::Uid => Ok(build_triple(&bytes)),
        TypeId::Int | TypeId::Float => Ok(quoted_number(&bytes)),
        TypeId::Geo => Err(QueryError::Invalid(
            "Geo id is not supported in rdf output".to_owned(),
        )),
        _ => Ok(bytes),
    }
}

/// Wraps bytes in angle brackets.
fn build_triple(val: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(val.len() + 2);
    buf.push(b'<');
    buf.extend_from_slice(val);
    buf.push(b'>');
    buf
}

/// Wraps bytes in quotes.
fn quoted_number(val: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(val.len() + 2);
    buf.push(b'"');
    buf.extend_from_slice(val);
    buf.push(b'"');
    buf
}

/// Rejects constructs without RDF rendering.
fn validate_subgraph_for_rdf(sg: &SubGraph) -> Result<()> {
    if sg.is_group_by() {
        return Err(QueryError::Invalid(
            "groupby is not supported in rdf output format".to_owned(),
        ));
    }
    if sg.attr == "uid" && sg.params.do_count && sg.is_internal() {
        return Err(QueryError::Invalid(
            "uid count is not supported in the rdf output format".to_owned(),
        ));
    }
    if sg.params.normalize {
        return Err(QueryError::Invalid(
            "normalize directive is not supported in the rdf output format".to_owned(),
        ));
    }
    if sg.params.ignore_reflex {
        return Err(QueryError::Invalid(
            "ignorereflex directive is not supported in the rdf output format".to_owned(),
        ));
    }
    if sg
        .src_func
        .as_ref()
        .is_some_and(|func| func.name == "checkpwd")
    {
        return Err(QueryError::Invalid(
            "chkpwd function is not supported in the rdf output format".to_owned(),
        ));
    }
    if sg.params.facet.is_some() && !sg.params.expand_all {
        return Err(QueryError::Invalid(
            "facets are not supported in the rdf output format".to_owned(),
        ));
    }
    Ok(())
}
