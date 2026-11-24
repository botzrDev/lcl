//! The rules for values people and files give lcl: one place, so the command
//! line and `import` accept exactly the same things. Each function returns
//! the value as lcl stores it, or an `InvalidValue` whose message is the
//! whole explanation.
//!
//! The text rules trim first, as the command line always has. `import`
//! stores what a file says without rewriting it, so it also refuses a value
//! these would change (see `transfer`).

use crate::models::InvalidValue;
use chrono::NaiveDate;

fn invalid(msg: impl Into<String>) -> InvalidValue {
    InvalidValue(msg.into())
}

/// A real calendar date written exactly as YYYY-MM-DD: 2026-02-30 and
/// 2026-2-3 are both refused.
pub fn date(s: &str) -> Result<NaiveDate, InvalidValue> {
    let b = s.as_bytes();
    let shaped = b.len() == 10
        && b.iter().enumerate().all(|(i, c)| {
            if i == 4 || i == 7 {
                *c == b'-'
            } else {
                c.is_ascii_digit()
            }
        });
    shaped
        .then(|| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
        .flatten()
        .ok_or_else(|| {
            invalid(format!(
                "invalid date \"{s}\"; expected a real calendar date as YYYY-MM-DD"
            ))
        })
}

/// `#rgb` or `#rrggbb`.
pub fn color(s: &str) -> Result<String, InvalidValue> {
    let hex = s.strip_prefix('#').unwrap_or("");
    if s.starts_with('#')
        && matches!(hex.len(), 3 | 6)
        && hex.chars().all(|c| c.is_ascii_hexdigit())
    {
        Ok(s.to_string())
    } else {
        Err(invalid(format!(
            "invalid color \"{s}\"; expected #rgb or #rrggbb"
        )))
    }
}

/// Trimmed; blank is refused with `blank`.
fn non_blank(s: &str, blank: &str) -> Result<String, InvalidValue> {
    let s = s.trim();
    if s.is_empty() {
        Err(invalid(blank))
    } else {
        Ok(s.to_string())
    }
}

/// An issue's or a doc's title, as given: it isn't trimmed, but one that is
/// empty or all whitespace is refused. The command line's rule only:
/// `import` and `export` take a blank title, because 0.1.0 stored them and
/// an upgraded workspace must still export.
pub fn title(s: &str) -> Result<String, InvalidValue> {
    if s.trim().is_empty() {
        Err(invalid("a title can't be blank"))
    } else {
        Ok(s.to_string())
    }
}

/// A comment's body, as given: it isn't trimmed, but one that is empty or
/// all whitespace is refused.
pub fn comment(s: &str) -> Result<String, InvalidValue> {
    if s.trim().is_empty() {
        Err(invalid("a comment can't be blank"))
    } else {
        Ok(s.to_string())
    }
}

/// A label name, trimmed; blank is refused.
pub fn label_name(s: &str) -> Result<String, InvalidValue> {
    non_blank(s, "a label name can't be empty")
}

/// A project name or reference, trimmed; blank is refused.
pub fn project_name(s: &str) -> Result<String, InvalidValue> {
    non_blank(s, "a project name can't be empty")
}

/// A milestone name or reference, trimmed; blank is refused.
pub fn milestone_name(s: &str) -> Result<String, InvalidValue> {
    non_blank(s, "a milestone name can't be empty")
}

/// A cycle name or reference, trimmed; blank is refused.
pub fn cycle_name(s: &str) -> Result<String, InvalidValue> {
    non_blank(s, "a cycle name can't be empty")
}

/// An assignee, trimmed; blank is refused. Unassigning is its own flag.
pub fn assignee(s: &str) -> Result<String, InvalidValue> {
    non_blank(
        s,
        "an assignee can't be empty; to unassign, use `issue update --no-assignee`",
    )
}

/// An issue's creator or a comment's author, trimmed; blank is refused
/// (leaving it out means LCL_AUTHOR, else USER, on the command line).
pub fn author(s: &str) -> Result<String, InvalidValue> {
    non_blank(
        s,
        "an author can't be empty; leave --author out to use LCL_AUTHOR or USER",
    )
}

/// An issue's ref: trimmed; at most 100 characters, none of them whitespace
/// or a control character. Blank is refused. Whether it is free, and not
/// shaped like this workspace's own references, is checked against the
/// workspace when it is written.
pub fn issue_ref(s: &str) -> Result<String, InvalidValue> {
    let s = s.trim();
    let chars = s.chars().count();
    if s.is_empty() {
        Err(invalid(
            "a ref can't be empty; to remove one, use issue update --no-ref",
        ))
    } else if chars > 100 {
        Err(invalid(format!(
            "a ref can be at most 100 characters; this one has {chars}"
        )))
    } else if s.chars().any(|c| c.is_whitespace() || c.is_control()) {
        Err(invalid(format!(
            "ref {s:?} contains whitespace or a control character"
        )))
    } else {
        Ok(s.to_string())
    }
}

/// An estimate: zero or more.
pub fn estimate(n: i64) -> Result<i64, InvalidValue> {
    if n < 0 {
        Err(invalid(format!("an estimate can't be negative; got {n}")))
    } else {
        Ok(n)
    }
}

/// A UUID as lcl reads one (36 characters, hyphenated, either case), in the
/// lowercase form lcl stores.
pub fn uuid(s: &str) -> Result<String, InvalidValue> {
    (s.len() == 36)
        .then(|| uuid::Uuid::parse_str(s).ok())
        .flatten()
        .map(|u| u.hyphenated().to_string())
        .ok_or_else(|| invalid(format!("invalid id \"{s}\"; expected a UUID")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_must_be_real_and_exactly_shaped() {
        assert!(date("2026-02-28").is_ok());
        for bad in [
            "2026-02-30",
            "2026-2-3",
            "tomorrow",
            "2026-13-01",
            " 2026-02-28",
            "",
        ] {
            assert!(date(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn uuids_are_lowercased() {
        assert_eq!(
            uuid("0F8FAD5B-D9CB-469F-A165-70867728950E").unwrap(),
            "0f8fad5b-d9cb-469f-a165-70867728950e"
        );
        for bad in [
            "0f8fad5bd9cb469fa16570867728950e",
            "{0f8fad5b-d9cb-469f-a165-70867728950e}",
            "nope",
            "",
        ] {
            assert!(uuid(bad).is_err(), "{bad:?}");
        }
    }
}
