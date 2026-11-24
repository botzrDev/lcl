//! Similar issues: the open issues that a new issue's title, and its
//! description, match closely. It uses the full-text index that search
//! uses, and no model. `lcl issue create` reports them, so an agent can
//! relate or mark a duplicate instead of filing a second copy, and `lcl
//! issue similar` runs the same check without creating anything.
//!
//! - The words are the title's, split as the index splits them (on
//!   anything but letters and digits) and lowercased. Common English words
//!   and the verbs of a tracker ("fix", "add", "update"; `COMMON`) are
//!   dropped, as is any word in more than a tenth of the workspace's
//!   issues (and in more than 50). A title made only of such words has
//!   nothing to compare, and finds nothing.
//! - A word matches its stem, without a plural or -ed or -ing: as a prefix
//!   when the stem has five letters or more ("crashes" finds "crash",
//!   "crashed" and "crashing"), else as written or as its stem. A word
//!   with a digit in it matches only as written.
//! - An open issue (Backlog, Todo, In Progress or In Review, not in the
//!   trash) is similar when its title and description, together, hold more
//!   than half of the words. The description's words, up to
//!   `DESCRIPTION_WORDS` of them, only rank the issues found.
//! - Best first, by bm25 over the title's and the description's words, a
//!   match in an issue's title counting twice one in its description. At
//!   most `LIMIT`.

use crate::db::{Store, fetch_issues};
use crate::error::Result;
use crate::models::{IssueId, RelatedIssue, Status};
use rusqlite::params;
use serde::Serialize;
use std::collections::{HashMap, HashSet};

/// The most issues a check returns.
pub const LIMIT: usize = 5;

/// The most words a description adds to the ranking.
pub const DESCRIPTION_WORDS: usize = 16;

/// Words too common to tell issues apart: English function words, and the
/// verbs and nouns most issue titles share. Lowercase; a word is checked
/// after it is lowercased. Pieces of contractions ("doesn", "isn") are here
/// because the index splits "doesn't" at the apostrophe.
const COMMON: &[&str] = &[
    "a", "about", "above", "add", "added", "adding", "adds", "after", "again", "against", "all",
    "allow", "allows", "also", "am", "an", "and", "any", "are", "aren", "as", "at", "be",
    "because", "been", "before", "being", "below", "between", "both", "bug", "bugs", "but", "by",
    "can", "cannot", "change", "changed", "changes", "could", "couldn", "did", "didn", "do",
    "does", "doesn", "doing", "don", "down", "during", "each", "etc", "few", "fix", "fixed",
    "fixes", "fixing", "for", "from", "further", "get", "gets", "had", "hadn", "handle", "has",
    "hasn", "have", "haven", "having", "he", "her", "here", "him", "his", "how", "i", "if", "in",
    "into", "is", "isn", "issue", "issues", "it", "its", "itself", "just", "make", "makes", "me",
    "more", "most", "must", "my", "need", "needs", "new", "no", "nor", "not", "now", "of", "off",
    "on", "once", "only", "or", "other", "our", "out", "over", "own", "per", "remove", "removed",
    "same", "set", "she", "should", "shouldn", "so", "some", "such", "support", "supports", "than",
    "that", "the", "their", "them", "then", "there", "these", "they", "this", "those", "through",
    "to", "too", "under", "until", "up", "update", "updated", "updates", "us", "use", "used",
    "uses", "using", "very", "via", "was", "wasn", "we", "were", "weren", "what", "when", "where",
    "which", "while", "who", "why", "will", "with", "won", "work", "working", "works", "would",
    "wouldn", "you", "your",
];

/// What a check found: the title's words it compared (common ones
/// dropped), and the similar open issues, best first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Similar {
    pub words: Vec<String>,
    pub issues: Vec<RelatedIssue>,
}

/// `text`'s words, as the index splits them, lowercased, in order, each
/// once, less those in `COMMON` and those of one character.
pub fn words(text: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    text.split(|c: char| !c.is_alphanumeric())
        .map(str::to_lowercase)
        .filter(|w| w.chars().count() > 1 && !COMMON.contains(&w.as_str()))
        .filter(|w| seen.insert(w.clone()))
        .collect()
}

