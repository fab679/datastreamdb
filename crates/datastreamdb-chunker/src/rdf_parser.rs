//! RDF N-Quad line parser. Port of `rdf_parser.go`.

#![warn(missing_docs)]

use std::collections::HashMap;

use datastreamdb_lex::{ITEM_EOF, ItemIterator, Lexer, StateFn};
use datastreamdb_proto::{HintType, Metadata};
use datastreamdb_tok::facets::facet_for;
use datastreamdb_types::strconv::parse_go_uint_base0;
use datastreamdb_types::{ApiValue, TypeId, Val, Value, convert, object_value};

use super::model::NQuad;
use super::rdf_state::{
    ITEM_COMMA, ITEM_COMMENT, ITEM_EQUAL, ITEM_LABEL, ITEM_LANGUAGE, ITEM_LEFT_ROUND, ITEM_LITERAL,
    ITEM_OBJECT, ITEM_OBJECT_FUNC, ITEM_OBJECT_TYPE, ITEM_PREDICATE, ITEM_RIGHT_ROUND, ITEM_STAR,
    ITEM_SUBJECT, ITEM_SUBJECT_FUNC, ITEM_TEXT, ITEM_VALID_END, ITEM_VAR_NAME, lex_text,
};
use crate::ChunkError;

/// Sanity check for subject, predicate, and object strings: empty is fine
/// (only subject/predicate emptiness is rejected later), otherwise at least
/// one letter or digit is required.
#[must_use]
pub fn sane(s: &str) -> bool {
    if s.is_empty() {
        return true;
    }
    s.chars().any(|c| c.is_alphabetic() || c.is_numeric())
}

/// Parses all N-Quads in a byte buffer (convenience wrapper; skips blanks).
///
/// # Errors
///
/// Returns [`ChunkError`] on the first malformed line.
pub fn parse_rdfs(data: &[u8]) -> Result<(Vec<NQuad>, Metadata), ChunkError> {
    let mut nqs = Vec::new();
    let mut lexer = Lexer::default();
    for line in data.split(|b| *b == b'\n') {
        let text = String::from_utf8_lossy(line);
        match parse_rdf(&text, &mut lexer) {
            Ok(nq) => nqs.push(nq),
            Err(ChunkError::Empty) => {}
            Err(err) => return Err(err),
        }
    }
    let metadata = calculate_type_hints(&nqs);
    Ok((nqs, metadata))
}

/// Parses one N-Quad line (`http://www.w3.org/TR/n-quads/`).
///
/// The caller supplies the lexer so it can be reused across lines.
///
/// # Errors
///
/// Returns [`ChunkError::Empty`] for blank/comment lines and
/// [`ChunkError`] otherwise.
pub fn parse_rdf(line: &str, lexer: &mut Lexer) -> Result<NQuad, ChunkError> {
    let mut nq = NQuad::default();
    let line = line.trim();
    if line.is_empty() {
        return Err(ChunkError::Empty);
    }
    lexer.reset(line);
    lexer.run(StateFn(lex_text));
    lexer
        .validate_result()
        .map_err(|e| ChunkError::message(e.to_string()))?;
    let mut it = lexer.new_iterator();
    let mut oval = String::new();
    let mut seen_oval = false;
    let mut vend = false;
    let mut is_comment_line = false;
    // Read items from the lexer until the valid end (or exhaustion).
    let mut done = false;
    while !done && it.advance() {
        let item = it.item();
        match item.typ {
            ITEM_SUBJECT => nq.subject = trim_spaces(&item.val),
            ITEM_SUBJECT_FUNC => nq.subject = parse_function(&mut it)?,
            ITEM_OBJECT_FUNC => nq.object_id = parse_function(&mut it)?,
            ITEM_PREDICATE => {
                // Split predicate and lang directive (ex: "name@en"), if needed.
                nq.predicate = trim_spaces(&item.val);
                crate::split_lang(&mut nq);
            }
            ITEM_OBJECT => nq.object_id = trim_spaces(&item.val),
            ITEM_STAR => {
                if nq.subject.is_empty() {
                    datastreamdb_x::STAR.clone_into(&mut nq.subject);
                } else if nq.predicate.is_empty() {
                    datastreamdb_x::STAR.clone_into(&mut nq.predicate);
                } else {
                    nq.object_value = Some(ApiValue::Default(datastreamdb_x::STAR.to_owned()));
                }
            }
            ITEM_LITERAL => {
                let bytes = datastreamdb_tok::facets::unquote(&item.val)
                    .map_err(|e| ChunkError::message(format!("while unquoting: {e}")))?;
                oval = String::from_utf8_lossy(&bytes).into_owned();
                seen_oval = true;
            }
            ITEM_LANGUAGE => nq.lang = item.val,
            ITEM_OBJECT_TYPE => {
                apply_object_type(&mut nq, &item.val, &oval)?;
            }
            ITEM_COMMENT => {
                is_comment_line = true;
                vend = true;
            }
            ITEM_VALID_END => {
                vend = true;
                if !it.advance() {
                    return Err(ChunkError::message(format!(
                        "invalid end of input. Input: [{line}]"
                    )));
                }
                // RDF N-Quads end with a newline; input is pre-split by
                // newline, so EOF or # must follow the dot.
                let item = it.item();
                if !(item.typ == ITEM_EOF || item.typ == ITEM_COMMENT) {
                    return Err(ChunkError::message(format!(
                        "invalid end of input. Expected newline or # after . Input: [{line}]"
                    )));
                }
                done = true;
            }
            ITEM_LABEL => {
                let text = trim_spaces(&item.val);
                nq.namespace = parse_go_uint_base0(&text).map_err(|_| {
                    ChunkError::message(format!("invalid namespace ID. Input: [{line}]"))
                })?;
            }
            ITEM_LEFT_ROUND => {
                it.prev(); // backup '('
                parse_facets_rdf(&mut it, &mut nq)
                    .map_err(|e| ChunkError::message(format!("could not parse facet: {e}")))?;
            }
            _ => {}
        }
    }
    finish_rdf(nq, line, oval, seen_oval, vend, is_comment_line)
}

