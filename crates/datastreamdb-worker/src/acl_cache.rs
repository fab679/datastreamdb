//! Group-rule cache for authorization (`worker/acl_cache.go`).
//!
//! [`AclCache`] keeps two views over the same rules: predicate → group →
//! permission (for fail-closed per-predicate checks) and user →
//! predicate → permission (OR-ed across the user's groups, for the
//! allowed-predicate lists queries carry). Updates replace one
//! namespace's rules wholesale, like Go.

use parking_lot::RwLock;
use std::collections::{HashMap, HashSet};

use datastreamdb_acl::entities::{Group, Operation};

/// Wildcard granting an operation on every non-ACL predicate.
pub const ALL_PREDS: &str = "datastreamdb.all";

/// Splits `ns-attr`, or `None` when malformed (fail closed).
fn split_predicate(predicate: &str) -> Option<(u64, &str)> {
    let (ns, attr) = predicate.split_once('-')?;
    if attr.is_empty() {
        return None;
    }
    u64::from_str_radix(ns, 16).ok().map(|ns| (ns, attr))
}

/// Cached group rules with interior mutability (shared across tasks).
#[derive(Debug, Default)]
pub struct AclCache {
    /// Predicate → group → permission bits.
    pred_perms: RwLock<HashMap<String, HashMap<String, i32>>>,
    /// User → predicate → OR-ed permission bits.
    user_pred_perms: RwLock<HashMap<String, HashMap<String, i32>>>,
    /// Whether rules were ever loaded.
    loaded: RwLock<bool>,
}

impl AclCache {
    /// Empty, unloaded cache.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Drops the loaded flag (forces a refresh on next use).
    pub fn reset(&self) {
        *self.loaded.write() = false;
    }

    /// Whether rules were ever loaded.
    #[must_use]
    pub fn loaded(&self) -> bool {
        *self.loaded.read()
    }

    /// Marks the cache loaded.
    pub fn set_loaded(&self) {
        *self.loaded.write() = true;
    }

    /// Replaces namespace `ns` rules from `groups` (Go `Update`).
    ///
    /// Predicates are namespaced on the way in; permissions OR across a
    /// user's groups. Other namespaces' rules are preserved.
    pub fn update(&self, ns: u64, groups: &[Group]) {
        let mut pred_perms: HashMap<String, HashMap<String, i32>> = HashMap::new();
        let mut user_perms: HashMap<String, HashMap<String, i32>> = HashMap::new();
        for group in groups {
            for rule in &group.rules {
                if rule.predicate.is_empty() {
                    continue;
                }
                let pred = datastreamdb_x::namespace_attr(ns, &rule.predicate);
                pred_perms
                    .entry(pred)
                    .or_default()
                    .insert(group.group_id.clone(), rule.perm);
            }
            for user in &group.users {
                let entry = user_perms.entry(user.user_id.clone()).or_default();
                for rule in &group.rules {
                    let pred = datastreamdb_x::namespace_attr(ns, &rule.predicate);
                    *entry.entry(pred).or_insert(0) |= rule.perm;
                }
            }
        }

        let mut cached = self.pred_perms.write();
        cached.retain(|pred, _| datastreamdb_x::parse_namespace(pred) != ns);
        cached.extend(pred_perms);
        drop(cached);

        let mut users = self.user_pred_perms.write();
        for perms in users.values_mut() {
            perms.retain(|pred, _| datastreamdb_x::parse_namespace(pred) != ns);
        }
        users.retain(|_, perms| !perms.is_empty());
        for (user_id, perms) in user_perms {
            users.entry(user_id).or_default().extend(perms);
        }
        drop(users);
        self.set_loaded();
    }

    /// Snapshot of one user's predicate permissions (empty when unknown).
    #[must_use]
    pub fn user_pred_perms(&self, user_id: &str) -> HashMap<String, i32> {
        self.user_pred_perms
            .read()
            .get(user_id)
            .cloned()
            .unwrap_or_default()
    }

    /// Whether `groups` may run `operation` on `predicate`.
    ///
    /// ACL predicates are never granted here (guardians bypass earlier);
    /// the `datastreamdb.all` wildcard covers the rest. Anything without a
    /// matching rule — including malformed predicates — is denied.
    #[must_use]
    pub fn authorize_predicate(
        &self,
        groups: &[String],
        predicate: &str,
        operation: Operation,
    ) -> bool {
        let Some((ns, attr)) = split_predicate(predicate) else {
            return false;
        };
        if datastreamdb_x::is_acl_predicate(attr) {
            return false;
        }
        if self.has_access_to_all(ns, groups, operation) {
            return true;
        }
        self.has_access_to_pred(predicate, groups, operation)
    }

    /// Whether `groups` hold the `datastreamdb.all` wildcard for `operation`.
    #[must_use]
    pub fn has_access_to_all(&self, ns: u64, groups: &[String], operation: Operation) -> bool {
        let pred = datastreamdb_x::namespace_attr(ns, ALL_PREDS);
        self.has_access_to_pred(&pred, groups, operation)
    }

