//! Schema text parser. Port of Go's `schema/parse.go`.

#![warn(missing_docs)]

use datastreamdb_lex::{ITEM_EOF, Item, ItemIterator, Lexer, StateFn};
use datastreamdb_proto::{Directive, OptionPair, SchemaUpdate, TypeUpdate, VectorIndexSpec};
use datastreamdb_tok::tok::get_tokenizer;
use datastreamdb_types::TypeId;
use datastreamdb_types::strconv::parse_go_uint_base0;

use super::lexer::{
    ITEM_AT, ITEM_COLON, ITEM_COMMA, ITEM_DOT, ITEM_EXCLAMATION, ITEM_LEFT_CURL, ITEM_LEFT_ROUND,
    ITEM_LEFT_SQUARE, ITEM_NEW_LINE, ITEM_NUMBER, ITEM_QUOTED_TEXT, ITEM_RIGHT_CURL,
    ITEM_RIGHT_ROUND, ITEM_RIGHT_SQUARE, ITEM_TEXT, lex_text,
};
use crate::SchemaError;

/// Parsed schema and type updates.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ParsedSchema {
    /// Predicate updates, in source order.
    pub preds: Vec<SchemaUpdate>,
    /// Type updates, in source order.
    pub types: Vec<TypeUpdate>,
}

/// Schema text type names (exactly Go's `typeNameMap`; no `object`).
fn schema_type(name: &str) -> Option<TypeId> {
    Some(match name {
        "default" => TypeId::Default,
        "binary" => TypeId::Binary,
        "int" => TypeId::Int,
        "float" => TypeId::Float,
        "bool" => TypeId::Bool,
        "datetime" => TypeId::DateTime,
        "geo" => TypeId::Geo,
        "uid" => TypeId::Uid,
        "string" => TypeId::String,
        "password" => TypeId::Password,
        "bigfloat" => TypeId::BigFloat,
        "float32vector" => TypeId::VFloat,
        _ => return None,
    })
}

fn parse_directive(
    it: &mut ItemIterator,
    schema: &mut SchemaUpdate,
    t: TypeId,
) -> Result<(), SchemaError> {
    it.advance();
    let next = it.item();
    if next.typ != ITEM_TEXT {
        return Err(next.errorf("Missing directive name").into());
    }
    match next.val.as_str() {
        "reverse" => {
            if t != TypeId::Uid {
                return Err(next.errorf("Cannot reverse for non-UID type").into());
            }
            schema.directive = Directive::Reverse as i32;
        }
        "index" => {
            let (tokenizer, specs) = parse_index_directive(it, &schema.predicate, t)?;
            schema.directive = Directive::Index as i32;
            schema.tokenizer = tokenizer;
            schema.index_specs = specs;
        }
        "count" => schema.count = true,
        "upsert" => schema.upsert = true,
        "unique" => schema.unique = true,
        "noconflict" => schema.no_conflict = true,
        "lang" => {
            if t != TypeId::String || schema.list {
                return Err(next
                    .errorf(format!(
                        "@lang directive can only be specified for string type. Got: [{t}] for attr: [{}]",
                        schema.predicate
                    ))
                    .into());
            }
            schema.lang = true;
        }
        _ => return Err(next.errorf("Invalid index specification").into()),
    }
    it.advance();
    Ok(())
}