/// `word` without one ending (-ing, -ed, -es after s, x, z, ch or sh, or
/// -s, not after s), if four letters or more are left; else `word`.
fn stem(word: &str) -> &str {
    let left = |w: &str| w.chars().count() >= 4;
    for end in ["ing", "ed"] {
        if let Some(s) = word.strip_suffix(end).filter(|s| left(s)) {
            return s;
        }
    }
    if let Some(s) = word
        .strip_suffix("es")
        .filter(|s| ["s", "x", "z", "ch", "sh"].iter().any(|e| s.ends_with(e)))
    {
        return if left(s) { s } else { word };
    }
    match word.strip_suffix('s') {
        Some(s) if left(s) && !s.ends_with('s') => s,
        _ => word,
    }
}

/// The FTS5 expression that finds `word`: its stem as a prefix when the
/// stem is five letters or more; else the word, or its stem. A word with a
/// digit is matched as written: "500" must not find "5000".
fn expression(word: &str) -> String {
    let quote = |w: &str| format!("\"{}\"", w.replace('"', "\"\""));
    if !word.chars().all(char::is_alphabetic) {
        return quote(word);
    }
    let s = stem(word);
    if s.chars().count() >= 5 {
        format!("{}*", quote(s))
    } else if s != word {
        format!("({} OR {})", quote(s), quote(word))
    } else {
        quote(word)
    }
}

/// How many of `n` words an issue must hold: more than half.
fn needed(n: usize) -> usize {
    n / 2 + 1
}

