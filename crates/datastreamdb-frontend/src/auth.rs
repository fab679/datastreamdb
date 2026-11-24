//! Request authentication and predicate authorization (`edgraph/access.go`).
//!
//! [`AuthContext`] carries the pieces every guarded handler needs: JWT
//! [`Secrets`](datastreamdb_acl::keys::Secrets), the shared [`AclCache`], and
//! token TTLs. Queries degrade to their allowed predicates (blocked
//! subgraphs are pruned, like Go); mutations and alters fail with
//! `PermissionDenied`; missing or bad credentials fail with
//! `Unauthenticated`. Guardians bypass everything. With no [`AuthContext`]
//! configured, all requests stay open, like Go's ACL-off mode.
//!
//! Divergences (all fail-closed): upsert `QueryVars` pruning is skipped
//! (upsert execution itself is still deferred); the shared-cloud
//! `shouldAllowAcls` gate always passes (single namespace until M6
//! tenancy); audit spans are not emitted (audit crate slice).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use datastreamdb_acl::entities::{Operation, READ};
use datastreamdb_acl::jwt::Identity;
use datastreamdb_worker::AclCache;
use tonic::metadata::MetadataMap;

/// gRPC metadata key carrying the access JWT (lowercased by tonic).
pub const ACCESS_JWT_METADATA: &str = "accessjwt";

/// HTTP header carrying the access JWT (Go `AttachAccessJwt`).
pub const ACCESS_JWT_HEADER: &str = "X-DataStreamDB-AccessJWT";

/// Authentication + authorization context shared by handlers.
#[derive(Clone)]
pub struct AuthContext {
    /// JWT signing/verification keys.
    secrets: datastreamdb_acl::keys::Secrets,
    /// Group-rule cache (refreshed on first use).
    cache: Arc<AclCache>,
    /// Access-token TTL in seconds.
    access_ttl_secs: u64,
    /// Refresh-token TTL in seconds.
    refresh_ttl_secs: u64,
}

/// ACL configuration (parsed from `--acl` by the alpha binary).
#[derive(Debug, Clone)]
pub struct AclSettings {
    /// JWT algorithm name (`HS256` default).
    pub alg: String,
    /// Raw secret bytes (HMAC) or private-key PEM (asymmetric).
    pub secret: Vec<u8>,
    /// Access-token TTL in seconds.
    pub access_ttl_secs: u64,
    /// Refresh-token TTL in seconds.
    pub refresh_ttl_secs: u64,
}

impl Default for AclSettings {
    fn default() -> Self {
        Self {
            alg: datastreamdb_acl::keys::DEFAULT_ALG.to_owned(),
            secret: Vec::new(),
            access_ttl_secs: datastreamdb_acl::keys::DEFAULT_ACCESS_TTL_SECS,
            refresh_ttl_secs: datastreamdb_acl::keys::DEFAULT_REFRESH_TTL_SECS,
        }
    }
}

impl AuthContext {
    /// Builds a context from settings (fresh rule cache).
    ///
    /// # Errors
    ///
    /// Returns the key error when the algorithm or secret is rejected.
    pub fn from_settings(settings: &AclSettings) -> Result<Self, datastreamdb_acl::keys::KeyError> {
        let alg = datastreamdb_acl::keys::Alg::parse(&settings.alg)?;
        let secrets = datastreamdb_acl::keys::Secrets::from_private_key(alg, &settings.secret)?;
        Ok(Self::new(secrets, Arc::new(AclCache::new()))
            .with_ttls(settings.access_ttl_secs, settings.refresh_ttl_secs))
    }

    /// Builds a context over `secrets` with Go's default TTLs.
    pub fn new(secrets: datastreamdb_acl::keys::Secrets, cache: Arc<AclCache>) -> Self {
        Self {
            secrets,
            cache,
            access_ttl_secs: datastreamdb_acl::keys::DEFAULT_ACCESS_TTL_SECS,
            refresh_ttl_secs: datastreamdb_acl::keys::DEFAULT_REFRESH_TTL_SECS,
        }
    }

