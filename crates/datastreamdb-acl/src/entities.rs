//! ACL entities: users, groups, rules, and operations.
//!
//! Port of the data model in `acl/utils.go`: operation codes, the
//! [`User`]/[`Group`]/[`Acl`] shapes as stored in the graph, first-match
//! JSON unmarshalling, and group-ID extraction for JWT claims.

use serde::Deserialize;

/// Operation name for reads.
pub const OP_READ: &str = "Read";
/// Operation name for writes.
pub const OP_WRITE: &str = "Write";
/// Operation name for schema/drop alterations.
pub const OP_MODIFY: &str = "Modify";

/// A `DataStreamDB` data operation with its permission bit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Operation {
    /// Permission bit (`Read = 4`, `Write = 2`, `Modify = 1`).
    pub code: i32,
    /// Operation name.
    pub name: &'static str,
}

/// Read operation (queries).
pub const READ: Operation = Operation {
    code: 4,
    name: OP_READ,
};
/// Write operation (mutations).
pub const WRITE: Operation = Operation {
    code: 2,
    name: OP_WRITE,
};
/// Modify operation (schema alters, drops).
pub const MODIFY: Operation = Operation {
    code: 1,
    name: OP_MODIFY,
};

/// A rule granting `perm` bits on `predicate` (Go `Acl`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct Acl {
    /// Predicate the rule covers (`datastreamdb.rule.predicate`).
    #[serde(rename = "datastreamdb.rule.predicate", default)]
    pub predicate: String,
    /// Permission bits (`datastreamdb.rule.permission`).
    #[serde(rename = "datastreamdb.rule.permission", default)]
    pub perm: i32,
}

/// A group in the ACL system.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct Group {
    /// Node UID.
    #[serde(default)]
    pub uid: String,
    /// Group ID (`datastreamdb.xid`).
    #[serde(rename = "datastreamdb.xid", default)]
    pub group_id: String,
    /// Member users (reverse `datastreamdb.user.group` edge).
    #[serde(rename = "~datastreamdb.user.group", default)]
    pub users: Vec<User>,
    /// Rules attached to the group.
    #[serde(rename = "datastreamdb.acl.rule", default)]
    pub rules: Vec<Acl>,
}

/// A user in the ACL system.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct User {
    /// Node UID.
    #[serde(default)]
    pub uid: String,
    /// User ID (`datastreamdb.xid`).
    #[serde(rename = "datastreamdb.xid", default)]
    pub user_id: String,
    /// Bcrypt password hash (`datastreamdb.password`).
    #[serde(rename = "datastreamdb.password", default)]
    pub password: String,
    /// Tenancy namespace.
    #[serde(default)]
    pub namespace: u64,
    /// `checkpwd` result alias from login queries.
    #[serde(default)]
    pub password_match: bool,
    /// Groups the user belongs to.
    #[serde(rename = "datastreamdb.user.group", default)]
    pub groups: Vec<Group>,
}

/// Entity extraction failures.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EntityError {
    /// Response JSON does not decode.
    #[error("unable to unmarshal the query response: {0}")]
    Decode(String),
    /// More than one entity matched (xids must be unique).
    #[error("found multiple entities: {0}")]
    Multiple(String),
}

/// Extracts the first [`User`] under `key` (Go `UnmarshalUser`).
///
/// Returns `Ok(None)` when no user matched.
///
/// # Errors
///
/// Returns [`EntityError`] on malformed JSON or multiple matches.
pub fn unmarshal_user(json: &[u8], key: &str) -> Result<Option<User>, EntityError> {
    let table: std::collections::HashMap<String, Vec<User>> =
        serde_json::from_slice(json).map_err(|err| EntityError::Decode(err.to_string()))?;
    let users = table.get(key).cloned().unwrap_or_default();
    match users.len() {
        0 => Ok(None),
        1 => Ok(users.into_iter().next()),
        _ => Err(EntityError::Multiple(
            String::from_utf8_lossy(json).into_owned(),
        )),
    }
}

/// Extracts the first [`Group`] under `key` (Go `UnmarshalGroup`).
///
/// Returns `Ok(None)` when no group matched.
///
/// # Errors
///
/// Returns [`EntityError`] on malformed JSON or multiple matches.
pub fn unmarshal_group(json: &[u8], key: &str) -> Result<Option<Group>, EntityError> {
    let table: std::collections::HashMap<String, Vec<Group>> =
        serde_json::from_slice(json).map_err(|err| EntityError::Decode(err.to_string()))?;
    let groups = table.get(key).cloned().unwrap_or_default();
    match groups.len() {
        0 => Ok(None),
        1 => Ok(groups.into_iter().next()),
        _ => Err(EntityError::Multiple(
            String::from_utf8_lossy(json).into_owned(),
        )),
    }
}

