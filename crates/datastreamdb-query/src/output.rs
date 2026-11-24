//! JSON output. Port of the DQL half of `query/outputnode.go`:
//! tree assembly (`preTraverse`), normalization, and encoding.
//!
//! Delta from Go: nodes form a safe `Vec` tree (Go uses an unsafe arena
//! with linked lists); insertion order and byte output match. GraphQL
//! output (`toGraphqlJSON`) is deferred to the GraphQL milestone.

#![warn(missing_docs)]

use datastreamdb_types::{TypeId, Val, Value};

use crate::errors::{QueryError, Result};
pub use crate::output_render::{process_node_uids, to_json};

/// Maximum encoded response size (4 GiB, like Go).
pub const MAX_ENCODED_SIZE: u64 = 4 << 30;
/// Maximum nodes produced by `@normalize`.
pub const LIMIT_NORMALIZE_NODE: usize = 10_000;

/// One JSON tree node.
#[derive(Clone, Debug, Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "exact port of Go fastJsonNode"
)]
pub struct JsonNode {
    /// Attribute name.
    pub attr: String,
    /// Pre-rendered scalar bytes (`None` for objects).
    pub scalar: Option<Vec<u8>>,
    /// Render children as a JSON array.
    pub list: bool,
    /// Facets pseudo-parent (normalize barrier).
    pub facets_parent: bool,
    /// UID node (scalar holds the UID bytes).
    pub uid_node: bool,
    /// Order fixed already.
    pub visited: bool,
    /// Children (insertion order; see `Encoder::fix_order`).
    pub children: Vec<JsonNode>,
}

/// JSON tree builder and encoder.
#[derive(Debug, Default)]
pub struct Encoder {
    /// Output buffer.
    pub(crate) buf: Vec<u8>,
    /// Approximate output size (edge accounting).
    pub(crate) cur_size: std::cell::Cell<u64>,
}

