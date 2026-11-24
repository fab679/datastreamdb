//! JSON mutation parser and N-Quad buffer. Port of `json_parser.go`.

#![warn(missing_docs)]

use std::collections::{BTreeMap, HashMap};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use datastreamdb_proto::{HintType, Metadata};
use datastreamdb_tok::facets::{Facet, FacetJson, FacetValType, facet_for, to_binary};
use datastreamdb_types::strconv::{
    parse_go_float, parse_go_int, parse_go_uint_base0, quote_go_string,
};
use datastreamdb_types::{ApiValue, float_array_as_bytes, parse_time, parse_v_float};

use super::model::NQuad;
use crate::ChunkError;

/// Mutation operation: set (add) or delete N-Quads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    /// Parsed N-Quads are meant to be added.
    Set,
    /// Parsed N-Quads are meant to be deleted.
    Delete,
}

/// JSON value with verbatim number literals (port of Go's `json.Number`).
///
/// Go distinguishes `26` (int) from `26.0`/`1e3` (float) and rejects
/// out-of-range integer literals; all of that needs the literal text, so
/// numbers are captured raw via [`serde_json::value::RawValue`] instead of
/// serde's typed `Number`.
#[derive(Clone, Debug, PartialEq)]
enum JVal {
    /// JSON null.
    Null,
    /// Boolean.
    Bool(bool),
    /// Verbatim number literal (`26`, `26.0`, `-1e10`).
    Num(String),
    /// String.
    Str(String),
    /// Array.
    Arr(Vec<JVal>),
    /// Object (sorted by key; Go randomizes map order).
    Obj(BTreeMap<String, JVal>),
}

impl JVal {
    /// Parses one JSON value from raw text (must be exactly one value).
    fn from_raw(text: &str) -> Result<Self, ChunkError> {
        let err = || ChunkError::message(format!("invalid JSON value: {text:?}"));
        let text = text.trim_matches([' ', '\t', '\r', '\n']);
        let Some(first) = text.as_bytes().first() else {
            return Err(err());
        };
        match first {
            b'{' => {
                let map: BTreeMap<String, Box<serde_json::value::RawValue>> =
                    serde_json::from_str(text).map_err(|_| err())?;
                let mut obj = BTreeMap::new();
                for (k, v) in map {
                    obj.insert(k, Self::from_raw(v.get())?);
                }
                Ok(Self::Obj(obj))
            }
            b'[' => {
                let list: Vec<Box<serde_json::value::RawValue>> =
                    serde_json::from_str(text).map_err(|_| err())?;
                let mut arr = Vec::with_capacity(list.len());
                for v in list {
                    arr.push(Self::from_raw(v.get())?);
                }
                Ok(Self::Arr(arr))
            }
            b'"' => serde_json::from_str(text).map(Self::Str).map_err(|_| err()),
            b't' if text == "true" => Ok(Self::Bool(true)),
            b'f' if text == "false" => Ok(Self::Bool(false)),
            b'n' if text == "null" => Ok(Self::Null),
            _ => {
                if is_json_number(text) {
                    Ok(Self::Num(text.to_owned()))
                } else {
                    Err(err())
                }
            }
        }
    }

    /// Renders back to JSON (numbers verbatim).
    fn to_json(&self) -> String {
        match self {
            Self::Null => "null".to_owned(),
            Self::Bool(v) => v.to_string(),
            Self::Num(lit) => lit.clone(),
            Self::Str(s) => quote_go_string(s),
            Self::Arr(items) => {
                let inner: Vec<String> = items.iter().map(Self::to_json).collect();
                format!("[{}]", inner.join(","))
            }
            Self::Obj(map) => {
                let inner: Vec<String> = map
                    .iter()
                    .map(|(k, v)| format!("{}:{}", quote_go_string(k), v.to_json()))
                    .collect();
                format!("{{{}}}", inner.join(","))
            }
        }
    }
}

impl std::fmt::Display for JVal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_json())
    }
}

