//! Badger key encoding for predicates. Port of the key-builder half of
//! `x/keys.go` (byte prefixes, `generateKey`, data/reverse/index/count
//! keys, `ParsedKey`, split keys).
//!
//! [`ToBackupKey`](https://github.com/dgraph-io/dgraph/blob/main/x/keys.go)
//! / `FromBackupKey` and the plugin `ReservedNamespace` registry are deferred
//! to the backup/serving milestones.

#![warn(missing_docs)]

use super::keys::{NS_SEPARATOR, namespace_attr, parse_namespace_attr};

/// Key stores data.
pub const BYTE_DATA: u8 = 0x00;
/// Key stores an index.
pub const BYTE_INDEX: u8 = 0x02;
/// Key stores a reverse index.
pub const BYTE_REVERSE: u8 = 0x04;
/// Key stores a count index.
pub const BYTE_COUNT: u8 = 0x08;
/// Key stores a reverse count index.
pub const BYTE_COUNT_REV: u8 = BYTE_COUNT | BYTE_REVERSE;
/// Prefix for data, index and reverse keys.
pub const DEFAULT_PREFIX: u8 = 0x00;
/// Prefix for schema keys.
pub const BYTE_SCHEMA: u8 = 0x01;
/// Prefix for type keys.
pub const BYTE_TYPE: u8 = 0x02;
/// Prefix for an individual part of a multi-part list.
pub const BYTE_SPLIT: u8 = 0x04;
/// Prefix for keys which need to be discarded.
pub const BYTE_UNUSED: u8 = 0xff;

/// Key parsing/encoding failures.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum KeyError {
    /// Key shorter than the 9-byte header.
    #[error("Key length less than 9")]
    TooShort,
    /// Truncated attribute section.
    #[error("invalid format for key")]
    InvalidFormat,
    /// Declared attribute length overruns the key.
    #[error("invalid size for key")]
    InvalidSize,
    /// Data/reverse key with UID 0.
    #[error("invalid UID with value 0 for key")]
    ZeroUid,
    /// UID section shorter than 8 bytes.
    #[error("uid length < 8 for key")]
    ShortUid,
    /// Split suffix is not exactly 8 bytes.
    #[error("StartUid length != 8 for key")]
    BadStartUid,
    /// Split suffix shorter than 8 bytes.
    #[error("StartUid length < 8 for key")]
    ShortStartUid,
    /// Count section shorter than 4 bytes.
    #[error("count length < 4 for key")]
    ShortCount,
    /// Unknown data-type byte.
    #[error("invalid data type")]
    InvalidDataType,
    /// Only default-prefix keys can have a split key.
    #[error("only keys with default prefix can have a split key")]
    SplitPrefix,
    /// Replacement rune in a pre-21.03 attribute.
    #[error("replacement rune found while parsing attr")]
    ReplacementRune,
}

/// Parses namespaces stored in the pre-21.03 format (8 raw bytes + predicate).
///
/// # Errors
///
/// Returns [`KeyError::ReplacementRune`] when the input contains U+FFFD, or
/// [`KeyError::InvalidSize`] when it is shorter than 8 bytes.
pub fn attr_from_2103(attr: &str) -> Result<String, KeyError> {
    if attr.contains('\u{FFFD}') {
        return Err(KeyError::ReplacementRune);
    }
    let bytes = attr.as_bytes();
    if bytes.len() < 8 {
        return Err(KeyError::InvalidSize);
    }
    let ns = bytes[..8]
        .try_into()
        .map(u64::from_be_bytes)
        .map_err(|_| KeyError::InvalidSize)?;
    Ok(namespace_attr(ns, &String::from_utf8_lossy(&bytes[8..])))
}

/// Big-endian 8-byte namespace encoding.
#[must_use]
pub fn namespace_to_bytes(ns: u64) -> [u8; 8] {
    ns.to_be_bytes()
}

/// Builds namespaced attributes for a predicate list.
#[must_use]
pub fn namespace_attr_list(ns: u64, preds: &[String]) -> Vec<String> {
    preds.iter().map(|p| namespace_attr(ns, p)).collect()
}

