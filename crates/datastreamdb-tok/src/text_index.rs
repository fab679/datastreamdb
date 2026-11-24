//! Full-text search surface over tantivy.
//!
//! Decision (M2): tokenizers stay exact Go ports (index keys must match),
//! while ad-hoc full-text SEARCH runs on tantivy instead of bleve. This
//! module proves the surface with an in-memory index; persistent query
//! integration lands with `datastreamdb-query` (M3).

#![warn(missing_docs)]

use tantivy::collector::TopDocs;
use tantivy::doc;
use tantivy::query::QueryParser;
use tantivy::schema::{Field, STORED, Schema, TEXT};
use tantivy::{Index, IndexReader, IndexWriter, ReloadPolicy};

/// Search error.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TextIndexError {
    /// Backend failure (message only).
    #[error("tantivy: {0}")]
    Backend(String),
}

/// Minimal full-text index: add documents, search terms.
pub trait TextIndex {
    /// Adds a document (visible after [`TextIndex::commit`]).
    ///
    /// # Errors
    ///
    /// Returns [`TextIndexError`] on backend failure.
    fn add(&mut self, uid: u64, text: &str) -> Result<(), TextIndexError>;
    /// Makes added documents searchable.
    ///
    /// # Errors
    ///
    /// Returns [`TextIndexError`] on backend failure.
    fn commit(&mut self) -> Result<(), TextIndexError>;
    /// UIDs matching `term` (up to `limit`).
    ///
    /// # Errors
    ///
    /// Returns [`TextIndexError`] on backend failure.
    fn search(&self, term: &str, limit: usize) -> Result<Vec<u64>, TextIndexError>;
}

/// In-memory tantivy implementation of [`TextIndex`].
pub struct TantivyTextIndex {
    index: Index,
    writer: IndexWriter,
    reader: IndexReader,
    uid_field: Field,
    body_field: Field,
}

impl TantivyTextIndex {
    /// Empty in-memory index.
    ///
    /// # Errors
    ///
    /// Returns [`TextIndexError`] on backend failure.
    pub fn open() -> Result<Self, TextIndexError> {
        let mut schema = Schema::builder();
        let uid_field = schema.add_u64_field("uid", STORED);
        let body_field = schema.add_text_field("body", TEXT | STORED);
        let index = Index::create_in_ram(schema.build());
        let writer = index
            .writer(15_000_000)
            .map_err(|e| TextIndexError::Backend(e.to_string()))?;
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()
            .map_err(|e| TextIndexError::Backend(e.to_string()))?;
        Ok(Self {
            index,
            writer,
            reader,
            uid_field,
            body_field,
        })
    }
}

impl TextIndex for TantivyTextIndex {
    fn add(&mut self, uid: u64, text: &str) -> Result<(), TextIndexError> {
        self.writer
            .add_document(doc!(
                self.uid_field => uid,
                self.body_field => text,
            ))
            .map_err(|e| TextIndexError::Backend(e.to_string()))?;
        Ok(())
    }

    fn commit(&mut self) -> Result<(), TextIndexError> {
        self.writer
            .commit()
            .map_err(|e| TextIndexError::Backend(e.to_string()))?;
        self.reader
            .reload()
            .map_err(|e| TextIndexError::Backend(e.to_string()))?;
        Ok(())
    }

    fn search(&self, term: &str, limit: usize) -> Result<Vec<u64>, TextIndexError> {
        let searcher = self.reader.searcher();
        let parser = QueryParser::for_index(&self.index, vec![self.body_field]);
        let query = parser
            .parse_query(term)
            .map_err(|e| TextIndexError::Backend(e.to_string()))?;
        let top: Vec<(tantivy::Score, tantivy::DocAddress)> = searcher
            .search(&query, &TopDocs::with_limit(limit).order_by_score())
            .map_err(|e| TextIndexError::Backend(e.to_string()))?;
        let mut uids = Vec::with_capacity(top.len());
        for (_, addr) in top {
            let doc: tantivy::TantivyDocument = searcher
                .doc(addr)
                .map_err(|e| TextIndexError::Backend(e.to_string()))?;
            if let Some(value) = doc.get_first(self.uid_field) {
                let owned = tantivy::schema::OwnedValue::from(value);
                if let tantivy::schema::OwnedValue::U64(uid) = owned {
                    uids.push(uid);
                }
            }
        }
        Ok(uids)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indexes_and_searches() {
        let mut index = TantivyTextIndex::open().expect("open");
        index.add(1, "the quick brown fox").expect("add");
        index.add(2, "lazy dog sleeps").expect("add");
        index.commit().expect("commit");
        assert_eq!(index.search("quick", 10).expect("search"), [1]);
        assert_eq!(index.search("dog", 10).expect("search"), [2]);
        assert!(index.search("zebra", 10).expect("search").is_empty());
    }
}