fn parse_scalar_pair(
    it: &mut ItemIterator,
    predicate: &str,
    ns: u64,
) -> Result<SchemaUpdate, SchemaError> {
    it.advance();
    let next = it.item();
    // The '@' check looks redundant but covers both `name@en: string` and
    // `<name@en>: string` without touching the lexer and IRI values.
    if next.typ == ITEM_AT || predicate.contains('@') {
        return Err(next.errorf("Invalid '@' in name").into());
    }
    if next.typ != ITEM_COLON {
        return Err(next.errorf("Missing colon").into());
    }
    if !it.advance() {
        return Err(next
            .errorf("Invalid ending while trying to parse schema.")
            .into());
    }
    let mut next = it.item();
    let mut schema = SchemaUpdate {
        predicate: datastreamdb_x::namespace_attr(ns, predicate),
        ..SchemaUpdate::default()
    };
    // Could be list type.
    if next.typ == ITEM_LEFT_SQUARE {
        schema.list = true;
        if !it.advance() {
            return Err(next
                .errorf("Invalid ending while trying to parse schema.")
                .into());
        }
        next = it.item();
    }
    if next.typ != ITEM_TEXT {
        return Err(next.errorf("Missing Type").into());
    }
    // The case is ignored for types.
    let Some(t) = schema_type(&next.val.to_ascii_lowercase()) else {
        return Err(next.errorf("Undefined Type").into());
    };
    if schema.list && (t == TypeId::Password || t == TypeId::Bool) {
        return Err(next
            .errorf(format!("Unsupported type for list: [{t}]."))
            .into());
    }
    schema.value_type = t as i32;

    // Check for index / reverse.
    it.advance();
    next = it.item();
    if schema.list {
        if next.typ != ITEM_RIGHT_SQUARE {
            return Err(next
                .errorf(format!("Unclosed [ while parsing schema for: {predicate}"))
                .into());
        }
        if !it.advance() {
            return Err(next.errorf("Invalid ending").into());
        }
        next = it.item();
    }

    while next.typ == ITEM_AT {
        parse_directive(it, &mut schema, t)?;
        next = it.item();
    }
    if next.typ != ITEM_DOT {
        return Err(next.errorf("Invalid ending").into());
    }
    it.advance();
    next = it.item();
    if next.typ == ITEM_EOF {
        it.prev();
        return Ok(schema);
    }
    if next.typ != ITEM_NEW_LINE {
        return Err(next.errorf("Invalid ending").into());
    }
    Ok(schema)
}

/// Parses `@index(...)` (tokenizers and vector index specs).
fn parse_index_directive(
    it: &mut ItemIterator,
    predicate: &str,
    typ: TypeId,
) -> Result<(Vec<String>, Vec<VectorIndexSpec>), SchemaError> {
    let mut tokenizers = Vec::new();
    let mut vector_specs = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut seen_sortable = false;

    if typ == TypeId::Uid || typ == TypeId::Default || typ == TypeId::Password {
        return Err(it
            .item()
            .errorf(format!(
                "Indexing not allowed on predicate {predicate} of type {typ}"
            ))
            .into());
    }
    if !it.advance() {
        // Nothing to read.
        return Err(it.item().errorf("Invalid ending.").into());
    }
    let mut next = it.item();
    if next.typ != ITEM_LEFT_ROUND {
        it.prev(); // Backup.
        return Err(it
            .item()
            .errorf(format!(
                "Require type of tokenizer for pred: {predicate} for indexing."
            ))
            .into());
    }

    loop {
        let (token_text, vector_spec, sortable) = parse_token_or_vector_spec(it, predicate, typ)?;
        if sortable && seen_sortable {
            return Err(next
                .errorf(format!(
                    "Only one index tokenizer can be sortable for {predicate}"
                ))
                .into());
        }
        seen_sortable = sortable;
        if token_text.is_empty() {
            // The spec parser returns either text, a spec, or an error.
            let vector_spec = vector_spec.expect("vector spec");
            seen.insert(vector_spec.name.clone());
            vector_specs.push(vector_spec);
        } else if !seen.insert(token_text.clone()) {
            return Err(next
                .errorf(format!(
                    "Duplicate tokenizers defined for predicate {predicate}"
                ))
                .into());
        } else {
            tokenizers.push(token_text);
        }

        it.advance();
        next = it.item();
        if next.typ == ITEM_RIGHT_ROUND {
            break;
        }
        if next.typ != ITEM_COMMA {
            return Err(next
                .errorf(format!(
                    "Expected ',' or ')' but found '{}' for predicate '{predicate}'",
                    next.val
                ))
                .into());
        }
    }
    Ok((tokenizers, vector_specs))
}

