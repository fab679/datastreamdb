//! `datastreamdb acl` client flows (Go `acl/acl.go`, minus the CLI itself).
//!
//! [`AclAdmin`] drives user/group/rule management over the public API:
//! it logs in with guardian credentials, then runs queries and
//! mutations. Rule changes resolve UIDs client-side (read-then-write)
//! because upsert `cond` is still deferred; concurrent `chmod` calls can
//! interleave (documented, matches the tool's interactive use).

use std::collections::HashSet;
use std::fmt::Write as _;

use datastreamdb_proto::api::{
    Jwt, LoginRequest, Mutation, Request, data_stream_db_client::DataStreamDbClient,
};
use prost::Message as _;

/// Interceptor presenting a fixed token as `accessjwt` metadata.
#[derive(Clone, Debug)]
struct Token(tonic::metadata::MetadataValue<tonic::metadata::Ascii>);

impl tonic::service::Interceptor for Token {
    fn call(
        &mut self,
        mut request: tonic::Request<()>,
    ) -> Result<tonic::Request<()>, tonic::Status> {
        request.metadata_mut().insert("accessjwt", self.0.clone());
        Ok(request)
    }
}

/// Authenticated `DataStreamDB` client.
type AuthedClient = DataStreamDbClient<
    tonic::service::interceptor::InterceptedService<tonic::transport::Channel, Token>,
>;

/// Admin operation failures (messages mirror Go's).
#[derive(Debug, thiserror::Error)]
pub enum AdminError {
    /// Transport/dial failure.
    #[error("cannot reach alpha at {0}: {1}")]
    Dial(String, String),
    /// RPC failure.
    #[error("request failed: {0}")]
    Rpc(String),
    /// Login rejected.
    #[error("login failed: {0}")]
    Login(String),
    /// Entity already exists.
    #[error("{0}")]
    Conflict(String),
    /// Entity or rule not found.
    #[error("{0}")]
    NotFound(String),
    /// Malformed response or flag value.
    #[error("{0}")]
    Invalid(String),
}

/// Guardian credentials (`user=...;password=...;namespace=...`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GuardianCreds {
    /// Login user id.
    pub user: String,
    /// Login password.
    pub password: String,
    /// Tenancy namespace.
    pub namespace: u64,
}

/// Parses `--guardian-creds` superflag pairs.
///
/// # Errors
///
/// Returns [`AdminError::Invalid`] on malformed numbers.
pub fn parse_guardian_creds(flag: &str) -> Result<GuardianCreds, AdminError> {
    let mut creds = GuardianCreds::default();
    for part in flag.split(';') {
        let Some((key, value)) = part.trim().split_once('=') else {
            continue;
        };
        match key.trim() {
            "user" => value.trim().clone_into(&mut creds.user),
            "password" => value.trim().clone_into(&mut creds.password),
            "namespace" => {
                creds.namespace = value.trim().parse().map_err(|_| {
                    AdminError::Invalid(format!("bad namespace in --guardian-creds: {flag:?}"))
                })?;
            }
            _ => {}
        }
    }
    Ok(creds)
}

/// ACL administrator: guardian client plus user/group/rule operations.
pub struct AclAdmin {
    /// Authenticated client.
    client: AuthedClient,
    /// Read timestamp floor: the highest commit seen by this client.
    ///
    /// The admin flows are read-then-write (duplicate checks, rule
    /// resolution). Reading at `max_assigned` can race the commit of the
    /// previous mutation; pinning reads to the last commit makes the
    /// sequence read-your-writes.
    read_ts: u64,
}

impl AclAdmin {
    /// Dials `addr` and logs in with `creds`.
    ///
    /// # Errors
    ///
    /// Returns [`AdminError`] when dialing or login fails.
    pub async fn connect(addr: &str, creds: &GuardianCreds) -> Result<Self, AdminError> {
        let channel = tonic::transport::Endpoint::from_shared(format!("http://{addr}"))
            .map_err(|err| AdminError::Dial(addr.to_owned(), err.to_string()))?
            .connect()
            .await
            .map_err(|err| AdminError::Dial(addr.to_owned(), err.to_string()))?;
        let mut client = DataStreamDbClient::new(channel.clone());
        let body = client
            .login(LoginRequest {
                userid: creds.user.clone(),
                password: creds.password.clone(),
                namespace: creds.namespace,
                ..LoginRequest::default()
            })
            .await
            .map_err(|err| AdminError::Login(err.to_string()))?
            .into_inner()
            .json;
        let jwt = Jwt::decode(body.as_slice()).map_err(|err| AdminError::Login(err.to_string()))?;
        let token: tonic::metadata::MetadataValue<_> = jwt.access_jwt.parse().map_err(
            |err: tonic::metadata::errors::InvalidMetadataValue| AdminError::Login(err.to_string()),
        )?;
        Ok(Self {
            client: DataStreamDbClient::with_interceptor(channel, Token(token)),
            read_ts: 0,
        })
    }