    /// Overrides the token TTLs (tests, `--acl` flags).
    #[must_use]
    pub fn with_ttls(mut self, access_secs: u64, refresh_secs: u64) -> Self {
        self.access_ttl_secs = access_secs;
        self.refresh_ttl_secs = refresh_secs;
        self
    }

    /// JWT keys.
    #[must_use]
    pub fn secrets(&self) -> &datastreamdb_acl::keys::Secrets {
        &self.secrets
    }

    /// Rule cache.
    #[must_use]
    pub fn cache(&self) -> &Arc<AclCache> {
        &self.cache
    }

    /// Access-token TTL in seconds.
    #[must_use]
    pub fn access_ttl_secs(&self) -> u64 {
        self.access_ttl_secs
    }

    /// Refresh-token TTL in seconds.
    #[must_use]
    pub fn refresh_ttl_secs(&self) -> u64 {
        self.refresh_ttl_secs
    }
}

/// Extracts the access token from request metadata, if present.
#[must_use]
pub fn extract_token(metadata: &MetadataMap) -> Option<String> {
    metadata
        .get(ACCESS_JWT_METADATA)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

/// Resolves the caller's identity (Go `extractUserAndGroups`).
///
/// Returns `Ok(None)` when ACL is off or no token was presented.
///
/// # Errors
///
/// Returns `Unauthenticated` when a presented token fails to verify.
pub fn authenticate(
    auth: Option<&AuthContext>,
    metadata: &MetadataMap,
) -> Result<Option<Identity>, tonic::Status> {
    let Some(auth) = auth else {
        return Ok(None);
    };
    let Some(token) = extract_token(metadata) else {
        return Ok(None);
    };
    datastreamdb_acl::jwt::validate(auth.secrets(), &token)
        .map(Some)
        .map_err(|err| tonic::Status::unauthenticated(err.to_string()))
}

/// Partition of predicates into allowed and blocked (Go `PredResult`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PredResult {
    /// Allowed predicates; `None` means all (blanket `datastreamdb.all` grant).
    pub allowed: Option<Vec<String>>,
    /// Blocked predicates.
    pub blocked: HashSet<String>,
}

/// Partitions `preds` for `identity` (Go `authorizePreds`).
///
/// Guardians bypass; everyone else is checked per predicate against the
/// cache, with `datastreamdb.all` collapsing the allowed set to `None`.
#[must_use]
pub fn authorize_predicates<S: std::hash::BuildHasher>(
    auth: &AuthContext,
    identity: &Identity,
    preds: &HashSet<String, S>,
    operation: Operation,
    namespace: u64,
) -> PredResult {
    if datastreamdb_x::is_super_admin(&identity.groups) {
        return PredResult {
            allowed: None,
            blocked: HashSet::new(),
        };
    }
    let namespaced: HashSet<String> = preds
        .iter()
        .map(|pred| datastreamdb_x::namespace_attr(namespace, pred))
        .collect();
    let blocked = auth
        .cache()
        .blocked(&identity.groups, &namespaced, operation);
    // Report callers' spellings, not namespaced keys.
    let blocked: HashSet<String> = preds
        .iter()
        .filter(|pred| blocked.contains(&datastreamdb_x::namespace_attr(namespace, pred)))
        .cloned()
        .collect();
    if auth
        .cache()
        .has_access_to_all(namespace, &identity.groups, operation)
    {
        return PredResult {
            allowed: None,
            blocked,
        };
    }
    let allowed = auth
        .cache()
        .user_pred_perms(&identity.user_id)
        .into_iter()
        .filter(|(_, perm)| perm & operation.code != 0)
        .map(|(pred, _)| pred)
        .collect();
    PredResult {
        allowed: Some(allowed),
        blocked,
    }
}

