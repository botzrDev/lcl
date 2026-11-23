//! A workspace as a text file, and back: `Store::export` and `Store::import`.
//!
//! The format, "lcl-export/1", is JSON Lines: one record per line, each an
//! object with a "type". The workspace header is always the first line;
//! then labels, projects, milestones, cycles, issues, comments, relations
//! and docs. Export writes every key of a record, in a fixed order, null
//! when absent, and the same workspace always exports to the same bytes.
//!
//! The file carries no UUIDs for issues, labels, projects, milestones or
//! comments: they are named by number or name, and import makes new ids.
//! Cycles and docs have no unique name, so they keep their ids.
//!
//! Import restores what the file says exactly: numbers, refs, creators,
//! authors and timestamps are stored as given, and no lifecycle rule runs.
//! Every value passes the rules the command line applies (`crate::parse`),
//! and a text value those rules would trim is refused rather than rewritten.
//! Everything `relate`, parents and the rest refuse is refused too. The
//! whole file is read and checked before anything is written, and the
//! writes are one transaction: all of the file, or none of it.

use crate::db::{
    NEXT_ISSUE_NUMBER, Store, next_issue_number, put_meta, read_key, read_meta, ref_looks_own,
};
use crate::error::{LclError, Result, chain};
use crate::models::{InvalidValue, Priority, ProjectStatus, Status, identifier, now};
use crate::parse;
use rusqlite::{Connection, params};
use serde::Serialize;
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet, VecDeque};

/// The format this lcl writes and reads.
pub const FORMAT: &str = "lcl-export/1";

/// How many of each thing a file holds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Counts {
    pub issues: usize,
    pub comments: usize,
    pub relations: usize,
    pub labels: usize,
    pub projects: usize,
    pub milestones: usize,
    pub cycles: usize,
    pub docs: usize,
}

impl Counts {
    /// "540 issues, 1210 comments, …, 1 doc".
    pub fn phrase(&self) -> String {
        let n = |count: usize, noun: &str| {
            format!("{count} {noun}{}", if count == 1 { "" } else { "s" })
        };
        [
            n(self.issues, "issue"),
            n(self.comments, "comment"),
            n(self.relations, "relation"),
            n(self.labels, "label"),
            n(self.projects, "project"),
            n(self.milestones, "milestone"),
            n(self.cycles, "cycle"),
            n(self.docs, "doc"),
        ]
        .join(", ")
    }
}

/// An exported workspace: the file's text, and what it holds.
pub struct Export {
    pub text: String,
    pub counts: Counts,
}

/// What an import wrote (or, with `dry_run`, would have written).
#[derive(Debug, Clone, Serialize)]
pub struct ImportSummary {
    #[serde(flatten)]
    pub counts: Counts,
    /// The number the next created issue gets.
    pub next_issue_number: i64,
    pub dry_run: bool,
    /// The workspace's name and key, for the message.
    #[serde(skip)]
    pub workspace: String,
    #[serde(skip)]
    pub key: String,
}

/// The three relation rows lcl stores. `blocks`: from blocks to.
/// `duplicate`: from is a duplicate of to. `related` goes both ways.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Kind {
    Blocks,
    Duplicate,
    Related,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Kind::Blocks => "blocks",
            Kind::Duplicate => "duplicate",
            Kind::Related => "related",
        }
    }
    fn parse(s: &str) -> Option<Kind> {
        match s {
            "blocks" => Some(Kind::Blocks),
            "duplicate" => Some(Kind::Duplicate),
            "related" => Some(Kind::Related),
            _ => None,
        }
    }
}

// Export writes these, field by field in this order.

#[derive(Serialize)]
struct WorkspaceLine<'a> {
    r#type: &'static str,
    format: &'static str,
    name: &'a str,
    key: &'a str,
    description: &'a str,
    created_at: i64,
    next_issue_number: i64,
}

#[derive(Serialize)]
struct LabelLine {
    r#type: &'static str,
    name: String,
    color: String,
}

#[derive(Serialize)]
struct ProjectLine {
    r#type: &'static str,
    name: String,
    description: String,
    status: &'static str,
    target_date: Option<String>,
    created_at: i64,
    updated_at: i64,
}

#[derive(Serialize)]
struct MilestoneLine {
    r#type: &'static str,
    project: String,
    name: String,
    description: String,
    target_date: Option<String>,
    sort_order: i64,
    created_at: i64,
    updated_at: i64,
}

#[derive(Serialize)]
struct CycleLine {
    r#type: &'static str,
    id: String,
    name: String,
    starts_at: Option<String>,
    ends_at: Option<String>,
    created_at: i64,
}

#[derive(Serialize)]
struct IssueLine {
    r#type: &'static str,
    number: i64,
    title: String,
    description: String,
    status: &'static str,
    priority: &'static str,
    assignee: Option<String>,
    creator: Option<String>,
    r#ref: Option<String>,
    parent: Option<i64>,
    project: Option<String>,
    milestone: Option<String>,
    labels: Vec<String>,
    cycles: Vec<String>,
    estimate: Option<i64>,
    due_date: Option<String>,
    created_at: i64,
    updated_at: i64,
    started_at: Option<i64>,
    completed_at: Option<i64>,
    canceled_at: Option<i64>,
}

#[derive(Serialize)]
struct CommentLine {
    r#type: &'static str,
    issue: i64,
    body: String,
    author: Option<String>,
    created_at: i64,
}

#[derive(Serialize)]
struct RelationLine {
    r#type: &'static str,
    kind: &'static str,
    from: i64,
    to: i64,
}

#[derive(Serialize)]
struct DocLine {
    r#type: &'static str,
    id: String,
    title: String,
    content: String,
    parent: Option<String>,
    created_at: i64,
    updated_at: i64,
}

// The keys each record type has, in the order export writes them.
const WORKSPACE_KEYS: &[&str] = &[
    "type",
    "format",
    "name",
    "key",
    "description",
    "created_at",
    "next_issue_number",
];
const LABEL_KEYS: &[&str] = &["type", "name", "color"];
const PROJECT_KEYS: &[&str] = &[
    "type",
    "name",
    "description",
    "status",
    "target_date",
    "created_at",
    "updated_at",
];
const MILESTONE_KEYS: &[&str] = &[
    "type",
    "project",
    "name",
    "description",
    "target_date",
    "sort_order",
    "created_at",
    "updated_at",
];
const CYCLE_KEYS: &[&str] = &["type", "id", "name", "starts_at", "ends_at", "created_at"];
const ISSUE_KEYS: &[&str] = &[
    "type",
    "number",
    "title",
    "description",
    "status",
    "priority",
    "assignee",
    "creator",
    "ref",
    "parent",
    "project",
    "milestone",
    "labels",
    "cycles",
    "estimate",
    "due_date",
    "created_at",
    "updated_at",
    "started_at",
    "completed_at",
    "canceled_at",
];
const COMMENT_KEYS: &[&str] = &["type", "issue", "body", "author", "created_at"];
const RELATION_KEYS: &[&str] = &["type", "kind", "from", "to"];
const DOC_KEYS: &[&str] = &[
    "type",
    "id",
    "title",
    "content",
    "parent",
    "created_at",
    "updated_at",
];
const TYPES: &[&str] = &[
    "workspace",
    "label",
    "project",
    "milestone",
    "cycle",
    "issue",
    "comment",
    "relation",
    "doc",
];

// What import reads, checked value by value.

struct Header {
    key: String,
    description: Option<String>,
    created_at: Option<i64>,
    next_issue_number: Option<i64>,
}

struct LabelRec {
    line: usize,
    name: String,
    color: String,
}

struct ProjectRec {
    line: usize,
    name: String,
    description: String,
    status: ProjectStatus,
    target_date: Option<String>,
    created_at: i64,
    updated_at: i64,
}

struct MilestoneRec {
    line: usize,
    project: String,
    name: String,
    description: String,
    target_date: Option<String>,
    sort_order: i64,
    created_at: i64,
    updated_at: i64,
}

struct CycleRec {
    line: usize,
    id: Option<String>,
    name: String,
    starts_at: Option<String>,
    ends_at: Option<String>,
    created_at: i64,
}

struct IssueRec {
    line: usize,
    number: i64,
    title: String,
    description: String,
    status: Status,
    priority: Priority,
    assignee: Option<String>,
    creator: Option<String>,
    r#ref: Option<String>,
    parent: Option<i64>,
    project: Option<String>,
    milestone: Option<String>,
    labels: Vec<String>,
    cycles: Vec<String>,
    estimate: Option<i64>,
    due_date: Option<String>,
    created_at: i64,
    updated_at: i64,
    started_at: Option<i64>,
    completed_at: Option<i64>,
    canceled_at: Option<i64>,
}

struct CommentRec {
    issue: i64,
    body: String,
    author: Option<String>,
    created_at: i64,
}

struct RelationRec {
    line: usize,
    kind: Kind,
    from: i64,
    to: i64,
}

struct DocRec {
    line: usize,
    id: Option<String>,
    title: String,
    content: String,
    parent: Option<String>,
    created_at: i64,
    updated_at: i64,
}

/// A whole file, every value and reference checked.
struct File {
    header: Header,
    labels: Vec<LabelRec>,
    projects: Vec<ProjectRec>,
    milestones: Vec<MilestoneRec>,
    cycles: Vec<CycleRec>,
    issues: Vec<IssueRec>,
    comments: Vec<CommentRec>,
    relations: Vec<RelationRec>,
    docs: Vec<DocRec>,
}

impl File {
    fn counts(&self) -> Counts {
        Counts {
            issues: self.issues.len(),
            comments: self.comments.len(),
            relations: self.relations.len(),
            labels: self.labels.len(),
            projects: self.projects.len(),
            milestones: self.milestones.len(),
            cycles: self.cycles.len(),
            docs: self.docs.len(),
        }
    }
}

fn bad(line: usize, what: impl Into<String>, msg: impl Into<String>) -> LclError {
    LclError::ImportLine {
        line,
        what: what.into(),
        msg: msg.into(),
    }
}

/// "an" before "issue", "a" before the other record types.
fn article(kind: &str) -> &'static str {
    if kind.starts_with(['a', 'e', 'i', 'o', 'u']) {
        "an"
    } else {
        "a"
    }
}

