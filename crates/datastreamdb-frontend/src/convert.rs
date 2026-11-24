//! API N-Quad conversion and validation (`edgraph/server.go`).
//!
//! `api_nquad_to_chunker` turns `api.NQuad` protos into chunker quads for
//! the shared DQL edge builder. Validation mirrors Go's `validateNQuads`
//! (star rules, key syntax); the reserved-predicate ACL guard arrives
//! with M6.

use datastreamdb_chunker::NQuad as ChunkerNQuad;
use datastreamdb_proto::api::NQuad;
use datastreamdb_types::ApiValue;

/// Conversion failures (client errors).
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ConvertError {
    /// Malformed predicate, facet key, or star pattern.
    #[error("{0}")]
    Invalid(String),
    /// UID allocation or edge building failed.
    #[error("{0}")]
    Build(String),
}

/// Validates one key (predicate or facet): non-empty, no `~@`, no spaces.
fn validate_key(key: &str) -> Result<(), ConvertError> {
    if key.is_empty() {
        return Err(ConvertError::Invalid("Has zero length".to_owned()));
    }
    if key.contains(['~', '@']) {
        return Err(ConvertError::Invalid("Has invalid characters".to_owned()));
    }
    if key.chars().any(char::is_whitespace) {
        return Err(ConvertError::Invalid("Must not contain spaces".to_owned()));
    }
    Ok(())
}

/// Validates predicate length (Go `validatePredName`).
fn validate_pred_name(name: &str) -> Result<(), ConvertError> {
    if name.len() > u16::MAX as usize {
        let shown = name.chars().take(80).collect::<String>();
        return Err(ConvertError::Invalid(format!(
            "Predicate name length cannot be bigger than 2^16. Predicate: {shown}"
        )));
    }
    Ok(())
}

/// Validates predicate + facet keys of one N-Quad (Go `validateKeys`).
fn validate_keys(nq: &NQuad) -> Result<(), ConvertError> {
    validate_key(&nq.predicate)
        .map_err(|err| ConvertError::Invalid(format!("predicate {:?}: {err}", nq.predicate)))?;
    for facet in &nq.facets {
        validate_key(&facet.key)
            .map_err(|err| ConvertError::Invalid(format!("Facet {:?}, {err}", facet.key)))?;
    }
    Ok(())
}

/// Whether the object is the `*` wildcard (default-valued star).
fn is_ostar(nq: &NQuad) -> bool {
    matches!(
        nq.object_value.as_ref().and_then(|v| v.val.as_ref()),
        Some(datastreamdb_proto::api::value::Val::DefaultVal(star)) if star == "*"
    )
}

/// Validates set/del N-Quads (Go `validateNQuads`, minus the ACL guard).
pub(crate) fn validate_nquads(set: &[NQuad], del: &[NQuad]) -> Result<(), ConvertError> {
    for nq in set {
        validate_pred_name(&nq.predicate)?;
        if nq.subject == "*" || nq.predicate == "*" || is_ostar(nq) {
            return Err(ConvertError::Invalid(format!(
                "Cannot use star in set n-quad: {nq:?}"
            )));
        }
        validate_keys(nq)
            .map_err(|err| ConvertError::Invalid(format!("key error: {nq:?}: {err}")))?;
    }
    for nq in del {
        validate_pred_name(&nq.predicate)?;
        if nq.subject == "*" || (nq.predicate == "*" && !is_ostar(nq)) {
            return Err(ConvertError::Invalid(format!(
                "Only valid wildcard delete patterns are 'S * *' and 'S P *': {nq:?}"
            )));
        }
    }
    Ok(())
}

/// Maps an `api.Value` oneof to a chunker value (Go `TypeValFrom`).
///
/// Go maps `uid_val`/`date_val` to an empty string (they never reach the
/// UID path, which keys off `object_id`); this mirrors that gap exactly.
fn api_value_to_chunker(value: &datastreamdb_proto::api::Value) -> Option<ApiValue> {
    use datastreamdb_proto::api::value::Val;
    match value.val.as_ref()? {
        Val::DefaultVal(v) => Some(ApiValue::Default(v.clone())),
        Val::BytesVal(v) => Some(ApiValue::Bytes(v.clone())),
        Val::IntVal(v) => Some(ApiValue::Int(*v)),
        Val::BoolVal(v) => Some(ApiValue::Bool(*v)),
        Val::StrVal(v) => Some(ApiValue::Str(v.clone())),
        Val::DoubleVal(v) => Some(ApiValue::Double(*v)),
        Val::GeoVal(v) => Some(ApiValue::Geo(v.clone())),
        Val::DateVal(_) | Val::UidVal(_) => Some(ApiValue::Default(String::new())),
        Val::DatetimeVal(v) => Some(ApiValue::Datetime(v.clone())),
        Val::PasswordVal(v) => Some(ApiValue::Password(v.clone())),
        Val::BigfloatVal(v) => Some(ApiValue::Bigfloat(v.clone())),
        Val::Vfloat32Val(v) => Some(ApiValue::Vfloat(v.clone())),
    }
}