/// Extracts every [`Group`] under `key` (Go `UnmarshalGroups`).
///
/// # Errors
///
/// Returns [`EntityError`] on malformed JSON.
pub fn unmarshal_groups(json: &[u8], key: &str) -> Result<Vec<Group>, EntityError> {
    let table: std::collections::HashMap<String, Vec<Group>> =
        serde_json::from_slice(json).map_err(|err| EntityError::Decode(err.to_string()))?;
    Ok(table.get(key).cloned().unwrap_or_default())
}

/// Extracts every [`User`] under `key` (cache refresh shape).
///
/// # Errors
///
/// Returns [`EntityError`] on malformed JSON.
pub fn unmarshal_users(json: &[u8], key: &str) -> Result<Vec<User>, EntityError> {
    let table: std::collections::HashMap<String, Vec<User>> =
        serde_json::from_slice(json).map_err(|err| EntityError::Decode(err.to_string()))?;
    Ok(table.get(key).cloned().unwrap_or_default())
}

/// Collects group IDs for JWT claims (Go `GetGroupIDs`).
#[must_use]
pub fn group_ids(groups: &[Group]) -> Vec<String> {
    groups.iter().map(|group| group.group_id.clone()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const USER_JSON: &str = r#"{"user":[{"uid":"0x3","datastreamdb.xid":"alice",
        "datastreamdb.password":"$2a$10$abc","namespace":2,"password_match":true,
        "datastreamdb.user.group":[{"uid":"0x4","datastreamdb.xid":"dev"}]}]}"#;

    const GROUP_JSON: &str = r#"{"group":[{"uid":"0x4","datastreamdb.xid":"dev",
        "~datastreamdb.user.group":[{"uid":"0x3","datastreamdb.xid":"alice"}],
        "datastreamdb.acl.rule":[{"datastreamdb.rule.predicate":"name","datastreamdb.rule.permission":4}]}]}"#;

    #[test]
    fn operation_codes_match_go() {
        assert_eq!((READ.code, READ.name), (4, "Read"));
        assert_eq!((WRITE.code, WRITE.name), (2, "Write"));
        assert_eq!((MODIFY.code, MODIFY.name), (1, "Modify"));
    }

    #[test]
    fn user_round_trip() {
        let user = unmarshal_user(USER_JSON.as_bytes(), "user")
            .expect("decode")
            .expect("user");
        assert_eq!(user.user_id, "alice");
        assert_eq!(user.namespace, 2);
        assert!(user.password_match);
        assert_eq!(group_ids(&user.groups), vec!["dev".to_owned()]);
    }

    #[test]
    fn group_round_trip() {
        let group = unmarshal_group(GROUP_JSON.as_bytes(), "group")
            .expect("decode")
            .expect("group");
        assert_eq!(group.group_id, "dev");
        assert_eq!(group.users.len(), 1);
        assert_eq!(
            group.rules,
            vec![Acl {
                predicate: "name".to_owned(),
                perm: 4,
            }]
        );
    }

    #[test]
    fn missing_key_yields_none() {
        assert_eq!(unmarshal_user(b"{}", "user").expect("decode"), None);
        assert_eq!(unmarshal_group(b"{}", "group").expect("decode"), None);
        assert!(unmarshal_groups(b"{}", "group").expect("decode").is_empty());
    }

    #[test]
    fn multiple_matches_error() {
        let json = r#"{"user":[{"datastreamdb.xid":"a"},{"datastreamdb.xid":"b"}]}"#;
        assert!(matches!(
            unmarshal_user(json.as_bytes(), "user"),
            Err(EntityError::Multiple(_))
        ));
        let json = r#"{"group":[{"datastreamdb.xid":"a"},{"datastreamdb.xid":"b"}]}"#;
        assert!(matches!(
            unmarshal_group(json.as_bytes(), "group"),
            Err(EntityError::Multiple(_))
        ));
    }

    #[test]
    fn malformed_json_errors() {
        assert!(matches!(
            unmarshal_user(b"{nope", "user"),
            Err(EntityError::Decode(_))
        ));
    }
}