/// `name` is already on line `at`: the message for a second label, project
/// or milestone whose name matches one before it, ignoring case.
fn taken(thing: &str, name: &str, existing: &str, at: usize, scope: &str) -> String {
    let as_ = if name == existing {
        String::new()
    } else {
        format!(" as {existing:?}")
    };
    format!(
        "{thing} {name:?} is already on line {at}{as_}; {thing} names are unique{scope}, ignoring case"
    )
}

fn kind_of(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "true or false",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "a list",
        Value::Object(_) => "an object",
    }
}

/// One record: its line, its type, and the fields not yet read. Each getter
/// takes its field out, so what is left at the end is unknown keys.
struct Rec {
    line: usize,
    kind: &'static str,
    map: Map<String, Value>,
}

impl Rec {
    fn bad(&self, what: &str, msg: impl Into<String>) -> LclError {
        bad(self.line, what, msg)
    }

    /// Refuses any key this record type doesn't have: a converter's typo
    /// must fail, not be dropped.
    fn known(&self, keys: &[&str]) -> Result<()> {
        match self.map.keys().find(|k| !keys.contains(&k.as_str())) {
            Some(k) => Err(self.bad(
                self.kind,
                format!(
                    "unknown key {k:?}; {} {} record has: {}",
                    article(self.kind),
                    self.kind,
                    keys.join(", ")
                ),
            )),
            None => Ok(()),
        }
    }

    /// The field, which must be there and not null.
    fn required(&mut self, key: &str) -> Result<Value> {
        match self.map.remove(key) {
            None => Err(self.bad(
                key,
                format!("missing; every {} record needs one", self.kind),
            )),
            Some(Value::Null) => Err(self.bad(key, "can't be null")),
            Some(v) => Ok(v),
        }
    }

    /// The field, or None when it is left out. It can't be null: a null
    /// there is likelier a converter's mistake than a request for the
    /// default (`default`, named in the message).
    fn defaulted(&mut self, key: &str, default: &str) -> Result<Option<Value>> {
        match self.map.remove(key) {
            Some(Value::Null) => Err(self.bad(
                key,
                format!("can't be null; leave the key out for the default ({default})"),
            )),
            v => Ok(v),
        }
    }

    /// The field, or None when it is null or left out.
    fn nullable(&mut self, key: &str) -> Option<Value> {
        self.map.remove(key).filter(|v| !v.is_null())
    }

    fn string(&self, key: &str, v: Value) -> Result<String> {
        match v {
            Value::String(s) => Ok(s),
            v => Err(self.bad(key, format!("expected a string, got {}", kind_of(&v)))),
        }
    }

    fn int(&self, key: &str, v: Value) -> Result<i64> {
        match &v {
            Value::Number(n) => n.as_i64().ok_or_else(|| {
                self.bad(
                    key,
                    format!("expected a whole number that fits in 64 bits, got {n}"),
                )
            }),
            v => Err(self.bad(key, format!("expected a whole number, got {}", kind_of(v)))),
        }
    }

    fn strings(&self, key: &str, v: Value) -> Result<Vec<String>> {
        let Value::Array(items) = v else {
            return Err(self.bad(
                key,
                format!("expected a list of strings, got {}", kind_of(&v)),
            ));
        };
        items
            .into_iter()
            .map(|item| match item {
                Value::String(s) => Ok(s),
                other => Err(self.bad(
                    key,
                    format!(
                        "expected a list of strings, but one item is {}",
                        kind_of(&other)
                    ),
                )),
            })
            .collect()
    }

    fn req_str(&mut self, key: &str) -> Result<String> {
        let v = self.required(key)?;
        self.string(key, v)
    }
    fn req_int(&mut self, key: &str) -> Result<i64> {
        let v = self.required(key)?;
        self.int(key, v)
    }
    fn def_str(&mut self, key: &str, default: &str) -> Result<String> {
        match self.defaulted(key, default)? {
            Some(v) => self.string(key, v),
            None => Ok(default.to_string()),
        }
    }
    fn def_int(&mut self, key: &str, default: i64, named: &str) -> Result<i64> {
        match self.defaulted(key, named)? {
            Some(v) => self.int(key, v),
            None => Ok(default),
        }
    }
    fn opt_str(&mut self, key: &str) -> Result<Option<String>> {
        self.nullable(key).map(|v| self.string(key, v)).transpose()
    }
    fn opt_int(&mut self, key: &str) -> Result<Option<i64>> {
        self.nullable(key).map(|v| self.int(key, v)).transpose()
    }
    fn def_strs(&mut self, key: &str) -> Result<Vec<String>> {
        match self.defaulted(key, "[]")? {
            Some(v) => self.strings(key, v),
            None => Ok(vec![]),
        }
    }

    /// A value checked by `rule`, which must not change it: the rules trim
    /// (as the command line does), and import stores the file as written.
    fn exact(
        &self,
        key: &str,
        s: String,
        rule: fn(&str) -> std::result::Result<String, InvalidValue>,
    ) -> Result<String> {
        match rule(&s) {
            Err(e) => Err(self.bad(key, e.0)),
            Ok(t) if t != s => Err(self.bad(
                key,
                format!("{s:?} has leading or trailing whitespace; lcl stores it as {t:?}"),
            )),
            Ok(t) => Ok(t),
        }
    }
    fn exact_opt(
        &self,
        key: &str,
        s: Option<String>,
        rule: fn(&str) -> std::result::Result<String, InvalidValue>,
    ) -> Result<Option<String>> {
        s.map(|s| self.exact(key, s, rule)).transpose()
    }
    fn date(&self, key: &str, s: Option<String>) -> Result<Option<String>> {
        s.map(|s| parse::date(&s).map(|_| s).map_err(|e| self.bad(key, e.0)))
            .transpose()
    }
}

/// Splits the input into lines (the last newline is optional) and reads
/// each as a JSON object with a known "type".
fn records(input: &[u8]) -> Result<Vec<Rec>> {
    if input.is_empty() {
        return Err(bad(
            1,
            "workspace",
            "the file is empty; the first line must be the workspace header",
        ));
    }
    // Some editors on Windows add one; as "not JSON at column 1" it hides the cause.
    if input.starts_with(b"\xEF\xBB\xBF") {
        return Err(bad(
            1,
            "line",
            "the file starts with a UTF-8 byte order mark; save it without one",
        ));
    }
    let body = input.strip_suffix(b"\n").unwrap_or(input);
    let mut out = Vec::new();
    for (i, raw) in body.split(|b| *b == b'\n').enumerate() {
        let line = i + 1;
        let text = std::str::from_utf8(raw).map_err(|e| {
            bad(
                line,
                "line",
                format!("not UTF-8 (invalid byte at column {})", e.valid_up_to() + 1),
            )
        })?;
        if text.trim().is_empty() {
            return Err(bad(line, "line", "not JSON: the line is blank"));
        }
        let value: Value = serde_json::from_str(text).map_err(|e| {
            let msg = e.to_string();
            let msg = msg
                .rsplit_once(" at line ")
                .map_or(msg.as_str(), |(m, _)| m)
                .to_string();
            bad(
                line,
                "line",
                format!("not JSON at column {}: {msg}", e.column()),
            )
        })?;
        let Value::Object(mut map) = value else {
            return Err(bad(
                line,
                "line",
                format!("expected a JSON object, got {}", kind_of(&value)),
            ));
        };
        let kind = match map.remove("type") {
            None => return Err(bad(line, "type", "missing; every record has one")),
            Some(Value::String(t)) => {
                TYPES.iter().find(|k| **k == t).copied().ok_or_else(|| {
                    bad(
                        line,
                        "type",
                        format!(
                            "unknown record type {t:?}; expected one of: {}",
                            TYPES.join(", ")
                        ),
                    )
                })?
            }
            Some(v) => {
                return Err(bad(
                    line,
                    "type",
                    format!("expected a string, got {}", kind_of(&v)),
                ));
            }
        };
        if line == 1 && kind != "workspace" {
            return Err(bad(
                1,
                "type",
                format!(
                    "the first line must be the workspace header (type \"workspace\"), not {kind:?}"
                ),
            ));
        }
        if line > 1 && kind == "workspace" {
            return Err(bad(
                line,
                "type",
                "the workspace header can only be the first line",
            ));
        }
        // Restored so `known` sees the whole record.
        map.insert("type".into(), Value::String(kind.into()));
        out.push(Rec { line, kind, map });
    }
    Ok(out)
}

/// Reads and checks every value of every record, then every reference
/// between them. `now` is the import time, the default for timestamps.
fn read_file(input: &[u8], now: i64) -> Result<File> {
    let mut recs = records(input)?.into_iter();
    let mut first = recs
        .next()
        .expect("records() returns the header line or an error");
    let header = read_header(&mut first)?;
    let mut file = File {
        header,
        labels: vec![],
        projects: vec![],
        milestones: vec![],
        cycles: vec![],
        issues: vec![],
        comments: vec![],
        relations: vec![],
        docs: vec![],
    };
    let mut comment_lines = vec![];
    for mut r in recs {
        r.map.remove("type");
        match r.kind {
            "label" => file.labels.push(read_label(&mut r)?),
            "project" => file.projects.push(read_project(&mut r, now)?),
            "milestone" => file.milestones.push(read_milestone(&mut r, now)?),
            "cycle" => file.cycles.push(read_cycle(&mut r, now)?),
            "issue" => file.issues.push(read_issue(&mut r, now)?),
            "comment" => {
                comment_lines.push(r.line);
                file.comments.push(read_comment(&mut r, now)?);
            }
            "relation" => file.relations.push(read_relation(&mut r)?),
            "doc" => file.docs.push(read_doc(&mut r, now)?),
            other => unreachable!("records() admits only known types, not {other}"),
        }
    }
    check_references(&file, &comment_lines)?;
    Ok(file)
}