/// Collects query predicates plus the var→predicate map (Go
/// `parsePredsFromQuery`, recursive over children, filters, order,
///
/// group-by).
#[must_use]
pub fn collect_query_preds(
    query: &[datastreamdb_dql::GraphQuery],
) -> (HashSet<String>, HashMap<String, String>) {
    fn walk(
        blocks: &[datastreamdb_dql::GraphQuery],
        preds: &mut HashSet<String>,
        vars: &mut HashMap<String, String>,
    ) {
        for block in blocks {
            if let Some(func) = &block.func {
                if !func.attr.is_empty() {
                    preds.insert(func.attr.clone());
                }
            }
            if !block.var.is_empty() {
                vars.insert(block.var.clone(), block.attr.clone());
            }
            if !block.attr.is_empty()
                && block.attr != "uid"
                && block.attr != "expand"
                && block.attr != "val"
            {
                preds.insert(block.attr.clone());
            }
            for order in &block.order {
                preds.insert(order.attr.clone());
            }
            for attr in &block.groupby_attrs {
                preds.insert(attr.attr.clone());
            }
            collect_filter_preds(block.filter.as_deref(), preds);
            walk(&block.children, preds, vars);
        }
    }

    fn collect_filter_preds(
        filter: Option<&datastreamdb_dql::FilterTree>,
        preds: &mut HashSet<String>,
    ) {
        let Some(tree) = filter else {
            return;
        };
        if let Some(func) = &tree.func {
            if !func.attr.is_empty() {
                preds.insert(func.attr.clone());
            }
        }
        for child in &tree.child {
            collect_filter_preds(Some(child), preds);
        }
    }

    let mut preds = HashSet::new();
    let mut vars = HashMap::new();
    walk(query, &mut preds, &mut vars);
    // Variable names are not predicates.
    preds.retain(|pred| !pred.is_empty() && !vars.contains_key(pred));
    (preds, vars)
}

/// Removes blocked subgraphs/filters/orders (Go `removePredsFromQuery`).
pub fn remove_blocked_predicates<S: std::hash::BuildHasher>(
    query: &mut Vec<datastreamdb_dql::GraphQuery>,
    blocked: &HashSet<String, S>,
) {
    fn remove_filter<S: std::hash::BuildHasher>(
        filter: Option<Box<datastreamdb_dql::FilterTree>>,
        blocked: &HashSet<String, S>,
    ) -> Option<Box<datastreamdb_dql::FilterTree>> {
        let mut tree = filter?;
        if let Some(func) = &tree.func {
            if !func.attr.is_empty() && blocked.contains(&func.attr) {
                return None;
            }
        }
        let mut kept = Vec::with_capacity(tree.child.len());
        for child in std::mem::take(&mut tree.child) {
            if let Some(survivor) = remove_filter(Some(Box::new(child)), blocked) {
                kept.push(*survivor);
            }
        }
        tree.child = kept;
        Some(tree)
    }

    query.retain_mut(|block| {
        if let Some(func) = &block.func {
            if !func.attr.is_empty() && blocked.contains(&func.attr) {
                return false;
            }
        }
        if !block.attr.is_empty() {
            if blocked.contains(&block.attr) {
                return false;
            }
            if block.attr == "val"
                && block
                    .needs_var
                    .iter()
                    .any(|var| blocked.contains(&var.name))
            {
                return false;
            }
        }
        block.order.retain(|order| !blocked.contains(&order.attr));
        block.filter = remove_filter(block.filter.take(), blocked);
        block
            .groupby_attrs
            .retain(|attr| !blocked.contains(&attr.attr));
        remove_blocked_predicates(&mut block.children, blocked);
        true
    });
}

