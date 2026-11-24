//! Mutation and upsert parsing plus N-Quad edge building.
//!
//! Ports `parser_mutation.go`, `dql.go` (`ParseDQL`), and `mutation.go`.

#![warn(missing_docs)]

use std::collections::HashMap;

use datastreamdb_chunker::NQuad as ChunkNQuad;
use datastreamdb_lex::{ITEM_EOF, ItemIterator, Lexer, StateFn};
use datastreamdb_proto::{DirectedEdge, EdgeOp, Metadata, ValType, api};
use datastreamdb_types::strconv::parse_go_uint_base0;
use datastreamdb_types::{ApiValue, TypeId, Val, Value, marshal};

use super::state::{
    ITEM_LEFT_CURL, ITEM_MUTATION_OP, ITEM_MUTATION_OP_CONTENT, ITEM_RIGHT_CURL, ITEM_TEXT,
    ITEM_UPSERT_BLOCK, ITEM_UPSERT_BLOCK_OP, ITEM_UPSERT_BLOCK_OP_CONTENT, lex_top_level,
};
use crate::DqlError;

/// Wire mutation mirror (`api.Mutation` fields the parser fills).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ApiMutation {
    /// RDF to set (raw block bytes).
    pub set_nquads: Vec<u8>,
    /// RDF to delete (raw block bytes).
    pub del_nquads: Vec<u8>,
    /// Parsed set quads (filled by the query layer).
    pub set: Vec<ChunkNQuad>,
    /// Parsed delete quads (filled by the query layer).
    pub del: Vec<ChunkNQuad>,
    /// Upsert condition (`@if` content).
    pub cond: String,
}

/// Wire request mirror (`api.Request` fields the parser fills).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ApiRequest {
    /// Query text (empty for pure mutations).
    pub query: String,
    /// Mutations.
    pub mutations: Vec<ApiMutation>,
}

/// Parsed mutation (port of Go's `dql.Mutation`, used by the executor).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Mutation {
    /// Upsert condition.
    pub cond: String,
    /// Set quads.
    pub set: Vec<ChunkNQuad>,
    /// Delete quads.
    pub del: Vec<ChunkNQuad>,
    /// Allowed predicates (ACL).
    pub allowed_preds: Vec<String>,
    /// Parse metadata.
    pub metadata: Option<Metadata>,
}

/// Routes a top-level string to upsert/mutation/query parsing.
///
/// # Errors
///
/// Returns [`DqlError`] on malformed blocks.
pub fn parse_dql(text: &str) -> Result<ApiRequest, DqlError> {
    let mut lexer = Lexer::default();
    lexer.reset(text);
    lexer.run(StateFn(lex_top_level));
    lexer.validate_result()?;

    let mut it = lexer.new_iterator();
    if !it.advance() {
        return Err(it.errorf("Invalid mutation").into());
    }
    let item = it.item();
    if item.typ == ITEM_UPSERT_BLOCK {
        it.prev();
        return parse_upsert_block(&mut it);
    }

    it.advance();
    let item = it.item();
    if item.typ == ITEM_MUTATION_OP {
        it.prev();
        it.prev();
        let mu = parse_mutation_block(&mut it)?;
        return Ok(ApiRequest {
            query: String::new(),
            mutations: vec![mu],
        });
    }

    it.prev();
    it.prev();
    Ok(ApiRequest {
        query: text.to_owned(),
        mutations: Vec::new(),
    })
}

/// Parses a mutation or upsert block (rejects anything else).
///
/// # Errors
///
/// Returns [`DqlError`] on malformed blocks.
pub fn parse_mutation(text: &str) -> Result<ApiRequest, DqlError> {
    let mut lexer = Lexer::default();
    lexer.reset(text);
    lexer.run(StateFn(lex_top_level));
    lexer.validate_result()?;

    let mut it = lexer.new_iterator();
    if !it.advance() {
        return Err(it.errorf("Invalid mutation").into());
    }
    let item = it.item();
    let req = match item.typ {
        ITEM_UPSERT_BLOCK => parse_upsert_block(&mut it)?,
        ITEM_LEFT_CURL => {
            let mu = parse_mutation_block(&mut it)?;
            ApiRequest {
                query: String::new(),
                mutations: vec![mu],
            }
        }
        _ => {
            return Err(it
                .errorf(format!("Unexpected token: [{}]", item.val))
                .into());
        }
    };

    // Mutations must be enclosed in a single block.
    if it.advance() && it.item().typ != ITEM_EOF {
        return Err(it
            .errorf(format!(
                "Unexpected {} after the end of the block",
                it.item().val
            ))
            .into());
    }
    Ok(req)
}