/// Parses one tokenizer name or index factory plus options. Returns the
/// tokenizer text (or empty for factories), the vector spec, and sortability.
fn parse_token_or_vector_spec(
    it: &mut ItemIterator,
    predicate: &str,
    typ: TypeId,
) -> Result<(String, Option<VectorIndexSpec>, bool), SchemaError> {
    it.advance();
    let next = it.item();
    if next.typ != ITEM_TEXT {
        return Err(next
            .errorf(format!(
                "Expected token or VectorFactory name, but found '{}'",
                next.val
            ))
            .into());
    }
    let name = next.val.to_ascii_lowercase();
    if let Some(factory) = datastreamdb_tok::index::get_index_factory(&name) {
        // TODO: Consider allowing IndexFactory types not related to
        //       VectorIndex objects.
        if typ != TypeId::VFloat {
            return Err(next
                .errorf(format!(
                    "IndexFactory: {} isn't valid for predicate: {} of type: {typ}",
                    factory.factory_name(),
                    datastreamdb_x::parse_attr(predicate),
                ))
                .into());
        }
        let token_opts = parse_token_options(it, factory.as_ref())?;
        let allowed = factory.allowed_options();
        for pair in &token_opts {
            if let Err(err) = allowed.parsed_option(&pair.key, &pair.value) {
                return Err(next
                    .errorf(format!(
                        "IndexFactory: {} issues this error: '{err}'",
                        factory.factory_name(),
                    ))
                    .into());
            }
        }
        let vs = VectorIndexSpec {
            name: name.clone(),
            options: token_opts,
        };
        return Ok((String::new(), Some(vs), factory.is_sortable()));
    }

    // Look for custom tokenizer, and validate its type.
    let Some(tokenizer) = get_tokenizer(&name) else {
        return Err(next
            .errorf(format!("Invalid tokenizer {}", next.val))
            .into());
    };
    // Type is validated during tokenizer loading.
    let tokenizer_type = schema_type(tokenizer.tokenizer_type()).expect("tokenizer type");
    if tokenizer_type != typ {
        return Err(next
            .errorf(format!(
                "Tokenizer: {} isn't valid for predicate: {} of type: {typ}",
                tokenizer.name(),
                datastreamdb_x::parse_attr(predicate),
            ))
            .into());
    }
    Ok((name, None, tokenizer.is_sortable()))
}

/// Parses `(opt: "val", ...)` after a factory name (options are optional).
fn parse_token_options(
    it: &mut ItemIterator,
    _factory: &dyn datastreamdb_tok::index::IndexFactory,
) -> Result<Vec<OptionPair>, SchemaError> {
    let mut pairs = Vec::new();
    let (next_item, found) = it.peek_one();
    if !found {
        return Err(next_item
            .errorf("unexpected end of stream when looking for IndexFactory options")
            .into());
    }
    if next_item.typ == ITEM_COMMA || next_item.typ == ITEM_RIGHT_ROUND {
        return Ok(Vec::new());
    }
    if next_item.typ != ITEM_LEFT_ROUND {
        return Err(next_item
            .errorf(format!(
                "unexpected '{}' found when expecting '('",
                next_item.val
            ))
            .into());
    }
    it.advance(); // Reads initial '('.
    loop {
        pairs.push(parse_token_option(it)?);
        it.advance();
        let next_item = it.item();
        if next_item.typ == ITEM_RIGHT_ROUND {
            return Ok(pairs);
        }
        if next_item.typ != ITEM_COMMA {
            return Err(next_item
                .errorf(format!(
                    "unexpected '{}' found when expecting ',' or ')'",
                    next_item.val
                ))
                .into());
        }
    }
}