    /// Runs a read-only DQL query, returning the JSON body.
    async fn query_json(&mut self, query: &str) -> Result<Vec<u8>, AdminError> {
        let response = self
            .client
            .query(Request {
                query: query.to_owned(),
                start_ts: self.read_ts,
                ..Request::default()
            })
            .await
            .map_err(|err| AdminError::Rpc(err.to_string()))?
            .into_inner();
        if let Some(txn) = response.txn.as_ref() {
            self.read_ts = self.read_ts.max(txn.commit_ts);
        }
        Ok(response.json)
    }

    /// Runs set/del N-Quads with `commit_now`, failing on abort.
    async fn mutate(&mut self, set: &str, del: &str, what: &str) -> Result<(), AdminError> {
        let response = self
            .client
            .query(Request {
                mutations: vec![Mutation {
                    set_nquads: set.as_bytes().to_vec(),
                    del_nquads: del.as_bytes().to_vec(),
                    commit_now: true,
                    ..Mutation::default()
                }],
                commit_now: true,
                ..Request::default()
            })
            .await
            .map_err(|err| AdminError::Rpc(format!("unable to {what}: {err}")))?
            .into_inner();
        if let Some(txn) = response.txn.as_ref() {
            if txn.aborted {
                return Err(AdminError::Rpc(format!("unable to {what}: aborted")));
            }
            self.read_ts = self.read_ts.max(txn.commit_ts);
        }
        Ok(())
    }

    /// Fetches one user with groups (Go `queryUser`).
    ///
    /// # Errors
    ///
    /// Returns [`AdminError`] when the query or decode fails.
    pub async fn user(
        &mut self,
        userid: &str,
    ) -> Result<Option<datastreamdb_acl::entities::User>, AdminError> {
        let body = self
            .query_json(&format!(
                "{{ user(func: eq(datastreamdb.xid, \"{userid}\")) \
                @filter(type(datastreamdb.type.User)) {{ \
                uid datastreamdb.xid datastreamdb.user.group {{ uid datastreamdb.xid }} }} }}"
            ))
            .await?;
        datastreamdb_acl::entities::unmarshal_user(&body, "user")
            .map_err(|err| AdminError::Invalid(err.to_string()))
    }

    /// Fetches one group with users and rules (Go `queryGroup`, full).
    ///
    /// # Errors
    ///
    /// Returns [`AdminError`] when the query or decode fails.
    pub async fn group(
        &mut self,
        groupid: &str,
    ) -> Result<Option<datastreamdb_acl::entities::Group>, AdminError> {
        let body = self
            .query_json(&format!(
                "{{ group(func: eq(datastreamdb.xid, \"{groupid}\")) \
                @filter(type(datastreamdb.type.Group)) {{ \
                uid datastreamdb.xid \
                datastreamdb.acl.rule {{ uid datastreamdb.rule.predicate datastreamdb.rule.permission }} }} }}"
            ))
            .await?;
        let mut group = datastreamdb_acl::entities::unmarshal_group(&body, "group")
            .map_err(|err| AdminError::Invalid(err.to_string()))?;
        // Member users resolve forward (reverse edges stay unread here).
        if group.is_some() {
            let users = self.group_members(groupid).await?;
            if let Some(group) = group.as_mut() {
                group.users = users;
            }
        }
        Ok(group)
    }

    /// Lists member users of `groupid` (forward scan, no reverse edges).
    async fn group_members(
        &mut self,
        groupid: &str,
    ) -> Result<Vec<datastreamdb_acl::entities::User>, AdminError> {
        let body = self
            .query_json(
                "{ users(func: has(datastreamdb.xid)) @filter(type(datastreamdb.type.User)) { \
                uid datastreamdb.xid datastreamdb.user.group { datastreamdb.xid } } }",
            )
            .await?;
        let users = datastreamdb_acl::entities::unmarshal_users(&body, "users")
            .map_err(|err| AdminError::Invalid(err.to_string()))?;
        Ok(users
            .into_iter()
            .filter(|user| user.groups.iter().any(|g| g.group_id == groupid))
            .collect())
    }