/// Whether the attribute is a reverse (`~`) predicate.
#[must_use]
pub fn is_reverse_attr(attr: &str) -> bool {
    parse_namespace_attr(attr).1.as_bytes()[0] == b'~'
}

/// Parses `attr` into raw namespace bytes plus the bare predicate.
fn parse_namespace_bytes(attr: &str) -> ([u8; 8], &str) {
    let (ns, rest) = attr
        .split_once(NS_SEPARATOR)
        .expect("namespaced attr has separator");
    let ns = u64::from_str_radix(ns, 16).expect("hex namespace");
    (ns.to_be_bytes(), rest)
}

/// Writes the u16 length prefix plus attribute bytes into `buf`.
///
/// Returns the unwritten tail, like Go.
fn write_attr<'b>(buf: &'b mut [u8], attr: &str) -> &'b mut [u8] {
    assert!(attr.len() < u16::MAX as usize, "attr too long");
    #[allow(clippy::cast_possible_truncation, reason = "asserted < u16::MAX")]
    let len = attr.len() as u16;
    buf[..2].copy_from_slice(&len.to_be_bytes());
    let rest = &mut buf[2..];
    rest[..attr.len()].copy_from_slice(attr.as_bytes());
    &mut rest[attr.len()..]
}

/// Builds the key header (type byte, namespace, attribute) plus `extra` zero
/// bytes. Returns the buffer and the header length.
fn generate_key(type_byte: u8, attr: &str, extra: usize) -> (Vec<u8>, usize) {
    let (namespace, attr) = parse_namespace_bytes(attr);
    let prefix_len = 1 + 8 + 2 + attr.len();
    let mut buf = vec![0u8; prefix_len + extra];
    buf[0] = type_byte;
    buf[1..9].copy_from_slice(&namespace);
    write_attr(&mut buf[9..], attr);
    (buf, prefix_len)
}

/// Schema key for an attribute.
#[must_use]
pub fn schema_key(attr: &str) -> Vec<u8> {
    generate_key(BYTE_SCHEMA, attr, 0).0
}

/// Type key for a type name.
#[must_use]
pub fn type_key(attr: &str) -> Vec<u8> {
    generate_key(BYTE_TYPE, attr, 0).0
}

/// Data key for an attribute + UID.
#[must_use]
pub fn data_key(attr: &str, uid: u64) -> Vec<u8> {
    let (mut buf, prefix_len) = generate_key(DEFAULT_PREFIX, attr, 1 + 8);
    buf[prefix_len] = BYTE_DATA;
    buf[prefix_len + 1..prefix_len + 9].copy_from_slice(&uid.to_be_bytes());
    buf
}

/// Reverse key for an attribute + UID.
#[must_use]
pub fn reverse_key(attr: &str, uid: u64) -> Vec<u8> {
    let (mut buf, prefix_len) = generate_key(DEFAULT_PREFIX, attr, 1 + 8);
    buf[prefix_len] = BYTE_REVERSE;
    buf[prefix_len + 1..prefix_len + 9].copy_from_slice(&uid.to_be_bytes());
    buf
}

/// Index key sorting after every real term of `attr` (0xFF sentinel term).
#[must_use]
pub fn index_key_after_all_terms(attr: &str) -> Vec<u8> {
    let (mut buf, prefix_len) = generate_key(DEFAULT_PREFIX, attr, 1 + 1);
    buf[prefix_len] = BYTE_INDEX;
    buf[prefix_len + 1] = 0xff;
    buf
}

/// Index key for an attribute + term (terms are binary, like Go).
#[must_use]
pub fn index_key(attr: &str, term: &[u8]) -> Vec<u8> {
    let (mut buf, prefix_len) = generate_key(DEFAULT_PREFIX, attr, 1 + term.len());
    buf[prefix_len] = BYTE_INDEX;
    buf[prefix_len + 1..].copy_from_slice(term);
    buf
}

/// Count key for an attribute + count.
#[must_use]
pub fn count_key(attr: &str, count: u32, reverse: bool) -> Vec<u8> {
    let (mut buf, prefix_len) = generate_key(DEFAULT_PREFIX, attr, 1 + 4);
    buf[prefix_len] = if reverse { BYTE_COUNT_REV } else { BYTE_COUNT };
    buf[prefix_len + 1..prefix_len + 5].copy_from_slice(&count.to_be_bytes());
    buf
}

