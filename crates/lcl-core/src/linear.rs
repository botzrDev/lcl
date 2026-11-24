//! Linear to `lcl-export/1` (LOCL-27): a pure conversion from one Linear
//! team's issues, as Linear's GraphQL API returns them, to the lines `lcl
//! import` reads. The import then does every check and every write, so
//! there is one way into a workspace.
//!
//! The input is the API's, not the CSV export's: the CSV carries no
//! comments (LOCL-26). Field names are the API's (`identifier`, `state {
//! name type }`, `labels { nodes { name } }`, ...).
//!
//! - A Linear identifier `KEY-N` becomes issue number N. When KEY is not
//!   the workspace's key, the identifier is also kept as the issue's ref.
//! - The status comes from the state's type: backlog and triage Backlog,
//!   unstarted Todo, started In Progress (In Review when the state's name
//!   says review), completed Done, canceled Canceled, duplicate Duplicate.
//!   Priorities are the same 0 to 4 in both. Estimates must be whole.
//! - People are named by their Linear display name.
//! - A value lcl would refuse is a `Problem` naming the Linear issue it
//!   came from; nothing is changed to make it fit. The one exception is
//!   whitespace around a name, which lcl trims: the name is trimmed and a
//!   note says so.
//! - What lcl doesn't carry is left out and listed in the notes: cycles,
//!   documents, issues in Linear's trash, links to issues of another team,
//!   and comment threads (a reply becomes a plain comment, in time order).

use crate::models::{Priority, Status};
use crate::parse;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, HashMap};

/// One team's issues, as fetched from Linear's API.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Export {
    pub team: Team,
    pub issues: Vec<Issue>,
    /// The team's documents, when the fetch brought them: not converted.
    #[serde(default)]
    pub documents: Vec<Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Team {
    pub key: String,
}

/// A connection's nodes: `{ "nodes": [...] }`.
#[derive(Debug, Clone, Deserialize)]
pub struct Nodes<T> {
    pub nodes: Vec<T>,
}