/// Strict JSON number grammar: `-?(0|[1-9]\d*)(\.\d+)?([eE][+-]?\d+)?`.
fn is_json_number(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut i = 0;
    if bytes.first() == Some(&b'-') {
        i += 1;
    }
    match bytes.get(i) {
        Some(b'0') => i += 1,
        Some(b'1'..=b'9') => {
            while bytes.get(i).is_some_and(u8::is_ascii_digit) {
                i += 1;
            }
        }
        _ => return false,
    }
    if bytes.get(i) == Some(&b'.') {
        i += 1;
        let frac = i;
        while bytes.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        if i == frac {
            return false;
        }
    }
    if bytes.get(i).is_some_and(|b| matches!(b, b'e' | b'E')) {
        i += 1;
        if bytes.get(i).is_some_and(|b| matches!(b, b'+' | b'-')) {
            i += 1;
        }
        let exp = i;
        while bytes.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        if i == exp {
            return false;
        }
    }
    i == bytes.len()
}

/// Wrapping `i64 as u64` (Go's `uint64()` conversion wraps negatives).
#[allow(clippy::cast_sign_loss, reason = "Go uint64() wraps")]
fn wrap_u64(value: i64) -> u64 {
    value as u64
}

/// Removes all whitespace, like Go's `stripSpaces`.
fn strip_spaces(s: &str) -> String {
    s.chars().filter(|c| !crate::is_go_space(*c)).collect()
}

/// Parses a facet value to string/float/bool/datetime.
fn handle_basic_facets_type(key: &str, val: &JVal) -> Result<Facet, ChunkError> {
    match val {
        JVal::Str(v) => {
            if let Ok(t) = parse_time(v) {
                return to_binary(key, &FacetJson::Datetime(t), FacetValType::Datetime)
                    .map_err(|e| ChunkError::message(e.to_string()));
            }
            // FacetFor already converts the value to binary.
            facet_for(key, &quote_go_string(v)).map_err(|e| ChunkError::message(e.to_string()))
        }
        JVal::Num(literal) => {
            // NOTE: Go checks for '.' only here (not exponents).
            if literal.contains('.') {
                let f = parse_go_float(literal).map_err(|e| ChunkError::message(e.to_string()))?;
                to_binary(key, &FacetJson::Float(f), FacetValType::Float)
            } else {
                let i = parse_go_int(literal).map_err(|e| ChunkError::message(e.to_string()))?;
                to_binary(key, &FacetJson::Int(i), FacetValType::Int)
            }
            .map_err(|e| ChunkError::message(e.to_string()))
        }
        JVal::Bool(v) => to_binary(key, &FacetJson::Bool(*v), FacetValType::Bool)
            .map_err(|e| ChunkError::message(e.to_string())),
        _ => Err(ChunkError::message(
            "facet value can only be string/number/bool",
        )),
    }
}

/// Parses map-format facets for scalar-list predicates (`"pred|facet": {"0": v}`).
fn parse_map_facets(
    m: &BTreeMap<String, JVal>,
    prefix: &str,
) -> Result<Vec<HashMap<i64, Facet>>, ChunkError> {
    // This happens at root.
    if prefix.is_empty() {
        return Ok(Vec::new());
    }
    let mut maps = Vec::new();
    for (fname, facet_val) in m {
        if matches!(facet_val, JVal::Null) || !fname.starts_with(prefix) {
            continue;
        }
        let JVal::Obj(fm) = facet_val else {
            return Err(ChunkError::message(format!(
                "facets format should be of type map for scalarlist predicates, found: {facet_val} for facet: {fname}"
            )));
        };
        let mut idx_map = HashMap::with_capacity(fm.len());
        for (sidx, val) in fm {
            let key = &fname[prefix.len()..];
            let facet = handle_basic_facets_type(key, val)
                .map_err(|e| ChunkError::message(format!("facet: {fname}, index: {sidx}: {e}")))?;
            let idx = parse_go_int(sidx)
                .map_err(|e| ChunkError::message(format!("facet: {fname}, index: {sidx}: {e}")))?;
            idx_map.insert(idx, facet);
        }
        maps.push(idx_map);
    }
    Ok(maps)
}

/// Parses scalar facets (`"pred|facet": value`).
fn parse_scalar_facets(m: &BTreeMap<String, JVal>, prefix: &str) -> Result<Vec<Facet>, ChunkError> {
    // This happens at root.
    if prefix.is_empty() {
        return Ok(Vec::new());
    }
    let mut facets = Vec::new();
    for (fname, facet_val) in m {
        if matches!(facet_val, JVal::Null) || !fname.starts_with(prefix) {
            continue;
        }
        let key = &fname[prefix.len()..];
        let facet = handle_basic_facets_type(key, facet_val)
            .map_err(|e| ChunkError::message(format!("facet: {fname}: {e}")))?;
        facets.push(facet);
    }
    Ok(facets)
}

