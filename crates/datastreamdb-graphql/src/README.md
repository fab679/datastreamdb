# datastreamdb-graphql/src

Implementation of `datastreamdb-graphql`. Public API lives in `lib.rs`; one module
per Go source area (split early to stay under 1000 lines per file).

## Schema generation

- `schemagen` — `Handler` entry: parse, validate, complete, DG schema.
- `model` — internal schema model (types, fields, directives).
- `directives` — directive definition tables and arg specs.
- `meta` — schema meta: secrets, CORS, auth-meta parsing.
- `rules_pre`, `rules_post`, `rules_schema` — pre/post/structural validation.
- `rules_directive`, `rules_custom`, `rules_apollo` — per-directive validators.
- `rules_util` — shared validation helpers; `errors` — `ErrorList`.
- `dgschema` — `genDgSchema` (byte-exact DataStreamDB schema output).
- `complete`, `complete_ops` — filter/order/input/payload/query/mutation completion.
- `sdl` — completed-schema SDL rendering (`Stringify`).

## Auth

- `auth` — `AuthMeta` parsing, JWT HS/RS verification, claim extraction.
- `auth_rules` — `@auth` RBAC parsing and static evaluation.

## Resolvers

- `op` — bound operation model (fragments, variables, directives).
- `dql_render` — DQL text renderer for rewritten queries.
- `rewrite_vars`, `rewrite_filter`, `rewrite_auth` — variable/filter/auth fragments.
- `rewrite_select`, `rewrite_query`, `rewrite_object` — query rewriting.
- `rewrite_existence`, `rewrite_refs` — existence checks and reference handling.
- `rewrite_mutation` — add/update/delete rewriters + RDF lowering.
- `resolve`, `resolve_schema`, `resolve_mutation` — async-graphql dynamic
  execution over `datastreamdb-query` with normalization and geo conversion.

## Subscriptions

- `subscription` — poll-based hub (initial + change-only stream, epoch bump).