/// Parses `upsert { query {...} mutation [@if] {...} fragment ... }`.
fn parse_upsert_block(it: &mut ItemIterator) -> Result<ApiRequest, DqlError> {
    let mut req: Option<ApiRequest> = None;
    let mut query_text = String::new();
    let mut cond_text = String::new();
    let mut query_found = false;

    // ===>upsert<=== {...}
    if !it.advance() {
        return Err(it.errorf("Unexpected end of upsert block").into());
    }

    // upsert ===>{<=== ....}
    let item = it.item();
    if item.typ != ITEM_LEFT_CURL {
        return Err(item
            .errorf(format!(
                "Expected {{ at the start of block. Got: [{}]",
                item.val
            ))
            .into());
    }

    while it.advance() {
        let item = it.item();
        // upsert {... ===>}<===
        if item.typ == ITEM_RIGHT_CURL {
            match (&req, query_found) {
                (None, _) => return Err(it.errorf("Empty mutation block").into()),
                (_, false) => return Err(it.errorf("Query op not found in upsert block").into()),
                (Some(_), true) => {
                    let mut req = req.expect("checked");
                    req.query = query_text;
                    return Ok(req);
                }
            }
        }
        // upsert { mutation{...} ===>query<==={...}}
        if item.typ == ITEM_UPSERT_BLOCK_OP && item.val == "query" {
            if query_found {
                return Err(it.errorf("Multiple query ops inside upsert block").into());
            }
            query_found = true;
            if !it.advance() {
                return Err(it.errorf("Unexpected end of upsert block").into());
            }
            let item = it.item();
            if item.typ != ITEM_UPSERT_BLOCK_OP_CONTENT {
                return Err(item
                    .errorf(format!("Expecting brace, found '{}'", item.val))
                    .into());
            }
            query_text.push_str(&item.val);
            continue;
        }
        // upsert { ===>mutation<=== {...} query{...}}
        if item.typ == ITEM_UPSERT_BLOCK_OP && item.val == "mutation" {
            if !it.advance() {
                return Err(it.errorf("Unexpected end of upsert block").into());
            }

            // upsert { mutation ===>@if(...)<=== {....} query{...}}
            let item = it.item();
            if item.typ == ITEM_UPSERT_BLOCK_OP_CONTENT {
                item.val.clone_into(&mut cond_text);
                if !it.advance() {
                    return Err(it.errorf("Unexpected end of upsert block").into());
                }
            }

            // upsert @if(...) ===>{<=== ....}
            let mut mu = parse_mutation_block(it)?;
            mu.cond = std::mem::take(&mut cond_text);
            if let Some(req) = req.as_mut() {
                req.mutations.push(mu);
            } else {
                req = Some(ApiRequest {
                    query: String::new(),
                    mutations: vec![mu],
                });
            }
            continue;
        }
        // upsert { mutation{...} ===>fragment<==={...}}
        if item.typ == ITEM_UPSERT_BLOCK_OP && item.val == "fragment" {
            if !it.advance() {
                return Err(it.errorf("Unexpected end of upsert block").into());
            }
            let item = it.item();
            if item.typ != ITEM_UPSERT_BLOCK_OP_CONTENT {
                return Err(item
                    .errorf(format!("Expecting brace, found '{}'", item.val))
                    .into());
            }
            query_text.push_str("fragment");
            query_text.push_str(&item.val);
            continue;
        }
        return Err(it
            .errorf(format!("Unexpected token in upsert block [{}]", item.val))
            .into());
    }

    Err(it.errorf("Invalid upsert block").into())
}

/// Parses `{ set {...} delete {...} }`.
fn parse_mutation_block(it: &mut ItemIterator) -> Result<ApiMutation, DqlError> {
    let mut mu = ApiMutation::default();

    let item = it.item();
    if item.typ != ITEM_LEFT_CURL {
        return Err(item
            .errorf(format!(
                "Expected {{ at the start of block. Got: [{}]",
                item.val
            ))
            .into());
    }

    while it.advance() {
        let item = it.item();
        if item.typ == ITEM_TEXT {
            continue;
        }
        if item.typ == ITEM_RIGHT_CURL {
            return Ok(mu);
        }
        if item.typ == ITEM_MUTATION_OP {
            parse_mutation_op(it, &item.val, &mut mu)?;
        }
    }
    Err(it.errorf("Invalid mutation.").into())
}

