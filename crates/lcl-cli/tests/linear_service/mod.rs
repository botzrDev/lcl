//! A stand-in for Linear's GraphQL API on 127.0.0.1, for `lcl import
//! --linear`: it answers the five queries lcl sends (Team, Issues, Rest,
//! Projects, Documents) from one team's issues, held as API records, a
//! page at a time as Linear does. Its pages are `page` long whatever
//! `first` asks, so a small team still pages. It checks the key as Linear
//! does, answering 401 with Linear's error.
#![allow(dead_code)]

use crate::common::{Env, Out};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;

/// The one key it takes.
pub const KEY: &str = "lin_api_test";

#[derive(Debug, Default)]
pub struct State {
    /// `{"id", "key"}`.
    pub team: Value,
    /// As the API returns an issue, except that labels, comments and
    /// relations are plain arrays: it pages them.
    pub issues: Vec<Value>,
    /// `{"id", "documents": [{"id"}]}`.
    pub projects: Vec<Value>,
    pub page: usize,
    /// Each query's operation name, in order.
    pub queries: Vec<String>,
}

pub struct Linear {
    pub url: String,
    state: Arc<Mutex<State>>,
    server: Arc<tiny_http::Server>,
    thread: Option<JoinHandle<()>>,
}

impl Linear {
    /// Team `key` with `issues`, paged `page` at a time.
    pub fn start(key: &str, issues: Vec<Value>, page: usize) -> Self {
        let server = Arc::new(tiny_http::Server::http("127.0.0.1:0").expect("bind"));
        let port = server.server_addr().to_ip().unwrap().port();
        let state = Arc::new(Mutex::new(State {
            team: json!({"id": "team-1", "key": key}),
            issues,
            page,
            ..State::default()
        }));
        let thread = {
            let (server, state) = (server.clone(), state.clone());
            std::thread::spawn(move || {
                for request in server.incoming_requests() {
                    handle(&state, request);
                }
            })
        };
        Linear {
            url: format!("http://127.0.0.1:{port}/graphql"),
            state,
            server,
            thread: Some(thread),
        }
    }

    pub fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap()
    }

    /// `lcl ARGS` against this service, with `key` in LINEAR_API_KEY.
    pub fn lcl_with_key(&self, env: &Env, key: Option<&str>, args: &[&str]) -> Out {
        let url = self.url.clone();
        let key = key.map(str::to_string);
        env.lcl_with(args, move |cmd| {
            cmd.env("LCL_LINEAR_API", url);
            match key {
                Some(k) => cmd.env("LINEAR_API_KEY", k),
                None => cmd.env_remove("LINEAR_API_KEY"),
            };
        })
    }

    /// `lcl ARGS` against this service with its key.
    pub fn lcl(&self, env: &Env, args: &[&str]) -> Out {
        self.lcl_with_key(env, Some(KEY), args)
    }
}