/// Parses one `name: "value"` option pair.
fn parse_token_option(it: &mut ItemIterator) -> Result<OptionPair, SchemaError> {
    it.advance();
    let next_item = it.item();
    if next_item.typ != ITEM_TEXT {
        return Err(next_item
            .errorf(format!(
                "unexpected '{}' found when expecting option name",
                next_item.val
            ))
            .into());
    }
    let key = next_item.val.clone();
    it.advance();
    let next_item = it.item();
    if next_item.typ != ITEM_COLON {
        return Err(next_item
            .errorf(format!(
                "unexpected '{}' found when expecting ':'",
                next_item.val
            ))
            .into());
    }
    it.advance();
    let next_item = it.item();
    if next_item.typ != ITEM_QUOTED_TEXT {
        return Err(next_item
            .errorf(format!(
                "unexpected '{}' found when expecting quoted text",
                next_item.val
            ))
            .into());
    }
    let quoted = next_item.val;
    Ok(OptionPair {
        key,
        value: quoted[1..quoted.len() - 1].to_owned(),
    })
}

/// Whether the update carries tokenizers or vector specs.
#[must_use]
pub fn has_tokenizer_or_vector_spec(update: Option<&SchemaUpdate>) -> bool {
    update.is_some_and(|u| !u.tokenizer.is_empty() || !u.index_specs.is_empty())
}

/// Resolves default tokenizers and verifies tokenizer definitions.
fn resolve_tokenizers(updates: &[SchemaUpdate]) -> Result<(), SchemaError> {
    for schema in updates {
        let typ = TypeId::try_from(schema.value_type)
            .map_err(|_| SchemaError::message(format!("Unknown type id: {}", schema.value_type)))?;

        if (typ == TypeId::Uid || typ == TypeId::Default || typ == TypeId::Password)
            && schema.directive == Directive::Index as i32
        {
            return Err(SchemaError::message(format!(
                "Indexing not allowed on predicate {} of type {typ}",
                datastreamdb_x::parse_attr(&schema.predicate)
            )));
        }
        if typ == TypeId::Uid {
            continue;
        }

        let has = has_tokenizer_or_vector_spec(Some(schema));
        if !has && schema.directive == Directive::Index as i32 {
            return Err(SchemaError::message(format!(
                "Require type of tokenizer for pred: {} of type: {typ} for indexing.",
                schema.predicate
            )));
        }
        if has && schema.directive != Directive::Index as i32 {
            return Err(SchemaError::message(format!(
                "Tokenizers present without indexing on attr {}",
                datastreamdb_x::parse_attr(&schema.predicate)
            )));
        }
        // Check for valid tokenizer types and duplicates.
        let mut seen = std::collections::HashSet::new();
        let mut seen_sortable = false;
        for t in &schema.tokenizer {
            let Some(tokenizer) = get_tokenizer(t) else {
                return Err(SchemaError::message(format!("Invalid tokenizer {t}")));
            };
            // Type is validated during tokenizer loading.
            let tokenizer_type = schema_type(tokenizer.tokenizer_type()).expect("tokenizer type");
            if tokenizer_type != typ {
                return Err(SchemaError::message(format!(
                    "Tokenizer: {} isn't valid for predicate: {} of type: {typ}",
                    tokenizer.name(),
                    datastreamdb_x::parse_attr(&schema.predicate),
                )));
            }
            if !seen.insert(tokenizer.name().to_owned()) {
                return Err(SchemaError::message(format!(
                    "Duplicate tokenizers present for attr {}",
                    datastreamdb_x::parse_attr(&schema.predicate)
                )));
            }
            if tokenizer.is_sortable() {
                if seen_sortable {
                    return Err(SchemaError::message(format!(
                        "More than one sortable index encountered for: {}",
                        schema.predicate
                    )));
                }
                seen_sortable = true;
            }
        }
    }
    Ok(())
}

