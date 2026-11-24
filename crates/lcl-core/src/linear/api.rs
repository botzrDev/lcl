//! Fetching one Linear team's issues from Linear's GraphQL API (LOCL-28),
//! as the records `super::convert` reads. Only `lcl import --linear` reaches
//! Linear, and it only reads there.
//!
//! The key is a personal API key, read by the caller from `KEY_VAR` and
//! sent as the Authorization header as it is (Linear's form for a personal
//! key; an OAuth token goes as `Bearer TOKEN`). It is never stored.
//!
//! A query may cost at most 10,000 of Linear's complexity points, and a
//! connection multiplies what is inside it by its page size. So issues come
//! `ISSUES_PAGE` at a time with the first `NESTED_PAGE` of each issue's
//! labels, comments and relations (about 150 points an issue), and an issue
//! with more gets the rest by queries of its own.

use super::{Comment, Export, Issue, Named, Nodes, PageInfo, Relation, Team};
use crate::error::{LclError, Result};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::time::Duration;

/// The environment variable that holds the API key.
pub const KEY_VAR: &str = "LINEAR_API_KEY";
/// Linear's API.
pub const ENDPOINT: &str = "https://api.linear.app/graphql";
/// Names another address for it, for a proxy or a test.
pub const ENDPOINT_VAR: &str = "LCL_LINEAR_API";

const TIMEOUT: Duration = Duration::from_secs(60);
const ISSUES_PAGE: usize = 40;
const NESTED_PAGE: usize = 20;
/// For the rest of one issue's connection, or of a project's documents.
const MORE_PAGE: usize = 100;

const LABEL: &str = "name";
const COMMENT: &str = "body createdAt user { name displayName } parent { id }";
const RELATION: &str = "type relatedIssue { identifier }";

/// Linear's API, with a key.
pub struct Client {
    endpoint: String,
    key: String,
    agent: ureq::Agent,
}

/// The value at `path` in an answer's data.
fn at<'a>(data: &'a Value, path: &[&str]) -> &'a Value {
    path.iter().fold(data, |v, k| &v[*k])
}

/// The cursor to go on from, when a connection has more.
fn more<T>(c: &Nodes<T>) -> Option<String> {
    match &c.page_info {
        Some(PageInfo {
            has_next_page: true,
            end_cursor,
        }) => end_cursor.clone(),
        _ => None,
    }
}

fn unreadable(e: serde_json::Error) -> LclError {
    LclError::LinearRefused {
        message: format!("an answer this lcl can't read: {e}"),
    }
}