fn read_header(r: &mut Rec) -> Result<Header> {
    // The format first: a newer file's keys are only "unknown" to this lcl.
    match r.map.get("format") {
        None => return Err(r.bad("format", format!("missing; expected {FORMAT:?}"))),
        Some(Value::String(f)) if f == FORMAT => {}
        Some(Value::String(f)) => {
            return Err(r.bad(
                "format",
                format!("unsupported format {f:?}; this lcl reads {FORMAT}"),
            ));
        }
        Some(v) => {
            return Err(r.bad(
                "format",
                format!("expected the string {FORMAT:?}, got {}", kind_of(v)),
            ));
        }
    }
    r.known(WORKSPACE_KEYS)?;
    r.map.remove("type");
    r.map.remove("format");
    // The workspace keeps its own name; the file's is only checked.
    if let Some(v) = r.defaulted("name", "this workspace's name")? {
        r.string("name", v)?;
    }
    let key = r.req_str("key")?;
    let description = r
        .defaulted("description", "this workspace's description")?
        .map(|v| r.string("description", v))
        .transpose()?;
    let created_at = r
        .defaulted("created_at", "this workspace's created_at")?
        .map(|v| r.int("created_at", v))
        .transpose()?;
    let next_issue_number = r
        .defaulted("next_issue_number", "1")?
        .map(|v| r.int("next_issue_number", v))
        .transpose()?;
    if let Some(n) = next_issue_number
        && n < 1
    {
        return Err(r.bad("next_issue_number", format!("must be at least 1, not {n}")));
    }
    Ok(Header {
        key,
        description,
        created_at,
        next_issue_number,
    })
}

fn read_label(r: &mut Rec) -> Result<LabelRec> {
    r.known(LABEL_KEYS)?;
    let name = r.req_str("name")?;
    let name = r.exact("name", name, parse::label_name)?;
    let color = r.def_str("color", "#888888")?;
    let color = parse::color(&color).map_err(|e| r.bad("color", e.0))?;
    Ok(LabelRec {
        line: r.line,
        name,
        color,
    })
}

fn read_project(r: &mut Rec, now: i64) -> Result<ProjectRec> {
    r.known(PROJECT_KEYS)?;
    let name = r.req_str("name")?;
    let name = r.exact("name", name, parse::project_name)?;
    let description = r.def_str("description", "")?;
    let status = r.def_str("status", "Backlog")?;
    let status = status
        .parse::<ProjectStatus>()
        .map_err(|e| r.bad("status", e.0))?;
    let target_date = r.opt_str("target_date")?;
    let target_date = r.date("target_date", target_date)?;
    let created_at = r.def_int("created_at", now, "the import time")?;
    let updated_at = r.def_int("updated_at", created_at, "created_at")?;
    Ok(ProjectRec {
        line: r.line,
        name,
        description,
        status,
        target_date,
        created_at,
        updated_at,
    })
}

fn read_milestone(r: &mut Rec, now: i64) -> Result<MilestoneRec> {
    r.known(MILESTONE_KEYS)?;
    let project = r.req_str("project")?;
    let project = r.exact("project", project, parse::project_name)?;
    let name = r.req_str("name")?;
    let name = r.exact("name", name, parse::milestone_name)?;
    let description = r.def_str("description", "")?;
    let target_date = r.opt_str("target_date")?;
    let target_date = r.date("target_date", target_date)?;
    let sort_order = r.def_int("sort_order", 0, "0")?;
    let created_at = r.def_int("created_at", now, "the import time")?;
    let updated_at = r.def_int("updated_at", created_at, "created_at")?;
    Ok(MilestoneRec {
        line: r.line,
        project,
        name,
        description,
        target_date,
        sort_order,
        created_at,
        updated_at,
    })
}

fn read_cycle(r: &mut Rec, now: i64) -> Result<CycleRec> {
    r.known(CYCLE_KEYS)?;
    let id = r
        .defaulted("id", "a new id")?
        .map(|v| r.string("id", v))
        .transpose()?;
    let id = id
        .map(|id| parse::uuid(&id).map_err(|e| r.bad("id", e.0)))
        .transpose()?;
    let name = r.req_str("name")?;
    let starts_at = r.opt_str("starts_at")?;
    let starts_at = r.date("starts_at", starts_at)?;
    let ends_at = r.opt_str("ends_at")?;
    let ends_at = r.date("ends_at", ends_at)?;
    let created_at = r.def_int("created_at", now, "the import time")?;
    Ok(CycleRec {
        line: r.line,
        id,
        name,
        starts_at,
        ends_at,
        created_at,
    })
}

fn read_issue(r: &mut Rec, now: i64) -> Result<IssueRec> {
    r.known(ISSUE_KEYS)?;
    let number = r.req_int("number")?;
    if number < 1 || number == i64::MAX {
        return Err(r.bad(
            "number",
            format!("must be a positive number below {}, not {number}", i64::MAX),
        ));
    }
    let title = r.req_str("title")?;
    let description = r.def_str("description", "")?;
    let status = r.def_str("status", "Backlog")?;
    let status = status.parse::<Status>().map_err(|e| r.bad("status", e.0))?;
    let priority = r.def_str("priority", "None")?;
    let priority = priority
        .parse::<Priority>()
        .map_err(|e| r.bad("priority", e.0))?;
    let assignee = r.opt_str("assignee")?;
    let assignee = r.exact_opt("assignee", assignee, parse::assignee)?;
    let creator = r.opt_str("creator")?;
    let creator = r.exact_opt("creator", creator, parse::author)?;
    let issue_ref = r.opt_str("ref")?;
    let issue_ref = r.exact_opt("ref", issue_ref, parse::issue_ref)?;
    let parent = r.opt_int("parent")?;
    let project = r.opt_str("project")?;
    let project = r.exact_opt("project", project, parse::project_name)?;
    let milestone = r.opt_str("milestone")?;
    let milestone = r.exact_opt("milestone", milestone, parse::milestone_name)?;
    let labels = r.def_strs("labels")?;
    let labels = labels
        .into_iter()
        .map(|l| r.exact("labels", l, parse::label_name))
        .collect::<Result<Vec<_>>>()?;
    let cycles = r.def_strs("cycles")?;
    let cycles = cycles
        .into_iter()
        .map(|c| parse::uuid(&c).map_err(|e| r.bad("cycles", e.0)))
        .collect::<Result<Vec<_>>>()?;
    let estimate = r.opt_int("estimate")?;
    let estimate = estimate
        .map(|e| parse::estimate(e).map_err(|e| r.bad("estimate", e.0)))
        .transpose()?;
    let due_date = r.opt_str("due_date")?;
    let due_date = r.date("due_date", due_date)?;
    let created_at = r.def_int("created_at", now, "the import time")?;
    let updated_at = r.def_int("updated_at", created_at, "created_at")?;
    let started_at = r.opt_int("started_at")?;
    let completed_at = r.opt_int("completed_at")?;
    let canceled_at = r.opt_int("canceled_at")?;
    Ok(IssueRec {
        line: r.line,
        number,
        title,
        description,
        status,
        priority,
        assignee,
        creator,
        r#ref: issue_ref,
        parent,
        project,
        milestone,
        labels,
        cycles,
        estimate,
        due_date,
        created_at,
        updated_at,
        started_at,
        completed_at,
        canceled_at,
    })
}

fn read_comment(r: &mut Rec, now: i64) -> Result<CommentRec> {
    r.known(COMMENT_KEYS)?;
    let issue = r.req_int("issue")?;
    let body = r.req_str("body")?;
    let author = r.opt_str("author")?;
    let author = r.exact_opt("author", author, parse::author)?;
    let created_at = r.def_int("created_at", now, "the import time")?;
    Ok(CommentRec {
        issue,
        body,
        author,
        created_at,
    })
}

fn read_relation(r: &mut Rec) -> Result<RelationRec> {
    r.known(RELATION_KEYS)?;
    let kind = r.req_str("kind")?;
    let kind = Kind::parse(&kind).ok_or_else(|| {
        r.bad(
            "kind",
            format!("invalid relation kind {kind:?}; expected one of: blocks, related, duplicate"),
        )
    })?;
    let from = r.req_int("from")?;
    let to = r.req_int("to")?;
    Ok(RelationRec {
        line: r.line,
        kind,
        from,
        to,
    })
}

fn read_doc(r: &mut Rec, now: i64) -> Result<DocRec> {
    r.known(DOC_KEYS)?;
    let id = r
        .defaulted("id", "a new id")?
        .map(|v| r.string("id", v))
        .transpose()?;
    let id = id
        .map(|id| parse::uuid(&id).map_err(|e| r.bad("id", e.0)))
        .transpose()?;
    let title = r.req_str("title")?;
    let content = r.def_str("content", "")?;
    let parent = r.opt_str("parent")?;
    let parent = parent
        .map(|p| parse::uuid(&p).map_err(|e| r.bad("parent", e.0)))
        .transpose()?;
    let created_at = r.def_int("created_at", now, "the import time")?;
    let updated_at = r.def_int("updated_at", created_at, "created_at")?;
    Ok(DocRec {
        line: r.line,
        id,
        title,
        content,
        parent,
        created_at,
        updated_at,
    })
}

/// Why a relation can't be added to the ones before it.
#[derive(Debug)]
enum EdgeError {
    SelfRelation,
    /// The same row already came, at `at`.
    Twice {
        at: usize,
    },
    /// `path` runs from `from` back to it, as numbers.
    BlocksCycle {
        path: Vec<i64>,
    },
    /// `from` is already a duplicate of `current`, said at `at`.
    SecondTarget {
        current: i64,
        at: usize,
    },
    DuplicateCycle {
        path: Vec<i64>,
    },
}

/// Relations added one at a time, each checked as `relate` checks it.
#[derive(Default)]
struct Graph {
    /// Each stored row (related with the smaller number first), and where it came from.
    rows: HashMap<(i64, Kind, i64), usize>,
    blocks: HashMap<i64, Vec<i64>>,
    duplicate_of: HashMap<i64, (i64, usize)>,
    /// Set when every blocks row to come is known to make no cycle between
    /// them, and so with duplicate rows: `add` then skips its search for
    /// one, which costs a walk of the chain for each row added to it.
    blocks_acyclic: bool,
    duplicates_acyclic: bool,
}

