//! RDF + schema export (Go `worker/export.go`, data path).
//!
//! [`export_rdf`] streams one group's postings as N-Quads and
//! [`export_schema`] renders its schema text, both byte-compatible
//! with Go's exporters (namespace labels, type suffixes, facet
//! rendering). Callers wrap the writer in gzip/encryption like Go's
//! `ExportWriter` when writing files.

use std::fmt::Write as FmtWrite;
use std::io::Write;

use datastreamdb_types::{TypeId, Val, Value, convert};

/// Export failures.
#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    /// Underlying storage failure.
    #[error("export storage failed: {0}")]
    Storage(String),
    /// Output write failure.
    #[error("export write failed: {0}")]
    Io(String),
}

/// Export counters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExportStats {
    /// N-Quads (or schema lines) written.
    pub written: u64,
    /// Postings skipped on conversion failure.
    pub skipped: u64,
}

/// RDF type suffix per value type (Go `rdfTypeMap`; default has none).
fn rdf_type_suffix(tid: TypeId) -> Option<&'static str> {
    match tid {
        TypeId::String => Some("xs:string"),
        TypeId::DateTime => Some("xs:dateTime"),
        TypeId::Int => Some("xs:int"),
        TypeId::Float => Some("xs:float"),
        TypeId::Bool => Some("xs:boolean"),
        TypeId::Geo => Some("geo:geojson"),
        TypeId::Binary => Some("xs:base64Binary"),
        TypeId::Password => Some("xs:password"),
        TypeId::BigFloat => Some("xs:decimal"),
        TypeId::VFloat => Some("xs:[]float32"),
        _ => None,
    }
}

/// JSON-escapes a string (Go `escapedString`).
///
/// Go uses `json.Marshal`, which additionally escapes `<`, `>`, `&` and
/// U+2028/29 for HTML safety; replicate that for byte-identical exports.
fn escaped(text: &str) -> String {
    let raw = serde_json::to_string(text).unwrap_or_else(|_| format!("{text:?}"));
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        match ch {
            '<' => out.push_str("\\u003c"),
            '>' => out.push_str("\\u003e"),
            '&' => out.push_str("\\u0026"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            _ => out.push(ch),
        }
    }
    out
}

/// Converts posting bytes to a display string (Go `valToStr`).
fn value_to_string(tid: TypeId, bytes: &[u8]) -> Result<String, String> {
    let from = Val {
        tid,
        value: Value::Binary(bytes.to_vec()),
    };
    let converted = convert(&from, TypeId::String).map_err(|err| err.to_string())?;
    let Val {
        value: Value::String(text),
        ..
    } = converted
    else {
        return Err("conversion did not yield a string".to_owned());
    };
    Ok(text.trim_end_matches('\0').to_owned())
}

/// Renders one facet value (Go `facetToString` + string escaping).
fn facet_value_string(facet: &datastreamdb_proto::api::Facet) -> Result<(String, bool), String> {
    let val_type = match facet.val_type {
        0 => datastreamdb_tok::facets::FacetValType::String,
        1 => datastreamdb_tok::facets::FacetValType::Int,
        2 => datastreamdb_tok::facets::FacetValType::Float,
        3 => datastreamdb_tok::facets::FacetValType::Bool,
        4 => datastreamdb_tok::facets::FacetValType::Datetime,
        other => return Err(format!("unknown facet type {other}")),
    };
    let parsed =
        datastreamdb_tok::facets::val_for(val_type, &facet.value).map_err(|err| err.to_string())?;
    let is_string = matches!(parsed, datastreamdb_tok::facets::FacetValue::String(_));
    let text = match parsed {
        datastreamdb_tok::facets::FacetValue::Int(v) => v.to_string(),
        datastreamdb_tok::facets::FacetValue::Float(v) => v.to_string(),
        datastreamdb_tok::facets::FacetValue::Bool(v) => v.to_string(),
        datastreamdb_tok::facets::FacetValue::DateTime(v) => v.to_rfc3339(),
        datastreamdb_tok::facets::FacetValue::String(v) => v,
    };
    Ok((text, is_string))
}