/// Response for one JSON object.
struct MapResponse {
    /// UID retrieved or allocated for the node.
    uid: String,
    /// Namespace the node belongs to.
    namespace: u64,
    /// Facets on the edge connecting this node to its parent, if any.
    fcts: Vec<Facet>,
    /// Raw `pred|facet` entries hoisted out of the object.
    raw_facets: BTreeMap<String, JVal>,
}

fn handle_basic_type(pred: &str, v: &JVal, op: Op, nq: &mut NQuad) -> Result<(), ChunkError> {
    match v {
        JVal::Num(literal) => {
            if literal.contains(['.', 'e', 'E']) {
                let f = parse_go_float(literal).map_err(|e| ChunkError::message(e.to_string()))?;
                nq.object_value = Some(ApiValue::Double(f));
                return Ok(());
            }
            let i = parse_go_int(literal).map_err(|e| ChunkError::message(e.to_string()))?;
            nq.object_value = Some(ApiValue::Int(i));
        }
        JVal::Str(v) => {
            // Default value is considered as S P * deletion.
            if v.is_empty() && op == Op::Delete {
                nq.object_value = Some(ApiValue::Default(datastreamdb_x::STAR.to_owned()));
                return Ok(());
            }
            if let Ok(vf) = parse_v_float(v) {
                if !vf.is_empty() {
                    nq.object_value = Some(ApiValue::Vfloat(float_array_as_bytes(&vf)));
                    return Ok(());
                }
            }
            // Handle the uid function in upsert block.
            let s = strip_spaces(v);
            if s.starts_with("uid(") || s.starts_with("val(") {
                if !s.ends_with(')') {
                    return Err(ChunkError::message(format!(
                        "while processing '{s}', brackets are not closed properly"
                    )));
                }
                nq.object_id = s;
                return Ok(());
            }
            // In RDF everything is default, but in JSON we assume string.
            // The value is checked against the schema so a password type is
            // not overshadowed. Issue#2623.
            nq.object_value = Some(ApiValue::Str(v.clone()));
        }
        JVal::Bool(v) => {
            if !v && op == Op::Delete {
                nq.object_value = Some(ApiValue::Default(datastreamdb_x::STAR.to_owned()));
                return Ok(());
            }
            nq.object_value = Some(ApiValue::Bool(*v));
        }
        _ => {
            return Err(ChunkError::message(format!(
                "unexpected type for val for attr: {pred} while converting to nquad"
            )));
        }
    }
    Ok(())
}

/// Tries to read a two-key `{type, coordinates}` map as a geo value.
fn handle_geo_type(val: &BTreeMap<String, JVal>, nq: &mut NQuad) -> bool {
    if val.len() == 2 && val.contains_key("type") && val.contains_key("coordinates") {
        let text = JVal::Obj(val.clone()).to_json();
        if try_parse_as_geo(&text, nq) {
            return true;
        }
    }
    false
}

/// Parses `GeoJSON` text into a WKB geo value; `false` means "not geo".
///
/// Like Go, conversion failures fall through to regular map handling.
fn try_parse_as_geo(text: &str, nq: &mut NQuad) -> bool {
    let Ok(geom) = datastreamdb_types::geo::geo_from_json_text(text) else {
        return false;
    };
    nq.object_value = Some(ApiValue::Geo(datastreamdb_types::geo::geo_to_wkb(&geom)));
    true
}

/// Batches up to `batch_size` N-Quads per batch, drained with
/// [`NQuadBuffer::take_batches`]. A negative size buffers everything for one
/// batch at [`NQuadBuffer::flush`].
pub struct NQuadBuffer {
    batch_size: i64,
    pending: Vec<NQuad>,
    batches: Vec<Vec<NQuad>>,
    pred_hints: HashMap<String, HintType>,
}