impl<T> Default for Nodes<T> {
    fn default() -> Self {
        Nodes { nodes: Vec::new() }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Issue {
    pub identifier: String,
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    /// 0 none, 1 urgent, 2 high, 3 medium, 4 low: lcl's scale.
    pub priority: f64,
    #[serde(default)]
    pub estimate: Option<f64>,
    /// YYYY-MM-DD.
    #[serde(default)]
    pub due_date: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub started_at: Option<String>,
    #[serde(default)]
    pub completed_at: Option<String>,
    #[serde(default)]
    pub canceled_at: Option<String>,
    #[serde(default)]
    pub trashed: Option<bool>,
    pub state: State,
    #[serde(default)]
    pub labels: Nodes<Named>,
    #[serde(default)]
    pub assignee: Option<User>,
    #[serde(default)]
    pub creator: Option<User>,
    #[serde(default)]
    pub project: Option<Named>,
    #[serde(default)]
    pub project_milestone: Option<Named>,
    #[serde(default)]
    pub parent: Option<IssueLink>,
    #[serde(default)]
    pub cycle: Option<Value>,
    #[serde(default)]
    pub comments: Nodes<Comment>,
    /// The relations this issue is the source of.
    #[serde(default)]
    pub relations: Nodes<Relation>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct State {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Named {
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct User {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub display_name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct IssueLink {
    pub identifier: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Comment {
    pub body: String,
    pub created_at: String,
    #[serde(default)]
    pub user: Option<User>,
    /// The thread's first comment, for a reply.
    #[serde(default)]
    pub parent: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Relation {
    /// blocks, duplicate, related, or the old similar.
    #[serde(rename = "type")]
    pub kind: String,
    pub related_issue: IssueLink,
}

/// A value lcl would refuse, and the Linear issue it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    /// A Linear identifier, or what else the value belongs to.
    pub issue: String,
    pub field: String,
    pub message: String,
}

impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}: {}", self.issue, self.field, self.message)
    }
}

/// The converted file, and what it left out or changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conversion {
    /// `lcl-export/1`, one record a line.
    pub text: String,
    pub notes: Vec<String>,
}

/// lcl's status for a Linear state.
fn status(state: &State) -> Option<Status> {
    Some(match state.kind.as_str() {
        "backlog" | "triage" => Status::Backlog,
        "unstarted" => Status::Todo,
        "started" if state.name.to_lowercase().contains("review") => Status::InReview,
        "started" => Status::InProgress,
        "completed" => Status::Done,
        "canceled" => Status::Canceled,
        "duplicate" => Status::Duplicate,
        _ => return None,
    })
}

/// An ISO 8601 time as Unix seconds.
fn seconds(at: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(at)
        .ok()
        .map(|t| t.timestamp())
}

/// `KEY-N` as its key and number.
fn split(identifier: &str) -> Option<(&str, i64)> {
    let (key, n) = identifier.rsplit_once('-')?;
    let n: i64 = n.parse().ok()?;
    (!key.is_empty() && n > 0).then_some((key, n))
}

/// Collects problems and notes for one conversion.
struct Report {
    problems: Vec<Problem>,
    notes: Vec<String>,
}

impl Report {
    fn problem(&mut self, issue: &str, field: &str, message: impl Into<String>) {
        self.problems.push(Problem {
            issue: issue.to_string(),
            field: field.to_string(),
            message: message.into(),
        });
    }

    /// `value` by `rule`, as lcl stores it: refused, a problem; trimmed, a
    /// note.
    fn name(
        &mut self,
        issue: &str,
        field: &str,
        value: &str,
        rule: fn(&str) -> Result<String, crate::models::InvalidValue>,
    ) -> Option<String> {
        match rule(value) {
            Err(e) => {
                self.problem(issue, field, e.0);
                None
            }
            Ok(v) => {
                if v != value {
                    self.notes
                        .push(format!("{issue}: {field} {value:?} trimmed to {v:?}"));
                }
                Some(v)
            }
        }
    }
}

/// A person's name for lcl: their display name, else their full name.
fn person(u: &Option<User>) -> Option<&str> {
    let u = u.as_ref()?;
    [u.display_name.as_str(), u.name.as_str()]
        .into_iter()
        .find(|n| !n.trim().is_empty())
}

/// `input` as an `lcl-export/1` file for a workspace with key
/// `workspace_key`, or every value in it lcl would refuse.
pub fn convert(input: &Export, workspace_key: &str) -> Result<Conversion, Vec<Problem>> {
    let mut r = Report {
        problems: Vec::new(),
        notes: Vec::new(),
    };
    let team = input.team.key.trim();
    let ours = team.eq_ignore_ascii_case(workspace_key);

    // Numbers first: parents and relations name issues by them.
    let live: Vec<&Issue> = input
        .issues
        .iter()
        .filter(|i| i.trashed != Some(true))
        .collect();
    let trashed = input.issues.len() - live.len();
    let mut number_of: HashMap<String, i64> = HashMap::new();
    for i in &live {
        match split(&i.identifier) {
            Some((key, n)) if key.eq_ignore_ascii_case(team) => {
                if number_of.insert(i.identifier.to_uppercase(), n).is_some() {
                    r.problem(&i.identifier, "identifier", "comes twice in the input");
                }
            }
            _ => r.problem(
                &i.identifier,
                "identifier",
                format!("is not an issue of team {team} (KEY-N)"),
            ),
        }
    }
    let number = |identifier: &str| number_of.get(&identifier.to_uppercase()).copied();

    // Labels and projects by name, ignoring case, as lcl compares them;
    // the first spelling met is kept.
    let mut labels: BTreeMap<String, String> = BTreeMap::new();
    let mut projects: BTreeMap<String, String> = BTreeMap::new();
    let mut milestones: BTreeMap<(String, String), (String, String)> = BTreeMap::new();
    let mut issues: BTreeMap<i64, Value> = BTreeMap::new();
    let mut comments: Vec<(i64, i64, usize, Value)> = Vec::new();
    let mut relations: BTreeMap<(String, i64, i64), ()> = BTreeMap::new();
    let (mut in_cycles, mut replies, mut elsewhere) = (0, 0, 0);

    for i in &live {
        let id = i.identifier.as_str();
        let Some(n) = number(id) else { continue };
        let mut rec = Map::new();
        rec.insert("type".into(), json!("issue"));
        rec.insert("number".into(), json!(n));
        match parse::title(&i.title) {
            Ok(t) => {
                rec.insert("title".into(), json!(t));
            }
            Err(e) => r.problem(id, "title", e.0),
        }
        if let Some(d) = i.description.as_deref().filter(|d| !d.is_empty()) {
            rec.insert("description".into(), json!(d));
        }
        match status(&i.state) {
            Some(s) => {
                rec.insert("status".into(), json!(s.as_str()));
            }
            None => r.problem(
                id,
                "state",
                format!(
                    "{:?} has type {:?}, which lcl doesn't know",
                    i.state.name, i.state.kind
                ),
            ),
        }
        let priority = (i.priority.fract() == 0.0)
            .then(|| Priority::from_stored(i.priority as i64))
            .flatten();
        match priority {
            Some(p) => {
                rec.insert("priority".into(), json!(p.label()));
            }
            None => r.problem(id, "priority", format!("{} isn't 0 to 4", i.priority)),
        }
        if let Some(e) = i.estimate {
            match (e.fract() == 0.0).then(|| parse::estimate(e as i64)) {
                Some(Ok(e)) => {
                    rec.insert("estimate".into(), json!(e));
                }
                Some(Err(err)) => r.problem(id, "estimate", err.0),
                None => r.problem(
                    id,
                    "estimate",
                    format!("{e} isn't a whole number; lcl estimates are whole points"),
                ),
            }
        }
        if let Some(d) = &i.due_date {
            match parse::date(d) {
                Ok(d) => {
                    rec.insert("due_date".into(), json!(d.to_string()));
                }
                Err(e) => r.problem(id, "dueDate", e.0),
            }
        }
        for (key, field, at) in [
            ("created_at", "createdAt", Some(&i.created_at)),
            ("updated_at", "updatedAt", Some(&i.updated_at)),
            ("started_at", "startedAt", i.started_at.as_ref()),
            ("completed_at", "completedAt", i.completed_at.as_ref()),
            ("canceled_at", "canceledAt", i.canceled_at.as_ref()),
        ] {
            if let Some(at) = at {
                match seconds(at) {
                    Some(s) => {
                        rec.insert(key.into(), json!(s));
                    }
                    None => r.problem(id, field, format!("{at:?} isn't an ISO 8601 time")),
                }
            }
        }
        if let Some(a) = person(&i.assignee)
            && let Some(a) = r.name(id, "assignee", a, parse::assignee)
        {
            rec.insert("assignee".into(), json!(a));
        }
        if let Some(c) = person(&i.creator)
            && let Some(c) = r.name(id, "creator", c, parse::author)
        {
            rec.insert("creator".into(), json!(c));
        }
        if !ours && let Some(re) = r.name(id, "ref", id, parse::issue_ref) {
            rec.insert("ref".into(), json!(re));
        }
        let mut names = Vec::new();
        for l in &i.labels.nodes {
            if let Some(l) = r.name(id, "label", &l.name, parse::label_name) {
                let kept = labels.entry(l.to_lowercase()).or_insert_with(|| l.clone());
                if !names.contains(kept) {
                    names.push(kept.clone());
                }
            }
        }
        if !names.is_empty() {
            rec.insert("labels".into(), json!(names));
        }
        let project = i
            .project
            .as_ref()
            .and_then(|p| r.name(id, "project", &p.name, parse::project_name))
            .map(|p| projects.entry(p.to_lowercase()).or_insert(p).clone());
        if let Some(p) = &project {
            rec.insert("project".into(), json!(p));
        }
        if let Some(m) = &i.project_milestone {
            match (
                &project,
                r.name(id, "projectMilestone", &m.name, parse::milestone_name),
            ) {
                (Some(p), Some(m)) => {
                    let (_, kept) = milestones
                        .entry((p.to_lowercase(), m.to_lowercase()))
                        .or_insert((p.clone(), m));
                    rec.insert("milestone".into(), json!(kept));
                }
                (None, Some(_)) => r.problem(id, "projectMilestone", "has no project"),
                _ => {}
            }
        }
        if let Some(p) = &i.parent {
            match number(&p.identifier) {
                Some(pn) => {
                    rec.insert("parent".into(), json!(pn));
                }
                None => {
                    elsewhere += 1;
                    r.notes.push(format!(
                        "{id}: its parent {} is not one of these issues, and is left out",
                        p.identifier
                    ));
                }
            }
        }
        if i.cycle.is_some() {
            in_cycles += 1;
        }
        issues.insert(n, Value::Object(rec));

        for (k, c) in i.comments.nodes.iter().enumerate() {
            let Ok(body) = parse::comment(&c.body) else {
                r.problem(
                    id,
                    "comment",
                    format!("the comment of {} is blank", c.created_at),
                );
                continue;
            };
            let Some(at) = seconds(&c.created_at) else {
                r.problem(
                    id,
                    "comment",
                    format!("{:?} isn't an ISO 8601 time", c.created_at),
                );
                continue;
            };
            if c.parent.is_some() {
                replies += 1;
            }
            let mut rec = json!({"type": "comment", "issue": n, "body": body, "created_at": at});
            if let Some(a) = person(&c.user)
                && let Some(a) = r.name(id, "comment author", a, parse::author)
            {
                rec["author"] = json!(a);
            }
            comments.push((n, at, k, rec));
        }
        for rel in &i.relations.nodes {
            let Some(other) = number(&rel.related_issue.identifier) else {
                elsewhere += 1;
                r.notes.push(format!(
                    "{id}: its {} relation to {} is left out: that issue is not one of these",
                    rel.kind, rel.related_issue.identifier
                ));
                continue;
            };
            let key = match rel.kind.as_str() {
                "blocks" => ("blocks", n, other),
                "duplicate" => ("duplicate", n, other),
                "related" | "similar" => ("related", n.min(other), n.max(other)),
                kind => {
                    r.problem(id, "relation", format!("type {kind:?} isn't one lcl knows"));
                    continue;
                }
            };
            relations.insert((key.0.to_string(), key.1, key.2), ());
        }
    }

    if !r.problems.is_empty() {
        return Err(r.problems);
    }

    // The lines, and what each came from, for the import's own check.
    let mut lines: Vec<(String, Value)> = vec![(
        "the file".into(),
        json!({"type": "workspace", "format": "lcl-export/1", "key": workspace_key}),
    )];
    lines.extend(
        labels
            .values()
            .map(|l| (format!("label {l:?}"), json!({"type": "label", "name": l}))),
    );
    lines.extend(projects.values().map(|p| {
        (
            format!("project {p:?}"),
            json!({"type": "project", "name": p}),
        )
    }));
    lines.extend(milestones.values().map(|(p, m)| {
        (
            format!("milestone {m:?} of project {p:?}"),
            json!({"type": "milestone", "project": p, "name": m}),
        )
    }));
    let identifier = |n: i64| format!("{team}-{n}");
    lines.extend(issues.into_iter().map(|(n, rec)| (identifier(n), rec)));
    comments.sort_by_key(|(n, at, k, _)| (*n, *at, *k));
    lines.extend(
        comments
            .into_iter()
            .map(|(n, _, _, rec)| (format!("{}'s comment", identifier(n)), rec)),
    );
    lines.extend(relations.into_keys().map(|(kind, from, to)| {
        (
            format!(
                "{}'s {kind} relation to {}",
                identifier(from),
                identifier(to)
            ),
            json!({"type": "relation", "kind": kind, "from": from, "to": to}),
        )
    }));
    let text: String = lines.iter().map(|(_, rec)| format!("{rec}\n")).collect();

    if let Err(e) = crate::transfer::check_file(text.as_bytes()) {
        let problem = match e {
            crate::error::LclError::ImportLine { line, what, msg } => Problem {
                issue: lines
                    .get(line.wrapping_sub(1))
                    .map_or_else(|| "the file".to_string(), |(from, _)| from.clone()),
                field: what,
                message: msg,
            },
            other => Problem {
                issue: "the file".into(),
                field: "file".into(),
                message: other.to_string(),
            },
        };
        return Err(vec![problem]);
    }

    let mut notes = r.notes;
    if trashed > 0 {
        notes.push(format!("{trashed} issues in Linear's trash are left out"));
    }
    if in_cycles > 0 {
        notes.push(format!(
            "{in_cycles} issues are in Linear cycles: cycles are not converted"
        ));
    }
    if !input.documents.is_empty() {
        notes.push(format!(
            "{} documents are not converted",
            input.documents.len()
        ));
    }
    if replies > 0 {
        notes.push(format!(
            "{replies} replies in comment threads become plain comments, in time order"
        ));
    }
    if elsewhere > 0 {
        notes.push(format!(
            "{elsewhere} links to issues that are not among these are left out"
        ));
    }
    Ok(Conversion { text, notes })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An issue as the API returns it, with `extra` fields over the
    /// defaults.
    fn issue(identifier: &str, extra: Value) -> Value {
        let mut base = json!({
            "identifier": identifier,
            "title": format!("Issue {identifier}"),
            "description": null,
            "priority": 0,
            "estimate": null,
            "dueDate": null,
            "createdAt": "2026-11-17T14:31:44.372Z",
            "updatedAt": "2026-11-18T09:00:00.000Z",
            "startedAt": null,
            "completedAt": null,
            "canceledAt": null,
            "trashed": null,
            "state": {"name": "Backlog", "type": "backlog"},
            "labels": {"nodes": []},
            "assignee": null,
            "creator": {"name": "Ann Example", "displayName": "ann", "email": "ann@example.com"},
            "project": null,
            "projectMilestone": null,
            "parent": null,
            "cycle": null,
            "comments": {"nodes": []},
            "relations": {"nodes": []},
        });
        base.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        base
    }

    fn export(issues: Vec<Value>) -> Export {
        serde_json::from_value(json!({"team": {"key": "SMM"}, "issues": issues})).unwrap()
    }

    fn records(c: &Conversion) -> Vec<Value> {
        c.text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    fn of_type<'a>(recs: &'a [Value], t: &str) -> Vec<&'a Value> {
        recs.iter().filter(|r| r["type"] == t).collect()
    }

    fn rich() -> Export {
        export(vec![
            issue(
                "SMM-4",
                json!({
                    "title": "Social batch",
                    "description": "Line one\n\nLine two",
                    "priority": 2,
                    "estimate": 3,
                    "dueDate": "2026-11-24",
                    "startedAt": "2026-11-21T13:42:56.392Z",
                    "state": {"name": "In Review", "type": "started"},
                    "labels": {"nodes": [{"name": "content"}, {"name": "Needs-Human"}]},
                    "assignee": {"name": "P. Green", "displayName": "pag", "email": "p@example.com"},
                    "project": {"name": "buffer-management"},
                    "projectMilestone": {"name": "Nov 2026 goals"},
                    "comments": {"nodes": [
                        {"body": "Second", "createdAt": "2026-11-21T10:00:00.000Z",
                         "user": {"name": "Bo", "displayName": "bo"}, "parent": {"id": "c1"}},
                        {"body": "First", "createdAt": "2026-11-20T10:00:00.000Z",
                         "user": null, "parent": null},
                    ]},
                    "relations": {"nodes": [
                        {"type": "related", "relatedIssue": {"identifier": "SMM-5"}},
                        {"type": "blocks", "relatedIssue": {"identifier": "SMM-5"}},
                    ]},
                }),
            ),
            issue(
                "SMM-5",
                json!({
                    "state": {"name": "Shipped", "type": "completed"},
                    "completedAt": "2026-11-22T08:00:00.000Z",
                    "labels": {"nodes": [{"name": "needs-human"}]},
                    "parent": {"identifier": "SMM-4"},
                    "relations": {"nodes": [
                        {"type": "related", "relatedIssue": {"identifier": "SMM-4"}},
                        {"type": "similar", "relatedIssue": {"identifier": "JOB-1"}},
                    ]},
                }),
            ),
            issue(
                "SMM-9",
                json!({"state": {"name": "Dupe", "type": "duplicate"},
                       "relations": {"nodes": [{"type": "duplicate", "relatedIssue": {"identifier": "SMM-4"}}]},
                       "cycle": {"number": 3}}),
            ),
            issue("SMM-7", json!({"trashed": true})),
        ])
    }

    #[test]
    fn issues_labels_projects_comments_and_relations_are_converted() {
        let c = convert(&rich(), "SMM").unwrap();
        let recs = records(&c);
        assert_eq!(
            recs[0],
            json!({"type": "workspace", "format": "lcl-export/1", "key": "SMM"})
        );
        // Labels by name ignoring case, the first spelling kept.
        let labels: Vec<&Value> = of_type(&recs, "label")
            .into_iter()
            .map(|l| &l["name"])
            .collect();
        assert_eq!(labels, [&json!("content"), &json!("Needs-Human")]);
        assert_eq!(
            of_type(&recs, "milestone"),
            [
                &json!({"type": "milestone", "project": "buffer-management", "name": "Nov 2026 goals"})
            ]
        );
        let issues = of_type(&recs, "issue");
        // The key is the workspace's: numbers, no refs. SMM-7 is in the trash.
        assert_eq!(
            issues[0],
            &json!({
                "type": "issue", "number": 4, "title": "Social batch",
                "description": "Line one\n\nLine two", "status": "InReview",
                "priority": "High", "estimate": 3, "due_date": "2026-11-24",
                "created_at": 1_794_925_904, "updated_at": 1_794_992_400,
                "started_at": 1_795_268_576, "assignee": "pag", "creator": "ann",
                "labels": ["content", "Needs-Human"], "project": "buffer-management",
                "milestone": "Nov 2026 goals",
            })
        );
        let numbers: Vec<&Value> = issues.iter().map(|i| &i["number"]).collect();
        assert_eq!(numbers, [&json!(4), &json!(5), &json!(9)]);
        assert_eq!(
            (
                &issues[1]["status"],
                &issues[1]["parent"],
                &issues[1]["labels"]
            ),
            (&json!("Done"), &json!(4), &json!(["Needs-Human"]))
        );
        assert_eq!(issues[2]["status"], "Duplicate");
        // Comments oldest first; one with no user has no author.
        assert_eq!(
            of_type(&recs, "comment"),
            [
                &json!({"type": "comment", "issue": 4, "body": "First", "created_at": 1_795_168_800}),
                &json!({"type": "comment", "issue": 4, "body": "Second", "author": "bo", "created_at": 1_795_255_200}),
            ]
        );
        // "related" both ways is one relation.
        assert_eq!(
            of_type(&recs, "relation"),
            [
                &json!({"type": "relation", "kind": "blocks", "from": 4, "to": 5}),
                &json!({"type": "relation", "kind": "duplicate", "from": 9, "to": 4}),
                &json!({"type": "relation", "kind": "related", "from": 4, "to": 5}),
            ]
        );
        assert_eq!(
            c.notes,
            [
                "SMM-5: its similar relation to JOB-1 is left out: that issue is not one of these",
                "1 issues in Linear's trash are left out",
                "1 issues are in Linear cycles: cycles are not converted",
                "1 replies in comment threads become plain comments, in time order",
                "1 links to issues that are not among these are left out",
            ]
        );
    }

    #[test]
    fn another_key_keeps_the_identifier_as_the_ref() {
        let c = convert(&rich(), "DEMO").unwrap();
        let recs = records(&c);
        assert_eq!(recs[0]["key"], "DEMO");
        let issues = of_type(&recs, "issue");
        assert_eq!(
            (&issues[0]["number"], &issues[0]["ref"]),
            (&json!(4), &json!("SMM-4"))
        );
    }

    #[test]
    fn statuses_come_from_the_state_type() {
        let cases = [
            ("triage", "Triage", Some(Status::Backlog)),
            ("backlog", "Icebox", Some(Status::Backlog)),
            ("unstarted", "Ready", Some(Status::Todo)),
            ("started", "Doing", Some(Status::InProgress)),
            ("started", "Code review", Some(Status::InReview)),
            ("completed", "Shipped", Some(Status::Done)),
            ("canceled", "Won't do", Some(Status::Canceled)),
            ("duplicate", "Duplicate", Some(Status::Duplicate)),
            ("archived", "Old", None),
        ];
        for (kind, name, want) in cases {
            let state = State {
                name: name.into(),
                kind: kind.into(),
            };
            assert_eq!(status(&state), want, "{kind} {name}");
        }
    }

    #[test]
    fn values_lcl_would_refuse_are_reported_by_issue() {
        let input = export(vec![
            issue("SMM-1", json!({"estimate": 0.5, "priority": 2.5})),
            issue("SMM-2", json!({"title": "  ", "dueDate": "2026-02-30"})),
            issue(
                "SMM-3",
                json!({"state": {"name": "Gone", "type": "archived"},
                                  "createdAt": "yesterday"}),
            ),
            issue("OTHER-4", json!({})),
            issue(
                "SMM-5",
                json!({"labels": {"nodes": [{"name": " "}]},
                                  "comments": {"nodes": [{"body": " ", "createdAt": "2026-11-20T10:00:00Z"}]}}),
            ),
            issue("SMM-5", json!({})),
        ]);
        let problems: Vec<String> = convert(&input, "SMM")
            .unwrap_err()
            .iter()
            .map(|p| p.to_string())
            .collect();
        assert_eq!(
            problems,
            [
                "OTHER-4: identifier: is not an issue of team SMM (KEY-N)",
                "SMM-5: identifier: comes twice in the input",
                "SMM-1: priority: 2.5 isn't 0 to 4",
                "SMM-1: estimate: 0.5 isn't a whole number; lcl estimates are whole points",
                "SMM-2: title: a title can't be blank",
                "SMM-2: dueDate: invalid date \"2026-02-30\"; expected a real calendar date as YYYY-MM-DD",
                "SMM-3: state: \"Gone\" has type \"archived\", which lcl doesn't know",
                "SMM-3: createdAt: \"yesterday\" isn't an ISO 8601 time",
                "SMM-5: label: a label name can't be empty",
                "SMM-5: comment: the comment of 2026-11-20T10:00:00Z is blank",
            ]
        );
    }

    #[test]
    fn whitespace_around_a_name_is_trimmed_and_noted() {
        let input = export(vec![issue(
            "SMM-1",
            json!({"labels": {"nodes": [{"name": " bug "}]},
                   "assignee": {"name": "", "displayName": "ann "}}),
        )]);
        let c = convert(&input, "SMM").unwrap();
        let recs = records(&c);
        assert_eq!(
            (
                &of_type(&recs, "issue")[0]["labels"],
                &of_type(&recs, "issue")[0]["assignee"]
            ),
            (&json!(["bug"]), &json!("ann"))
        );
        assert_eq!(
            c.notes,
            [
                "SMM-1: assignee \"ann \" trimmed to \"ann\"",
                "SMM-1: label \" bug \" trimmed to \"bug\"",
            ]
        );
    }

    #[test]
    fn what_the_import_refuses_is_named_by_its_issue() {
        // An issue that blocks itself passes every field check; the import's
        // own check refuses it, and the problem names the Linear issue.
        let input = export(vec![issue(
            "SMM-1",
            json!({"relations": {"nodes": [{"type": "blocks", "relatedIssue": {"identifier": "SMM-1"}}]}}),
        )]);
        let problems = convert(&input, "SMM").unwrap_err();
        assert_eq!(problems.len(), 1);
        assert_eq!(
            problems[0].issue, "SMM-1's blocks relation to SMM-1",
            "{problems:?}"
        );
    }

    #[test]
    fn documents_and_an_empty_team() {
        let input: Export = serde_json::from_value(
            json!({"team": {"key": "SMM"}, "issues": [], "documents": [{"title": "Notes"}]}),
        )
        .unwrap();
        let c = convert(&input, "SMM").unwrap();
        assert_eq!(
            c.text,
            "{\"format\":\"lcl-export/1\",\"key\":\"SMM\",\"type\":\"workspace\"}\n"
        );
        assert_eq!(c.notes, ["1 documents are not converted"]);
    }

    #[test]
    fn the_output_passes_an_import_dry_run() {
        let mut store = crate::db::Store::open_in_memory().unwrap();
        store.set_meta(&[("name", "smm"), ("key", "SMM")]).unwrap();
        let c = convert(&rich(), "SMM").unwrap();
        let summary = store.import(c.text.as_bytes(), true).unwrap();
        assert!(summary.dry_run);
        assert_eq!(
            (
                summary.counts.issues,
                summary.counts.comments,
                summary.counts.relations,
                summary.counts.labels
            ),
            (3, 2, 3, 2)
        );
    }
}