impl Encoder {
    /// Fresh encoder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Finished bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.buf
    }

    /// Fresh object node.
    pub(crate) fn new_node(attr: &str) -> JsonNode {
        JsonNode {
            attr: attr.to_owned(),
            ..JsonNode::default()
        }
    }

    /// Scalar node (renders inline, never as an object).
    fn make_scalar_node(attr: &str, bytes: Vec<u8>, list: bool) -> JsonNode {
        JsonNode {
            attr: attr.to_owned(),
            scalar: Some(bytes),
            list,
            ..JsonNode::default()
        }
    }

    /// UID node (renders the UID as hex).
    fn make_uid_node(attr: &str, uid: u64) -> JsonNode {
        JsonNode {
            attr: attr.to_owned(),
            scalar: Some(uid.to_be_bytes().to_vec()),
            uid_node: true,
            ..JsonNode::default()
        }
    }

    /// Prepends a child (insertion order is restored by [`Encoder::fix_order`]).
    fn add_children(parent: &mut JsonNode, child: JsonNode) {
        parent.children.insert(0, child);
    }

    /// Merges same-attribute children (map semantics).
    pub(crate) fn add_map_child(parent: &mut JsonNode, val: JsonNode) {
        if let Some(existing) = parent
            .children
            .iter_mut()
            .find(|child| child.attr == val.attr)
        {
            let mut moved = val.children;
            moved.extend(std::mem::take(&mut existing.children));
            existing.children = moved;
        } else {
            Self::add_children(parent, val);
        }
    }

    /// Appends a list child.
    pub(crate) fn add_list_child(parent: &mut JsonNode, mut child: JsonNode) {
        child.list = true;
        Self::add_children(parent, child);
    }

    /// Sets the UID child (once).
    pub(crate) fn set_uid(node: &mut JsonNode, uid: u64, attr: &str) {
        if attr == "uid" && node.children.iter().any(|child| child.attr == attr) {
            return;
        }
        let child = Self::make_uid_node(attr, uid);
        Self::add_children(node, child);
    }

    /// Whether the node has children.
    pub(crate) fn is_empty(node: &JsonNode) -> bool {
        node.children.is_empty()
    }

    /// Adds a scalar value child.
    pub(crate) fn add_value(node: &mut JsonNode, attr: &str, val: &Val) {
        Self::add_list_value(node, attr, val, false);
    }

    /// Adds a scalar value child (optionally forced into list form).
    pub(crate) fn add_list_value(node: &mut JsonNode, attr: &str, val: &Val, list: bool) {
        if val.tid == TypeId::VFloat {
            if let Value::VFloat(vec) = &val.value {
                for num in vec {
                    let child = Self::make_scalar_node(attr, format!("{num:E}").into_bytes(), true);
                    Self::add_children(node, child);
                }
            }
            return;
        }
        let Ok(bytes) = val_to_bytes(val) else {
            return;
        };
        let child = Self::make_scalar_node(attr, bytes, list);
        Self::add_children(node, child);
    }

    /// Reverses prepend order back to insertion order (once per node).
    pub(crate) fn fix_order(node: &mut JsonNode) {
        if node.visited {
            return;
        }
        node.visited = true;
        node.children.reverse();
        for child in &mut node.children {
            Self::fix_order(child);
        }
    }

    /// Merges two normalized slices with the node budget enforced.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] when the merged result exceeds the node budget.
    fn merge(parent: &[Vec<JsonNode>], child: &[Vec<JsonNode>]) -> Result<Vec<Vec<JsonNode>>> {
        if parent.is_empty() {
            return Ok(child.to_vec());
        }
        let mut merged: Vec<Vec<JsonNode>> = Vec::new();
        let mut count = 0usize;
        for parents in parent {
            for children in child {
                count += parents.len() + children.len();
                if count > LIMIT_NORMALIZE_NODE {
                    return Err(QueryError::Invalid(
                        "Couldn't evaluate @normalize directive - too many results".to_owned(),
                    ));
                }
                let mut combined = parents.clone();
                combined.extend_from_slice(children);
                merged.push(combined);
            }
        }
        Ok(merged)
    }

    /// Stable merge-sort of a child list by attribute.
    fn merge_sort(list: &mut [JsonNode]) {
        list.sort_by(|left, right| left.attr.cmp(&right.attr));
    }

    /// Flattens one object level (cartesian product over children).
    pub(crate) fn normalize(node: &JsonNode) -> Result<Vec<Vec<JsonNode>>> {
        let expandable = node
            .children
            .iter()
            .filter(|child| !child.children.is_empty() && !child.facets_parent)
            .count();
        if expandable == 0 {
            return Ok(vec![node.children.clone()]);
        }

        let mut scalars = Vec::new();
        for child in &node.children {
            if child.children.is_empty() || child.facets_parent {
                scalars.push(child.clone());
            }
        }
        let mut merged = vec![scalars];
        let mut idx = 0;
        while idx < node.children.len() {
            let child = &node.children[idx];
            if child.children.is_empty() || child.facets_parent {
                idx += 1;
                continue;
            }
            let mut group: Vec<Vec<JsonNode>> = Vec::new();
            while idx < node.children.len() && node.children[idx].attr == child.attr {
                group.push(node.children[idx].children.clone());
                idx += 1;
            }
            merged = Self::merge(&merged, &group)?;
        }

        for list in &mut merged {
            Self::merge_sort(list);
            list.retain(|child| child.attr != "uid");
        }
        Ok(merged)
    }

    /// Writes `"attr":`.
    fn write_key(&mut self, attr: &str) {
        self.buf.push(b'"');
        self.buf.extend_from_slice(attr.as_bytes());
        self.buf.extend_from_slice(b"\":");
    }

    /// Attaches facet children (scalar or per-element form).
    pub(crate) fn attach_facets(
        node: &mut JsonNode,
        field_name: &str,
        is_list: bool,
        facets: &[datastreamdb_proto::api::Facet],
        facet_idx: usize,
    ) -> Result<()> {
        for facet in facets {
            let name = facet_name(field_name, facet);
            let val = facet_to_val(facet)?;
            if is_list {
                let mut facet_node = Self::new_node(&name);
                Self::add_value(&mut facet_node, &facet_idx.to_string(), &val);
                facet_node.facets_parent = true;
                Self::add_map_child(node, facet_node);
            } else {
                Self::add_value(node, &name, &val);
            }
        }
        Ok(())
    }

    /// Encodes one node (scalars inline, objects grouped by attribute).
    pub(crate) fn encode(&mut self, node: &JsonNode) -> Result<()> {
        if node.children.is_empty() {
            let bytes = scalar_bytes(node);
            self.buf.extend_from_slice(&bytes);
            return Ok(());
        }
        self.buf.push(b'{');
        let mut count = 0usize;
        let mut idx = 0;
        while idx < node.children.len() {
            let child = &node.children[idx];
            let next_same = node
                .children
                .get(idx + 1)
                .is_some_and(|next| next.attr == child.attr);
            count += 1;
            if next_same {
                if count == 1 {
                    self.write_key(&child.attr);
                    self.buf.push(b'[');
                }
                self.encode(child)?;
            } else {
                if count == 1 {
                    self.write_key(&child.attr);
                    if child.list {
                        self.buf.push(b'[');
                    }
                }
                self.encode(child)?;
                if count > 1 || child.list {
                    self.buf.push(b']');
                }
                count = 0;
            }
            if idx + 1 < node.children.len() {
                self.buf.push(b',');
            }
            idx += 1;
        }
        self.buf.push(b'}');
        Ok(())
    }
}