/// Maps an `api.Facet` to a chunker facet.
fn facet_to_chunker(facet: &datastreamdb_proto::api::Facet) -> datastreamdb_tok::facets::Facet {
    use datastreamdb_proto::api::facet::ValType as ApiValType;
    use datastreamdb_tok::facets::FacetValType;
    let val_type =
        ApiValType::try_from(facet.val_type).map_or(FacetValType::String, |typed| match typed {
            ApiValType::String => FacetValType::String,
            ApiValType::Int => FacetValType::Int,
            ApiValType::Float => FacetValType::Float,
            ApiValType::Bool => FacetValType::Bool,
            ApiValType::Datetime => FacetValType::Datetime,
        });
    datastreamdb_tok::facets::Facet {
        key: facet.key.clone(),
        value: facet.value.clone(),
        val_type,
        tokens: facet.tokens.clone(),
        alias: facet.alias.clone(),
    }
}

/// Converts one API N-Quad to a chunker N-Quad.
///
/// Shared by mutation conversion and the drop-attr alter path.
pub(crate) fn api_nquad_to_chunker(nq: &NQuad) -> ChunkerNQuad {
    ChunkerNQuad {
        subject: nq.subject.clone(),
        predicate: nq.predicate.clone(),
        object_id: nq.object_id.clone(),
        object_value: nq.object_value.as_ref().and_then(api_value_to_chunker),
        lang: nq.lang.clone(),
        facets: nq.facets.iter().map(facet_to_chunker).collect(),
        namespace: nq.namespace,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nquad(subject: &str, predicate: &str) -> NQuad {
        NQuad {
            subject: subject.to_owned(),
            predicate: predicate.to_owned(),
            object_value: Some(datastreamdb_proto::api::Value {
                val: Some(datastreamdb_proto::api::value::Val::StrVal("x".to_owned())),
            }),
            ..NQuad::default()
        }
    }

    #[test]
    fn star_rules_match_go() {
        assert!(validate_nquads(&[nquad("0x2", "name")], &[]).is_ok());
        assert!(validate_nquads(&[nquad("*", "name")], &[]).is_err());
        assert!(validate_nquads(&[nquad("0x2", "*")], &[]).is_err());
        assert!(validate_nquads(&[], &[nquad("0x2", "name")]).is_ok());
        // 'S * *' deletes are valid; subject stars are not.
        let mut star_obj = nquad("0x2", "*");
        star_obj.object_value = Some(datastreamdb_proto::api::Value {
            val: Some(datastreamdb_proto::api::value::Val::DefaultVal(
                "*".to_owned(),
            )),
        });
        assert!(validate_nquads(&[], &[star_obj]).is_ok());
        assert!(validate_nquads(&[], &[nquad("*", "name")]).is_err());
    }

    #[test]
    fn key_rules_match_go() {
        assert!(validate_nquads(&[nquad("0x2", "foo@bar")], &[]).is_err());
        assert!(validate_nquads(&[nquad("0x2", "foo bar")], &[]).is_err());
        assert!(validate_nquads(&[nquad("0x2", "")], &[]).is_err());
        // Deletes skip key validation (lets users fix bad predicates).
        assert!(validate_nquads(&[], &[nquad("0x2", "foo@bar")]).is_ok());
    }

    #[test]
    fn value_oneof_maps_like_go() {
        use datastreamdb_proto::api::value::Val;
        let mapped = |val: Val| {
            api_value_to_chunker(&datastreamdb_proto::api::Value { val: Some(val) })
                .expect("mapped")
        };
        assert!(matches!(mapped(Val::IntVal(7)), ApiValue::Int(7)));
        assert!(matches!(mapped(Val::BoolVal(true)), ApiValue::Bool(true)));
        assert!(matches!(mapped(Val::DoubleVal(1.5)), ApiValue::Double(_)));
        assert!(matches!(
            mapped(Val::StrVal("s".to_owned())),
            ApiValue::Str(_)
        ));
        assert!(matches!(mapped(Val::BytesVal(vec![1])), ApiValue::Bytes(_)));
        assert!(matches!(mapped(Val::GeoVal(vec![1])), ApiValue::Geo(_)));
        assert!(matches!(
            mapped(Val::DatetimeVal(vec![1])),
            ApiValue::Datetime(_)
        ));
        assert!(matches!(
            mapped(Val::PasswordVal("p".to_owned())),
            ApiValue::Password(_)
        ));
        assert!(matches!(
            mapped(Val::BigfloatVal(vec![1])),
            ApiValue::Bigfloat(_)
        ));
        assert!(matches!(
            mapped(Val::Vfloat32Val(vec![1])),
            ApiValue::Vfloat(_)
        ));
        assert!(matches!(
            mapped(Val::DefaultVal("d".to_owned())),
            ApiValue::Default(_)
        ));
        // Go's gap: uid/date fall through to an empty string.
        assert!(matches!(
            mapped(Val::UidVal(9)),
            ApiValue::Default(ref s) if s.is_empty()
        ));
        assert!(matches!(
            mapped(Val::DateVal(vec![1])),
            ApiValue::Default(ref s) if s.is_empty()
        ));
    }
}