/// Exports one namespace as N-Quads (Go `toRDF` per posting list).
///
/// Scans every data list, skipping split parts, `_predicate_`, index
/// artifacts, and unparseable keys exactly like Go's `ChooseKey`.
/// Conversion failures skip single postings (counted, like Go's logs).
///
/// # Errors
///
/// Returns [`ExportError`] on storage or write failure.
pub fn export_rdf(
    engine: &datastreamdb_posting::PostingEngine,
    namespace: u64,
    read_ts: u64,
    out: &mut dyn Write,
) -> Result<ExportStats, ExportError> {
    let mut stats = ExportStats::default();
    let pairs = engine
        .store()
        .scan_prefix(datastreamdb_posting::store::PARTITION, b"")
        .map_err(|err| ExportError::Storage(err.to_string()))?;
    for (key_bytes, _) in &pairs {
        let key = match datastreamdb_x::keycodec::parse(key_bytes) {
            Ok(key) if key.is_data() && !key.has_start_uid && key.attr != "_predicate_" => key,
            _ => continue,
        };
        let (ns, attr) = split_ns_attr(&key.attr);
        if namespace != u64::MAX && ns != namespace {
            continue;
        }
        if SKIPPED_ATTRS.contains(&attr) {
            continue;
        }
        let Ok(list) = engine.get_list(key_bytes, read_ts) else {
            stats.skipped += 1;
            continue;
        };
        if is_graphql_type_node(attr, &list, read_ts) {
            continue;
        }
        // The GraphQL schema goes to its own file (Go `ToExportKvList`).
        if attr == "datastreamdb.graphql.schema" {
            continue;
        }
        let prefix = format!("<{:#x}> <{attr}> ", key.uid);
        let walked = list.iterate(read_ts, 0, &mut |posting| {
            match render_posting(&prefix, posting, ns) {
                Ok(line) => {
                    if out.write_all(line.as_bytes()).is_err() {
                        return Err(new_io_error());
                    }
                    stats.written += 1;
                }
                Err(_) => stats.skipped += 1,
            }
            Ok(())
        });
        if walked.is_err() {
            return Err(ExportError::Io("list iteration failed".to_owned()));
        }
    }
    out.flush()
        .map_err(|err| ExportError::Io(err.to_string()))?;
    Ok(stats)
}

/// Splits a namespaced attribute (bare names land in namespace 0).
fn split_ns_attr(attr: &str) -> (u64, &str) {
    attr.split_once('-')
        .and_then(|(ns, rest)| u64::from_str_radix(ns, 16).ok().map(|ns| (ns, rest)))
        .unwrap_or((0, attr))
}

/// Internal predicates never exported (Go `ToExportKvList` skips).
const SKIPPED_ATTRS: &[&str] = &[
    "datastreamdb.graphql.xid",
    "datastreamdb.drop.op",
    "datastreamdb.graphql.p_query",
    "datastreamdb.cors",
    "datastreamdb.graphql.schema_created_at",
    "datastreamdb.graphql.schema_history",
    "datastreamdb.graphql.p_sha256hash",
];

/// Whether the GraphQL-layer `datastreamdb.type` node must be skipped (Go parity).
fn is_graphql_type_node(attr: &str, list: &datastreamdb_posting::List, read_ts: u64) -> bool {
    if attr != "datastreamdb.type" {
        return false;
    }
    let Ok(vals) = list.all_values(read_ts) else {
        return false;
    };
    if vals.len() != 1 {
        return false;
    }
    string_of(&vals[0]) == "datastreamdb.graphql"
}

/// Best-effort string view of a value (export-only).
fn string_of(val: &Val) -> String {
    match &val.value {
        Value::String(text) => text.clone(),
        Value::Binary(bytes) => String::from_utf8_lossy(bytes).into_owned(),
        _ => String::new(),
    }
}