/// A key parsed into its attributes. Owns its data, like Go.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ParsedKey {
    /// Namespaced attribute.
    pub attr: String,
    /// UID for data/reverse keys.
    pub uid: u64,
    /// Split start UID for multi-part list keys.
    pub start_uid: u64,
    /// Term for index keys.
    pub term: Vec<u8>,
    /// Count for count keys.
    pub count: u32,
    /// Data-type byte (`ByteData`, `ByteIndex`, ...).
    pub byte_type: u8,
    /// Whether the key addresses one split of a multi-part list.
    pub has_start_uid: bool,
    byte_prefix: u8,
}

impl ParsedKey {
    /// Whether the key is a data key.
    #[must_use]
    pub fn is_data(&self) -> bool {
        (self.byte_prefix == DEFAULT_PREFIX || self.byte_prefix == BYTE_SPLIT)
            && self.byte_type == BYTE_DATA
    }

    /// Whether the key is a reverse key.
    #[must_use]
    pub fn is_reverse(&self) -> bool {
        (self.byte_prefix == DEFAULT_PREFIX || self.byte_prefix == BYTE_SPLIT)
            && self.byte_type == BYTE_REVERSE
    }

    /// Whether the key is a count or reverse-count key.
    #[must_use]
    pub fn is_count_or_count_rev(&self) -> bool {
        self.is_count() || self.is_count_rev()
    }

    /// Whether the key is a count key.
    #[must_use]
    pub fn is_count(&self) -> bool {
        (self.byte_prefix == DEFAULT_PREFIX || self.byte_prefix == BYTE_SPLIT)
            && self.byte_type == BYTE_COUNT
    }

    /// Whether the key is a reverse-count key.
    #[must_use]
    pub fn is_count_rev(&self) -> bool {
        (self.byte_prefix == DEFAULT_PREFIX || self.byte_prefix == BYTE_SPLIT)
            && self.byte_type == BYTE_COUNT_REV
    }

    /// Whether the key is an index key.
    #[must_use]
    pub fn is_index(&self) -> bool {
        (self.byte_prefix == DEFAULT_PREFIX || self.byte_prefix == BYTE_SPLIT)
            && self.byte_type == BYTE_INDEX
    }

    /// Whether the key is a schema key.
    #[must_use]
    pub fn is_schema(&self) -> bool {
        self.byte_prefix == BYTE_SCHEMA
    }

    /// Whether the key is a type key.
    #[must_use]
    pub fn is_type(&self) -> bool {
        self.byte_prefix == BYTE_TYPE
    }

    /// Whether the key is of the given type byte.
    #[must_use]
    pub fn is_of_type(&self, typ: u8) -> bool {
        match typ {
            BYTE_COUNT | BYTE_COUNT_REV => self.is_count_or_count_rev(),
            BYTE_REVERSE => self.is_reverse(),
            BYTE_INDEX => self.is_index(),
            BYTE_DATA => self.is_data(),
            _ => false,
        }
    }

    /// First key after this key's predicate (reverse iteration).
    ///
    /// # Panics
    ///
    /// Never in practice (asserts an internal length invariant, like Go).
    #[must_use]
    pub fn skip_predicate(&self) -> Vec<u8> {
        let (mut buf, prefix_len) = generate_key(self.byte_prefix, &self.attr, 1);
        assert_eq!(buf.len() - prefix_len, 1);
        buf[prefix_len] = 0xff;
        buf
    }

    /// Prefix for data keys of this attribute.
    #[must_use]
    pub fn data_prefix(&self) -> Vec<u8> {
        let (mut buf, prefix_len) = generate_key(self.byte_prefix, &self.attr, 1);
        buf[prefix_len] = BYTE_DATA;
        buf
    }

    /// Prefix for index keys of this attribute.
    #[must_use]
    pub fn index_prefix(&self) -> Vec<u8> {
        let (mut buf, prefix_len) = generate_key(DEFAULT_PREFIX, &self.attr, 1);
        buf[prefix_len] = BYTE_INDEX;
        buf
    }