/// Converts a typed literal (`^^<type>`) into the N-Quad's object value.
fn apply_object_type(nq: &mut NQuad, item_val: &str, oval: &str) -> Result<(), ChunkError> {
    if nq.predicate == datastreamdb_x::STAR || nq.subject == datastreamdb_x::STAR {
        return Err(ChunkError::message(
            "if predicate/subject is *, value should be * as well",
        ));
    }
    let val = trim_spaces(item_val);
    // TODO: Check if this condition is required.
    if val == "*" {
        return Err(ChunkError::message("itemObject can't be *"));
    }
    // Find the storage type from the type map.
    let Some(t) = rdf_type(&val) else {
        return Err(ChunkError::message(format!("unrecognized rdf type {val}")));
    };
    if oval.is_empty() && t != TypeId::String {
        return Err(ChunkError::message("invalid ObjectValue"));
    }
    let mut src = Val {
        tid: TypeId::String,
        value: Value::Binary(oval.as_bytes().to_vec()),
    };
    // If this is a password value don't re-encrypt. issue#2765
    if t == TypeId::Password {
        src.tid = t;
    }
    let converted = convert(&src, t).map_err(|e| ChunkError::message(e.to_string()))?;
    nq.object_value =
        Some(object_value(t, &converted.value).map_err(|e| ChunkError::message(e.to_string()))?);
    Ok(())
}

/// Validates a fully lexed N-Quad (end marker, required fields, sanity).
fn finish_rdf(
    mut nq: NQuad,
    line: &str,
    oval: String,
    seen_oval: bool,
    vend: bool,
    is_comment_line: bool,
) -> Result<NQuad, ChunkError> {
    if !vend {
        return Err(ChunkError::message(format!(
            "invalid end of input. Input: [{line}]"
        )));
    }
    if is_comment_line {
        return Err(ChunkError::Empty);
    }
    // Set the default value only for seen `""` literals without a value yet.
    if seen_oval && nq.object_value.is_none() {
        nq.object_value = Some(ApiValue::Default(oval));
    }
    if nq.subject.is_empty() || nq.predicate.is_empty() {
        return Err(ChunkError::message(format!(
            "empty required fields in NQuad. Input: [{line}]"
        )));
    }
    if nq.object_id.is_empty() && nq.object_value.is_none() {
        return Err(ChunkError::message(format!(
            "no Object in NQuad. Input: [{line}]"
        )));
    }
    if !sane(&nq.subject) || !sane(&nq.predicate) || !sane(&nq.object_id) {
        // Don't format the full line, as it may contain sensitive information.
        return Err(ChunkError::message(format!(
            "NQuad failed sanity check. Subject: {:?}, Predicate: {:?}, ObjectId: {:?}",
            nq.subject, nq.predicate, nq.object_id
        )));
    }
    Ok(nq)
}

/// Trims ASCII spaces (Go trims only `' '`, not tabs, here).
fn trim_spaces(s: &str) -> String {
    s.trim_matches(' ').to_owned()
}