impl NQuadBuffer {
    /// Returns a buffer with the given batch size.
    ///
    /// # Panics
    ///
    /// Panics when a positive `batch_size` does not fit `usize` (32-bit
    /// targets with huge sizes; Go fails its allocation there too).
    #[must_use]
    pub fn new(batch_size: i64) -> Self {
        let pending = if batch_size > 0 {
            let capacity = usize::try_from(batch_size).expect("checked positive");
            Vec::with_capacity(capacity)
        } else {
            Vec::new()
        };
        Self {
            batch_size,
            pending,
            batches: Vec::new(),
            pred_hints: HashMap::new(),
        }
    }

    /// Pushes one N-Quad, flushing a full batch.
    ///
    /// # Panics
    ///
    /// Panics when a positive `batch_size` does not fit `usize` (see
    /// [`NQuadBuffer::new`]).
    pub fn push(&mut self, nq: NQuad) {
        self.pending.push(nq);
        if self.batch_size > 0 {
            let batch_size = usize::try_from(self.batch_size).expect("checked positive");
            if self.pending.len() >= batch_size {
                let batch = std::mem::replace(&mut self.pending, Vec::with_capacity(batch_size));
                self.batches.push(batch);
            }
        }
    }

    /// N-Quads buffered but not yet batched.
    #[must_use]
    pub fn pending(&self) -> &[NQuad] {
        &self.pending
    }

    /// Drains completed batches (replaces Go's channel).
    pub fn take_batches(&mut self) -> Vec<Vec<NQuad>> {
        std::mem::take(&mut self.batches)
    }

    /// Parse metadata aggregated so far.
    #[must_use]
    pub fn metadata(&self) -> Metadata {
        let mut metadata = Metadata::default();
        for (pred, hint) in &self.pred_hints {
            metadata.set_hint(pred, *hint);
        }
        metadata
    }

    /// Aggregates a predicate hint (conflicts widen to `LIST`).
    pub fn push_pred_hint(&mut self, pred: &str, hint: HintType) {
        let hint = match self.pred_hints.get(pred) {
            Some(old) if *old != hint => HintType::List,
            _ => hint,
        };
        self.pred_hints.insert(pred.to_owned(), hint);
    }

    /// Flushes buffered N-Quads into a final batch. The buffer should not be
    /// used afterwards.
    pub fn flush(&mut self) {
        if !self.pending.is_empty() {
            let batch = std::mem::take(&mut self.pending);
            self.batches.push(batch);
        }
    }

    /// S `*` `*` deletion for UID-only objects.
    fn check_for_deletion(&mut self, mr: &MapResponse, keys: usize, op: Op) {
        // Since uid is the only key, this must be S * * deletion.
        if op == Op::Delete && !mr.uid.is_empty() && keys == 1 && mr.raw_facets.is_empty() {
            self.push(NQuad {
                subject: mr.uid.clone(),
                predicate: datastreamdb_x::STAR.to_owned(),
                namespace: mr.namespace,
                object_value: Some(ApiValue::Default(datastreamdb_x::STAR.to_owned())),
                ..NQuad::default()
            });
        }
    }