impl Drop for Linear {
    fn drop(&mut self) {
        self.server.unblock();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// An issue as the API returns it, with `extra` over the defaults.
pub fn issue(identifier: &str, extra: Value) -> Value {
    let n: u32 = identifier.rsplit('-').next().unwrap().parse().unwrap();
    let mut base = json!({
        "id": format!("issue-{identifier}"),
        "identifier": identifier,
        "title": format!("Issue {n}"),
        "description": null,
        "priority": 0,
        "estimate": null,
        "dueDate": null,
        "createdAt": format!("2026-11-{:02}T10:00:00.000Z", 1 + n % 20),
        "updatedAt": "2026-11-22T10:00:00.000Z",
        "startedAt": null,
        "completedAt": null,
        "canceledAt": null,
        "trashed": null,
        "state": {"name": "Todo", "type": "unstarted"},
        "assignee": null,
        "creator": {"name": "Ann Example", "displayName": "ann"},
        "project": null,
        "projectMilestone": null,
        "parent": null,
        "cycle": null,
        "labels": [],
        "comments": [],
        "relations": [],
    });
    base.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    base
}

/// A comment as the API returns it.
pub fn comment(body: &str, at: &str) -> Value {
    json!({"body": body, "createdAt": at, "user": {"name": "Bo Example", "displayName": "bo"}, "parent": null})
}

/// Up to `page` of `items` after cursor `after`, as a connection.
fn connection(items: &[Value], after: &Value, page: usize) -> Value {
    let start: usize = after.as_str().map_or(0, |a| a.parse().unwrap());
    let end = (start + page).min(items.len());
    json!({
        "pageInfo": {"hasNextPage": end < items.len(), "endCursor": end.to_string()},
        "nodes": items[start..end],
    })
}

fn handle(state: &Mutex<State>, mut request: tiny_http::Request) {
    let mut body = String::new();
    let _ = request.as_reader().read_to_string(&mut body);
    let key = request
        .headers()
        .iter()
        .find(|h| h.field.equiv("Authorization"))
        .map(|h| h.value.as_str().to_string());
    let (status, reply) = {
        let mut s = state.lock().unwrap();
        answer(&mut s, key.as_deref(), &body)
    };
    let content_type =
        tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap();
    let _ = request.respond(
        tiny_http::Response::from_string(reply.to_string())
            .with_status_code(status)
            .with_header(content_type),
    );
}

fn answer(s: &mut State, key: Option<&str>, body: &str) -> (u16, Value) {
    if key != Some(KEY) {
        return (
            401,
            json!({"errors": [{"message": "Authentication required, not authenticated",
                "extensions": {"type": "authentication error", "code": "AUTHENTICATION_ERROR"}}]}),
        );
    }
    let body: Value = serde_json::from_str(body).expect("a JSON request");
    let query = body["query"].as_str().expect("a query");
    let vars = &body["variables"];
    let name = query
        .trim()
        .strip_prefix("query ")
        .and_then(|q| q.split('(').next())
        .expect("a named query")
        .to_string();
    s.queries.push(name.clone());
    let issue_by_id = |id: &Value| {
        s.issues
            .iter()
            .find(|i| &i["id"] == id)
            .expect("an issue it has")
            .clone()
    };
    let data = match name.as_str() {
        "Team" => {
            let wanted = vars["key"].as_str().unwrap().to_lowercase();
            let nodes: Vec<&Value> = std::iter::once(&s.team)
                .filter(|t| t["key"].as_str().unwrap().to_lowercase() == wanted)
                .collect();
            json!({"teams": {"nodes": nodes}})
        }
        "Issues" => {
            assert_eq!(vars["team"], s.team["id"]);
            let shown: Vec<Value> = s
                .issues
                .iter()
                .map(|i| {
                    let mut i = i.clone();
                    for field in ["labels", "comments", "relations"] {
                        let all = i[field].as_array().unwrap().clone();
                        i[field] = connection(&all, &Value::Null, s.page);
                    }
                    i
                })
                .collect();
            json!({"team": {"issues": connection(&shown, &vars["after"], s.page)}})
        }
        "Rest" => {
            let issue = issue_by_id(&vars["id"]);
            let field = ["labels", "comments", "relations"]
                .into_iter()
                .find(|f| query.contains(&format!("{f}(")))
                .expect("a connection of an issue");
            let all = issue[field].as_array().unwrap().clone();
            json!({"issue": {field: connection(&all, &vars["after"], s.page)}})
        }
        "Projects" => {
            let shown: Vec<Value> = s
                .projects
                .iter()
                .map(|p| {
                    let docs = p["documents"].as_array().unwrap().clone();
                    json!({"id": p["id"], "documents": connection(&docs, &Value::Null, s.page)})
                })
                .collect();
            json!({"team": {"projects": connection(&shown, &vars["after"], s.page)}})
        }
        "Documents" => {
            let p = s
                .projects
                .iter()
                .find(|p| p["id"] == vars["id"])
                .expect("a project it has");
            let docs = p["documents"].as_array().unwrap().clone();
            json!({"project": {"documents": connection(&docs, &vars["after"], s.page)}})
        }
        other => panic!("a query this stand-in doesn't know: {other}"),
    };
    (200, json!({"data": data}))
}