/// Scalar bytes (UIDs render as hex; missing scalars render empty like
/// Go's arena offset 0).
fn scalar_bytes(node: &JsonNode) -> Vec<u8> {
    let Some(bytes) = &node.scalar else {
        return Vec::new();
    };
    if node.uid_node {
        let mut raw = [0u8; 8];
        let len = bytes.len().min(8);
        raw[..len].copy_from_slice(&bytes[..len]);
        return format!("\"{:#x}\"", u64::from_be_bytes(raw)).into_bytes();
    }
    bytes.clone()
}

/// Facet output name (alias wins, else `field|key`).
fn facet_name(field_name: &str, facet: &datastreamdb_proto::api::Facet) -> String {
    if !facet.alias.is_empty() {
        return facet.alias.clone();
    }
    format!("{field_name}|{}", facet.key)
}

/// Decodes a stored facet into a typed value.
fn facet_to_val(facet: &datastreamdb_proto::api::Facet) -> Result<Val> {
    use datastreamdb_types::{TypeId, Val, Value};
    let tid = match facet.val_type {
        1 => TypeId::Int,
        2 => TypeId::Float,
        3 => TypeId::Bool,
        4 => TypeId::DateTime,
        _ => TypeId::String,
    };
    if tid == TypeId::String {
        return Ok(Val {
            tid,
            value: Value::String(String::from_utf8_lossy(&facet.value).into_owned()),
        });
    }
    datastreamdb_types::convert(
        &Val {
            tid: TypeId::Binary,
            value: Value::Binary(facet.value.clone()),
        },
        tid,
    )
    .map_err(|err| QueryError::Invalid(format!("bad facet value: {err}")))
}

/// Renders one value into JSON bytes.
pub(crate) fn val_to_bytes(val: &Val) -> Result<Vec<u8>> {
    match val.tid {
        TypeId::String | TypeId::Default => match &val.value {
            Value::String(text) => Ok(string_json_marshal(text)),
            Value::Binary(bytes) => Ok(string_json_marshal(&String::from_utf8_lossy(bytes))),
            _ => serde_json::to_vec(&format!("{:?}", val.value)).map_err(QueryError::from),
        },
        TypeId::Binary => match &val.value {
            Value::Binary(bytes) => Ok(format!("{bytes:?}").into_bytes()),
            _ => Err(QueryError::Invalid("bad binary value".to_owned())),
        },
        TypeId::Int => match val.value {
            Value::Int(num) => Ok(num.to_string().into_bytes()),
            _ => Err(QueryError::Invalid("bad int value".to_owned())),
        },
        TypeId::Float => match val.value {
            Value::Float(num) => {
                if !num.is_finite() {
                    return Err(QueryError::Invalid(
                        "Unsupported floating point number in float field".to_owned(),
                    ));
                }
                Ok(format!("{num:?}").into_bytes())
            }
            _ => Err(QueryError::Invalid("bad float value".to_owned())),
        },
        TypeId::Bool => match val.value {
            Value::Bool(true) => Ok(b"true".to_vec()),
            Value::Bool(false) => Ok(b"false".to_vec()),
            _ => Err(QueryError::Invalid("bad bool value".to_owned())),
        },
        TypeId::DateTime => match &val.value {
            Value::DateTime(when) => Ok(marshal_time_json(when)),
            _ => Err(QueryError::Invalid("bad datetime value".to_owned())),
        },
        TypeId::Geo => match &val.value {
            Value::Geo(geom) => {
                let geo = geojson::Geometry {
                    bbox: None,
                    value: geom.clone(),
                    foreign_members: None,
                };
                serde_json::to_vec(&geojson::GeoJson::Geometry(geo)).map_err(QueryError::from)
            }
            _ => Err(QueryError::Invalid("bad geo value".to_owned())),
        },
        TypeId::BigFloat => match &val.value {
            Value::BigFloat(num) => Ok(num.to_string().into_bytes()),
            _ => Err(QueryError::Invalid("bad bigfloat value".to_owned())),
        },
        TypeId::Uid => match val.value {
            Value::Uid(uid) => Ok(format!("\"{uid:#x}\"").into_bytes()),
            Value::Int(num) => Ok(format!("\"{num:#x}\"").into_bytes()),
            _ => Err(QueryError::Invalid("bad uid value".to_owned())),
        },
        TypeId::Password => match &val.value {
            Value::Password(text) | Value::String(text) => Ok(format!("{text:?}").into_bytes()),
            _ => Err(QueryError::Invalid("bad password value".to_owned())),
        },
        TypeId::VFloat => match &val.value {
            Value::VFloat(vec) => serde_json::to_vec(vec).map_err(QueryError::from),
            _ => Err(QueryError::Invalid("bad vfloat value".to_owned())),
        },
        TypeId::Object | TypeId::Undefined => {
            Err(QueryError::Invalid("Unsupported types.Val.Tid".to_owned()))
        }
    }
}