    /// Raw predicate check behind [`AclCache::authorize_predicate`].
    fn has_access_to_pred(&self, predicate: &str, groups: &[String], operation: Operation) -> bool {
        let cached = self.pred_perms.read();
        let Some(group_perms) = cached.get(predicate) else {
            return false;
        };
        groups.iter().any(|group| {
            group_perms
                .get(group)
                .is_some_and(|perm| perm & operation.code != 0)
        })
    }

    /// Predicates from `preds` that `groups` may not run `operation` on.
    #[must_use]
    pub fn blocked<S: std::hash::BuildHasher>(
        &self,
        groups: &[String],
        preds: &HashSet<String, S>,
        operation: Operation,
    ) -> HashSet<String> {
        preds
            .iter()
            .filter(|pred| !self.authorize_predicate(groups, pred, operation))
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use datastreamdb_acl::entities::{Acl, READ, User, WRITE};

    fn group(id: &str, users: &[&str], rules: &[(&str, i32)]) -> Group {
        Group {
            uid: String::new(),
            group_id: id.to_owned(),
            users: users
                .iter()
                .map(|user| User {
                    uid: String::new(),
                    user_id: (*user).to_owned(),
                    password: String::new(),
                    namespace: 0,
                    password_match: false,
                    groups: Vec::new(),
                })
                .collect(),
            rules: rules
                .iter()
                .map(|(predicate, perm)| Acl {
                    predicate: (*predicate).to_owned(),
                    perm: *perm,
                })
                .collect(),
        }
    }

    fn seeded() -> AclCache {
        let cache = AclCache::new();
        assert!(!cache.loaded());
        cache.update(
            0,
            &[
                group("dev", &["alice"], &[("name", 4), ("friend", 6)]),
                group("ops", &["alice", "bob"], &[("name", 7)]),
            ],
        );
        assert!(cache.loaded());
        cache
    }

    #[test]
    fn authorizes_by_operation_bits() {
        let cache = seeded();
        let alice = vec!["dev".to_owned(), "ops".to_owned()];
        // friend: dev 6 (read+write), ops silent.
        assert!(cache.authorize_predicate(&alice, "0-friend", READ));
        assert!(cache.authorize_predicate(&alice, "0-friend", WRITE));
        // Unknown predicate: no rule matches, denied.
        assert!(!cache.authorize_predicate(&alice, "0-age", READ));
        let bob = vec!["ops".to_owned()];
        assert!(!cache.authorize_predicate(&bob, "0-friend", READ));
        assert!(cache.authorize_predicate(&bob, "0-name", READ));
    }

    #[test]
    fn user_permissions_or_across_groups() {
        let cache = seeded();
        let perms = cache.user_pred_perms("alice");
        assert_eq!(perms.get("0-name"), Some(&7));
        assert_eq!(perms.get("0-friend"), Some(&6));
        assert!(cache.user_pred_perms("mallory").is_empty());
    }

    #[test]
    fn acl_predicates_never_grant() {
        let cache = seeded();
        let groups = vec!["dev".to_owned()];
        assert!(!cache.authorize_predicate(&groups, "0-datastreamdb.password", READ));
        let preds: HashSet<String> = ["0-name".to_owned(), "0-age".to_owned()]
            .into_iter()
            .collect();
        let blocked = cache.blocked(&groups, &preds, READ);
        assert_eq!(blocked, ["0-age".to_owned()].into_iter().collect());
    }

    #[test]
    fn wildcard_covers_non_acl_predicates() {
        let cache = AclCache::new();
        cache.update(0, &[group("staff", &["zoe"], &[(ALL_PREDS, 4)])]);
        let groups = vec!["staff".to_owned()];
        assert!(cache.authorize_predicate(&groups, "0-anything", READ));
        assert!(!cache.authorize_predicate(&groups, "0-anything", WRITE));
        assert!(!cache.authorize_predicate(&groups, "0-datastreamdb.xid", READ));
    }

    #[test]
    fn update_replaces_one_namespace() {
        let cache = seeded();
        cache.update(1, &[group("dev", &["alice"], &[("name", 4)])]);
        // Namespace 0 rules survive alongside namespace 1 rules.
        assert!(cache.authorize_predicate(&["dev".to_owned()], "0-friend", READ));
        assert!(cache.authorize_predicate(&["dev".to_owned()], "1-name", READ));
        assert!(!cache.authorize_predicate(&["dev".to_owned()], "1-friend", READ));
        // Reloading namespace 0 drops its old friend rule.
        cache.update(0, &[group("dev", &["alice"], &[("name", 4)])]);
        assert!(!cache.authorize_predicate(&["dev".to_owned()], "0-friend", READ));
        assert!(cache.authorize_predicate(&["dev".to_owned()], "0-name", READ));
    }

    #[test]
    fn reset_forces_reload() {
        let cache = seeded();
        cache.reset();
        assert!(!cache.loaded());
    }

    #[test]
    fn malformed_predicates_deny_without_panic() {
        let cache = seeded();
        let groups = vec!["dev".to_owned()];
        for bad in ["", "name", "-name", "0-", "zz-name", "0-"] {
            assert!(!cache.authorize_predicate(&groups, bad, READ), "{bad:?}");
        }
    }
}