/// Authorizes a parsed query in place (Go `authorizeQuery`).
///
/// Guardians pass untouched; everyone else authenticates, partitions,
/// and degrades to the allowed subset (`allowed_preds` attached per
/// block for `_all_` expansion).
///
/// Callers must load the cache first (the coordinator refreshes it
/// via [`datastreamdb_worker::AclCache`]; an unloaded cache denies
/// everything here.
///
/// # Errors
///
/// Returns `Unauthenticated` for missing identity.
pub fn authorize_query(
    auth: &AuthContext,
    identity: Option<&Identity>,
    query: &mut Vec<datastreamdb_dql::GraphQuery>,
    namespace: u64,
) -> Result<(), tonic::Status> {
    let Some(identity) = identity else {
        return Err(tonic::Status::unauthenticated("no access JWT presented"));
    };
    if datastreamdb_x::is_super_admin(&identity.groups) {
        return Ok(());
    }
    let (preds, vars) = collect_query_preds(query);
    let result = authorize_predicates(auth, identity, &preds, READ, namespace);
    if result.blocked.is_empty() {
        attach_allowed(query, &result);
        return Ok(());
    }
    // Blocked predicates also block the variables defined on them.
    let pred_to_var: HashMap<&String, &String> =
        vars.iter().map(|(var, pred)| (pred, var)).collect();
    let mut blocked = result.blocked.clone();
    for pred in pred_to_var.keys() {
        if blocked.contains(*pred) {
            if let Some(var) = pred_to_var.get(pred) {
                blocked.insert((*var).clone());
            }
        }
    }
    remove_blocked_predicates(query, &blocked);
    attach_allowed(query, &result);
    Ok(())
}

/// Attaches the allowed list to top-level blocks (Go parity).
fn attach_allowed(query: &mut [datastreamdb_dql::GraphQuery], result: &PredResult) {
    let allowed = result.allowed.clone().unwrap_or_default();
    for block in query.iter_mut() {
        block.allowed_preds.clone_from(&allowed);
    }
}

/// Authorizes `NQuad` predicates for mutation (Go `authorizeMutation`).
///
/// Guardians may mutate anything except ACL-permission payloads; everyone
/// else needs `Write` on every touched predicate.
///
/// # Errors
///
/// Returns `Unauthenticated` for missing identity, `PermissionDenied`
/// for blocked predicates.
pub fn authorize_mutation(
    auth: &AuthContext,
    identity: Option<&Identity>,
    set: &[datastreamdb_chunker::NQuad],
    del: &[datastreamdb_chunker::NQuad],
    namespace: u64,
) -> Result<(), tonic::Status> {
    let Some(identity) = identity else {
        return Err(tonic::Status::unauthenticated("no access JWT presented"));
    };
    if datastreamdb_x::is_super_admin(&identity.groups) {
        if is_acl_pred_mutation(set) {
            return Err(tonic::Status::permission_denied(
                "the permission of ACL predicates can not be changed",
            ));
        }
        if is_acl_pred_mutation(del) {
            return Err(tonic::Status::permission_denied(
                "ACL predicates can't be deleted",
            ));
        }
        return Ok(());
    }
    let preds: HashSet<String> = set
        .iter()
        .chain(del.iter())
        .map(|quad| quad.predicate.clone())
        .collect();
    let result = authorize_predicates(
        auth,
        identity,
        &preds,
        datastreamdb_acl::entities::WRITE,
        namespace,
    );
    if result.blocked.is_empty() {
        return Ok(());
    }
    let mut blocked: Vec<&String> = result.blocked.iter().collect();
    blocked.sort();
    Err(tonic::Status::permission_denied(format!(
        "unauthorized to mutate following predicates: {}\n",
        blocked
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(" ")
    )))
}

/// Whether quads carry ACL-permission payloads (Go `isAclPredMutation`).
///
/// Only `datastreamdb.group.acl` byte payloads decoding as rule lists count;
/// undecodable payloads are ignored, like Go.
fn is_acl_pred_mutation(quads: &[datastreamdb_chunker::NQuad]) -> bool {
    for quad in quads {
        if quad.predicate != "datastreamdb.group.acl" {
            continue;
        }
        let Some(bytes) = quad.object_value.as_ref().and_then(|v| match v {
            datastreamdb_types::ApiValue::Bytes(bytes) => Some(bytes),
            _ => None,
        }) else {
            continue;
        };
        let Ok(rules): Result<Vec<datastreamdb_acl::entities::Acl>, _> =
            serde_json::from_slice(bytes)
        else {
            continue;
        };
        if rules
            .iter()
            .any(|rule| datastreamdb_x::is_acl_predicate(&rule.predicate))
        {
            return true;
        }
    }
    false
}