/// Whether the links `from -> to` make no cycle (Kahn's algorithm).
fn acyclic(links: impl Iterator<Item = (i64, i64)>) -> bool {
    let mut out: HashMap<i64, Vec<i64>> = HashMap::new();
    let mut incoming: HashMap<i64, usize> = HashMap::new();
    for (from, to) in links {
        out.entry(from).or_default().push(to);
        incoming.entry(from).or_default();
        *incoming.entry(to).or_default() += 1;
    }
    let mut free: Vec<i64> = incoming
        .iter()
        .filter(|(_, n)| **n == 0)
        .map(|(node, _)| *node)
        .collect();
    let mut left = incoming.len();
    while let Some(node) = free.pop() {
        left -= 1;
        for to in out.get(&node).into_iter().flatten() {
            let n = incoming.get_mut(to).expect("every link's end is counted");
            *n -= 1;
            if *n == 0 {
                free.push(*to);
            }
        }
    }
    left == 0
}

impl Graph {
    /// A graph for exactly these rows, added one at a time with `add`. It
    /// looks at them all first, so rows that make no cycle are added without
    /// a search each.
    fn expecting(rows: impl Iterator<Item = (Kind, i64, i64)> + Clone) -> Graph {
        let of = |kind: Kind| {
            rows.clone()
                .filter(move |(k, _, _)| *k == kind)
                .map(|(_, from, to)| (from, to))
        };
        Graph {
            blocks_acyclic: acyclic(of(Kind::Blocks)),
            duplicates_acyclic: acyclic(of(Kind::Duplicate)),
            ..Graph::default()
        }
    }

    fn add(
        &mut self,
        kind: Kind,
        from: i64,
        to: i64,
        at: usize,
    ) -> std::result::Result<(), EdgeError> {
        if from == to {
            return Err(EdgeError::SelfRelation);
        }
        let row = match kind {
            Kind::Related => (from.min(to), kind, from.max(to)),
            _ => (from, kind, to),
        };
        if let Some(&first) = self.rows.get(&row) {
            return Err(EdgeError::Twice { at: first });
        }
        match kind {
            Kind::Blocks => {
                if !self.blocks_acyclic
                    && let Some(path) = self.blocks_path(to, from)
                {
                    return Err(EdgeError::BlocksCycle {
                        path: std::iter::once(from).chain(path).collect(),
                    });
                }
                self.blocks.entry(from).or_default().push(to);
            }
            Kind::Duplicate => {
                if let Some(&(current, at)) = self.duplicate_of.get(&from) {
                    return Err(EdgeError::SecondTarget { current, at });
                }
                // Each issue has one target at most: the chain from `to` is a line.
                let mut path = vec![from, to];
                let mut next = match self.duplicates_acyclic {
                    true => None,
                    false => self.duplicate_of.get(&to).map(|(t, _)| *t),
                };
                while let Some(n) = next {
                    path.push(n);
                    if n == from {
                        return Err(EdgeError::DuplicateCycle { path });
                    }
                    if path.len() > self.duplicate_of.len() + 2 {
                        break;
                    }
                    next = self.duplicate_of.get(&n).map(|(t, _)| *t);
                }
                self.duplicate_of.insert(from, (to, at));
            }
            Kind::Related => {}
        }
        self.rows.insert(row, at);
        Ok(())
    }

    /// The shortest chain of blocks rows from `start` to `goal`, start and
    /// goal included, or None.
    fn blocks_path(&self, start: i64, goal: i64) -> Option<Vec<i64>> {
        let mut came_from: HashMap<i64, Option<i64>> = HashMap::from([(start, None)]);
        let mut queue = VecDeque::from([start]);
        while let Some(n) = queue.pop_front() {
            for &next in self.blocks.get(&n).into_iter().flatten() {
                if came_from.contains_key(&next) {
                    continue;
                }
                came_from.insert(next, Some(n));
                if next == goal {
                    let mut path = vec![next];
                    let mut at = Some(n);
                    while let Some(p) = at {
                        path.push(p);
                        at = came_from[&p];
                    }
                    path.reverse();
                    return Some(path);
                }
                queue.push_back(next);
            }
        }
        None
    }
}

/// What an import says about a relation that can't be added.
fn edge_message(key: &str, e: &EdgeError, kind: Kind, from: i64, to: i64) -> String {
    let id = |n: i64| identifier(key, n);
    let named =
        |path: &[i64], sep: &str| chain(&path.iter().map(|n| id(*n)).collect::<Vec<_>>(), sep);
    match e {
        EdgeError::SelfRelation => LclError::SelfRelation(id(from)).to_string(),
        EdgeError::Twice { at } => format!(
            "{} {} {} is already on line {at}",
            id(from),
            kind.as_str(),
            id(to)
        ),
        EdgeError::BlocksCycle { path } => LclError::BlocksCycle {
            blocker: id(from),
            blocked: id(to),
            path: named(path, " blocks "),
        }
        .to_string(),
        EdgeError::SecondTarget { current, at } => format!(
            "{} is already a duplicate of {} (line {at})",
            id(from),
            id(*current)
        ),
        EdgeError::DuplicateCycle { path } => LclError::DuplicateCycle {
            issue: id(from),
            target: id(to),
            path: named(path, " -> "),
        }
        .to_string(),
    }
}

/// Follows `up` from `start` and returns the path back to it, if the chain
/// loops through `start`. Stops at a missing link or another loop.
fn loop_through<T: Copy + Eq + std::hash::Hash>(
    start: T,
    up: impl Fn(T) -> Option<T>,
) -> Option<Vec<T>> {
    let mut path = vec![start];
    let mut seen = HashSet::from([start]);
    let mut at = up(start);
    while let Some(n) = at {
        path.push(n);
        if n == start {
            return Some(path);
        }
        if !seen.insert(n) {
            return None;
        }
        at = up(n);
    }
    None
}

/// Every node that is on a loop of `up` links. One pass over the links, where
/// calling `loop_through` for each node would walk a long chain once per
/// node on it.
fn on_loops<T: Copy + Eq + std::hash::Hash>(
    nodes: impl IntoIterator<Item = T>,
    up: impl Fn(T) -> Option<T>,
) -> HashSet<T> {
    let mut done: HashSet<T> = HashSet::new();
    let mut looped = HashSet::new();
    for start in nodes {
        if done.contains(&start) {
            continue;
        }
        // The walk from `start`, and each node's place on it.
        let mut walk = vec![start];
        let mut place = HashMap::from([(start, 0)]);
        let mut at = up(start);
        while let Some(n) = at {
            if let Some(&from) = place.get(&n) {
                looped.extend(walk[from..].iter().copied());
                break;
            }
            if done.contains(&n) {
                break;
            }
            place.insert(n, walk.len());
            walk.push(n);
            at = up(n);
        }
        done.extend(walk);
    }
    looped
}