impl Store {
    /// The open issues similar to an issue with this `title` and
    /// `description`, best first, at most `LIMIT`, leaving out `exclude`
    /// (the issue just created). It writes nothing.
    pub fn similar_issues(
        &self,
        title: &str,
        description: &str,
        exclude: Option<&IssueId>,
    ) -> Result<Similar> {
        self.read(|conn| {
            let all: i64 = conn.query_row("SELECT count(*) FROM issues", [], |r| r.get(0))?;
            // More than this many issues and a word is too common.
            let common = (all / 10).max(50);
            let mut find =
                conn.prepare("SELECT rowid FROM issues_fts WHERE issues_fts MATCH ?1 LIMIT ?2")?;
            // The issues holding `word`, or None when it is too common.
            let mut holders = |word: &str| -> Result<Option<HashSet<i64>>> {
                let rows: HashSet<i64> = find
                    .query_map(params![expression(word), common + 1], |r| r.get(0))?
                    .collect::<std::result::Result<_, _>>()?;
                Ok((rows.len() as i64 <= common).then_some(rows))
            };

            let mut kept = Vec::new();
            let mut held: HashMap<i64, usize> = HashMap::new();
            for word in words(title) {
                if let Some(rows) = holders(&word)? {
                    for row in rows {
                        *held.entry(row).or_default() += 1;
                    }
                    kept.push(word);
                }
            }
            if kept.is_empty() {
                return Ok(Similar {
                    words: kept,
                    issues: vec![],
                });
            }
            let mut ranked_by = kept.clone();
            for word in words(description) {
                if ranked_by.len() >= kept.len() + DESCRIPTION_WORDS {
                    break;
                }
                if !ranked_by.contains(&word) && holders(&word)?.is_some() {
                    ranked_by.push(word);
                }
            }

            let need = needed(kept.len());
            let close: HashSet<i64> = held
                .into_iter()
                .filter(|(_, n)| *n >= need)
                .map(|(row, _)| row)
                .collect();
            let any = |ws: &[String]| {
                ws.iter()
                    .map(|w| expression(w))
                    .collect::<Vec<_>>()
                    .join(" OR ")
            };
            // Only rows with a title word, ranked by every word.
            let query = format!("({}) AND ({})", any(&kept), any(&ranked_by));
            let open = [
                Status::Backlog,
                Status::Todo,
                Status::InProgress,
                Status::InReview,
            ];
            let mut stmt = conn.prepare(
                "SELECT i.rowid, i.id, i.number, bm25(issues_fts, 2.0, 1.0)
                 FROM issues_fts JOIN issues i ON i.rowid = issues_fts.rowid
                 WHERE issues_fts MATCH ?1 AND i.deleted_at IS NULL
                   AND i.status IN (?2, ?3, ?4, ?5) AND i.id IS NOT ?6",
            )?;
            let mut found = Vec::new();
            let rows = stmt.query_map(
                params![
                    query,
                    open[0].as_str(),
                    open[1].as_str(),
                    open[2].as_str(),
                    open[3].as_str(),
                    exclude.map(IssueId::as_str),
                ],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)?,
                        r.get::<_, f64>(3)?,
                    ))
                },
            )?;
            for row in rows {
                let (rowid, id, number, rank) = row?;
                if close.contains(&rowid) {
                    found.push((rank, number, id));
                }
            }
            // Best first; of two as good, the newer.
            found.sort_by(|a, b| a.0.total_cmp(&b.0).then(b.1.cmp(&a.1)));
            let ids: Vec<String> = found.into_iter().take(LIMIT).map(|(_, _, id)| id).collect();
            // fetch_issues keeps no order: put them back in ours.
            let mut by_id: HashMap<String, RelatedIssue> = fetch_issues(conn, &ids)?
                .into_iter()
                .map(|i| {
                    let brief = RelatedIssue {
                        identifier: i.identifier,
                        title: i.title,
                        status: i.status,
                    };
                    (i.id.as_str().to_string(), brief)
                })
                .collect();
            let issues = ids.iter().filter_map(|id| by_id.remove(id)).collect();
            Ok(Similar {
                words: kept,
                issues,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{NewIssue, Placement};

    #[test]
    fn words_are_split_as_the_index_splits_them_less_common_ones() {
        assert_eq!(
            words("Fix the login-redirect: it doesn't LOOP on Café 2 v1.3"),
            ["login", "redirect", "loop", "café", "v1"]
        );
        assert_eq!(words("Add a new thing to the thing"), ["thing"]);
        assert!(words("Fix the bug in the update").is_empty());
        assert!(words("").is_empty());
    }

    #[test]
    fn a_word_matches_its_stem() {
        let cases = [
            ("crashes", "\"crash\"*"),
            ("crashing", "\"crash\"*"),
            ("crashed", "\"crash\"*"),
            ("redirects", "\"redirect\"*"),
            ("login", "\"login\"*"),
            ("loops", "(\"loop\" OR \"loops\")"),
            ("mode", "\"mode\""),
            ("class", "\"class\"*"),
            ("boxes", "\"boxes\"*"),
            ("ui", "\"ui\""),
            ("alpha7", "\"alpha7\""),
            ("500s", "\"500s\""),
        ];
        for (word, expr) in cases {
            assert_eq!(expression(word), expr, "{word}");
        }
    }

    #[test]
    fn more_than_half_of_the_words() {
        let n: Vec<usize> = (1..=6).map(needed).collect();
        assert_eq!(n, [1, 2, 2, 3, 3, 4]);
    }

    fn store(titles: &[(&str, &str, Status)]) -> Store {
        let mut store = Store::open_in_memory().unwrap();
        store.set_meta(&[("name", "demo"), ("key", "D")]).unwrap();
        for (title, description, status) in titles {
            store
                .create_issue(
                    NewIssue {
                        title: title.to_string(),
                        description: description.to_string(),
                        status: *status,
                        ..NewIssue::default()
                    },
                    &Placement::default(),
                )
                .unwrap();
        }
        store
    }

    fn found(s: &Similar) -> Vec<&str> {
        s.issues.iter().map(|i| i.identifier.as_str()).collect()
    }

    #[test]
    fn open_issues_holding_most_of_the_words_best_first() {
        use Status::*;
        let store = store(&[
            ("Login redirect loops", "", Todo),                     // D-1
            ("Redirect after login is slow", "", Backlog),          // D-2
            ("Login page typo", "", InProgress),                    // D-3
            ("Login redirect loop on Safari", "", Done),            // D-4
            ("Parser crash", "the login form redirects", InReview), // D-5
            ("Login redirect", "", Canceled),                       // D-6
            ("Login redirects twice", "", Duplicate),               // D-7
        ]);
        let s = store
            .similar_issues("The login redirect loops", "", None)
            .unwrap();
        assert_eq!(s.words, ["login", "redirect", "loops"]);
        // D-3 holds one word of three; Done, Canceled and Duplicate are
        // left out.
        assert_eq!(found(&s), ["D-1", "D-2", "D-5"]);
        assert_eq!(
            s.issues[0],
            RelatedIssue {
                identifier: "D-1".into(),
                title: "Login redirect loops".into(),
                status: Todo
            }
        );
        // The issue itself is left out.
        let one = store
            .list_issues(
                &Default::default(),
                Default::default(),
                crate::db::Page::ALL,
            )
            .unwrap();
        let d1 = &one.issues.iter().find(|i| i.number == 1).unwrap().id;
        let s = store
            .similar_issues("Login redirect loops", "", Some(d1))
            .unwrap();
        assert_eq!(found(&s), ["D-2", "D-5"]);
    }

    #[test]
    fn the_description_ranks_but_does_not_find() {
        use Status::*;
        // D-1 and D-2 differ only in their descriptions' words.
        let mut rows = vec![
            ("Export is slow", "Only with a large workspace"), // D-1
            ("Export is slow", "Only on a small laptop"),      // D-2
            ("Installer for Windows", "large workspace"),      // D-3
        ];
        rows.extend([("Chore", ""); 8]);
        let titles: Vec<(&str, &str, Status)> = rows.iter().map(|(t, d)| (*t, *d, Todo)).collect();
        let store = store(&titles);
        // As good as each other: the newer first.
        let plain = store.similar_issues("Slow export", "", None).unwrap();
        assert_eq!(found(&plain), ["D-2", "D-1"]);
        let told = store
            .similar_issues("Slow export", "Seen on a large workspace", None)
            .unwrap();
        assert_eq!(found(&told), ["D-1", "D-2"]);
        // D-3 has the description's words, and none of the title's.
        assert_eq!(told.words, ["slow", "export"]);
    }

    #[test]
    fn a_title_of_common_words_finds_nothing() {
        let store = store(&[
            ("Fix the bug", "", Status::Todo),
            ("Update it", "", Status::Todo),
        ]);
        let s = store
            .similar_issues("Fix the bug in the update", "", None)
            .unwrap();
        assert_eq!(
            s,
            Similar {
                words: vec![],
                issues: vec![]
            }
        );
        // Nothing to search with: an empty title, or only punctuation.
        for title in ["", "--- ?? ---"] {
            assert!(
                store
                    .similar_issues(title, "", None)
                    .unwrap()
                    .issues
                    .is_empty()
            );
        }
    }

    #[test]
    fn a_word_in_more_than_a_tenth_of_the_issues_is_too_common() {
        // 600 issues, 61 of them about the parser: "parser" is in more than
        // 60, a tenth, and is dropped; "tokenizer" is in 2.
        let mut rows: Vec<(String, Status)> = (0..538)
            .map(|n| (format!("Chore {n}"), Status::Todo))
            .collect();
        rows.extend((0..60).map(|n| (format!("Parser case {n}"), Status::Todo)));
        rows.push(("Parser tokenizer hangs".into(), Status::Todo));
        rows.push(("Tokenizer hangs on tabs".into(), Status::Todo));
        let titles: Vec<(&str, &str, Status)> =
            rows.iter().map(|(t, s)| (t.as_str(), "", *s)).collect();
        let store = store(&titles);
        let s = store.similar_issues("Parser tokenizer", "", None).unwrap();
        assert_eq!(s.words, ["tokenizer"]);
        assert_eq!(found(&s), ["D-599", "D-600"]);
        let s = store.similar_issues("Parser", "", None).unwrap();
        assert_eq!(
            s,
            Similar {
                words: vec![],
                issues: vec![]
            }
        );
    }

    #[test]
    fn at_most_five() {
        let rows: Vec<String> = (0..8).map(|n| format!("Sync stalls {n}")).collect();
        let titles: Vec<(&str, &str, Status)> = rows
            .iter()
            .map(|t| (t.as_str(), "", Status::Todo))
            .collect();
        let store = store(&titles);
        let s = store.similar_issues("Sync stalls", "", None).unwrap();
        assert_eq!(found(&s), ["D-8", "D-7", "D-6", "D-5", "D-4"]);
    }
}