/// Renders one posting as an N-Quad line (Go `toRDF`).
fn render_posting(
    prefix: &str,
    posting: &datastreamdb_proto::pb::Posting,
    namespace: u64,
) -> Result<String, String> {
    use datastreamdb_proto::pb::posting::PostingType;
    const REF: i32 = PostingType::Ref as i32;
    const VALUE_LANG: i32 = PostingType::ValueLang as i32;
    let mut line = String::from(prefix);
    if posting.posting_type == REF {
        write!(line, "<{:#x}>", posting.uid).map_err(|err| err.to_string())?;
    } else {
        let tid = TypeId::try_from(posting.val_type)
            .map_err(|_| format!("bad type {}", posting.val_type))?;
        line.push_str(&escaped(&value_to_string(tid, &posting.value)?));
        if posting.posting_type == VALUE_LANG {
            line.push('@');
            line.push_str(&String::from_utf8_lossy(&posting.lang_tag));
        } else if tid != TypeId::Default && tid != TypeId::Uid {
            let suffix = rdf_type_suffix(tid).ok_or_else(|| format!("no RDF type for {tid:?}"))?;
            line.push_str("^^<");
            line.push_str(suffix);
            line.push('>');
        }
    }
    write!(line, " <{namespace:#x}>").map_err(|err| err.to_string())?;
    if !posting.facets.is_empty() {
        line.push_str(" (");
        for (index, facet) in posting.facets.iter().enumerate() {
            if index > 0 {
                line.push(',');
            }
            let (mut value, is_string) = facet_value_string(facet)?;
            if is_string {
                value = escaped(&value);
            }
            line.push_str(&facet.key);
            line.push('=');
            line.push_str(&value);
        }
        line.push(')');
    }
    line.push_str(" .\n");
    Ok(line)
}

/// Storage error constructor (avoids the concrete backend type).
fn new_io_error() -> datastreamdb_posting::errors::PostingError {
    datastreamdb_posting::errors::PostingError::Corrupt("export write failed".to_owned())
}

/// Renders one JSON object for a posting (Go `toJSON` per posting).
fn render_json_posting(
    uid: u64,
    attr: &str,
    posting: &datastreamdb_proto::pb::Posting,
    namespace: u64,
) -> Result<String, String> {
    use datastreamdb_proto::pb::posting::PostingType;
    const REF: i32 = PostingType::Ref as i32;
    const VALUE_LANG: i32 = PostingType::ValueLang as i32;
    let mut obj = format!("  {{\"uid\":\"{uid:#x}\",\"namespace\":\"{namespace:#x}\"");
    if posting.posting_type == REF {
        write!(obj, ",\"{attr}\":[").map_err(|err| err.to_string())?;
        write!(obj, "{{\"uid\":\"{:#x}\"", posting.uid).map_err(|err| err.to_string())?;
        write_json_facets(&mut obj, attr, &posting.facets)?;
        obj.push_str("}]");
    } else {
        if posting.posting_type == VALUE_LANG {
            let lang = String::from_utf8_lossy(&posting.lang_tag);
            write!(obj, ",\"{attr}@{lang}\":").map_err(|err| err.to_string())?;
        } else {
            write!(obj, ",\"{attr}\":").map_err(|err| err.to_string())?;
        }
        let tid = TypeId::try_from(posting.val_type)
            .map_err(|_| format!("bad type {}", posting.val_type))?;
        let mut text = value_to_string(tid, &posting.value)?;
        if !tid.is_number() {
            text = escaped(&text);
        }
        obj.push_str(&text);
        write_json_facets(&mut obj, attr, &posting.facets)?;
    }
    obj.push('}');
    Ok(obj)
}

/// Appends `"attr|key":value` facet pairs (Go `toJSON.writeFacets`).
fn write_json_facets(
    obj: &mut String,
    attr: &str,
    facets: &[datastreamdb_proto::api::Facet],
) -> Result<(), String> {
    for facet in facets {
        let val_type = match facet.val_type {
            0 => datastreamdb_tok::facets::FacetValType::String,
            1 => datastreamdb_tok::facets::FacetValType::Int,
            2 => datastreamdb_tok::facets::FacetValType::Float,
            3 => datastreamdb_tok::facets::FacetValType::Bool,
            4 => datastreamdb_tok::facets::FacetValType::Datetime,
            other => return Err(format!("unknown facet type {other}")),
        };
        let parsed = datastreamdb_tok::facets::val_for(val_type, &facet.value)
            .map_err(|err| err.to_string())?;
        let tid = datastreamdb_tok::facets::type_id_for(val_type);
        let mut text = match parsed {
            datastreamdb_tok::facets::FacetValue::Int(v) => v.to_string(),
            datastreamdb_tok::facets::FacetValue::Float(v) => v.to_string(),
            datastreamdb_tok::facets::FacetValue::Bool(v) => v.to_string(),
            datastreamdb_tok::facets::FacetValue::DateTime(v) => v.to_rfc3339(),
            datastreamdb_tok::facets::FacetValue::String(v) => v,
        };
        if !tid.is_number() {
            text = escaped(&text);
        }
        write!(obj, ",\"{attr}|{}\":{text}", facet.key).map_err(|err| err.to_string())?;
    }
    Ok(())
}