/// Every reference between records: names and numbers that must exist,
/// things that must be unique, and the rules that span records (parent
/// loops, relations). Errors name the line of the record at fault.
fn check_references(file: &File, comment_lines: &[usize]) -> Result<()> {
    let key = file.header.key.as_str();
    let id = |n: i64| identifier(key, n);

    let mut labels: HashMap<String, (&str, usize)> = HashMap::new();
    for l in &file.labels {
        if let Some((existing, at)) = labels.get(&l.name.to_ascii_lowercase()) {
            return Err(bad(
                l.line,
                "name",
                taken("label", &l.name, existing, *at, ""),
            ));
        }
        labels.insert(l.name.to_ascii_lowercase(), (&l.name, l.line));
    }

    let mut projects: HashMap<String, (&str, usize)> = HashMap::new();
    for p in &file.projects {
        if let Some((existing, at)) = projects.get(&p.name.to_ascii_lowercase()) {
            return Err(bad(
                p.line,
                "name",
                taken("project", &p.name, existing, *at, ""),
            ));
        }
        projects.insert(p.name.to_ascii_lowercase(), (&p.name, p.line));
    }

    // (project, milestone), both lowercased.
    let mut milestones: HashMap<(String, String), (&str, usize)> = HashMap::new();
    for m in &file.milestones {
        if !projects.contains_key(&m.project.to_ascii_lowercase()) {
            return Err(bad(
                m.line,
                "project",
                format!("no project named {:?} in the file", m.project),
            ));
        }
        let k = (m.project.to_ascii_lowercase(), m.name.to_ascii_lowercase());
        if let Some((existing, at)) = milestones.get(&k) {
            return Err(bad(
                m.line,
                "name",
                taken("milestone", &m.name, existing, *at, " in a project"),
            ));
        }
        milestones.insert(k, (&m.name, m.line));
    }

    let mut cycles: HashMap<&str, usize> = HashMap::new();
    for c in &file.cycles {
        if let Some(id) = &c.id {
            if let Some(at) = cycles.get(id.as_str()) {
                return Err(bad(
                    c.line,
                    "id",
                    format!("cycle {id} is already on line {at}"),
                ));
            }
            cycles.insert(id, c.line);
        }
    }

    let mut docs: HashMap<&str, (usize, Option<&str>)> = HashMap::new();
    for d in &file.docs {
        if let Some(id) = &d.id {
            if let Some((at, _)) = docs.get(id.as_str()) {
                return Err(bad(
                    d.line,
                    "id",
                    format!("doc {id} is already on line {at}"),
                ));
            }
            docs.insert(id, (d.line, d.parent.as_deref()));
        }
    }
    let doc_up = |n: &str| docs.get(n).and_then(|(_, p)| *p);
    let looped_docs = on_loops(docs.keys().copied(), doc_up);
    for d in &file.docs {
        let Some(parent) = &d.parent else { continue };
        if !docs.contains_key(parent.as_str()) {
            return Err(bad(
                d.line,
                "parent",
                format!("no doc with id {parent} in the file"),
            ));
        }
        if let Some(id) = &d.id {
            if parent == id {
                return Err(bad(
                    d.line,
                    "parent",
                    LclError::DocSelfParent(id.clone()).to_string(),
                ));
            }
            if looped_docs.contains(id.as_str())
                && let Some(path) = loop_through(id.as_str(), doc_up)
            {
                let path: Vec<String> = path.into_iter().map(String::from).collect();
                return Err(bad(
                    d.line,
                    "parent",
                    format!("doc parents make a cycle: {}", chain(&path, " -> ")),
                ));
            }
        }
    }

    let mut numbers: HashMap<i64, usize> = HashMap::new();
    let mut parents: HashMap<i64, i64> = HashMap::new();
    let mut refs: HashMap<String, (&str, i64)> = HashMap::new();
    for i in &file.issues {
        if let Some(at) = numbers.get(&i.number) {
            return Err(bad(
                i.line,
                "number",
                format!("{} is already on line {at}", id(i.number)),
            ));
        }
        numbers.insert(i.number, i.line);
        if let Some(p) = i.parent {
            parents.insert(i.number, p);
        }
        if let Some(r) = &i.r#ref {
            if ref_looks_own(key, r) {
                return Err(bad(
                    i.line,
                    "ref",
                    LclError::RefLooksOwn(r.clone()).to_string(),
                ));
            }
            if let Some((_, holder)) = refs.get(&r.to_ascii_lowercase()) {
                return Err(bad(
                    i.line,
                    "ref",
                    LclError::RefTaken {
                        given: r.clone(),
                        holder: id(*holder),
                    }
                    .to_string(),
                ));
            }
            refs.insert(r.to_ascii_lowercase(), (r, i.number));
        }
    }
    let looped_issues = on_loops(parents.keys().copied(), |n| parents.get(&n).copied());
    for i in &file.issues {
        if let Some(p) = i.parent {
            if p == i.number {
                return Err(bad(
                    i.line,
                    "parent",
                    LclError::SelfParent(id(i.number)).to_string(),
                ));
            }
            if !numbers.contains_key(&p) {
                return Err(bad(
                    i.line,
                    "parent",
                    format!("there is no issue {} in the file", id(p)),
                ));
            }
            if looped_issues.contains(&i.number)
                && let Some(path) = loop_through(i.number, |n| parents.get(&n).copied())
            {
                let path: Vec<String> = path.into_iter().map(id).collect();
                let e = LclError::ParentCycle {
                    issue: id(i.number),
                    parent: id(p),
                    path: chain(&path, " -> "),
                };
                return Err(bad(i.line, "parent", e.to_string()));
            }
        }
        match (&i.project, &i.milestone) {
            (None, Some(m)) => {
                return Err(bad(
                    i.line,
                    "milestone",
                    format!("milestone {m:?} needs a project: the issue has none"),
                ));
            }
            (Some(p), m) => {
                if !projects.contains_key(&p.to_ascii_lowercase()) {
                    return Err(bad(
                        i.line,
                        "project",
                        format!("no project named {p:?} in the file"),
                    ));
                }
                if let Some(m) = m
                    && !milestones.contains_key(&(p.to_ascii_lowercase(), m.to_ascii_lowercase()))
                {
                    let elsewhere: Vec<&str> = file
                        .milestones
                        .iter()
                        .filter(|x| x.name.eq_ignore_ascii_case(m))
                        .map(|x| x.project.as_str())
                        .collect();
                    let msg = if elsewhere.is_empty() {
                        format!("no milestone named {m:?} in project {p:?} in the file")
                    } else {
                        format!(
                            "milestone {m:?} is not in project {p:?} (it is in {})",
                            elsewhere
                                .iter()
                                .map(|e| format!("{e:?}"))
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    };
                    return Err(bad(i.line, "milestone", msg));
                }
            }
            (None, None) => {}
        }
        let mut seen = HashSet::new();
        for l in &i.labels {
            if !labels.contains_key(&l.to_ascii_lowercase()) {
                return Err(bad(
                    i.line,
                    "labels",
                    format!("no label named {l:?} in the file"),
                ));
            }
            if !seen.insert(l.to_ascii_lowercase()) {
                return Err(bad(
                    i.line,
                    "labels",
                    format!("label {l:?} is listed twice (ignoring case)"),
                ));
            }
        }
        let mut seen = HashSet::new();
        for c in &i.cycles {
            if !cycles.contains_key(c.as_str()) {
                return Err(bad(
                    i.line,
                    "cycles",
                    format!("no cycle with id {c} in the file"),
                ));
            }
            if !seen.insert(c) {
                return Err(bad(i.line, "cycles", format!("cycle {c} is listed twice")));
            }
        }
    }

    for (c, line) in file.comments.iter().zip(comment_lines) {
        if !numbers.contains_key(&c.issue) {
            return Err(bad(
                *line,
                "issue",
                format!("there is no issue {} in the file", id(c.issue)),
            ));
        }
    }

    let mut graph = Graph::expecting(file.relations.iter().map(|r| (r.kind, r.from, r.to)));
    for r in &file.relations {
        for (field, n) in [("from", r.from), ("to", r.to)] {
            if !numbers.contains_key(&n) {
                return Err(bad(
                    r.line,
                    field,
                    format!("there is no issue {} in the file", id(n)),
                ));
            }
        }
        graph.add(r.kind, r.from, r.to, r.line).map_err(|e| {
            bad(
                r.line,
                "relation",
                edge_message(key, &e, r.kind, r.from, r.to),
            )
        })?;
    }
    Ok(())
}

impl Store {
    /// Imports a whole "lcl-export/1" file into this workspace, which must be
    /// empty and have the file's key (ignoring ASCII case). The file is read
    /// and checked first; then the emptiness check, every insert and the
    /// counter update run in one `BEGIN IMMEDIATE` transaction, committed,
    /// or with `dry_run` rolled back.
    ///
    /// The issue-number counter becomes the largest of its current value,
    /// the file's next_issue_number and one past the largest number
    /// imported: it never goes down. The workspace keeps its name; its
    /// description and created_at come from the file when given.
    pub fn import(&mut self, input: &[u8], dry_run: bool) -> Result<ImportSummary> {
        let at = now();
        let file = read_file(input, at)?;
        let tx = crate::db::begin_write(&mut self.conn, &mut self.fts_checked)?;
        // Issue and doc parents may be inserted after the rows that name
        // them. The check runs at commit.
        tx.pragma_update(None, "defer_foreign_keys", true)?;
        let key = read_key(&tx)?;
        if !key.eq_ignore_ascii_case(&file.header.key) {
            return Err(LclError::ImportKeyMismatch {
                file: file.header.key.clone(),
                workspace: key,
            });
        }
        let workspace = read_meta(&tx, "name")?.unwrap_or_default();
        let has = contents(&tx)?;
        if !has.is_empty() {
            return Err(LclError::ImportNotEmpty { workspace, has });
        }
        let next = insert(&tx, &file, at)?;
        if dry_run {
            tx.rollback()?;
        } else {
            tx.commit()?;
        }
        Ok(ImportSummary {
            counts: file.counts(),
            next_issue_number: next,
            dry_run,
            workspace,
            key,
        })
    }
}

/// What the workspace holds, as "1 issue and 2 labels"; "" when nothing.
fn contents(conn: &Connection) -> Result<String> {
    let mut parts = vec![];
    for (table, one, many) in [
        ("issues", "issue", "issues"),
        ("labels", "label", "labels"),
        ("projects", "project", "projects"),
        ("milestones", "milestone", "milestones"),
        ("cycles", "cycle", "cycles"),
        ("docs", "doc", "docs"),
        ("comments", "comment", "comments"),
        ("issue_relations", "relation", "relations"),
    ] {
        let n: i64 = conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))?;
        if n > 0 {
            let mut part = format!("{n} {}", if n == 1 { one } else { many });
            // What is in the trash counts: its numbers, refs and names are
            // taken. Say so, since no list shows it without --trashed.
            if ["issues", "labels", "docs", "comments"].contains(&table) {
                let trashed: i64 = conn.query_row(
                    &format!("SELECT count(*) FROM {table} WHERE deleted_at IS NOT NULL"),
                    [],
                    |r| r.get(0),
                )?;
                if trashed > 0 {
                    part.push_str(&format!(" ({trashed} in the trash)"));
                }
            }
            parts.push(part);
        }
    }
    Ok(match parts.len() {
        0 => String::new(),
        1 => parts.remove(0),
        _ => {
            let last = parts.pop().expect("two or more");
            format!("{} and {last}", parts.join(", "))
        }
    })
}

fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Writes a checked file in dependency order and returns the new counter.
fn insert(conn: &Connection, file: &File, at: i64) -> Result<i64> {
    let mut label_ids: HashMap<String, String> = HashMap::new();
    {
        let mut stmt = conn.prepare("INSERT INTO labels(id, name, color) VALUES (?1, ?2, ?3)")?;
        for l in &file.labels {
            let id = new_id();
            stmt.execute(params![id, l.name, l.color])?;
            label_ids.insert(l.name.to_ascii_lowercase(), id);
        }
    }
    let mut project_ids: HashMap<String, String> = HashMap::new();
    {
        let mut stmt = conn.prepare("INSERT INTO projects(id, name, description, status, target_date, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)")?;
        for p in &file.projects {
            let id = new_id();
            stmt.execute(params![
                id,
                p.name,
                p.description,
                p.status.as_str(),
                p.target_date,
                p.created_at,
                p.updated_at
            ])?;
            project_ids.insert(p.name.to_ascii_lowercase(), id);
        }
    }
    let mut milestone_ids: HashMap<(String, String), String> = HashMap::new();
    {
        let mut stmt = conn.prepare("INSERT INTO milestones(id, project_id, name, description, target_date, sort_order, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)")?;
        for m in &file.milestones {
            let id = new_id();
            let project = &project_ids[&m.project.to_ascii_lowercase()];
            stmt.execute(params![
                id,
                project,
                m.name,
                m.description,
                m.target_date,
                m.sort_order,
                m.created_at,
                m.updated_at
            ])?;
            milestone_ids.insert(
                (m.project.to_ascii_lowercase(), m.name.to_ascii_lowercase()),
                id,
            );
        }
    }
    {
        let mut stmt = conn.prepare("INSERT INTO cycles(id, name, starts_at, ends_at, created_at) VALUES (?1, ?2, ?3, ?4, ?5)")?;
        for c in &file.cycles {
            stmt.execute(params![
                c.id.clone().unwrap_or_else(new_id),
                c.name,
                c.starts_at,
                c.ends_at,
                c.created_at
            ])?;
        }
    }
    {
        let mut stmt = conn.prepare("INSERT INTO docs(id, title, content, parent_id, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)")?;
        for d in &file.docs {
            stmt.execute(params![
                d.id.clone().unwrap_or_else(new_id),
                d.title,
                d.content,
                d.parent,
                d.created_at,
                d.updated_at
            ])?;
        }
    }
    let issue_ids: HashMap<i64, String> =
        file.issues.iter().map(|i| (i.number, new_id())).collect();
    {
        let mut stmt = conn.prepare(
            "INSERT INTO issues(id, number, title, description, status, priority, assignee, parent_id, estimate, due_date, created_at, updated_at, completed_at, started_at, canceled_at, project_id, milestone_id, ref, creator) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)",
        )?;
        let mut label =
            conn.prepare("INSERT INTO issue_labels(issue_id, label_id) VALUES (?1, ?2)")?;
        let mut cycle =
            conn.prepare("INSERT INTO issue_cycles(issue_id, cycle_id) VALUES (?1, ?2)")?;
        // Parents first. A row inserted before the one its parent_id names
        // leaves a foreign key waiting, and while one waits, SQLite scans
        // the table for each row inserted: a long chain listed child first
        // would cost a scan for every issue in the file.
        let by_number: HashMap<i64, &IssueRec> =
            file.issues.iter().map(|i| (i.number, i)).collect();
        let mut placed: HashSet<i64> = HashSet::new();
        let mut parents_first: Vec<&IssueRec> = Vec::with_capacity(file.issues.len());
        for i in &file.issues {
            // check_references found every parent and no loop.
            let mut above = vec![];
            let mut at = Some(i);
            while let Some(i) = at
                && placed.insert(i.number)
            {
                above.push(i);
                at = i.parent.map(|p| by_number[&p]);
            }
            parents_first.extend(above.into_iter().rev());
        }
        for i in parents_first {
            let id = &issue_ids[&i.number];
            let parent = i.parent.map(|p| &issue_ids[&p]);
            let project = i
                .project
                .as_ref()
                .map(|p| &project_ids[&p.to_ascii_lowercase()]);
            let milestone = match (&i.project, &i.milestone) {
                (Some(p), Some(m)) => {
                    Some(&milestone_ids[&(p.to_ascii_lowercase(), m.to_ascii_lowercase())])
                }
                _ => None,
            };
            stmt.execute(params![
                id,
                i.number,
                i.title,
                i.description,
                i.status.as_str(),
                i.priority.as_i64(),
                i.assignee,
                parent,
                i.estimate,
                i.due_date,
                i.created_at,
                i.updated_at,
                i.completed_at,
                i.started_at,
                i.canceled_at,
                project,
                milestone,
                i.r#ref,
                i.creator
            ])?;
            for l in &i.labels {
                label.execute(params![id, label_ids[&l.to_ascii_lowercase()]])?;
            }
            for c in &i.cycles {
                cycle.execute(params![id, c])?;
            }
        }
    }
    {
        // File order, so same-second comments keep their order (rowid).
        let mut stmt = conn.prepare("INSERT INTO comments(id, issue_id, body, author, created_at) VALUES (?1, ?2, ?3, ?4, ?5)")?;
        for c in &file.comments {
            stmt.execute(params![
                new_id(),
                issue_ids[&c.issue],
                c.body,
                c.author,
                c.created_at
            ])?;
        }
    }
    {
        let mut stmt = conn.prepare("INSERT INTO issue_relations(issue_id, related_id, kind, created_at) VALUES (?1, ?2, ?3, ?4)")?;
        for r in &file.relations {
            let (a, b) = (&issue_ids[&r.from], &issue_ids[&r.to]);
            // As `relate` stores it: related with the smaller id first.
            let (from, to) = if r.kind == Kind::Related && b < a {
                (b, a)
            } else {
                (a, b)
            };
            stmt.execute(params![from, to, r.kind.as_str(), at])?;
        }
    }
    let past_max = file.issues.iter().map(|i| i.number).max().unwrap_or(0) + 1;
    let next = next_issue_number(conn)?
        .max(file.header.next_issue_number.unwrap_or(1))
        .max(past_max);
    put_meta(conn, NEXT_ISSUE_NUMBER, &next.to_string())?;
    if let Some(d) = &file.header.description {
        put_meta(conn, "description", d)?;
    }
    if let Some(c) = file.header.created_at {
        put_meta(conn, "created_at", &c.to_string())?;
    }
    Ok(next)
}

impl Store {
    /// The workspace as an "lcl-export/1" file, read in one snapshot. A
    /// stored value import would refuse (one a hand edit or an older lcl
    /// wrote) is a problem: every one is listed, with the command that fixes
    /// it where there is one, and nothing is exported. Every file this
    /// returns imports.
    pub fn export(&self) -> Result<Export> {
        let (text, counts) = self.read(export_in)?;
        // The guarantee, checked: a file import refuses is lcl's bug.
        if let Err(e) = read_file(text.as_bytes(), now()) {
            return Err(LclError::ExportRoundTrip(e.to_string()));
        }
        Ok(Export { text, counts })
    }
}

/// `rule` accepts `s` unchanged.
fn passes(
    s: &str,
    rule: fn(&str) -> std::result::Result<String, InvalidValue>,
) -> std::result::Result<(), String> {
    match rule(s) {
        Err(e) => Err(e.0),
        Ok(t) if t != s => Err(format!("{s:?} has leading or trailing whitespace")),
        Ok(_) => Ok(()),
    }
}

/// The problem text for a value only a hand edit (or an older lcl) could
/// have stored.
fn by_hand(what: String, why: String, column: &str) -> String {
    format!("{what}: {why}; no lcl command can fix it (column {column})")
}