    /// Creates a user (Go `userAdd`).
    ///
    /// # Errors
    ///
    /// Returns [`AdminError::Conflict`] when the id exists, [`AdminError`]
    /// when the mutation fails.
    pub async fn add_user(&mut self, userid: &str, password: &str) -> Result<(), AdminError> {
        if self.user(userid).await?.is_some() {
            return Err(AdminError::Conflict(format!(
                "unable to create user because of conflict: {userid}"
            )));
        }
        let set = format!(
            "_:u <datastreamdb.xid> \"{userid}\" .\n_:u <datastreamdb.password> \"{password}\" .\n_:u <datastreamdb.type> \"datastreamdb.type.User\" .\n"
        );
        self.mutate(&set, "", &format!("create user {userid}"))
            .await
    }

    /// Creates a group (Go `groupAdd`).
    ///
    /// # Errors
    ///
    /// Returns [`AdminError::Conflict`] when the id exists, [`AdminError`]
    /// when the mutation fails.
    pub async fn add_group(&mut self, groupid: &str) -> Result<(), AdminError> {
        if self.group(groupid).await?.is_some() {
            return Err(AdminError::Conflict(format!(
                "group {groupid:?} already exists"
            )));
        }
        let set = format!(
            "_:g <datastreamdb.xid> \"{groupid}\" .\n_:g <datastreamdb.type> \"datastreamdb.type.Group\" .\n"
        );
        self.mutate(&set, "", &format!("create group {groupid}"))
            .await
    }

    /// Deletes a user and its edges (Go `userOrGroupDel`, explicit edges).
    ///
    /// # Errors
    ///
    /// Returns [`AdminError::NotFound`] when the id is unknown.
    pub async fn del_user(&mut self, userid: &str) -> Result<(), AdminError> {
        let Some(user) = self.user(userid).await? else {
            return Err(AdminError::NotFound(format!(
                "unable to delete {userid:?} since it does not exist"
            )));
        };
        let mut del = format!(
            "<{}> <datastreamdb.xid> * .\n<{}> <datastreamdb.password> * .\n<{}> <datastreamdb.type> * .\n",
            user.uid, user.uid, user.uid
        );
        for group in &user.groups {
            writeln!(
                del,
                "<{}> <datastreamdb.user.group> <{}> .",
                user.uid, group.uid
            )
            .expect("format to string");
        }
        self.mutate("", &del, &format!("delete {userid:?}")).await
    }

    /// Deletes a group, its rules, and member links (Go `userOrGroupDel`).
    ///
    /// # Errors
    ///
    /// Returns [`AdminError::NotFound`] when the id is unknown.
    pub async fn del_group(&mut self, groupid: &str) -> Result<(), AdminError> {
        let Some(group) = self.group(groupid).await? else {
            return Err(AdminError::NotFound(format!(
                "unable to delete {groupid:?} since it does not exist"
            )));
        };
        let mut del = format!(
            "<{}> <datastreamdb.xid> * .\n<{}> <datastreamdb.type> * .\n",
            group.uid, group.uid
        );
        for rule in self.group_rules(&group.uid).await? {
            writeln!(del,
                "<{}> <datastreamdb.acl.rule> <{}> .\n<{}> <datastreamdb.rule.predicate> * .\n<{}> <datastreamdb.rule.permission> * .",
                group.uid, rule.0, rule.0, rule.0
            ).expect("format to string");
        }
        for user in &group.users {
            writeln!(
                del,
                "<{}> <datastreamdb.user.group> <{}> .",
                user.uid, group.uid
            )
            .expect("format to string");
        }
        self.mutate("", &del, &format!("delete {groupid:?}")).await
    }

    /// Lists a group's rule uids (uid, predicate).
    async fn group_rules(&mut self, group_uid: &str) -> Result<Vec<(String, String)>, AdminError> {
        let body = self
            .query_json(&format!(
                "{{ g(func: uid({group_uid})) {{ \
                datastreamdb.acl.rule {{ uid datastreamdb.rule.predicate }} }} }}"
            ))
            .await?;
        let json: serde_json::Value =
            serde_json::from_slice(&body).map_err(|err| AdminError::Invalid(err.to_string()))?;
        let mut out = Vec::new();
        if let Some(rules) = json
            .get("g")
            .and_then(|g| g.as_array())
            .and_then(|g| g.first())
            .and_then(|g| g.get("datastreamdb.acl.rule"))
            .and_then(|r| r.as_array())
        {
            for rule in rules {
                if let (Some(uid), Some(pred)) = (
                    rule.get("uid").and_then(serde_json::Value::as_str),
                    rule.get("datastreamdb.rule.predicate").and_then(|p| {
                        p.as_str()
                            .or_else(|| p.get("value").and_then(serde_json::Value::as_str))
                    }),
                ) {
                    out.push((uid.to_owned(), pred.to_owned()));
                }
            }
        }
        Ok(out)
    }

