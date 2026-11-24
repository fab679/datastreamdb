//! GraphQL layer: schema-gen, auth, resolve, subscribe.
//!
//! Rust port of the upstream Go `graphql` tree on top of `async-graphql`. The scaffold
//! proves the execution pipeline with a health schema; `DataStreamDB` schema-gen, auth,
//! resolvers, and subscriptions follow per `TODO.md`.

#![warn(missing_docs)]

use async_graphql::{EmptyMutation, EmptySubscription, Object, Schema};

pub mod admin;
pub mod auth;
pub mod auth_rules;
pub mod complete;
pub mod complete_ops;
pub mod dgschema;
pub mod directives;
pub mod dql_render;
pub mod errors;
pub mod meta;
pub mod model;
pub mod op;
pub mod resolve;
pub mod resolve_mutation;
pub mod resolve_schema;
pub mod rewrite_auth;
pub mod rewrite_existence;
pub mod rewrite_filter;
pub mod rewrite_mutation;
pub mod rewrite_object;
pub mod rewrite_query;
pub mod rewrite_refs;
pub mod rewrite_select;
pub mod rewrite_vars;
pub mod rules_apollo;
pub mod rules_custom;
pub mod rules_directive;
pub mod rules_post;
pub mod rules_pre;
pub mod rules_schema;
pub mod rules_util;
pub mod schemagen;
pub mod sdl;
pub mod subscription;

/// Root query object (scaffold: health only).
#[derive(Debug, Default)]
pub struct QueryRoot;

#[Object]
impl QueryRoot {
    /// Liveness probe; full query surface lands with schema-gen (M4).
    async fn health(&self) -> &str {
        "ok"
    }

    /// Workspace version, mirroring the admin `version` field shape.
    async fn version(&self) -> &str {
        env!("CARGO_PKG_VERSION")
    }
}

/// Executable schema type used by servers and embedded hosts.
pub type DataStreamDBSchema = Schema<QueryRoot, EmptyMutation, EmptySubscription>;

/// Builds a fresh executable schema.
#[must_use]
pub fn schema() -> DataStreamDBSchema {
    Schema::build(QueryRoot, EmptyMutation, EmptySubscription).finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn health_query_executes() {
        let response = schema().execute("{ health version }").await;
        assert!(response.errors.is_empty(), "{:?}", response.errors);
        let data = response.data.into_json().expect("json");
        assert_eq!(data["health"], "ok");
        assert_eq!(data["version"], env!("CARGO_PKG_VERSION"));
    }
}