/// Exports one namespace as JSON objects (Go `toJSON`).
///
/// Objects are framed as a JSON array (`[\n` ... `\n]\n`) with `,\n`
/// separators, matching Go's `exportFormats["json"]` pre/post strings.
///
/// # Errors
///
/// Returns [`ExportError`] on storage or write failure.
pub fn export_json(
    engine: &datastreamdb_posting::PostingEngine,
    namespace: u64,
    read_ts: u64,
    out: &mut dyn Write,
) -> Result<ExportStats, ExportError> {
    let mut stats = ExportStats::default();
    out.write_all(b"[\n")
        .map_err(|err| ExportError::Io(err.to_string()))?;
    let mut wrote_any = false;
    let pairs = engine
        .store()
        .scan_prefix(datastreamdb_posting::store::PARTITION, b"")
        .map_err(|err| ExportError::Storage(err.to_string()))?;
    for (key_bytes, _) in &pairs {
        let key = match datastreamdb_x::keycodec::parse(key_bytes) {
            Ok(key) if key.is_data() && !key.has_start_uid && key.attr != "_predicate_" => key,
            _ => continue,
        };
        let (ns, attr) = split_ns_attr(&key.attr);
        if namespace != u64::MAX && ns != namespace {
            continue;
        }
        if SKIPPED_ATTRS.contains(&attr) || attr == "datastreamdb.graphql.schema" {
            continue;
        }
        let Ok(list) = engine.get_list(key_bytes, read_ts) else {
            stats.skipped += 1;
            continue;
        };
        if is_graphql_type_node(attr, &list, read_ts) {
            continue;
        }
        let walked = list.iterate(read_ts, 0, &mut |posting| {
            match render_json_posting(key.uid, attr, posting, ns) {
                Ok(obj) => {
                    if wrote_any && out.write_all(b",\n").is_err() {
                        return Err(new_io_error());
                    }
                    wrote_any = true;
                    if out.write_all(obj.as_bytes()).is_err() {
                        return Err(new_io_error());
                    }
                    stats.written += 1;
                }
                Err(_) => stats.skipped += 1,
            }
            Ok(())
        });
        if walked.is_err() {
            return Err(ExportError::Io("list iteration failed".to_owned()));
        }
    }
    out.write_all(b"\n]\n")
        .map_err(|err| ExportError::Io(err.to_string()))?;
    out.flush()
        .map_err(|err| ExportError::Io(err.to_string()))?;
    Ok(stats)
}

/// Exports schema text plus type blocks (Go `toSchema`/`toType`).
///
/// # Errors
///
/// Returns [`ExportError::Io`] when writing fails.
pub fn export_schema(
    schema: &datastreamdb_schema::SchemaState,
    namespace: u64,
    out: &mut dyn Write,
) -> Result<ExportStats, ExportError> {
    let mut stats = ExportStats::default();
    let mut preds = schema.predicates();
    preds.sort();
    for pred in preds {
        let Some(update) = schema.get(&pred, false) else {
            continue;
        };
        let (ns, _) = split_ns_attr(&update.predicate);
        if namespace != u64::MAX && ns != namespace {
            continue;
        }
        let line = render_schema(&update, ns);
        out.write_all(line.as_bytes())
            .map_err(|err| ExportError::Io(err.to_string()))?;
        stats.written += 1;
    }
    let mut types = schema.types();
    types.sort();
    for name in types {
        let Some(update) = schema.get_type(&name) else {
            continue;
        };
        let (ns, _) = split_ns_attr(&update.type_name);
        if namespace != u64::MAX && ns != namespace {
            continue;
        }
        let line = render_type(&update, ns);
        out.write_all(line.as_bytes())
            .map_err(|err| ExportError::Io(err.to_string()))?;
        stats.written += 1;
    }
    out.flush()
        .map_err(|err| ExportError::Io(err.to_string()))?;
    Ok(stats)
}