    /// Parses one JSON object into N-Quads. Returns the response plus the
    /// post-facet-extraction key count (for [`NQuadBuffer::check_for_deletion`]).
    fn map_to_nquads(
        &mut self,
        m: &mut BTreeMap<String, JVal>,
        op: Op,
        parent_pred: &str,
    ) -> Result<(MapResponse, usize), ChunkError> {
        let mut mr = MapResponse {
            uid: String::new(),
            namespace: 0,
            fcts: Vec::new(),
            raw_facets: BTreeMap::new(),
        };
        // Move all facets from the global map to the smaller rawFacets map.
        m.retain(|k, v| {
            if k.contains(datastreamdb_x::FACET_DELIMITER) {
                mr.raw_facets.insert(k.clone(), v.clone());
                false
            } else {
                true
            }
        });

        // Check field in map.
        mr.uid = extract_uid(m, op)?;
        mr.namespace = extract_namespace(m)?;
        let namespace = mr.namespace;

        // Owned iteration avoids cloning values.
        let keys = m.len();
        let owned = std::mem::take(m);
        for (pred, v) in owned {
            // The uid was extracted above; facets ride with their predicate.
            if pred == "uid" || pred == "namespace" {
                continue;
            }
            if matches!(v, JVal::Null) {
                if op == Op::Delete {
                    // This corresponds to edge deletion.
                    self.push_null_star(&mr.uid, namespace, &pred);
                }
                // If op is Set, ignore this triplet and continue.
                continue;
            }

            let mut nq = NQuad {
                subject: mr.uid.clone(),
                predicate: pred.clone(),
                namespace,
                ..NQuad::default()
            };
            let prefix = format!("{pred}{}", datastreamdb_x::FACET_DELIMITER);
            if !matches!(v, JVal::Arr(_)) {
                nq.facets = parse_scalar_facets(&mr.raw_facets, &prefix)?;
            }
            // Split predicate and lang directive (ex: "name@en"), if needed.
            crate::split_lang(&mut nq);

            match v {
                JVal::Str(_) | JVal::Num(_) | JVal::Bool(_) => {
                    handle_basic_type(&pred, &v, op, &mut nq)?;
                    self.push(nq);
                    self.push_pred_hint(&pred, HintType::Single);
                }
                JVal::Obj(map) => {
                    self.handle_map_pred(nq, &pred, map, op)?;
                }
                JVal::Arr(items) => {
                    self.handle_list_pred(&mr.uid, namespace, &pred, &items, &mr.raw_facets, op)?;
                }
                // Null is handled above; this mirrors Go's dead default.
                JVal::Null => {
                    return Err(ChunkError::message(format!(
                        "unexpected type for val for attr: {pred} while converting to nquad"
                    )));
                }
            }
        }

        mr.fcts = parse_scalar_facets(
            &mr.raw_facets,
            &format!("{parent_pred}{}", datastreamdb_x::FACET_DELIMITER),
        )?;
        Ok((mr, keys))
    }

    /// Pushes an `S P *` deletion for a null value.
    fn push_null_star(&mut self, uid: &str, namespace: u64, pred: &str) {
        let mut nq = NQuad {
            subject: uid.to_owned(),
            predicate: pred.to_owned(),
            namespace,
            object_value: Some(ApiValue::Default(datastreamdb_x::STAR.to_owned())),
            ..NQuad::default()
        };
        // Split predicate and lang directive (ex: "name@en"), the only way to
        // send language with JSON mutations.
        crate::split_lang(&mut nq);
        self.push(nq);
    }

    /// Handles an object value: geo literal or connecting edge.
    fn handle_map_pred(
        &mut self,
        mut nq: NQuad,
        pred: &str,
        map: BTreeMap<String, JVal>,
        op: Op,
    ) -> Result<(), ChunkError> {
        if map.is_empty() {
            return Ok(());
        }
        if handle_geo_type(&map, &mut nq) {
            self.push(nq);
            self.push_pred_hint(pred, HintType::Single);
            return Ok(());
        }
        let mut nested = map;
        let (cr, _) = self.map_to_nquads(&mut nested, op, pred)?;
        // Add the connecting edge between the entities.
        nq.object_id = cr.uid;
        nq.facets = cr.fcts;
        self.push(nq);
        self.push_pred_hint(pred, HintType::Single);
        Ok(())
    }

    /// Handles a list value: scalar items, nested objects, or an error.
    fn handle_list_pred(
        &mut self,
        uid: &str,
        namespace: u64,
        pred: &str,
        items: &[JVal],
        raw_facets: &BTreeMap<String, JVal>,
        op: Op,
    ) -> Result<(), ChunkError> {
        self.push_pred_hint(pred, HintType::List);
        // NOTE: facets_map_slice stays empty unless scalar list.
        let mut facets_map_slice: Vec<HashMap<i64, Facet>> = Vec::new();
        for (idx, item) in items.iter().enumerate() {
            if idx == 0 {
                // Determine if this is a scalar list (bools do not count).
                if matches!(item, JVal::Str(_) | JVal::Num(_)) {
                    let prefix = format!("{pred}{}", datastreamdb_x::FACET_DELIMITER);
                    facets_map_slice = parse_map_facets(raw_facets, &prefix)?;
                }
            }
            match item {
                JVal::Str(_) | JVal::Num(_) => {
                    let nq =
                        list_scalar_nq(uid, namespace, pred, item, idx, &facets_map_slice, op)?;
                    self.push(nq);
                }
                JVal::Obj(map) => {
                    let nq = self.list_map_nq(uid, namespace, pred, map, op)?;
                    self.push(nq);
                }
                _ => {
                    return Err(ChunkError::message(format!(
                        "got unsupported type for list: {pred}"
                    )));
                }
            }
        }
        Ok(())
    }

