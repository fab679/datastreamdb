//! Schema metadata: secrets, CORS origins/headers, auth metadata. Port of
//! `parseMetaInfo` / `metaInfo` in `dgraph-main/graphql/schema/schemagen.go`.

#![warn(missing_docs)]

use std::collections::{HashMap, HashSet};

use crate::auth::{self, AuthMeta};
use crate::errors::{GraphQlError, Result};

/// Default CORS headers. Port of Go `x.AccessControlAllowedHeaders`.
pub const ACCESS_CONTROL_ALLOWED_HEADERS: &str = "X-DataStreamDB-AccessToken, X-DataStreamDB-AuthToken, \
     Content-Type, Content-Length, Accept-Encoding, Cache-Control, X-CSRF-Token, X-Auth-Token, \
     X-Requested-With";

/// Metadata extracted from `# DataStreamDB.*` schema comments.
#[derive(Debug, Clone, Default)]
pub struct MetaInfo {
    /// Secrets from `# DataStreamDB.Secret` comments.
    pub secrets: HashMap<String, String>,
    /// Extra CORS headers from `@custom` forward headers plus the auth header.
    pub extra_cors_headers: Vec<String>,
    /// Allowed CORS origins from `# DataStreamDB.Allow-Origin` comments.
    pub allowed_cors_origins: HashSet<String>,
    /// Authorization metadata, when the schema declares it.
    pub auth_meta: Option<AuthMeta>,
}

impl MetaInfo {
    /// `Access-Control-Allow-Headers` value for this schema.
    #[must_use]
    pub fn allowed_cors_headers(&self) -> String {
        let mut headers = vec![ACCESS_CONTROL_ALLOWED_HEADERS.to_owned()];
        headers.extend(self.extra_cors_headers.iter().cloned());
        headers.join(",")
    }

    /// Allowed CORS origins.
    #[must_use]
    pub fn allowed_cors_origins(&self) -> &HashSet<String> {
        &self.allowed_cors_origins
    }

    /// Authorization metadata, when present.
    #[must_use]
    pub fn auth_meta(&self) -> Option<&AuthMeta> {
        self.auth_meta.as_ref()
    }
}

/// Parses `# DataStreamDB.*` comments from the input schema. Port of Go
/// `parseMetaInfo` with Go-exact error messages.
///
/// # Errors
///
/// Returns [`GraphQlError`] on malformed comments or auth metadata.
pub fn parse_meta_info(schema: &str) -> Result<MetaInfo> {
    let mut info = MetaInfo::default();
    let mut auth_secret = String::new();

    for line in schema.lines() {
        let text = line.trim();
        let Some(rest) = text.strip_prefix('#') else {
            continue;
        };
        let header = rest.trim();

        if let Some(_auth) = header.strip_prefix(auth::AUTH_META_HEADER) {
            if !auth_secret.is_empty() {
                return Err(GraphQlError::new(format!(
                    "DataStreamDB.Authorization should be only be specified once in a schema, \
                     found second mention: {text}"
                )));
            }
            text.clone_into(&mut auth_secret);
            continue;
        }

        if header.starts_with("DataStreamDB.Allow-Origin") {
            let parts: Vec<&str> = text.split_whitespace().collect();
            if parts.len() != 3 {
                return Err(GraphQlError::new(format!(
                    "incorrect format for specifying DataStreamDB.Allow-Origin found for comment: \
                     `{text}`, it should be `# DataStreamDB.Allow-Origin \"http://example.com\"`"
                )));
            }
            let origin: String = serde_json::from_str(parts[2]).map_err(|_| {
                GraphQlError::new(format!(
                    "incorrect format for specifying DataStreamDB.Allow-Origin found for comment: \
                     `{text}`, it should be `# DataStreamDB.Allow-Origin \"http://example.com\"`"
                ))
            })?;
            info.allowed_cors_origins.insert(origin);
            continue;
        }

        if !header.starts_with("DataStreamDB.Secret") {
            continue;
        }
        let parts: Vec<&str> = text.split_whitespace().collect();
        if parts.len() < 4 {
            return Err(GraphQlError::new(format!(
                "incorrect format for specifying DataStreamDB secret found for comment: `{text}`, \
                 it should be `# DataStreamDB.Secret key value`"
            )));
        }
        let val = parts[3..].join(" ");
        if val.chars().filter(|ch| *ch == '"').count() != 2
            || !val.starts_with('"')
            || !val.ends_with('"')
        {
            return Err(GraphQlError::new(format!(
                "incorrect format for specifying DataStreamDB secret found for comment: `{text}`, \
                 it should be `# DataStreamDB.Secret key value`"
            )));
        }
        let val = val.trim_matches('"').to_owned();
        let key = parts[2].trim_matches('"').to_owned();
        info.secrets.insert(key, val);
    }

    if !auth_secret.is_empty() {
        info.auth_meta = auth::parse_auth_meta(&auth_secret)?;
    }
    Ok(info)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_secrets_and_origins() {
        let schema = "# DataStreamDB.Secret api_key \"s3cret\"\n\
             # DataStreamDB.Allow-Origin \"http://example.com\"\n\
             type T { f: String }";
        let meta = parse_meta_info(schema).expect("meta");
        assert_eq!(
            meta.secrets.get("api_key").map(String::as_str),
            Some("s3cret")
        );
        assert!(meta.allowed_cors_origins.contains("http://example.com"));
        assert!(meta.auth_meta.is_none());
    }

    #[test]
    fn rejects_duplicate_authorization() {
        let schema = "# DataStreamDB.Authorization {}\n# DataStreamDB.Authorization {}";
        let err = parse_meta_info(schema).expect_err("must fail");
        assert!(err.message.contains("only be specified once"), "{err}");
    }

    #[test]
    fn rejects_bad_secret_format() {
        let schema = "# DataStreamDB.Secret key value";
        let err = parse_meta_info(schema).expect_err("must fail");
        assert!(err.message.contains("incorrect format"), "{err}");
    }
}