/// Renders one schema line (`[0xN] <pred>:type[...] .`, Go `toSchema`).
/// Renders vector index specs (Go `formatVectorSchema`).
fn render_vector_schema(update: &datastreamdb_proto::pb::SchemaUpdate) -> String {
    let mut out = String::from(" @index(");
    // Note: Go's separator condition (`len-1 < j`) never fires, so it emits
    // no comma between multiple specs; match that quirk byte-for-byte.
    for spec in &update.index_specs {
        out.push_str(&spec.name);
        out.push('(');
        for (index, opt) in spec.options.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            out.push_str(&opt.key);
            out.push_str(":\"");
            out.push_str(&opt.value);
            out.push('"');
        }
        out.push(')');
    }
    out.push(')');
    out
}

fn render_schema(update: &datastreamdb_proto::pb::SchemaUpdate, namespace: u64) -> String {
    use datastreamdb_proto::pb::schema_update::Directive;
    let mut line = format!("[{namespace:#x}] <{}>:", strip_ns(&update.predicate));
    if update.list {
        line.push('[');
    }
    let tid = TypeId::try_from(update.value_type).unwrap_or(TypeId::Default);
    line.push_str(tid.as_str());
    if update.list {
        line.push(']');
    }
    if update.directive == Directive::Reverse as i32 {
        line.push_str(" @reverse");
    } else if update.directive == Directive::Index as i32 && !update.tokenizer.is_empty() {
        line.push_str(" @index(");
        line.push_str(&update.tokenizer.join(","));
        line.push(')');
    } else if !update.index_specs.is_empty() {
        line.push_str(&render_vector_schema(update));
    }
    if update.count {
        line.push_str(" @count");
    }
    if update.lang {
        line.push_str(" @lang");
    }
    if update.upsert {
        line.push_str(" @upsert");
    }
    if update.unique {
        line.push_str(" @unique");
    }
    line.push_str(" . \n");
    line
}

/// Renders one type block (Go `toType`).
fn render_type(update: &datastreamdb_proto::pb::TypeUpdate, namespace: u64) -> String {
    let mut line = format!(
        "[{namespace:#x}] type <{}> {{\n",
        strip_ns(&update.type_name)
    );
    for field in &update.fields {
        let mut name = strip_ns(&field.predicate).to_owned();
        if name.starts_with('~') {
            name = format!("<{name}>");
        }
        line.push('\t');
        line.push_str(&name);
        line.push('\n');
    }
    line.push_str("}\n");
    line
}