fn parse_type_declaration(it: &mut ItemIterator, ns: u64) -> Result<TypeUpdate, SchemaError> {
    // The iterator is on the `type` keyword.
    if it.item().typ != ITEM_TEXT || it.item().val != "type" {
        return Err(it
            .item()
            .errorf(format!("Expected type keyword. Got {}", it.item().val))
            .into());
    }
    it.advance();
    if it.item().typ != ITEM_TEXT {
        return Err(it
            .item()
            .errorf(format!("Expected type name. Got {}", it.item().val))
            .into());
    }
    let mut update = TypeUpdate {
        type_name: datastreamdb_x::namespace_attr(ns, &it.item().val),
        ..TypeUpdate::default()
    };
    it.advance();
    if it.item().typ != ITEM_LEFT_CURL {
        return Err(it
            .item()
            .errorf(format!("Expected {{. Got {}", it.item().val))
            .into());
    }

    let mut fields: Vec<SchemaUpdate> = Vec::new();
    while it.advance() {
        let item = it.item();
        match item.typ {
            ITEM_RIGHT_CURL => {
                it.advance();
                if it.item().typ != ITEM_NEW_LINE && it.item().typ != ITEM_EOF {
                    return Err(it
                        .item()
                        .errorf(format!(
                            "Expected new line or EOF after type declaration. Got {}",
                            it.item()
                        ))
                        .into());
                }
                it.prev();

                let mut field_set = std::collections::HashSet::new();
                for field in &fields {
                    if !field_set.insert(field.predicate.clone()) {
                        return Err(it
                            .item()
                            .errorf(format!(
                                "Duplicate fields with name: {}",
                                datastreamdb_x::parse_attr(&field.predicate)
                            ))
                            .into());
                    }
                }
                update.fields = fields;
                return Ok(update);
            }
            ITEM_TEXT => {
                fields.push(parse_type_field(it, &update.type_name, ns)?);
            }
            ITEM_NEW_LINE => {}
            _ => {
                return Err(it
                    .item()
                    .errorf(format!("Unexpected token. Got {}", it.item().val))
                    .into());
            }
        }
    }
    Err(SchemaError::message("Shouldn't reach here."))
}

fn parse_type_field(
    it: &mut ItemIterator,
    type_name: &str,
    ns: u64,
) -> Result<SchemaUpdate, SchemaError> {
    let field = SchemaUpdate {
        predicate: datastreamdb_x::namespace_attr(ns, &it.item().val),
        ..SchemaUpdate::default()
    };
    let mut list = false;
    it.advance();

    // Simplified definitions only require the field name.
    if it.item().typ == ITEM_NEW_LINE {
        return Ok(field);
    }
    // For backwards compatibility, process old-format definitions but ignore
    // the information after the colon.
    if it.item().typ != ITEM_COLON {
        return Err(it
            .item()
            .errorf(format!(
                "Missing colon in type declaration. Got {}",
                it.item().val
            ))
            .into());
    }
    it.advance();
    if it.item().typ == ITEM_LEFT_SQUARE {
        list = true;
        it.advance();
    }
    if it.item().typ != ITEM_TEXT {
        return Err(it
            .item()
            .errorf(format!(
                "Missing field type in type declaration. Got {}",
                it.item().val
            ))
            .into());
    }
    it.advance();
    if it.item().typ == ITEM_EXCLAMATION {
        it.advance();
    }
    if list {
        if it.item().typ != ITEM_RIGHT_SQUARE {
            return Err(it
                .item()
                .errorf(format!(
                    "Expected matching square bracket. Got {}",
                    it.item().val
                ))
                .into());
        }
        it.advance();
        if it.item().typ == ITEM_EXCLAMATION {
            it.advance();
        }
    }
    if it.item().typ != ITEM_NEW_LINE {
        return Err(it
            .item()
            .errorf(format!(
                "Expected new line after field declaration. Got {}",
                it.item().val
            ))
            .into());
    }

    tracing::warn!(
        "Type declaration for type {type_name} includes deprecated information about field type \
         for field {} which will be ignored.",
        datastreamdb_x::parse_attr(&field.predicate)
    );
    Ok(field)
}