/// Renders a datetime (Go-compatible, tolerating odd zones).
fn marshal_time_json(when: &chrono::DateTime<chrono::FixedOffset>) -> Vec<u8> {
    let offset = when.offset().local_minus_utc();
    if datastreamdb_types::good_time_zone(offset) {
        return format!(
            "\"{}\"",
            when.to_rfc3339_opts(chrono::SecondsFormat::AutoSi, false)
        )
        .into_bytes();
    }
    let shifted = *when + chrono::Duration::seconds(i64::from(offset));
    let text = shifted
        .with_timezone(&chrono::Utc)
        .to_rfc3339_opts(chrono::SecondsFormat::AutoSi, false);
    let zone = offset / 60;
    let (sign, zone) = if zone < 0 { ('-', -zone) } else { ('+', zone) };
    format!(
        "\"{}{}{:02}:{:02}\"",
        &text[..text.len() - 1],
        sign,
        zone / 60,
        zone % 60
    )
    .into_bytes()
}

/// Escapes one string exactly like Go's `stringJsonMarshal` (HTML on).
fn string_json_marshal(text: &str) -> Vec<u8> {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let bytes = text.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len() + 2);
    out.push(b'"');
    let mut start = 0;
    let mut idx = 0;
    while idx < bytes.len() {
        let byte = bytes[idx];
        if byte < 0x80 {
            let safe = byte >= 0x20
                && byte != b'"'
                && byte != b'\\'
                && byte != b'<'
                && byte != b'>'
                && byte != b'&';
            if safe {
                idx += 1;
                continue;
            }
            out.extend_from_slice(&bytes[start..idx]);
            out.push(b'\\');
            match byte {
                b'\\' | b'"' => out.push(byte),
                b'\n' => out.push(b'n'),
                b'\r' => out.push(b'r'),
                b'\t' => out.push(b't'),
                _ => {
                    out.extend_from_slice(b"u00");
                    out.push(HEX[usize::from(byte >> 4)]);
                    out.push(HEX[usize::from(byte & 15)]);
                }
            }
            idx += 1;
            start = idx;
            continue;
        }
        let rest = &text[idx..];
        let mut chars = rest.chars();
        let Some(chr) = chars.next() else {
            break;
        };
        let size = chr.len_utf8();
        if chr == '\u{FFFD}' && size == 1 {
            out.extend_from_slice(&bytes[start..idx]);
            out.extend_from_slice(b"\\ufffd");
            idx += size;
            start = idx;
            continue;
        }
        if chr == '\u{2028}' || chr == '\u{2029}' {
            out.extend_from_slice(&bytes[start..idx]);
            out.extend_from_slice(b"\\u202");
            out.push(HEX[(chr as u32 & 15) as usize]);
            idx += size;
            start = idx;
            continue;
        }
        idx += size;
    }
    out.extend_from_slice(&bytes[start..]);
    out.push(b'"');
    out
}