/// Strips a `ns-` prefix for display (Go `ParseAttr`).
fn strip_ns(attr: &str) -> &str {
    split_ns_attr(attr).1
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn test_schema() -> Arc<datastreamdb_schema::SchemaState> {
        use datastreamdb_proto::{SchemaUpdate, ValType};
        let schema = Arc::new(datastreamdb_schema::SchemaState::new());
        schema.set(
            "0-name",
            SchemaUpdate {
                predicate: "0-name".to_owned(),
                value_type: ValType::String as i32,
                directive: datastreamdb_proto::Directive::Index as i32,
                tokenizer: vec!["exact".to_owned()],
                ..SchemaUpdate::default()
            },
        );
        schema.set(
            "0-friend",
            SchemaUpdate {
                predicate: "0-friend".to_owned(),
                value_type: ValType::Uid as i32,
                list: true,
                ..SchemaUpdate::default()
            },
        );
        schema
    }

    fn test_engine() -> Arc<datastreamdb_posting::PostingEngine> {
        use datastreamdb_proto::{DirectedEdge, EdgeOp, ValType};
        let engine = datastreamdb_posting::PostingEngine::open(
            Arc::new(datastreamdb_store::MemoryBackend::new()),
            test_schema(),
            64,
        );
        let txn = engine.begin();
        let mut edge = DirectedEdge {
            entity: 2,
            attr: "0-name".to_owned(),
            value: b"Alice".to_vec(),
            value_type: ValType::String as i32,
            op: EdgeOp::Set as i32,
            ..DirectedEdge::default()
        };
        let key = datastreamdb_x::data_key("0-name", 2);
        txn.get(&key)
            .expect("list")
            .add_mutation_with_index(&mut edge, &txn)
            .expect("mutate");
        let mut edge = DirectedEdge {
            entity: 2,
            attr: "0-friend".to_owned(),
            value_id: 3,
            value_type: ValType::Uid as i32,
            op: EdgeOp::Set as i32,
            ..DirectedEdge::default()
        };
        let key = datastreamdb_x::data_key("0-friend", 2);
        txn.get(&key)
            .expect("list")
            .add_mutation_with_index(&mut edge, &txn)
            .expect("mutate");
        engine.commit(&txn).expect("commit");
        engine
    }

    #[test]
    fn exports_golden_rdf() {
        let engine = test_engine();
        let read_ts = engine.oracle().max_assigned();
        let mut out = Vec::new();
        let stats = export_rdf(&engine, 0, read_ts, &mut out).expect("export");
        assert_eq!(stats.written, 2);
        let text = String::from_utf8(out).expect("utf8");
        let mut lines: Vec<&str> = text.lines().collect();
        lines.sort_unstable();
        assert_eq!(
            lines,
            vec![
                "<0x2> <friend> <0x3> <0x0> .",
                "<0x2> <name> \"Alice\"^^<xs:string> <0x0> .",
            ]
        );
    }

    #[test]
    fn export_roundtrips_through_chunker() {
        let engine = test_engine();
        let read_ts = engine.oracle().max_assigned();
        let mut out = Vec::new();
        export_rdf(&engine, 0, read_ts, &mut out).expect("export");
        let (quads, _) = datastreamdb_chunker::parse_rdfs(&out).expect("reparse");
        assert_eq!(quads.len(), 2);
        assert!(quads.iter().any(|q| q.predicate == "name"));
        assert!(quads.iter().any(|q| q.predicate == "friend"));
    }

    #[test]
    fn render_covers_ref_lang_facets() {
        use datastreamdb_proto::ValType as PValType;
        use datastreamdb_proto::api::Facet;
        use datastreamdb_proto::pb::{Posting, posting::PostingType};

        // Reference posting.
        let line = render_posting(
            "<0x2> <friend> ",
            &Posting {
                uid: 3,
                posting_type: PostingType::Ref as i32,
                ..Posting::default()
            },
            0,
        )
        .expect("ref");
        assert_eq!(line, "<0x2> <friend> <0x3> <0x0> .\n");

        // Language-tagged value.
        let line = render_posting(
            "<0x2> <name> ",
            &Posting {
                value: "Alícia".as_bytes().to_vec(),
                val_type: PValType::String as i32,
                posting_type: PostingType::ValueLang as i32,
                lang_tag: b"pt".to_vec(),
                ..Posting::default()
            },
            0,
        )
        .expect("lang");
        assert_eq!(line, "<0x2> <name> \"Alícia\"@pt <0x0> .\n");

        // Facets (string escaped, int raw, comma-joined without spaces).
        let line = render_posting(
            "<0x2> <name> ",
            &Posting {
                value: b"Ada".to_vec(),
                val_type: PValType::String as i32,
                posting_type: 1,
                facets: vec![
                    Facet {
                        key: "since".to_owned(),
                        value: 2020i64.to_le_bytes().to_vec(),
                        val_type: 1,
                        ..Facet::default()
                    },
                    Facet {
                        key: "note".to_owned(),
                        value: b"a \"b\"".to_vec(),
                        val_type: 0,
                        ..Facet::default()
                    },
                ],
                ..Posting::default()
            },
            7,
        )
        .expect("facets");
        assert_eq!(
            line,
            "<0x2> <name> \"Ada\"^^<xs:string> <0x7> (since=2020,note=\"a \\\"b\\\"\") .\n"
        );
    }

    #[test]
    fn escaped_matches_go_json_marshal() {
        // Go's `json.Marshal` HTML-escapes `<`, `>`, `&`.
        assert_eq!(
            escaped("a<b>&\"c\""),
            "\"a\\u003cb\\u003e\\u0026\\\"c\\\"\""
        );
        assert_eq!(escaped("Ding!\u{7}Ding!"), "\"Ding!\\u0007Ding!\"");
    }

    #[test]
    fn renders_go_verbatim_goldens() {
        use datastreamdb_proto::ValType as PValType;
        use datastreamdb_proto::pb::Posting;
        // Mirrors `dgraph-main/worker/export_test.go` expectations.
        let line = render_posting(
            "<0x5> <name> ",
            &Posting {
                value: Vec::new(),
                val_type: PValType::Default as i32,
                posting_type: 1,
                ..Posting::default()
            },
            0,
        )
        .expect("empty");
        assert_eq!(line, "<0x5> <name> \"\" <0x0> .\n");
        let line = render_posting(
            "<0x9> <name> ",
            &Posting {
                value: b"ns2".to_vec(),
                val_type: PValType::Default as i32,
                posting_type: 1,
                ..Posting::default()
            },
            2,
        )
        .expect("ns2");
        assert_eq!(line, "<0x9> <name> \"ns2\" <0x2> .\n");
    }

    #[test]
    fn exports_json_array() {
        let engine = test_engine();
        let read_ts = engine.oracle().max_assigned();
        let mut out = Vec::new();
        let stats = export_json(&engine, 0, read_ts, &mut out).expect("export");
        assert_eq!(stats.written, 2);
        let text = String::from_utf8(out).expect("utf8");
        assert!(text.starts_with("[\n"));
        assert!(text.ends_with("\n]\n"));
        let value: serde_json::Value = serde_json::from_str(&text).expect("json");
        let items = value.as_array().expect("array");
        assert_eq!(items.len(), 2);
        assert!(items.iter().any(|v| v.get("name").is_some()));
        assert!(items.iter().any(|v| v.get("friend").is_some()));
        for item in items {
            assert_eq!(item.get("namespace").and_then(|v| v.as_str()), Some("0x0"));
        }
    }

    #[test]
    fn skips_internal_predicates() {
        use datastreamdb_proto::{DirectedEdge, EdgeOp, ValType};
        let schema = test_schema();
        let engine = datastreamdb_posting::PostingEngine::open(
            Arc::new(datastreamdb_store::MemoryBackend::new()),
            schema,
            64,
        );
        let txn = engine.begin();
        let mut edge = DirectedEdge {
            entity: 9,
            attr: "0-datastreamdb.cors".to_owned(),
            value: b"x".to_vec(),
            value_type: ValType::String as i32,
            op: EdgeOp::Set as i32,
            ..DirectedEdge::default()
        };
        let key = datastreamdb_x::data_key("0-datastreamdb.cors", 9);
        txn.get(&key)
            .expect("list")
            .add_mutation_with_index(&mut edge, &txn)
            .expect("mutate");
        engine.commit(&txn).expect("commit");
        let read_ts = engine.oracle().max_assigned();
        let mut out = Vec::new();
        let stats = export_rdf(&engine, 0, read_ts, &mut out).expect("export");
        assert_eq!(stats.written, 0);
        assert!(out.is_empty());
    }

    #[test]
    fn export_schema_roundtrips() {
        let schema = test_schema();
        let mut out = Vec::new();
        let stats = export_schema(&schema, 0, &mut out).expect("export");
        assert_eq!(stats.written, 2);
        let text = String::from_utf8(out).expect("utf8");
        assert!(text.contains("[0x0] <name>:string @index(exact) . \n"));
        assert!(text.contains("[0x0] <friend>:[uid] . \n"));
        let parsed = datastreamdb_schema::parse_with_namespace(&text, 0).expect("reparse");
        assert_eq!(parsed.preds.len(), 2);
    }
}