impl Client {
    /// A client for the API at `endpoint` (`https://…`; `http://` only for
    /// localhost and 127.0.0.1) with `key`. A bad address is checked first.
    pub fn new(endpoint: &str, key: Option<String>) -> Result<Self> {
        let endpoint = endpoint.trim().to_string();
        let local = ["http://localhost", "http://127.0.0.1"].iter().any(|p| {
            endpoint
                .strip_prefix(p)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with([':', '/']))
        });
        if !(endpoint.starts_with("https://") && endpoint.len() > "https://".len() || local) {
            return Err(LclError::LinearAddress(endpoint));
        }
        let key = key
            .map(|k| k.trim().to_string())
            .filter(|k| !k.is_empty())
            .ok_or(LclError::LinearNoKey)?;
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(TIMEOUT))
            .http_status_as_error(false)
            .build()
            .into();
        Ok(Client {
            endpoint,
            key,
            agent,
        })
    }

    /// The `data` of `query`'s answer, or the error Linear gave.
    fn query(&self, query: &str, variables: Value) -> Result<Value> {
        let mut response = self
            .agent
            .post(&self.endpoint)
            .header("Authorization", &self.key)
            .header("User-Agent", format!("lcl/{}", env!("CARGO_PKG_VERSION")))
            .send_json(json!({"query": query, "variables": variables}))
            .map_err(|e| LclError::LinearNetwork {
                detail: e.to_string(),
            })?;
        let status = response.status().as_u16();
        let retry_after = response
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok());
        let body: Option<Value> = response.body_mut().read_json().ok();
        let error = body.as_ref().and_then(|b| b["errors"].get(0));
        let code = error
            .and_then(|e| e["extensions"]["code"].as_str())
            .unwrap_or("");
        let message = error
            .and_then(|e| e["message"].as_str())
            .unwrap_or("no message")
            .to_string();
        if status == 401 || status == 403 || code == "AUTHENTICATION_ERROR" {
            return Err(LclError::LinearAuth { message });
        }
        if status == 429 || code == "RATELIMITED" {
            return Err(LclError::LinearRateLimited {
                seconds: retry_after,
            });
        }
        if status >= 500 {
            return Err(LclError::LinearNetwork {
                detail: format!("it answered {status}: {message}"),
            });
        }
        match body {
            Some(b) if error.is_none() && status < 400 && b["data"].is_object() => {
                Ok(b["data"].clone())
            }
            Some(_) => Err(LclError::LinearRefused {
                message: format!("{message} ({status})"),
            }),
            None => Err(LclError::LinearNetwork {
                detail: format!("an answer that isn't JSON ({status})"),
            }),
        }
    }

    /// Every node of the connection at `path` in `query`'s answers, page by
    /// page from `after`. `query` takes `$after`.
    fn pages<T: DeserializeOwned>(
        &self,
        query: &str,
        mut variables: Value,
        path: &[&str],
        mut after: Option<String>,
    ) -> Result<Vec<T>> {
        let mut all = Vec::new();
        loop {
            variables["after"] = json!(after);
            let data = self.query(query, variables.clone())?;
            let page: Nodes<T> =
                serde_json::from_value(at(&data, path).clone()).map_err(unreadable)?;
            all.extend(page.nodes);
            match page.page_info {
                Some(PageInfo {
                    has_next_page: true,
                    end_cursor: Some(c),
                }) => after = Some(c),
                _ => return Ok(all),
            }
        }
    }

    /// Team `key`'s issues, archived ones too (Linear archives what has
    /// been done a while), and the documents of its projects.
    pub fn fetch(&self, key: &str) -> Result<Export> {
        let key = key.trim();
        let data = self.query(
            "query Team($key: String!) {
               teams(filter: { key: { eqIgnoreCase: $key } }) { nodes { id key } }
             }",
            json!({"key": key}),
        )?;
        let team = &data["teams"]["nodes"][0];
        let (Some(id), Some(team_key)) = (team["id"].as_str(), team["key"].as_str()) else {
            return Err(LclError::LinearTeamNotFound(key.to_string()));
        };
        let issues_query = format!(
            "query Issues($team: String!, $after: String) {{
               team(id: $team) {{
                 issues(first: {ISSUES_PAGE}, after: $after, includeArchived: true) {{
                   pageInfo {{ hasNextPage endCursor }}
                   nodes {{
                     id identifier title description priority estimate dueDate
                     createdAt updatedAt startedAt completedAt canceledAt trashed
                     state {{ name type }}
                     assignee {{ name displayName }} creator {{ name displayName }}
                     project {{ name }} projectMilestone {{ name }}
                     parent {{ identifier }} cycle {{ id }}
                     labels(first: {NESTED_PAGE}) {{ pageInfo {{ hasNextPage endCursor }} nodes {{ {LABEL} }} }}
                     comments(first: {NESTED_PAGE}) {{ pageInfo {{ hasNextPage endCursor }} nodes {{ {COMMENT} }} }}
                     relations(first: {NESTED_PAGE}) {{ pageInfo {{ hasNextPage endCursor }} nodes {{ {RELATION} }} }}
                   }}
                 }}
               }}
             }}"
        );
        let mut issues: Vec<Issue> = self.pages(
            &issues_query,
            json!({"team": id}),
            &["team", "issues"],
            None,
        )?;
        for issue in &mut issues {
            self.complete(issue)?;
        }
        let documents = self.documents(id)?;
        Ok(Export {
            team: Team {
                key: team_key.to_string(),
            },
            issues,
            documents,
        })
    }

    /// The rest of `issue`'s labels, comments and relations, when the first
    /// page didn't hold them all.
    fn complete(&self, issue: &mut Issue) -> Result<()> {
        let Some(id) = issue.id.clone() else {
            return Ok(());
        };
        if let Some(after) = more(&issue.labels) {
            let rest: Vec<Named> = self.rest(&id, "labels", LABEL, after)?;
            issue.labels.nodes.extend(rest);
        }
        if let Some(after) = more(&issue.comments) {
            let rest: Vec<Comment> = self.rest(&id, "comments", COMMENT, after)?;
            issue.comments.nodes.extend(rest);
        }
        if let Some(after) = more(&issue.relations) {
            let rest: Vec<Relation> = self.rest(&id, "relations", RELATION, after)?;
            issue.relations.nodes.extend(rest);
        }
        Ok(())
    }

    /// Issue `id`'s connection `field` (its nodes with `fields`), after
    /// cursor `after`.
    fn rest<T: DeserializeOwned>(
        &self,
        id: &str,
        field: &str,
        fields: &str,
        after: String,
    ) -> Result<Vec<T>> {
        let query = format!(
            "query Rest($id: String!, $after: String) {{
               issue(id: $id) {{
                 {field}(first: {MORE_PAGE}, after: $after) {{
                   pageInfo {{ hasNextPage endCursor }} nodes {{ {fields} }}
                 }}
               }}
             }}"
        );
        self.pages(&query, json!({"id": id}), &["issue", field], Some(after))
    }

    /// The documents of team `id`'s projects: they are not converted, but
    /// the conversion says how many there are.
    fn documents(&self, id: &str) -> Result<Vec<Value>> {
        let query = format!(
            "query Projects($team: String!, $after: String) {{
               team(id: $team) {{
                 projects(first: {NESTED_PAGE}, after: $after, includeArchived: true) {{
                   pageInfo {{ hasNextPage endCursor }}
                   nodes {{
                     id
                     documents(first: {NESTED_PAGE}) {{ pageInfo {{ hasNextPage endCursor }} nodes {{ id }} }}
                   }}
                 }}
               }}
             }}"
        );
        let projects: Vec<Value> =
            self.pages(&query, json!({"team": id}), &["team", "projects"], None)?;
        let more_query = format!(
            "query Documents($id: String!, $after: String) {{
               project(id: $id) {{
                 documents(first: {MORE_PAGE}, after: $after) {{
                   pageInfo {{ hasNextPage endCursor }} nodes {{ id }}
                 }}
               }}
             }}"
        );
        let mut documents = Vec::new();
        for p in projects {
            let first: Nodes<Value> =
                serde_json::from_value(p["documents"].clone()).map_err(unreadable)?;
            let after = more(&first);
            documents.extend(first.nodes);
            if let (Some(after), Some(project)) = (after, p["id"].as_str()) {
                let rest: Vec<Value> = self.pages(
                    &more_query,
                    json!({"id": project}),
                    &["project", "documents"],
                    Some(after),
                )?;
                documents.extend(rest);
            }
        }
        Ok(documents)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_address_must_be_https_or_local_and_a_key_is_needed() {
        let key = || Some("k".to_string());
        assert!(Client::new(ENDPOINT, key()).is_ok());
        assert!(Client::new("http://127.0.0.1:8080/graphql", key()).is_ok());
        for bad in [
            "http://api.linear.app/graphql",
            "https://",
            "ftp://x",
            "http://localhost.example",
        ] {
            assert!(
                matches!(Client::new(bad, key()), Err(LclError::LinearAddress(_))),
                "{bad}"
            );
        }
        for none in [None, Some("  ".to_string())] {
            assert!(matches!(
                Client::new(ENDPOINT, none),
                Err(LclError::LinearNoKey)
            ));
        }
    }
}