fn parse_namespace(it: &mut ItemIterator) -> Result<u64, SchemaError> {
    let (first, second) = {
        let next_items = it
            .peek(2)
            .map_err(|e| SchemaError::message(format!("Unable to peek: {e}")))?;
        (next_items[0].clone(), next_items[1].clone())
    };
    if first.typ != ITEM_NUMBER || second.typ != ITEM_RIGHT_SQUARE {
        return Err(SchemaError::message("Typed oes not match the expected"));
    }
    let ns = parse_go_uint_base0(&first.val).map_err(|e| SchemaError::message(e.to_string()))?;
    it.advance();
    it.advance();
    // The namespace is parsed. Now move to the next item.
    if !it.advance() {
        return Err(SchemaError::message(format!(
            "No schema found after namespace. Got: {first}"
        )));
    }
    Ok(ns)
}

fn is_type_declaration(item: &Item, it: &ItemIterator) -> bool {
    if item.val != "type" {
        return false;
    }
    let Ok(next_items) = it.peek(2) else {
        return false;
    };
    if next_items.len() != 2 {
        return false;
    }
    if next_items[0].typ != ITEM_TEXT {
        return false;
    }
    if next_items[1].typ != ITEM_LEFT_CURL {
        return false;
    }
    true
}

/// Parses schema text. `u64::MAX` preserves namespaces; otherwise every
/// predicate/type is forced into `namespace`.
fn parse_inner(s: &str, namespace: u64) -> Result<ParsedSchema, SchemaError> {
    let mut result = ParsedSchema::default();
    let mut lexer = Lexer::default();
    lexer.reset(s);
    lexer.run(StateFn(lex_text));
    lexer.validate_result()?;

    let mut it = lexer.new_iterator();
    while it.advance() {
        let item = it.item();
        match item.typ {
            ITEM_EOF => {
                resolve_tokenizers(&result.preds)
                    .map_err(|e| SchemaError::message(format!("failed to enrich schema: {e}")))?;
                return Ok(result);
            }
            ITEM_TEXT => {
                // Without namespace info, use root when preserving, else the
                // passed namespace.
                let mut ns = datastreamdb_x::ROOT_NAMESPACE;
                if namespace != u64::MAX {
                    ns = namespace;
                }
                parse_type_or_schema(&item, &mut it, ns, &mut result)?;
            }
            ITEM_LEFT_SQUARE => {
                // We expect a namespace.
                let mut ns = parse_namespace(&mut it)
                    .map_err(|e| SchemaError::message(format!("While parsing namespace:: {e}")))?;
                if namespace != u64::MAX {
                    ns = namespace;
                }
                // parseNamespace already advanced past the namespace.
                let item = it.item();
                parse_type_or_schema(&item, &mut it, ns, &mut result)?;
            }
            ITEM_NEW_LINE => {}
            _ => {
                return Err(it
                    .item()
                    .errorf(format!("Unexpected token: {item} while parsing schema"))
                    .into());
            }
        }
    }
    Err(SchemaError::message("Shouldn't reach here"))
}

fn parse_type_or_schema(
    item: &Item,
    it: &mut ItemIterator,
    ns: u64,
    result: &mut ParsedSchema,
) -> Result<(), SchemaError> {
    if is_type_declaration(item, it) {
        result.types.push(parse_type_declaration(it, ns)?);
        return Ok(());
    }
    result.preds.push(parse_scalar_pair(it, &item.val, ns)?);
    Ok(())
}

/// Parses schema text, preserving namespaces (root namespace by default).
///
/// # Errors
///
/// Returns [`SchemaError`] on malformed schema.
pub fn parse(s: &str) -> Result<ParsedSchema, SchemaError> {
    parse_inner(s, u64::MAX)
}

/// Parses schema text, forcing every predicate/type into `namespace`.
///
/// # Errors
///
/// Returns [`SchemaError`] on malformed schema.
pub fn parse_with_namespace(s: &str, namespace: u64) -> Result<ParsedSchema, SchemaError> {
    parse_inner(s, namespace)
}