fn export_in(conn: &Connection) -> Result<(String, Counts)> {
    let mut problems: Vec<String> = vec![];
    let mut out = String::new();
    let mut counts = Counts::default();
    let line = |out: &mut String, v: &dyn erased::Line| {
        out.push_str(&v.json());
        out.push('\n');
    };

    let key = read_key(conn)?;
    let id = |n: i64| identifier(&key, n);
    let name = read_meta(conn, "name")?.unwrap_or_default();
    let description = read_meta(conn, "description")?.unwrap_or_default();
    let created_at = match read_meta(conn, "created_at")? {
        Some(v) => v.parse::<i64>().map_err(|_| v).ok(),
        None => None,
    };
    if created_at.is_none() {
        problems.push(by_hand(
            "workspace".into(),
            "created_at is missing or not a whole number".into(),
            "meta.created_at",
        ));
    }
    let next = next_issue_number(conn)?;
    line(
        &mut out,
        &WorkspaceLine {
            r#type: "workspace",
            format: FORMAT,
            name: &name,
            key: &key,
            description: &description,
            created_at: created_at.unwrap_or(0),
            next_issue_number: next,
        },
    );

    let mut label_names: HashMap<String, String> = HashMap::new();
    {
        let mut stmt = conn
            .prepare("SELECT id, name, color FROM labels WHERE deleted_at IS NULL ORDER BY name COLLATE NOCASE, name, id")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        for row in rows {
            let (lid, lname, color) = row?;
            if let Err(why) = passes(&lname, parse::label_name) {
                problems.push(by_hand(format!("label {lname:?}"), why, "labels.name"));
            }
            if let Err(e) = parse::color(&color) {
                problems.push(by_hand(format!("label {lname:?}"), e.0, "labels.color"));
            }
            label_names.insert(lid, lname.clone());
            counts.labels += 1;
            line(
                &mut out,
                &LabelLine {
                    r#type: "label",
                    name: lname,
                    color,
                },
            );
        }
    }

    let mut project_names: HashMap<String, String> = HashMap::new();
    {
        let mut stmt = conn.prepare("SELECT id, name, description, status, target_date, created_at, updated_at FROM projects ORDER BY name COLLATE NOCASE, name, id")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, i64>(5)?,
                r.get::<_, i64>(6)?,
            ))
        })?;
        for row in rows {
            let (pid, pname, pdesc, status, target, created, updated) = row?;
            if let Err(why) = passes(&pname, parse::project_name) {
                problems.push(by_hand(format!("project {pname:?}"), why, "projects.name"));
            }
            let status = match ProjectStatus::parse(&status) {
                Some(s) => s.as_str(),
                None => {
                    problems.push(by_hand(
                        format!("project {pname:?}"),
                        format!("status {status:?} isn't one lcl knows"),
                        "projects.status",
                    ));
                    "Backlog"
                }
            };
            if let Some(d) = &target
                && let Err(e) = parse::date(d)
            {
                problems.push(format!("project {pname:?}: target_date: {}; fix it with lcl project update {} --target-date YYYY-MM-DD (or --no-target-date)", e.0, shell(&pname)));
            }
            project_names.insert(pid, pname.clone());
            counts.projects += 1;
            line(
                &mut out,
                &ProjectLine {
                    r#type: "project",
                    name: pname,
                    description: pdesc,
                    status,
                    target_date: target,
                    created_at: created,
                    updated_at: updated,
                },
            );
        }
    }

    let mut milestone_names: HashMap<String, (String, String)> = HashMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT m.id, p.name, m.name, m.description, m.target_date, m.sort_order, m.created_at, m.updated_at, m.project_id FROM milestones m LEFT JOIN projects p ON p.id = m.project_id \
             ORDER BY p.name COLLATE NOCASE, p.name, m.sort_order, m.name COLLATE NOCASE, m.name, m.id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, i64>(5)?,
                r.get::<_, i64>(6)?,
                r.get::<_, i64>(7)?,
                r.get::<_, String>(8)?,
            ))
        })?;
        for row in rows {
            let (mid, pname, mname, mdesc, target, sort, created, updated, pid) = row?;
            let Some(pname) = pname else {
                problems.push(by_hand(
                    format!("milestone {mname:?}"),
                    format!("its project {pid} doesn't exist"),
                    "milestones.project_id",
                ));
                continue;
            };
            if let Err(why) = passes(&mname, parse::milestone_name) {
                problems.push(by_hand(
                    format!("milestone {mname:?} in project {pname:?}"),
                    why,
                    "milestones.name",
                ));
            }
            if let Some(d) = &target
                && let Err(e) = parse::date(d)
            {
                problems.push(format!(
                    "milestone {mname:?} in project {pname:?}: target_date: {}; fix it with lcl milestone update {} --project {} --target-date YYYY-MM-DD (or --no-target-date)",
                    e.0, shell(&mname), shell(&pname)
                ));
            }
            milestone_names.insert(mid, (pid, mname.clone()));
            counts.milestones += 1;
            line(
                &mut out,
                &MilestoneLine {
                    r#type: "milestone",
                    project: pname,
                    name: mname,
                    description: mdesc,
                    target_date: target,
                    sort_order: sort,
                    created_at: created,
                    updated_at: updated,
                },
            );
        }
    }

    let mut cycle_ids: HashSet<String> = HashSet::new();
    {
        let mut stmt = conn.prepare(
            "SELECT id, name, starts_at, ends_at, created_at FROM cycles ORDER BY created_at, id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, i64>(4)?,
            ))
        })?;
        for row in rows {
            let (cid, cname, starts, ends, created) = row?;
            if parse::uuid(&cid).ok().as_deref() != Some(cid.as_str()) {
                problems.push(by_hand(
                    format!("cycle {cid}"),
                    "its id is not a lowercase UUID".into(),
                    "cycles.id",
                ));
            }
            for (col, d) in [("starts_at", &starts), ("ends_at", &ends)] {
                if let Some(d) = d
                    && let Err(e) = parse::date(d)
                {
                    problems.push(by_hand(
                        format!("cycle {cid} ({cname:?})"),
                        format!("{col}: {}", e.0),
                        &format!("cycles.{col}"),
                    ));
                }
            }
            cycle_ids.insert(cid.clone());
            counts.cycles += 1;
            line(
                &mut out,
                &CycleLine {
                    r#type: "cycle",
                    id: cid,
                    name: cname,
                    starts_at: starts,
                    ends_at: ends,
                    created_at: created,
                },
            );
        }
    }

    // ----- issues: read all first, since parents and links refer across them
    struct Row {
        id: String,
        number: i64,
        title: String,
        description: String,
        status: String,
        priority: i64,
        assignee: Option<String>,
        parent_id: Option<String>,
        estimate: Option<i64>,
        due_date: Option<String>,
        created_at: i64,
        updated_at: i64,
        completed_at: Option<i64>,
        started_at: Option<i64>,
        canceled_at: Option<i64>,
        project_id: Option<String>,
        milestone_id: Option<String>,
        r#ref: Option<String>,
        creator: Option<String>,
    }
    let issues: Vec<Row> = {
        let mut stmt = conn.prepare(
            "SELECT id, number, title, description, status, priority, assignee, parent_id, estimate, due_date, created_at, updated_at, completed_at, started_at, canceled_at, project_id, milestone_id, ref, creator \
             FROM issues WHERE deleted_at IS NULL ORDER BY number",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(Row {
                id: r.get(0)?,
                number: r.get(1)?,
                title: r.get(2)?,
                description: r.get(3)?,
                status: r.get(4)?,
                priority: r.get(5)?,
                assignee: r.get(6)?,
                parent_id: r.get(7)?,
                estimate: r.get(8)?,
                due_date: r.get(9)?,
                created_at: r.get(10)?,
                updated_at: r.get(11)?,
                completed_at: r.get(12)?,
                started_at: r.get(13)?,
                canceled_at: r.get(14)?,
                project_id: r.get(15)?,
                milestone_id: r.get(16)?,
                r#ref: r.get(17)?,
                creator: r.get(18)?,
            })
        })?;
        rows.collect::<std::result::Result<_, _>>()?
    };
    let numbers: HashMap<String, i64> = issues.iter().map(|i| (i.id.clone(), i.number)).collect();

    // Links of a trashed issue are left out with it (see `crate::trash`).
    const NOT_TRASHED: &str =
        "WHERE issue_id NOT IN (SELECT id FROM issues WHERE deleted_at IS NOT NULL)";
    const NO_TRASHED_LABEL: &str =
        "AND label_id NOT IN (SELECT id FROM labels WHERE deleted_at IS NOT NULL)";
    let mut labels_of: HashMap<String, Vec<String>> = HashMap::new();
    {
        let mut stmt = conn.prepare(&format!(
            "SELECT issue_id, label_id FROM issue_labels {NOT_TRASHED} {NO_TRASHED_LABEL} ORDER BY issue_id, label_id"
        ))?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        for row in rows {
            let (iid, lid) = row?;
            match (numbers.get(iid.as_str()), label_names.get(&lid)) {
                (Some(_), Some(lname)) => labels_of.entry(iid).or_default().push(lname.clone()),
                _ => problems.push(by_hand(
                    format!("issue_labels row ({iid}, {lid})"),
                    "it names an issue or label that doesn't exist".into(),
                    "issue_labels",
                )),
            }
        }
    }
    let mut cycles_of: HashMap<String, Vec<String>> = HashMap::new();
    {
        let mut stmt = conn.prepare(&format!(
            "SELECT issue_id, cycle_id FROM issue_cycles {NOT_TRASHED} ORDER BY issue_id, cycle_id"
        ))?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        for row in rows {
            let (iid, cid) = row?;
            if numbers.contains_key(iid.as_str()) && cycle_ids.contains(&cid) {
                cycles_of.entry(iid).or_default().push(cid);
            } else {
                problems.push(by_hand(
                    format!("issue_cycles row ({iid}, {cid})"),
                    "it names an issue or cycle that doesn't exist".into(),
                    "issue_cycles",
                ));
            }
        }
    }

    let parent_of: HashMap<i64, i64> = issues
        .iter()
        .filter_map(|i| Some((i.number, *numbers.get(i.parent_id.as_deref()?)?)))
        .collect();
    let looped_issues = on_loops(parent_of.keys().copied(), |n| parent_of.get(&n).copied());
    for i in issues {
        let ident = id(i.number);
        let what = format!("issue {ident}");
        if i.number < 1 || i.number == i64::MAX {
            problems.push(by_hand(
                what.clone(),
                format!("its number {} is out of range", i.number),
                "issues.number",
            ));
        }
        let status = match Status::parse(&i.status) {
            Some(s) => s.as_str(),
            None => {
                problems.push(by_hand(
                    what.clone(),
                    format!("status {:?} isn't one lcl knows", i.status),
                    "issues.status",
                ));
                "Backlog"
            }
        };
        let priority = match Priority::from_stored(i.priority) {
            Some(p) => p.label(),
            None => {
                problems.push(by_hand(
                    what.clone(),
                    format!("priority {} isn't one lcl knows (expected 0-4)", i.priority),
                    "issues.priority",
                ));
                "None"
            }
        };
        if let Some(a) = &i.assignee
            && let Err(why) = passes(a, parse::assignee)
        {
            problems.push(format!("{what}: assignee: {why}; fix it with lcl issue update {ident} --assignee NAME (or --no-assignee)"));
        }
        if let Some(c) = &i.creator
            && let Err(why) = passes(c, parse::author)
        {
            problems.push(by_hand(
                what.clone(),
                format!("creator: {why}"),
                "issues.creator",
            ));
        }
        if let Some(r) = &i.r#ref {
            let why = passes(r, parse::issue_ref).err().or_else(|| {
                ref_looks_own(&key, r).then(|| LclError::RefLooksOwn(r.clone()).to_string())
            });
            if let Some(why) = why {
                problems.push(format!(
                    "{what}: ref: {why}; fix it with lcl issue update {ident} --no-ref"
                ));
            }
        }
        let parent = match &i.parent_id {
            None => None,
            Some(p) => match numbers.get(p.as_str()) {
                Some(n) => {
                    if looped_issues.contains(&i.number)
                        && let Some(path) = loop_through(i.number, |n| parent_of.get(&n).copied())
                    {
                        let path: Vec<String> = path.into_iter().map(id).collect();
                        problems.push(format!("{what}: its parents make a cycle ({}); fix it with lcl issue update {ident} --no-parent", chain(&path, " -> ")));
                    }
                    Some(*n)
                }
                None => {
                    problems.push(format!("{what}: parent {p} is not an issue; fix it with lcl issue update {ident} --no-parent"));
                    None
                }
            },
        };
        if let Some(e) = i.estimate
            && let Err(err) = parse::estimate(e)
        {
            problems.push(format!("{what}: estimate: {}; fix it with lcl issue update {ident} --estimate N (or --no-estimate)", err.0));
        }
        if let Some(d) = &i.due_date
            && let Err(e) = parse::date(d)
        {
            problems.push(format!("{what}: due_date: {}; fix it with lcl issue update {ident} --due YYYY-MM-DD (or --no-due)", e.0));
        }
        let project = match &i.project_id {
            None => None,
            Some(p) => match project_names.get(p) {
                Some(n) => Some(n.clone()),
                None => {
                    problems.push(by_hand(
                        what.clone(),
                        format!("its project {p} doesn't exist"),
                        "issues.project_id",
                    ));
                    None
                }
            },
        };
        let milestone = match &i.milestone_id {
            None => None,
            Some(m) => match milestone_names.get(m) {
                Some((pid, mname)) if Some(pid) == i.project_id.as_ref() => Some(mname.clone()),
                Some(_) => {
                    problems.push(by_hand(
                        what.clone(),
                        format!("its milestone {m} is in another project"),
                        "issues.milestone_id",
                    ));
                    None
                }
                None => {
                    problems.push(by_hand(
                        what.clone(),
                        format!("its milestone {m} doesn't exist"),
                        "issues.milestone_id",
                    ));
                    None
                }
            },
        };
        let mut labels = labels_of.remove(&i.id).unwrap_or_default();
        labels.sort_by(|a, b| {
            a.to_ascii_lowercase()
                .cmp(&b.to_ascii_lowercase())
                .then(a.cmp(b))
        });
        let mut cycles = cycles_of.remove(&i.id).unwrap_or_default();
        cycles.sort();
        counts.issues += 1;
        line(
            &mut out,
            &IssueLine {
                r#type: "issue",
                number: i.number,
                title: i.title,
                description: i.description,
                status,
                priority,
                assignee: i.assignee,
                creator: i.creator,
                r#ref: i.r#ref,
                parent,
                project,
                milestone,
                labels,
                cycles,
                estimate: i.estimate,
                due_date: i.due_date,
                created_at: i.created_at,
                updated_at: i.updated_at,
                started_at: i.started_at,
                completed_at: i.completed_at,
                canceled_at: i.canceled_at,
            },
        );
    }

    {
        let mut stmt = conn.prepare("SELECT i.number, c.body, c.author, c.created_at, c.id, c.issue_id FROM comments c LEFT JOIN issues i ON i.id = c.issue_id WHERE i.deleted_at IS NULL AND c.deleted_at IS NULL ORDER BY i.number, c.created_at, c.rowid")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, Option<i64>>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
            ))
        })?;
        for row in rows {
            let (number, body, author, created, cid, iid) = row?;
            let Some(number) = number else {
                problems.push(by_hand(
                    format!("comment {cid}"),
                    format!("its issue {iid} doesn't exist"),
                    "comments.issue_id",
                ));
                continue;
            };
            if let Some(a) = &author
                && let Err(why) = passes(a, parse::author)
            {
                problems.push(by_hand(
                    format!("comment {cid} on {}", id(number)),
                    format!("author: {why}"),
                    "comments.author",
                ));
            }
            counts.comments += 1;
            line(
                &mut out,
                &CommentLine {
                    r#type: "comment",
                    issue: number,
                    body,
                    author,
                    created_at: created,
                },
            );
        }
    }

    // ----- relations: by (from, kind, to), related smaller number first
    {
        let mut stmt = conn.prepare(
            "SELECT issue_id, related_id, kind FROM issue_relations
             WHERE issue_id NOT IN (SELECT id FROM issues WHERE deleted_at IS NOT NULL)
               AND related_id NOT IN (SELECT id FROM issues WHERE deleted_at IS NOT NULL)",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        let mut rels: Vec<(i64, Kind, i64)> = vec![];
        for row in rows {
            let (a, b, kind) = row?;
            match (
                numbers.get(a.as_str()),
                numbers.get(b.as_str()),
                Kind::parse(&kind),
            ) {
                (Some(&from), Some(&to), Some(kind)) => rels.push(if kind == Kind::Related {
                    (from.min(to), kind, from.max(to))
                } else {
                    (from, kind, to)
                }),
                _ => problems.push(by_hand(
                    format!("relation ({a}, {kind}, {b})"),
                    "it names an issue that doesn't exist, or a kind lcl doesn't know".into(),
                    "issue_relations",
                )),
            }
        }
        rels.sort_by(|x, y| (x.0, x.1.as_str(), x.2).cmp(&(y.0, y.1.as_str(), y.2)));
        let mut graph = Graph::expecting(rels.iter().map(|&(from, kind, to)| (kind, from, to)));
        for (i, &(from, kind, to)) in rels.iter().enumerate() {
            if let Err(e) = graph.add(kind, from, to, i) {
                let typed = match kind {
                    Kind::Duplicate => "duplicate-of",
                    k => k.as_str(),
                };
                let why = match &e {
                    EdgeError::BlocksCycle { path } => format!(
                        "the blocks relations make a cycle ({})",
                        chain(&path.iter().map(|n| id(*n)).collect::<Vec<_>>(), " blocks ")
                    ),
                    EdgeError::DuplicateCycle { path } => format!(
                        "the duplicate-of relations make a cycle ({})",
                        chain(&path.iter().map(|n| id(*n)).collect::<Vec<_>>(), " -> ")
                    ),
                    EdgeError::SecondTarget { current, .. } => {
                        format!("{} is also a duplicate of {}", id(from), id(*current))
                    }
                    EdgeError::SelfRelation | EdgeError::Twice { .. } => {
                        "the row can't exist".into()
                    }
                };
                problems.push(format!(
                    "relation {} {typed} {}: {why}; fix it with lcl issue unrelate {} {typed} {}",
                    id(from),
                    id(to),
                    id(from),
                    id(to)
                ));
                continue;
            }
            counts.relations += 1;
            line(
                &mut out,
                &RelationLine {
                    r#type: "relation",
                    kind: kind.as_str(),
                    from,
                    to,
                },
            );
        }
    }

    {
        let mut stmt = conn.prepare("SELECT id, title, content, parent_id, created_at, updated_at FROM docs WHERE deleted_at IS NULL ORDER BY created_at, id")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, i64>(5)?,
            ))
        })?;
        let docs: Vec<_> = rows.collect::<std::result::Result<_, _>>()?;
        let parent_of: HashMap<&str, Option<&str>> = docs
            .iter()
            .map(|d| (d.0.as_str(), d.3.as_deref()))
            .collect();
        let doc_up = |n: &str| parent_of.get(n).copied().flatten();
        let looped_docs = on_loops(parent_of.keys().copied(), doc_up);
        for (did, title, content, parent, created, updated) in &docs {
            if parse::uuid(did).ok().as_deref() != Some(did.as_str()) {
                problems.push(by_hand(
                    format!("doc {did}"),
                    "its id is not a lowercase UUID".into(),
                    "docs.id",
                ));
            }
            if let Some(p) = parent {
                if !parent_of.contains_key(p.as_str()) {
                    problems.push(format!("doc {did}: parent {p} is not a doc; fix it with lcl doc update {did} --no-parent"));
                } else if looped_docs.contains(did.as_str())
                    && let Some(path) = loop_through(did.as_str(), doc_up)
                {
                    let path: Vec<String> = path.into_iter().map(String::from).collect();
                    problems.push(format!("doc {did}: its parents make a cycle ({}); fix it with lcl doc update {did} --no-parent", chain(&path, " -> ")));
                }
            }
            counts.docs += 1;
            line(
                &mut out,
                &DocLine {
                    r#type: "doc",
                    id: did.clone(),
                    title: title.clone(),
                    content: content.clone(),
                    parent: parent.clone(),
                    created_at: *created,
                    updated_at: *updated,
                },
            );
        }
    }

    if !problems.is_empty() {
        return Err(LclError::ExportBlocked { problems });
    }
    Ok((out, counts))
}