/// Parses one `set`/`delete` operation's content.
fn parse_mutation_op(
    it: &mut ItemIterator,
    op: &str,
    mu: &mut ApiMutation,
) -> Result<(), DqlError> {
    let mut parse = false;
    while it.advance() {
        let item = it.item();
        if item.typ == ITEM_TEXT {
            continue;
        }
        if item.typ == ITEM_LEFT_CURL {
            if parse {
                return Err(it.errorf("Too many left curls in set mutation.").into());
            }
            parse = true;
        }
        if item.typ == ITEM_MUTATION_OP_CONTENT {
            if !parse {
                return Err(it.errorf("Mutation syntax invalid.").into());
            }
            match op {
                "set" => mu.set_nquads = item.val.into_bytes(),
                "delete" => mu.del_nquads = item.val.into_bytes(),
                "schema" => {
                    return Err(it
                        .errorf("Altering schema not supported through http client.")
                        .into());
                }
                "dropall" => {
                    return Err(it
                        .errorf("Dropall not supported through http client.")
                        .into());
                }
                _ => return Err(it.errorf("Invalid mutation operation.").into()),
            }
        }
        if item.typ == ITEM_RIGHT_CURL {
            return Ok(());
        }
    }
    Err(it.errorf("Invalid mutation formatting.").into())
}

/// Parses a UID string (base-0); zero and garbage both error.
///
/// # Errors
///
/// Returns [`DqlError`] on zero or unparseable input.
pub fn parse_uid(xid: &str) -> Result<u64, DqlError> {
    match parse_go_uint_base0(xid) {
        Ok(0) => Err(DqlError::message("UID must to be greater than 0")),
        Ok(uid) => Ok(uid),
        Err(e) => Err(DqlError::message(e.to_string())),
    }
}

/// N-Quad plus edge-building methods (port of Go's `dql.NQuad`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NQuad(pub ChunkNQuad);

/// Converts an API value into a typed `Val` (Geo/DateTime/BigFloat stay bytes).
#[must_use]
pub fn type_val_from(value: &ApiValue) -> Val {
    match value {
        ApiValue::Bytes(v) => Val {
            tid: TypeId::Binary,
            value: Value::Binary(v.clone()),
        },
        ApiValue::Int(v) => Val {
            tid: TypeId::Int,
            value: Value::Int(*v),
        },
        ApiValue::Str(v) => Val {
            tid: TypeId::String,
            value: Value::String(v.clone()),
        },
        ApiValue::Bool(v) => Val {
            tid: TypeId::Bool,
            value: Value::Bool(*v),
        },
        ApiValue::Double(v) => Val {
            tid: TypeId::Float,
            value: Value::Float(*v),
        },
        ApiValue::Geo(v) => Val {
            tid: TypeId::Geo,
            value: Value::Binary(v.clone()),
        },
        ApiValue::Bigfloat(v) => Val {
            tid: TypeId::BigFloat,
            value: Value::Binary(v.clone()),
        },
        ApiValue::Datetime(v) => Val {
            tid: TypeId::DateTime,
            value: Value::Binary(v.clone()),
        },
        ApiValue::Password(v) => Val {
            tid: TypeId::Password,
            value: Value::Password(v.clone()),
        },
        ApiValue::Vfloat(v) => Val {
            tid: TypeId::VFloat,
            value: Value::VFloat(datastreamdb_types::value::bytes_as_float_array(v)),
        },
        ApiValue::Default(v) => Val {
            tid: TypeId::Default,
            value: Value::String(v.clone()),
        },
    }
}

/// Marshals an N-Quad value to bytes plus its type id.
fn byte_val(nq: &NQuad) -> Result<(Vec<u8>, TypeId), DqlError> {
    // The object value is always present here (checked by value_type routing).
    let value = nq.0.object_value.as_ref().expect("object value");
    // Geo/DateTime/BigFloat arrive pre-marshalled (client or parser did it).
    match value {
        ApiValue::Geo(v) => return Ok((v.clone(), TypeId::Geo)),
        ApiValue::Datetime(v) => return Ok((v.clone(), TypeId::DateTime)),
        ApiValue::Bigfloat(v) => return Ok((v.clone(), TypeId::BigFloat)),
        _ => {}
    }
    let typed = type_val_from(value);
    let tid = typed.tid;
    let out = marshal(&typed, TypeId::Binary).map_err(|e| DqlError::message(e.to_string()))?;
    let Value::Binary(bytes) = out.value else {
        return Err(DqlError::message("marshal did not produce bytes"));
    };
    Ok((bytes, tid))
}

/// Resolves a subject/object to a UID (numeric, blank-node map, or xid map).
fn to_uid(
    subject: &str,
    new_to_uid: &HashMap<String, u64, impl std::hash::BuildHasher>,
) -> Result<u64, DqlError> {
    match parse_go_uint_base0(subject) {
        Ok(0) => Err(DqlError::message("UID must to be greater than 0")),
        Ok(id) => Ok(id),
        Err(_) => {
            // It's an xid.
            if let Some(id) = new_to_uid.get(subject) {
                return Ok(*id);
            }
            Err(DqlError::message(format!(
                "UID not found/generated for xid {subject}\n"
            )))
        }
    }
}