    /// Prefix for reverse keys of this attribute.
    #[must_use]
    pub fn reverse_prefix(&self) -> Vec<u8> {
        let (mut buf, prefix_len) = generate_key(DEFAULT_PREFIX, &self.attr, 1);
        buf[prefix_len] = BYTE_REVERSE;
        buf
    }

    /// Prefix for count keys of this attribute.
    #[must_use]
    pub fn count_prefix(&self, reverse: bool) -> Vec<u8> {
        let (mut buf, prefix_len) = generate_key(DEFAULT_PREFIX, &self.attr, 1);
        buf[prefix_len] = if reverse { BYTE_COUNT_REV } else { BYTE_COUNT };
        buf
    }
}

impl std::fmt::Display for ParsedKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.is_index() {
            write!(
                f,
                "UID: {}, Attr: {}, IsIndex: true, Term: {:?}",
                self.uid, self.attr, self.term
            )
        } else if self.is_count_or_count_rev() {
            write!(
                f,
                "UID: {}, Attr: {}, IsCount/Ref: true, Count: {}",
                self.uid, self.attr, self.count
            )
        } else {
            write!(
                f,
                "UID: {}, Attr: {}, Data key, prefix; {}, byte: {}",
                self.uid, self.attr, self.byte_prefix, self.byte_type
            )
        }
    }
}

/// Prefix matching every schema key.
#[must_use]
pub fn schema_prefix() -> Vec<u8> {
    vec![BYTE_SCHEMA]
}

/// Prefix matching every type key.
#[must_use]
pub fn type_prefix() -> Vec<u8> {
    vec![BYTE_TYPE]
}

/// Prefix for every non-schema key of a predicate.
///
/// # Panics
///
/// Never in practice (asserts an internal length invariant, like Go).
#[must_use]
pub fn predicate_prefix(predicate: &str) -> Vec<u8> {
    let (buf, prefix_len) = generate_key(DEFAULT_PREFIX, predicate, 0);
    assert_eq!(buf.len(), prefix_len);
    buf
}

/// Prefix for every data key of a namespace.
#[must_use]
pub fn data_prefix(ns: u64) -> Vec<u8> {
    let mut buf = vec![0u8; 1 + 8];
    buf[0] = DEFAULT_PREFIX;
    buf[1..].copy_from_slice(&ns.to_be_bytes());
    buf
}

/// Builds the key of the list split starting at `start_uid`.
///
/// # Errors
///
/// Returns [`KeyError::SplitPrefix`] unless the base key has the default prefix.
pub fn split_key(base_key: &[u8], start_uid: u64) -> Result<Vec<u8>, KeyError> {
    let mut key = vec![0u8; base_key.len() + 8];
    key[..base_key.len()].copy_from_slice(base_key);
    if key[0] != DEFAULT_PREFIX {
        return Err(KeyError::SplitPrefix);
    }
    key[0] = BYTE_SPLIT;
    key[base_key.len()..].copy_from_slice(&start_uid.to_be_bytes());
    Ok(key)
}

/// Big-endian u64 from exactly 8 bytes.
fn u64_from(bytes: &[u8]) -> Result<u64, KeyError> {
    bytes
        .try_into()
        .map(u64::from_be_bytes)
        .map_err(|_| KeyError::InvalidSize)
}

/// Big-endian u32 from exactly 4 bytes.
fn u32_from(bytes: &[u8]) -> Result<u32, KeyError> {
    bytes
        .try_into()
        .map(u32::from_be_bytes)
        .map_err(|_| KeyError::InvalidSize)
}