    /// Builds a list item holding a nested object (geo or connecting edge).
    fn list_map_nq(
        &mut self,
        uid: &str,
        namespace: u64,
        pred: &str,
        map: &BTreeMap<String, JVal>,
        op: Op,
    ) -> Result<NQuad, ChunkError> {
        let mut nq = NQuad {
            subject: uid.to_owned(),
            predicate: pred.to_owned(),
            namespace,
            ..NQuad::default()
        };
        // Maps can mean geojson or a connecting entity.
        if handle_geo_type(map, &mut nq) {
            return Ok(nq);
        }
        let mut nested = map.clone();
        let (cr, _) = self.map_to_nquads(&mut nested, op, pred)?;
        nq.object_id = cr.uid;
        nq.facets = cr.fcts;
        Ok(nq)
    }

    /// Parses a JSON buffer, pushing N-Quads into the buffer.
    ///
    /// Reads one top-level value like Go's decoder (trailing bytes ignored).
    ///
    /// # Errors
    ///
    /// Returns [`ChunkError`] on malformed JSON or values.
    pub fn parse_json(&mut self, data: &[u8], op: Op) -> Result<(), ChunkError> {
        let text = std::str::from_utf8(data)
            .map_err(|e| ChunkError::message(format!("invalid JSON: {e}")))?;
        // Capture one raw value (trailing bytes ignored, like Go), then parse.
        let mut de = serde_json::Deserializer::from_str(text);
        let raw = <Box<serde_json::value::RawValue> as serde::Deserialize>::deserialize(&mut de)
            .map_err(|e| ChunkError::message(format!("invalid JSON: {e}")))?;
        match JVal::from_raw(raw.get())? {
            JVal::Null => Ok(()),
            JVal::Obj(map) => {
                if map.is_empty() {
                    return Ok(());
                }
                let mut m = map;
                let (mr, keys) = self.map_to_nquads(&mut m, op, "")?;
                self.check_for_deletion(&mr, keys, op);
                Ok(())
            }
            JVal::Arr(list) => {
                if list.is_empty() {
                    return Ok(());
                }
                for obj in list {
                    let JVal::Obj(map) = obj else {
                        return Err(ChunkError::message("only array of map allowed at root"));
                    };
                    let mut m = map;
                    let (mr, keys) = self.map_to_nquads(&mut m, op, "")?;
                    self.check_for_deletion(&mr, keys, op);
                }
                Ok(())
            }
            _ => Err(ChunkError::message("only array of map allowed at root")),
        }
    }
}

/// Reads the object UID: number, blank node, `uid(...)`, or uint text.
/// Missing UIDs allocate a blank node (sets) or error (deletes).
fn extract_uid(m: &BTreeMap<String, JVal>, op: Op) -> Result<String, ChunkError> {
    let mut found = String::new();
    if let Some(uid_val) = m.get("uid") {
        let mut uid = 0u64;
        match uid_val {
            JVal::Num(literal) => {
                let i = parse_go_int(literal).map_err(|e| ChunkError::message(e.to_string()))?;
                uid = wrap_u64(i);
            }
            JVal::Str(text) => {
                let s = strip_spaces(text);
                if text.is_empty() {
                    uid = 0;
                } else if text.starts_with("_:") {
                    found.clone_from(text);
                } else if s.starts_with("uid(") {
                    found = s;
                } else {
                    match parse_go_uint_base0(text) {
                        Ok(u) => uid = u,
                        Err(e) => return Err(ChunkError::message(e.to_string())),
                    }
                }
            }
            _ => {}
        }
        if uid > 0 {
            found = uid.to_string();
        }
    }
    if found.is_empty() {
        if op == Op::Delete {
            // Delete operations with a non-nil value must have an uid specified.
            return Err(ChunkError::EmptyUid);
        }
        found = next_blank();
    }
    Ok(found)
}