    /// Changes a user's password (Go `changePassword`).
    ///
    /// # Errors
    ///
    /// Returns [`AdminError::NotFound`] when the id is unknown.
    pub async fn change_password(
        &mut self,
        userid: &str,
        password: &str,
    ) -> Result<(), AdminError> {
        let Some(user) = self.user(userid).await? else {
            return Err(AdminError::NotFound(format!(
                "user {userid:?} does not exist"
            )));
        };
        let set = format!("<{}> <datastreamdb.password> \"{password}\" .\n", user.uid);
        self.mutate(&set, "", &format!("change password for {userid}"))
            .await
    }

    /// Replaces a user's group list (Go `userMod`, diffed).
    ///
    /// # Errors
    ///
    /// Returns [`AdminError::NotFound`] for unknown users or groups.
    pub async fn set_user_groups(
        &mut self,
        userid: &str,
        groups: &[String],
    ) -> Result<(), AdminError> {
        let Some(user) = self.user(userid).await? else {
            return Err(AdminError::NotFound(format!(
                "user {userid:?} does not exist"
            )));
        };
        let want: HashSet<&String> = groups.iter().collect();
        let have: HashSet<&String> = user.groups.iter().map(|g| &g.group_id).collect();
        let mut set = String::new();
        let mut del = String::new();
        for group in want.difference(&have) {
            let Some(target) = self.group(group).await? else {
                return Err(AdminError::NotFound(format!(
                    "group {group:?} does not exist"
                )));
            };
            writeln!(
                set,
                "<{}> <datastreamdb.user.group> <{}> .",
                user.uid, target.uid
            )
            .expect("format to string");
        }
        for group in have.difference(&want) {
            let Some(target) = self.group(group).await? else {
                continue;
            };
            writeln!(
                del,
                "<{}> <datastreamdb.user.group> <{}> .",
                user.uid, target.uid
            )
            .expect("format to string");
        }
        if set.is_empty() && del.is_empty() {
            return Ok(());
        }
        self.mutate(&set, &del, &format!("modify groups for {userid}"))
            .await
    }

    /// Adds, updates, or (negative `perm`) deletes a group rule (Go `chMod`).
    ///
    /// UIDs resolve client-side first (read-then-write; upsert `cond`
    /// support would make this atomic).
    ///
    /// # Errors
    ///
    /// Returns [`AdminError`] on bad permissions, unknown groups, or
    /// mutation failure.
    pub async fn chmod(
        &mut self,
        groupid: &str,
        predicate: &str,
        perm: i64,
    ) -> Result<(), AdminError> {
        if predicate.is_empty() {
            return Err(AdminError::Invalid("no predicates specified".to_owned()));
        }
        if perm > 7 {
            return Err(AdminError::Invalid(format!(
                "the perm value must be less than or equal to 7, the provided value is {perm}"
            )));
        }
        let Some(group) = self.group(groupid).await? else {
            return Err(AdminError::NotFound(format!(
                "group <{groupid}> doesn't exist"
            )));
        };
        let rules = self.group_rules(&group.uid).await?;
        let existing = rules.iter().find(|(_, pred)| pred == predicate);
        if perm < 0 {
            let Some((uid, _)) = existing else {
                return Ok(());
            };
            let del = format!(
                "<{}> <datastreamdb.acl.rule> <{}> .\n<{}> <datastreamdb.rule.predicate> * .\n<{}> <datastreamdb.rule.permission> * .\n",
                group.uid, uid, uid, uid
            );
            return self
                .mutate("", &del, &format!("delete rule for {predicate}"))
                .await;
        }
        if let Some((uid, _)) = existing {
            let set = format!("<{uid}> <datastreamdb.rule.permission> \"{perm}\" .\n");
            return self
                .mutate(&set, "", &format!("update rule for {predicate}"))
                .await;
        }
        let set = format!(
            "_:rule <datastreamdb.rule.predicate> \"{predicate}\" .\n_:rule <datastreamdb.rule.permission> \"{perm}\" .\n<{}> <datastreamdb.acl.rule> _:rule .\n",
            group.uid
        );
        self.mutate(&set, "", &format!("create rule for {predicate}"))
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guardian_creds_parse() {
        let creds = parse_guardian_creds("user=groot;password=s3cret;namespace=2").expect("parse");
        assert_eq!(creds.user, "groot");
        assert_eq!(creds.password, "s3cret");
        assert_eq!(creds.namespace, 2);
        let creds = parse_guardian_creds("user=groot;password=x").expect("parse");
        assert_eq!(creds.namespace, 0);
        assert!(parse_guardian_creds("namespace=abc").is_err());
    }
}