/// Parses a key. Never borrows the input, like Go.
///
/// # Errors
///
/// Returns [`KeyError`] on truncated or malformed keys. Delta from Go: keys
/// that end right after the attribute return [`KeyError::InvalidDataType`]
/// instead of panicking on an out-of-range index.
pub fn parse(key: &[u8]) -> Result<ParsedKey, KeyError> {
    let mut p = ParsedKey::default();
    if key.len() < 9 {
        return Err(KeyError::TooShort);
    }
    p.byte_prefix = key[0];
    let namespace = &key[1..9];
    let key = &key[9..];
    if p.byte_prefix == BYTE_UNUSED {
        return Ok(p);
    }

    p.has_start_uid = p.byte_prefix == BYTE_SPLIT;
    if key.len() < 3 {
        return Err(KeyError::InvalidFormat);
    }
    let sz = key[..2]
        .try_into()
        .map(u16::from_be_bytes)
        .map_err(|_| KeyError::InvalidFormat)? as usize;
    let k = &key[2..];
    if k.len() < sz {
        return Err(KeyError::InvalidSize);
    }
    let ns = namespace
        .try_into()
        .map(u64::from_be_bytes)
        .map_err(|_| KeyError::TooShort)?;
    p.attr = namespace_attr(ns, &String::from_utf8_lossy(&k[..sz]));
    let k = &k[sz..];

    match p.byte_prefix {
        BYTE_SCHEMA | BYTE_TYPE => return Ok(p),
        _ => {}
    }

    let Some((&byte_type, k)) = k.split_first() else {
        return Err(KeyError::InvalidDataType);
    };
    p.byte_type = byte_type;

    match p.byte_type {
        BYTE_DATA | BYTE_REVERSE => {
            if k.len() < 8 {
                return Err(KeyError::ShortUid);
            }
            p.uid = u64_from(k.get(..8).ok_or(KeyError::ShortUid)?)?;
            if p.uid == 0 {
                return Err(KeyError::ZeroUid);
            }
            if !p.has_start_uid {
                return Ok(p);
            }
            if k.len() != 16 {
                return Err(KeyError::BadStartUid);
            }
            p.start_uid = u64_from(k.get(8..16).ok_or(KeyError::BadStartUid)?)?;
        }
        BYTE_INDEX => {
            if !p.has_start_uid {
                p.term = k.to_vec();
                return Ok(p);
            }
            if k.len() < 8 {
                return Err(KeyError::ShortStartUid);
            }
            let (term, start) = k.split_at(k.len() - 8);
            p.term = term.to_vec();
            p.start_uid = u64_from(start)?;
        }
        BYTE_COUNT | BYTE_COUNT_REV => {
            if k.len() < 4 {
                return Err(KeyError::ShortCount);
            }
            p.count = u32_from(k.get(..4).ok_or(KeyError::ShortCount)?)?;
            if !p.has_start_uid {
                return Ok(p);
            }
            if k.len() != 12 {
                return Err(KeyError::BadStartUid);
            }
            p.start_uid = u64_from(k.get(4..12).ok_or(KeyError::BadStartUid)?)?;
        }
        _ => return Err(KeyError::InvalidDataType),
    }
    Ok(p)
}