/// Converts tok facets to wire facets.
fn wire_facets(facets: &[datastreamdb_tok::facets::Facet]) -> Vec<api::Facet> {
    facets
        .iter()
        .map(|f| api::Facet {
            key: f.key.clone(),
            value: f.value.clone(),
            // Discriminants match api.Facet.ValType by construction.
            val_type: f.val_type as i32,
            tokens: f.tokens.clone(),
            alias: f.alias.clone(),
        })
        .collect()
}

fn copy_value(out: &mut DirectedEdge, nq: &NQuad) -> Result<(), DqlError> {
    let (bytes, tid) = byte_val(nq)?;
    out.value = bytes;
    out.value_type = tid as i32;
    Ok(())
}

impl NQuad {
    fn edge_prototype(&self, subject_uid: u64) -> DirectedEdge {
        DirectedEdge {
            entity: subject_uid,
            attr: self.0.predicate.clone(),
            namespace: self.0.namespace,
            lang: self.0.lang.clone(),
            facets: wire_facets(&self.0.facets),
            ..DirectedEdge::default()
        }
    }

    /// Edge connecting subject and object UIDs.
    #[must_use]
    pub fn create_uid_edge(&self, subject_uid: u64, object_uid: u64) -> DirectedEdge {
        let mut out = self.edge_prototype(subject_uid);
        out.value_id = object_uid;
        out.value_type = ValType::Uid as i32;
        out
    }

    /// Value edge for the subject (predicate/lang/facets from the N-Quad).
    ///
    /// # Errors
    ///
    /// Returns [`DqlError`] when the value cannot be marshalled.
    pub fn create_value_edge(&self, subject_uid: u64) -> Result<DirectedEdge, DqlError> {
        let mut out = self.edge_prototype(subject_uid);
        copy_value(&mut out, self)?;
        Ok(out)
    }

    /// Converts a `* p *` N-Quad into a predicate-deletion edge.
    ///
    /// # Errors
    ///
    /// Returns [`DqlError`] unless subject and object are both `*`.
    pub fn to_delete_pred_edge(&self) -> Result<DirectedEdge, DqlError> {
        // Go compares Go-protobuf String() output, which never equals _STAR_ALL
        // for non-nil values; Debug formatting preserves that property.
        let obj_string = match &self.0.object_value {
            None => "<nil>".to_owned(),
            Some(v) => format!("{v:?}"),
        };
        if self.0.subject != datastreamdb_x::STAR && obj_string != datastreamdb_x::STAR {
            return Err(DqlError::message(format!(
                "Subject and object both should be *. Got: {:?}",
                self.0
            )));
        }
        let mut out = DirectedEdge {
            // Entity 0 plus a STAR value marks predicate deletion.
            entity: 0,
            attr: self.0.predicate.clone(),
            namespace: self.0.namespace,
            lang: self.0.lang.clone(),
            facets: wire_facets(&self.0.facets),
            op: EdgeOp::Del as i32,
            ..DirectedEdge::default()
        };
        copy_value(&mut out, self)?;
        Ok(out)
    }

    /// Builds the edge, resolving XIDs through `new_to_uid`.
    ///
    /// # Errors
    ///
    /// Returns [`DqlError`] on zero UIDs, unknown XIDs, or bad values.
    pub fn to_edge_using(
        &self,
        new_to_uid: &HashMap<String, u64, impl std::hash::BuildHasher>,
    ) -> Result<DirectedEdge, DqlError> {
        let subject_uid = to_uid(&self.0.subject, new_to_uid)?;
        if subject_uid == 0 {
            return Err(DqlError::message(format!(
                "Subject should be > 0 for nquad: {:?}",
                self.0
            )));
        }
        match self.value_type() {
            datastreamdb_x::ValueTypeInfo::Uid => {
                let object_uid = to_uid(&self.0.object_id, new_to_uid)?;
                if object_uid == 0 {
                    return Err(DqlError::message(format!(
                        "ObjectId should be > 0 for nquad: {:?}",
                        self.0
                    )));
                }
                Ok(self.create_uid_edge(subject_uid, object_uid))
            }
            datastreamdb_x::ValueTypeInfo::Plain | datastreamdb_x::ValueTypeInfo::Multi => {
                self.create_value_edge(subject_uid)
            }
            _ => Err(DqlError::message(format!(
                "Unknown value type for nquad: {:?}",
                self.0
            ))),
        }
    }

    fn value_type(&self) -> datastreamdb_x::ValueTypeInfo {
        datastreamdb_x::value_type(
            self.0.object_value.is_some(),
            !self.0.lang.is_empty(),
            self.0.object_id.is_empty(),
        )
    }
}
