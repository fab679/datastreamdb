//! User/group creation mutations (Go `CreateUserNQuads`/`CreateGroupNQuads`).
//!
//! These builders emit the exact `NQuad` shapes the `datastreamdb acl` tooling and
//! the guardian/groot bootstrap write; uids stay blank (`_:newuser`,
//! `_:newgroup`) for the mutation layer to assign.

use datastreamdb_proto::api::{NQuad, Value, value::Val};

/// Blank node allocated for a new user.
pub const NEW_USER_BLANK: &str = "_:newuser";
/// Blank node allocated for a new group.
pub const NEW_GROUP_BLANK: &str = "_:newgroup";
/// User type tag.
pub const USER_TYPE: &str = "datastreamdb.type.User";
/// Group type tag.
pub const GROUP_TYPE: &str = "datastreamdb.type.Group";

/// String-valued `NQuad` helper.
fn str_quad(subject: &str, predicate: &str, value: &str) -> NQuad {
    NQuad {
        subject: subject.to_owned(),
        predicate: predicate.to_owned(),
        object_value: Some(Value {
            val: Some(Val::StrVal(value.to_owned())),
        }),
        ..NQuad::default()
    }
}

/// `NQuad`s storing a user with `user_id` and a (plain-text, server-hashed)
/// `password` in the ACL system.
#[must_use]
pub fn create_user_nquads(user_id: &str, password: &str) -> Vec<NQuad> {
    vec![
        str_quad(NEW_USER_BLANK, "datastreamdb.xid", user_id),
        str_quad(NEW_USER_BLANK, "datastreamdb.password", password),
        str_quad(NEW_USER_BLANK, "datastreamdb.type", USER_TYPE),
    ]
}

/// `NQuad`s storing a group with `group_id` in the ACL system.
#[must_use]
pub fn create_group_nquads(group_id: &str) -> Vec<NQuad> {
    vec![
        str_quad(NEW_GROUP_BLANK, "datastreamdb.xid", group_id),
        str_quad(NEW_GROUP_BLANK, "datastreamdb.type", GROUP_TYPE),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn str_val(quad: &NQuad) -> &str {
        match quad.object_value.as_ref().and_then(|v| v.val.as_ref()) {
            Some(Val::StrVal(text)) => text,
            other => panic!("expected StrVal, got {other:?}"),
        }
    }

    #[test]
    fn user_nquads_match_go() {
        let quads = create_user_nquads("alice", "secret123");
        assert_eq!(quads.len(), 3);
        for quad in &quads {
            assert_eq!(quad.subject, "_:newuser");
        }
        assert_eq!(quads[0].predicate, "datastreamdb.xid");
        assert_eq!(str_val(&quads[0]), "alice");
        assert_eq!(quads[1].predicate, "datastreamdb.password");
        assert_eq!(str_val(&quads[1]), "secret123");
        assert_eq!(quads[2].predicate, "datastreamdb.type");
        assert_eq!(str_val(&quads[2]), "datastreamdb.type.User");
    }

    #[test]
    fn group_nquads_match_go() {
        let quads = create_group_nquads("dev");
        assert_eq!(quads.len(), 2);
        for quad in &quads {
            assert_eq!(quad.subject, "_:newgroup");
        }
        assert_eq!(quads[0].predicate, "datastreamdb.xid");
        assert_eq!(str_val(&quads[0]), "dev");
        assert_eq!(quads[1].predicate, "datastreamdb.type");
        assert_eq!(str_val(&quads[1]), "datastreamdb.type.Group");
    }
}
