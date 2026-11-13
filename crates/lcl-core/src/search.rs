//! Search: what a user typed, made safe for FTS5, and the shape of the
//! results. `Store::search` runs it.
//!
//! The query is split on whitespace and every word must match. Each word is
//! literal: it becomes an FTS5 string, so `AND`, `OR`, `NOT`, `NEAR`, quotes,
//! parentheses, `:` `^` `+` `-` and a lone `*` are plain text, never query
//! syntax. The unicode61 tokenizer then splits a word like `BZR-1532` into
//! the phrase `bzr 1532`, which finds `BZR-1532` as written. A word ending in
//! `*` (after at least one other character) matches as a prefix.

use crate::error::LclError;
use crate::models::{Doc, Issue, IssueRef};
use serde::Serialize;

/// A search query, parsed. Holds at least one word with something to search
/// for, so every query it builds is valid FTS5.
#[derive(Debug, Clone)]
pub struct SearchQuery {
    raw: String,
    /// One FTS5 string per word that has token characters.
    phrases: Vec<String>,
    /// The issue the whole query names, when it is exactly one `KEY-N` or
    /// `#N`. Whether it is this workspace's is decided at search time.
    identifier: Option<IssueRef>,
}

impl SearchQuery {
    /// Refuses a query that is empty or only whitespace, and one whose words
    /// all lack token characters (`--`, `*`, a lone `"`): FTS5 would find
    /// nothing in them. Words without token characters are dropped from a
    /// query that has others.
    pub fn parse(q: &str) -> Result<Self, LclError> {
        let words: Vec<&str> = q.split_whitespace().collect();
        if words.is_empty() {
            return Err(LclError::EmptyQuery);
        }
        let phrases: Vec<String> = words.iter().filter(|w| has_token_chars(w)).map(|w| phrase(w)).collect();
        if phrases.is_empty() {
            return Err(LclError::NothingToSearch(q.to_string()));
        }
        let identifier = match words.as_slice() {
            [w] => match w.parse::<IssueRef>() {
                Ok(r @ IssueRef::Number { key: Some(_), .. }) => Some(r),
                // `N` parses as a number too, but a bare number is just text.
                Ok(r @ IssueRef::Number { key: None, .. }) if w.starts_with('#') => Some(r),
                _ => None,
            },
            _ => None,
        };
        Ok(Self { raw: q.to_string(), phrases, identifier })
    }

    /// The query as typed.
    pub fn raw(&self) -> &str {
        &self.raw
    }

    /// Every word, ANDed: the MATCH expression for a row.
    pub(crate) fn all_words(&self) -> String {
        self.phrases.join(" ")
    }

    /// Any word, in column `col` only: which columns of a matched row hold
    /// a word of the query.
    pub(crate) fn any_word_in(&self, col: &str) -> String {
        format!("{{{col}}} : ({})", self.phrases.join(" OR "))
    }

    pub(crate) fn identifier(&self) -> Option<&IssueRef> {
        self.identifier.as_ref()
    }
}

/// `w` as an FTS5 string, inner `"` doubled. A trailing `*` after at least
/// one other character makes it a prefix: `"writ"*`.
fn phrase(w: &str) -> String {
    match w.strip_suffix('*') {
        Some(stem) if !stem.is_empty() => format!("\"{}\"*", stem.replace('"', "\"\"")),
        _ => format!("\"{}\"", w.replace('"', "\"\"")),
    }
}

/// Whether unicode61 (default options) finds a token in `w`: it treats
/// Unicode letters (L*), numbers (N*) and private-use characters (Co) as
/// token characters, everything else as separators. `is_alphanumeric` is
/// L* + N* plus a few marks; where the two disagree the word is searched
/// and finds nothing, which is harmless.
fn has_token_chars(w: &str) -> bool {
    w.chars().any(|c| c.is_alphanumeric() || matches!(c, '\u{E000}'..='\u{F8FF}' | '\u{F0000}'..='\u{FFFFD}' | '\u{100000}'..='\u{10FFFD}'))
}

/// Where an issue matched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Field {
    /// The query is the issue's `KEY-N` or `#N`.
    Identifier,
    Title,
    Description,
    /// Every word is in one of its comments.
    Comments,
    /// The query is the issue's ref, ignoring ASCII case.
    Ref,
}

impl Field {
    pub fn as_str(self) -> &'static str {
        match self {
            Field::Identifier => "identifier",
            Field::Title => "title",
            Field::Description => "description",
            Field::Comments => "comments",
            Field::Ref => "ref",
        }
    }
}

/// An issue found by a search, and where.
#[derive(Debug, Clone, Serialize)]
pub struct IssueHit {
    #[serde(flatten)]
    pub issue: Issue,
    pub matched_in: Vec<Field>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchResults {
    pub query: String,
    /// Best first: the issue the query names, then by bm25 (the best of the
    /// issue's own match and its comments' matches), then newest.
    pub issues: Vec<IssueHit>,
    pub docs: Vec<Doc>,
    pub issue_total: usize,
    pub doc_total: usize,
    /// Applies to each list.
    pub limit: usize,
    /// Whether either list was cut to `limit`.
    pub truncated: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_become_literal_strings() {
        let q = |s: &str| SearchQuery::parse(s).map(|q| q.all_words());
        assert_eq!(q("BZR-1532").unwrap(), r#""BZR-1532""#);
        assert_eq!(q(r#"say "hi"#).unwrap(), r#""say" """hi""#);
        assert_eq!(q("AND OR NOT NEAR").unwrap(), r#""AND" "OR" "NOT" "NEAR""#);
        assert_eq!(q("writ* x").unwrap(), r#""writ"* "x""#);
        assert_eq!(q("(x, x) a:b ^x +x -x").unwrap(), r#""(x," "x)" "a:b" "^x" "+x" "-x""#);
        assert_eq!(q("foo -- *").unwrap(), r#""foo""#, "words with nothing to search for are dropped");
        assert!(matches!(SearchQuery::parse(""), Err(LclError::EmptyQuery)));
        assert!(matches!(SearchQuery::parse(" \t\n"), Err(LclError::EmptyQuery)));
        for nothing in ["--", "*", "\"", "-- * \"\""] {
            assert!(matches!(SearchQuery::parse(nothing), Err(LclError::NothingToSearch(_))), "{nothing}");
        }
    }

    #[test]
    fn only_key_n_and_hash_n_name_an_issue() {
        let id = |s: &str| SearchQuery::parse(s).unwrap().identifier;
        assert!(matches!(id("DREA-3"), Some(IssueRef::Number { key: Some(_), number: 3 })));
        assert!(matches!(id("#3"), Some(IssueRef::Number { key: None, number: 3 })));
        for text in ["3", "DREA-3 x", "0f8fad5b-d9cb-469f-a165-70867728950e", "DREA-", "writ*"] {
            assert!(id(text).is_none(), "{text}");
        }
    }
}