/// Authorizes an alter over `preds` (Go `authorizeAlter`).
///
/// Guardians may alter anything; everyone else is barred from drop-all /
/// drop-data and needs `Modify` on every touched predicate.
///
/// # Errors
///
/// Returns `Unauthenticated` for missing identity, `PermissionDenied`
/// for blocked predicates or guardian-only drops.
pub fn authorize_alter<S: std::hash::BuildHasher>(
    auth: &AuthContext,
    identity: Option<&Identity>,
    preds: &HashSet<String, S>,
    drop_all_or_data: bool,
    namespace: u64,
) -> Result<(), tonic::Status> {
    let Some(identity) = identity else {
        return Err(tonic::Status::unauthenticated("no access JWT presented"));
    };
    if datastreamdb_x::is_super_admin(&identity.groups) {
        return Ok(());
    }
    if drop_all_or_data {
        return Err(tonic::Status::permission_denied(format!(
            "only guardians are allowed to drop all data, but the current user is {}",
            identity.user_id
        )));
    }
    let result = authorize_predicates(
        auth,
        identity,
        preds,
        datastreamdb_acl::entities::MODIFY,
        namespace,
    );
    if result.blocked.is_empty() {
        return Ok(());
    }
    let mut blocked: Vec<&String> = result.blocked.iter().collect();
    blocked.sort();
    Err(tonic::Status::permission_denied(format!(
        "unauthorized to alter following predicates: {}\n",
        blocked
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(" ")
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use datastreamdb_acl::entities::{Acl, Group, User};
    use std::collections::HashMap;

    fn test_auth() -> AuthContext {
        let secrets = datastreamdb_acl::keys::Secrets::from_hmac(
            datastreamdb_acl::keys::Alg::Hs256,
            &[7u8; 32],
        )
        .expect("hmac");
        let cache = Arc::new(AclCache::new());
        cache.update(
            0,
            &[Group {
                uid: "0x1".to_owned(),
                group_id: "dev".to_owned(),
                users: vec![User {
                    uid: "0x2".to_owned(),
                    user_id: "alice".to_owned(),
                    ..User::default()
                }],
                rules: vec![Acl {
                    predicate: "name".to_owned(),
                    perm: 4,
                }],
            }],
        );
        AuthContext::new(secrets, cache)
    }

    fn alice() -> Identity {
        Identity {
            user_id: "alice".to_owned(),
            groups: vec!["dev".to_owned()],
            namespace: 0,
        }
    }

    fn parse(text: &str) -> Vec<datastreamdb_dql::GraphQuery> {
        datastreamdb_dql::parse(datastreamdb_dql::Request {
            text: text.to_owned(),
            variables: HashMap::new(),
        })
        .expect("parse")
        .query
    }

    #[test]
    fn collects_predicates_recursively() {
        let query =
            parse("{ q(func: has(name)) @filter(eq(age, 3)) { name friend { nickname } } }");
        let (preds, _) = collect_query_preds(&query);
        assert!(preds.contains("name"));
        assert!(preds.contains("friend"));
        assert!(preds.contains("nickname"));
        assert!(preds.contains("age"));
        assert!(!preds.contains("uid"));
    }

    #[test]
    fn removes_blocked_subgraphs() {
        let mut query = parse("{ q(func: has(name)) { name friend { nickname } } }");
        let blocked: HashSet<String> = ["friend".to_owned()].into_iter().collect();
        remove_blocked_predicates(&mut query, &blocked);
        assert_eq!(query.len(), 1);
        let attrs: Vec<&str> = query[0]
            .children
            .iter()
            .map(|child| child.attr.as_str())
            .collect();
        assert_eq!(attrs, vec!["name"]);
    }

    #[test]
    fn guardian_queries_pass_untouched() {
        let auth = test_auth();
        let guardian = Identity::guardian();
        let mut query = parse("{ q(func: has(secret)) { secret } }");
        authorize_query(&auth, Some(&guardian), &mut query, 0).expect("guardian");
        assert_eq!(query.len(), 1);
    }

    #[test]
    fn missing_identity_fails_closed() {
        let auth = test_auth();
        let mut query = parse("{ q(func: has(name)) { name } }");
        let err = authorize_query(&auth, None, &mut query, 0).expect_err("auth");
        assert_eq!(err.code(), tonic::Code::Unauthenticated);
    }

    #[test]
    fn queries_degrade_to_allowed() {
        let auth = test_auth();
        let alice = alice();
        let mut query = parse("{ q(func: has(name)) { name friend } }");
        authorize_query(&auth, Some(&alice), &mut query, 0).expect("degrade");
        let attrs: Vec<&str> = query[0]
            .children
            .iter()
            .map(|child| child.attr.as_str())
            .collect();
        assert_eq!(attrs, vec!["name"]);
        assert_eq!(query[0].allowed_preds, vec!["0-name".to_owned()]);
    }

    #[test]
    fn mutations_need_write_bits() {
        use datastreamdb_types::ApiValue;
        let auth = test_auth();
        let alice = alice();
        let quad = datastreamdb_chunker::NQuad {
            predicate: "name".to_owned(),
            ..datastreamdb_chunker::NQuad::default()
        };
        // Read-only rule: mutation denied.
        let err = authorize_mutation(&auth, Some(&alice), std::slice::from_ref(&quad), &[], 0)
            .expect_err("deny");
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
        assert!(err.message().contains("name"));
        // Guardians pass plain mutations...
        let guardian = Identity::guardian();
        authorize_mutation(&auth, Some(&guardian), &[quad], &[], 0).expect("guardian");
        // ...but not ACL-permission payloads.
        let payload = datastreamdb_chunker::NQuad {
            predicate: "datastreamdb.group.acl".to_owned(),
            object_value: Some(ApiValue::Bytes(
                br#"[{"datastreamdb.rule.predicate":"datastreamdb.password","datastreamdb.rule.permission":7}]"#
                    .to_vec(),
            )),
            ..datastreamdb_chunker::NQuad::default()
        };
        let err = authorize_mutation(&auth, Some(&guardian), &[payload], &[], 0)
            .expect_err("acl payload");
        assert!(err.message().contains("can not be changed"));
    }

    #[test]
    fn alters_need_modify_or_guardianship() {
        let auth = test_auth();
        let alice = alice();
        let preds: HashSet<String> = ["name".to_owned()].into_iter().collect();
        // Read-only rule: alter denied.
        let err = authorize_alter(&auth, Some(&alice), &preds, false, 0).expect_err("deny");
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
        // Drop-all is guardian-only.
        let err = authorize_alter(&auth, Some(&alice), &HashSet::new(), true, 0).expect_err("drop");
        assert!(err.message().contains("only guardians"));
        let guardian = Identity::guardian();
        authorize_alter(&auth, Some(&guardian), &preds, true, 0).expect("guardian");
        // Missing identity fails closed.
        let err = authorize_alter(&auth, None, &preds, false, 0).expect_err("auth");
        assert_eq!(err.code(), tonic::Code::Unauthenticated);
    }

    #[test]
    fn metadata_without_token_is_anonymous() {
        let auth = test_auth();
        let metadata = MetadataMap::new();
        assert!(
            authenticate(Some(&auth), &metadata)
                .expect("anon")
                .is_none()
        );
        assert!(authenticate(None, &metadata).expect("off").is_none());
    }
}