/// `s` as one shell word, for a command in a message.
fn shell(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./:@".contains(c))
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

/// Serializing each line type through one closure.
mod erased {
    pub trait Line {
        fn json(&self) -> String;
    }
    impl<T: serde::Serialize> Line for T {
        fn json(&self) -> String {
            serde_json::to_string(self).expect("a line of plain fields serializes")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loops(links: &[(i64, i64)]) -> Vec<i64> {
        let up: HashMap<i64, i64> = links.iter().copied().collect();
        let mut found: Vec<i64> = on_loops(up.keys().copied(), |n| up.get(&n).copied())
            .into_iter()
            .collect();
        found.sort();
        found
    }

    /// `on_loops` agrees with `loop_through` asked of every node.
    #[test]
    fn on_loops_finds_exactly_the_nodes_on_a_loop() {
        assert_eq!(loops(&[]), [] as [i64; 0]);
        assert_eq!(loops(&[(1, 2), (2, 3)]), [] as [i64; 0]);
        assert_eq!(loops(&[(1, 1)]), [1]);
        assert_eq!(loops(&[(1, 2), (2, 1)]), [1, 2]);
        // A tail into a loop is not on it; a dangling link ends a walk.
        assert_eq!(
            loops(&[(5, 4), (4, 1), (1, 2), (2, 3), (3, 1), (9, 8)]),
            [1, 2, 3]
        );
        // Two loops, and a chain that joins a walk already made.
        assert_eq!(
            loops(&[(1, 2), (2, 1), (3, 4), (4, 3), (6, 5), (7, 6), (5, 1)]),
            [1, 2, 3, 4]
        );
        let links = [(5, 4), (4, 1), (1, 2), (2, 3), (3, 1), (7, 7), (8, 9)];
        let up: HashMap<i64, i64> = links.iter().copied().collect();
        let one_by_one: Vec<i64> = (1..=9)
            .filter(|n| loop_through(*n, |n| up.get(&n).copied()).is_some())
            .collect();
        assert_eq!(loops(&links), one_by_one);
    }

    #[test]
    fn acyclic_tells_a_cycle_from_a_diamond() {
        assert!(acyclic([].into_iter()));
        assert!(acyclic([(1, 2), (1, 3), (2, 4), (3, 4)].into_iter()));
        assert!(acyclic([(1, 2), (1, 2)].into_iter()));
        assert!(!acyclic([(1, 1)].into_iter()));
        assert!(!acyclic([(1, 2), (2, 3), (3, 1), (4, 5)].into_iter()));
    }

    /// Knowing the rows make no cycle changes no answer `add` gives.
    #[test]
    fn a_graph_expecting_its_rows_answers_as_one_that_searches() {
        let sets: [&[(Kind, i64, i64)]; 4] = [
            &[
                (Kind::Blocks, 1, 2),
                (Kind::Blocks, 2, 3),
                (Kind::Blocks, 1, 3),
            ],
            &[
                (Kind::Blocks, 1, 2),
                (Kind::Blocks, 2, 3),
                (Kind::Blocks, 3, 1),
            ],
            &[
                (Kind::Duplicate, 1, 2),
                (Kind::Duplicate, 2, 3),
                (Kind::Duplicate, 1, 3),
            ],
            &[
                (Kind::Duplicate, 1, 2),
                (Kind::Duplicate, 2, 1),
                (Kind::Related, 2, 1),
            ],
        ];
        for rows in sets {
            let mut searching = Graph::default();
            let mut expecting = Graph::expecting(rows.iter().copied());
            for (at, &(kind, from, to)) in rows.iter().enumerate() {
                let a = searching
                    .add(kind, from, to, at)
                    .map_err(|e| format!("{e:?}"));
                let b = expecting
                    .add(kind, from, to, at)
                    .map_err(|e| format!("{e:?}"));
                assert_eq!(a, b, "{rows:?} row {at}");
            }
        }
    }

    #[test]
    fn a_long_chain_is_named_by_its_ends() {
        let steps = |n: usize| (1..=n).map(|i| i.to_string()).collect::<Vec<_>>();
        assert_eq!(chain(&steps(3), " -> "), "1 -> 2 -> 3");
        assert_eq!(chain(&steps(11), "-"), "1-2-3-4-5-6-7-8-9-10-11");
        assert_eq!(chain(&steps(12), "-"), "1-2-3-4-5-… 2 more …-8-9-10-11-12");
    }
}