/// Reads the object namespace (root by default).
fn extract_namespace(m: &BTreeMap<String, JVal>) -> Result<u64, ChunkError> {
    let mut namespace = datastreamdb_x::ROOT_NAMESPACE;
    if let Some(ns) = m.get("namespace") {
        match ns {
            JVal::Num(literal) => {
                let nsi = parse_go_int(literal).map_err(|e| ChunkError::message(e.to_string()))?;
                namespace = wrap_u64(nsi);
            }
            JVal::Str(text) => {
                let s = strip_spaces(text);
                if s.is_empty() {
                    namespace = 0;
                } else {
                    match parse_go_uint_base0(&s) {
                        Ok(n) => namespace = n,
                        Err(e) => return Err(ChunkError::message(e.to_string())),
                    }
                }
            }
            _ => {}
        }
    }
    Ok(namespace)
}

/// Builds a scalar list item, attaching its indexed facets.
#[allow(clippy::too_many_arguments, reason = "mirrors Go's inline block")]
fn list_scalar_nq(
    uid: &str,
    namespace: u64,
    pred: &str,
    item: &JVal,
    idx: usize,
    facets_map_slice: &[HashMap<i64, Facet>],
    op: Op,
) -> Result<NQuad, ChunkError> {
    let mut nq = NQuad {
        subject: uid.to_owned(),
        predicate: pred.to_owned(),
        namespace,
        ..NQuad::default()
    };
    handle_basic_type(pred, item, op, &mut nq)?;
    // Populate facets from the maps (each maps one item index to its facet).
    let mut fts = Vec::new();
    for fm in facets_map_slice {
        if let Some(ft) = fm.get(&i64::try_from(idx).expect("list index fits")) {
            fts.push(ft.clone());
        }
    }
    nq.facets = fts;
    // NOTE: like Go, list items keep the predicate unsplit (no lang here).
    Ok(nq)
}

/// Parses JSON into N-Quads in one call (convenience wrapper; high memory).
///
/// # Errors
///
/// Returns [`ChunkError`] on malformed JSON or values.
pub fn parse_json(data: &[u8], op: Op) -> Result<(Vec<NQuad>, Metadata), ChunkError> {
    let mut buf = NQuadBuffer::new(-1);
    buf.parse_json(data, op)?;
    buf.flush();
    let mut batches = buf.take_batches();
    let nqs = batches.pop().unwrap_or_default();
    Ok((nqs, buf.metadata()))
}

/// Blank-node counter (process-wide, like Go's `nextIdx`).
static NEXT_IDX: AtomicU64 = AtomicU64::new(0);

/// Random node-id salt (avoids collisions with user UIDs).
static RANDOM_ID: OnceLock<u32> = OnceLock::new();

fn random_id() -> u32 {
    *RANDOM_ID.get_or_init(|| {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0x2545_F491, |d| d.subsec_nanos());
        nanos ^ std::process::id().wrapping_mul(0x9E37_79B9)
    })
}

fn next_blank() -> String {
    let id = NEXT_IDX.fetch_add(1, Ordering::Relaxed) + 1;
    format!("_:dg.{}.{}", random_id(), id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn number_grammar() {
        for ok in [
            "0", "-0", "26", "-26", "26.0", "-1e10", "0E-0", "1E+2", "1.5e-3",
        ] {
            assert!(is_json_number(ok), "{ok}");
        }
        for bad in [
            "", "-", "01", "1.", ".5", "1e", "1e+", "NaN", "0x1", "1_0", "--1", "e5",
        ] {
            assert!(!is_json_number(bad), "{bad}");
        }
    }

    #[test]
    fn raw_values_keep_literals() {
        match JVal::from_raw("{\"a\": 26, \"b\": 1e3, \"c\": [true, null]}").expect("parse") {
            JVal::Obj(map) => {
                assert_eq!(map["a"], JVal::Num("26".into()));
                assert_eq!(map["b"], JVal::Num("1e3".into()));
                match &map["c"] {
                    JVal::Arr(items) => assert_eq!(items, &[JVal::Bool(true), JVal::Null]),
                    other => panic!("{other:?}"),
                }
            }
            other => panic!("{other:?}"),
        }
        assert!(JVal::from_raw("01").is_err());
        assert!(JVal::from_raw("{\"a\": }").is_err());
    }

    #[test]
    fn blank_nodes_sequence() {
        let a = next_blank();
        let b = next_blank();
        assert!(a.starts_with("_:dg."));
        assert_ne!(a, b);
    }
}