/// Parses `uid(<name>)`, stripping whitespace.
fn parse_function(it: &mut ItemIterator) -> Result<String, ChunkError> {
    let item = it.item();
    let mut text = item.val;
    it.advance();
    let item = it.item();
    if item.typ != ITEM_LEFT_ROUND {
        return Err(ChunkError::message(format!(
            "expected '(', found: {}",
            item.val
        )));
    }
    it.advance();
    let item = it.item();
    if item.typ != ITEM_VAR_NAME {
        return Err(ChunkError::message(format!(
            "expected variable name, found: {}",
            item.val
        )));
    }
    if item.val.trim().is_empty() {
        return Err(ChunkError::message("empty variable name in function call"));
    }
    text.push('(');
    text.push_str(&item.val);
    text.push(')');
    it.advance();
    let item = it.item();
    if item.typ != ITEM_RIGHT_ROUND {
        return Err(ChunkError::message(format!(
            "expected ')', found: {}",
            item.val
        )));
    }
    Ok(text)
}

fn parse_facets_rdf(it: &mut ItemIterator, nq: &mut NQuad) -> Result<(), ChunkError> {
    if !it.advance() {
        return Err(ChunkError::message("unexpected end of facets"));
    }
    let item = it.item();
    if item.typ != ITEM_LEFT_ROUND {
        return Err(ChunkError::message(format!(
            "expected '(' but found {} at facet",
            item.val
        )));
    }
    while it.advance() {
        // Parse key.
        let item = it.item();
        if item.typ != ITEM_TEXT {
            return Err(ChunkError::message(format!(
                "expected key but found {}",
                item.val
            )));
        }
        let key = item.val.trim().to_owned();
        if key.is_empty() {
            return Err(ChunkError::message("empty facetKeys not allowed"));
        }
        // Parse =.
        if !it.advance() {
            return Err(ChunkError::message("unexpected end of facets"));
        }
        let item = it.item();
        if item.typ != ITEM_EQUAL {
            return Err(ChunkError::message(format!(
                "expected = after facetKey. Found {}",
                item.val
            )));
        }
        // Parse value or empty value.
        if !it.advance() {
            return Err(ChunkError::message("unexpected end of facets"));
        }
        let item = it.item();
        let mut facet_val = "";
        if item.typ == ITEM_TEXT {
            facet_val = &item.val;
        }
        let facet = facet_for(&key, facet_val).map_err(|e| ChunkError::message(e.to_string()))?;
        nq.facets.push(facet);
        // Empty value case.
        if item.typ == ITEM_RIGHT_ROUND {
            break;
        }
        if item.typ == ITEM_COMMA {
            continue;
        }
        if item.typ != ITEM_TEXT {
            return Err(ChunkError::message(format!(
                "expected , or ) or text but found {}",
                item.val
            )));
        }
        // Value was present; get either ')' or ','.
        if !it.advance() {
            return Err(ChunkError::message("unexpected end of facets"));
        }
        let item = it.item();
        if item.typ == ITEM_RIGHT_ROUND {
            break;
        }
        if item.typ == ITEM_COMMA {
            continue;
        }
        return Err(ChunkError::message(format!(
            "expected , or ) after facet. Received {}",
            item.val
        )));
    }
    Ok(())
}

/// Counts `<subject, pred>` pairs to hint scalar vs list schema creation.
fn calculate_type_hints(nqs: &[NQuad]) -> Metadata {
    let mut counts: HashMap<(&str, &str), usize> = HashMap::new();
    let mut metadata = Metadata::default();
    for nq in nqs {
        let count = counts
            .entry((nq.subject.as_str(), nq.predicate.as_str()))
            .or_insert(0);
        *count += 1;
        if *count > 1 {
            metadata.set_hint(&nq.predicate, HintType::List);
        }
    }
    metadata
}

/// RDF datatype IRI to storage type (port of Go's `typeMap`).
fn rdf_type(name: &str) -> Option<TypeId> {
    Some(match name {
        "xs:password" => TypeId::Password,
        "xs:boolean" | "http://www.w3.org/2001/XMLSchema#boolean" => TypeId::Bool,
        "xs:base64Binary" => TypeId::Binary,
        "xs:decimal" => TypeId::BigFloat,
        "geo:geojson" => TypeId::Geo,
        "xs:[]float32" => TypeId::VFloat,
        "xs:string" | "http://www.w3.org/2001/XMLSchema#string" => TypeId::String,
        "xs:date"
        | "xs:dateTime"
        | "http://www.w3.org/2001/XMLSchema#dateTime"
        | "http://www.w3.org/2001/XMLSchema#date"
        | "http://www.w3.org/2001/XMLSchema#gYear"
        | "http://www.w3.org/2001/XMLSchema#gYearMonth" => TypeId::DateTime,
        "xs:int"
        | "xs:integer"
        | "xs:positiveInteger"
        | "http://www.w3.org/2001/XMLSchema#int"
        | "http://www.w3.org/2001/XMLSchema#positiveInteger"
        | "http://www.w3.org/2001/XMLSchema#integer" => TypeId::Int,
        "xs:double"
        | "xs:float"
        | "http://www.w3.org/2001/XMLSchema#double"
        | "http://www.w3.org/2001/XMLSchema#float" => TypeId::Float,
        _ => return None,
    })
}
