//! Value-type info for edges/postings/N-Quads. Port of `x/values.go`.

#![warn(missing_docs)]

/// Kind of data inside a `DirectedEdge`, `Posting`, or N-Quad.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ValueTypeInfo {
    /// Unknown type.
    Unknown = 0,
    /// No UID and no value.
    Empty = 1,
    /// UID value.
    Uid = 2,
    /// Plain value without a language tag.
    Plain = 3,
    /// Value in a multi-value posting list (like language).
    Multi = 4,
}

/// Decides the value type from presence flags.
#[must_use]
pub const fn value_type(has_value: bool, has_lang: bool, has_special_id: bool) -> ValueTypeInfo {
    if has_value && has_lang {
        ValueTypeInfo::Multi
    } else if has_value {
        ValueTypeInfo::Plain
    } else if has_special_id {
        ValueTypeInfo::Empty
    } else {
        // Note: Go's final `default` arm (Unknown) is unreachable: the three
        // boolean conditions above exhaust the space. We return Uid for
        // `!has_value && !has_special_id`, matching Go's explicit arm.
        ValueTypeInfo::Uid
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn value_type_matrix() {
        assert_eq!(value_type(true, true, false), ValueTypeInfo::Multi);
        assert_eq!(value_type(true, false, false), ValueTypeInfo::Plain);
        assert_eq!(value_type(false, false, true), ValueTypeInfo::Empty);
        assert_eq!(value_type(false, false, false), ValueTypeInfo::Uid);
        assert_eq!(value_type(false, true, true), ValueTypeInfo::Empty);
    }
}