/// Whether the key addresses the `datastreamdb.drop.op` marker.
///
/// # Errors
///
/// Returns [`KeyError`] when the key cannot be parsed.
pub fn is_drop_op_key(key: &[u8]) -> Result<bool, KeyError> {
    let pk = parse(key)?;
    Ok(pk.is_data() && super::keys::parse_attr(&pk.attr) == "datastreamdb.drop.op")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_key_layout() {
        let key = data_key("0-name", 0x0102);
        assert_eq!(key[0], DEFAULT_PREFIX);
        assert_eq!(&key[1..9], &[0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(&key[9..11], &[0, 4]);
        assert_eq!(&key[11..15], b"name");
        assert_eq!(key[15], BYTE_DATA);
        assert_eq!(&key[16..24], &[0, 0, 0, 0, 0, 0, 1, 2]);
        let pk = parse(&key).expect("parse");
        assert_eq!(pk.attr, "0-name");
        assert_eq!(pk.uid, 0x0102);
        assert!(pk.is_data());
        assert!(!pk.is_index());
    }

    #[test]
    fn reverse_index_count_keys_roundtrip() {
        let pk = parse(&reverse_key("f2-friend", 7)).expect("parse");
        assert!(pk.is_reverse());
        assert_eq!((pk.attr.as_str(), pk.uid), ("f2-friend", 7));

        let pk = parse(&index_key("0-name", b"term")).expect("parse");
        assert!(pk.is_index());
        assert_eq!(pk.term, b"term");

        let pk = parse(&count_key("0-friend", 42, false)).expect("parse");
        assert!(pk.is_count());
        assert!(!pk.is_count_rev());
        assert_eq!(pk.count, 42);
        let pk = parse(&count_key("0-friend", 42, true)).expect("parse");
        assert!(pk.is_count_rev());
        assert!(pk.is_count_or_count_rev());

        let pk = parse(&schema_key("0-name")).expect("parse");
        assert!(pk.is_schema());
        let pk = parse(&type_key("0-Person")).expect("parse");
        assert!(pk.is_type());
    }

    #[test]
    fn split_keys_roundtrip() {
        let base = data_key("0-friend", 9);
        let split = split_key(&base, 100).expect("split");
        assert_eq!(split[0], BYTE_SPLIT);
        let pk = parse(&split).expect("parse");
        assert!(pk.has_start_uid);
        assert!(pk.is_data());
        assert_eq!((pk.uid, pk.start_uid), (9, 100));

        let term = index_key("0-name", b"abc");
        let pk = parse(&split_key(&term, 5).expect("split")).expect("parse");
        assert_eq!((pk.term.as_slice(), pk.start_uid), (b"abc".as_slice(), 5));

        assert_eq!(
            split_key(&[BYTE_SPLIT], 1).expect_err("prefix"),
            KeyError::SplitPrefix
        );
    }

    #[test]
    fn prefixes_match_keys() {
        let pk = parse(&data_key("0-name", 1)).expect("parse");
        assert!(data_key("0-name", 2).starts_with(&pk.data_prefix()));
        assert!(index_key("0-name", b"x").starts_with(&pk.index_prefix()));
        assert!(reverse_key("0-name", 3).starts_with(&pk.reverse_prefix()));
        assert!(count_key("0-name", 1, false).starts_with(&pk.count_prefix(false)));
        assert!(count_key("0-name", 1, true).starts_with(&pk.count_prefix(true)));
        assert!(schema_key("0-a").starts_with(&schema_prefix()));
        assert!(data_key("0-a", 1).starts_with(&predicate_prefix("0-a")));
        assert!(data_key("0-a", 1).starts_with(&data_prefix(0)));
        assert!(!data_key("1-a", 1).starts_with(&data_prefix(0)));
    }

    #[test]
    fn malformed_keys_error() {
        assert_eq!(parse(&[]).expect_err("empty"), KeyError::TooShort);
        assert_eq!(parse(&[0u8; 8]).expect_err("short"), KeyError::TooShort);
        assert_eq!(
            parse(&data_key("0-name", 0)).expect_err("zero uid"),
            KeyError::ZeroUid
        );
        let mut truncated = data_key("0-name", 1);
        truncated.truncate(20);
        assert_eq!(parse(&truncated).expect_err("trunc"), KeyError::ShortUid);
        let mut bad_type = data_key("0-name", 1);
        bad_type[15] = 0x77;
        assert_eq!(
            parse(&bad_type).expect_err("type"),
            KeyError::InvalidDataType
        );
        // Ends right after the attribute: Go panics, we error.
        let mut bare = data_key("0-name", 1);
        bare.truncate(15);
        assert_eq!(parse(&bare).expect_err("bare"), KeyError::InvalidDataType);
    }

    #[test]
    fn attr_helpers() {
        assert_eq!(namespace_to_bytes(0xf2), [0, 0, 0, 0, 0, 0, 0, 0xf2]);
        assert_eq!(
            namespace_attr_list(0, &["a".to_owned()]),
            ["0-a".to_owned()]
        );
        assert!(is_reverse_attr("0-~friend"));
        assert!(!is_reverse_attr("0-friend"));
        assert!(attr_from_2103("replacement \u{FFFD}").is_err());
        let mut old = vec![0u8; 8];
        old.extend_from_slice(b"name");
        let old = String::from_utf8(old).expect("utf8");
        assert_eq!(attr_from_2103(&old).expect("2103"), "0-name");
        assert!(is_drop_op_key(&data_key("0-datastreamdb.drop.op", 1)).expect("drop"));
        assert!(!is_drop_op_key(&data_key("0-name", 1)).expect("nodrop"));
    }
}
