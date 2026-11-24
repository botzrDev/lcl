//! A reference sync service for the tests, on 127.0.0.1: the service's
//! part of docs/sync-protocol.md that the client relies on. It numbers what
//! it accepts (seq), accepts a change pushed twice once, refuses a clock
//! more than a minute ahead, hands out blocks, and doesn't apply a change
//! to a purged record. It makes no corrections of its own: a test scripts
//! those with `inject`. Knobs on `State` make it fail the ways a real one
//! can.
#![allow(dead_code)]

use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;

/// The one token it takes.
pub const TOKEN: &str = "test-token";

/// A change it accepted.
#[derive(Debug, Clone)]
pub struct Logged {
    pub seq: i64,
    pub machine: String,
    /// As the machine sent it.
    pub change: Value,
    pub applied: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Replica {
    pub key: String,
    pub name: String,
    pub description: String,
    pub created_at: i64,
    /// The next issue number it hands out.
    pub counter: i64,
    pub machines: Vec<String>,
    pub log: Vec<Logged>,
    /// Records purged, by id.
    pub purged: BTreeSet<String>,
    /// The ids of each push's changes, in order, as they came.
    pub pushes: Vec<Vec<i64>>,
    /// The clocks of each push's changes.
    pub push_clocks: Vec<Vec<String>>,
}

/// A request, as it came.
#[derive(Debug, Clone)]
pub struct Seen {
    pub method: String,
    pub url: String,
    pub authorization: Option<String>,
    pub user_agent: Option<String>,
}

#[derive(Debug)]
pub struct State {
    pub replicas: BTreeMap<String, Replica>,
    pub requests: Vec<Seen>,
    /// The numbers in a block. The protocol says 100; the client must not
    /// care.
    pub block_size: i64,
    /// Numbers skipped after each block, as if handed to someone else.
    pub block_gap: i64,
    /// Added to the time, in ms: negative puts its clock behind.
    pub skew_ms: i64,
    /// Answers 503 to everything.
    pub down: bool,
    /// Accepts the next push, then answers 500, as if the answer was lost.
    pub lose_next_push_answer: bool,
    made: u64,
}

impl Default for State {
    fn default() -> Self {
        State {
            replicas: BTreeMap::new(),
            requests: Vec::new(),
            block_size: 100,
            block_gap: 0,
            skew_ms: 0,
            down: false,
            lose_next_push_answer: false,
            made: 0,
        }
    }
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

impl State {
    /// The one replica, for a test that made one.
    #[track_caller]
    pub fn replica(&mut self) -> &mut Replica {
        assert_eq!(self.replicas.len(), 1, "one replica");
        self.replicas.values_mut().next().unwrap()
    }

    fn block(&mut self, workspace: &str) -> Value {
        let (size, gap) = (self.block_size, self.block_gap);
        let r = self.replicas.get_mut(workspace).unwrap();
        let first = r.counter;
        r.counter += size + gap;
        json!({"first": first, "last": first + size - 1})
    }

    fn machine(&mut self, workspace: &str) -> String {
        self.made += 1;
        let m = format!("{:016x}", 0xa1b2_c3d4_e5f6_0700_u64 + self.made);
        self.replicas
            .get_mut(workspace)
            .unwrap()
            .machines
            .push(m.clone());
        m
    }

    /// Adds a change of the service's own (a correction, a browser edit)
    /// to the replica's log, as the next seq.
    pub fn inject(&mut self, workspace: &str, change: Value) -> i64 {
        let r = self.replicas.get_mut(workspace).expect("the replica");
        accept(r, "service", &change)
    }

    /// The service purges a record itself, as a browser edit would.
    pub fn purge(&mut self, workspace: &str, kind: &str, record: &str) -> i64 {
        let n = self.replicas[workspace].log.len() as i64 + 1;
        let clock = format!("{:013}-0000-service", now_ms() + self.skew_ms);
        self.inject(
            workspace,
            json!({"id": n, "clock": clock, "kind": kind, "record": record, "field": "*", "old": null, "new": null, "author": null}),
        )
    }
}

/// The seq of `change` from `machine`, accepting it if it is new.
fn accept(r: &mut Replica, machine: &str, change: &Value) -> i64 {
    let id = change["id"].as_i64().unwrap();
    if let Some(l) = r
        .log
        .iter()
        .find(|l| l.machine == machine && l.change["id"].as_i64() == Some(id))
    {
        return l.seq;
    }
    let record = change["record"].as_str().unwrap().to_string();
    let field = change["field"].as_str().unwrap();
    // A member's other end (label:UUID, cycle:UUID, blocks:UUID, ...), and
    // a new comment's issue.
    let other = field
        .split_once(':')
        .map(|(_, id)| id)
        .or(change["new"]["issue_id"].as_str().filter(|_| field == "*"));
    let applied = !r.purged.contains(&record) && other.is_none_or(|o| !r.purged.contains(o));
    if field == "*" && change["new"].is_null() {
        r.purged.insert(record);
    }
    let seq = r.log.len() as i64 + 1;
    r.log.push(Logged {
        seq,
        machine: machine.to_string(),
        change: change.clone(),
        applied,
    });
    seq
}

/// The service, answering on 127.0.0.1 until dropped.
pub struct Service {
    pub url: String,
    state: Arc<Mutex<State>>,
    server: Arc<tiny_http::Server>,
    thread: Option<JoinHandle<()>>,
}

impl Service {
    pub fn start() -> Self {
        let server = Arc::new(tiny_http::Server::http("127.0.0.1:0").expect("bind"));
        let port = server.server_addr().to_ip().unwrap().port();
        let state = Arc::new(Mutex::new(State::default()));
        let thread = {
            let (server, state) = (server.clone(), state.clone());
            std::thread::spawn(move || {
                for request in server.incoming_requests() {
                    handle(&state, request);
                }
            })
        };
        Service {
            url: format!("http://127.0.0.1:{port}"),
            state,
            server,
            thread: Some(thread),
        }
    }

    pub fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap()
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        self.server.unblock();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn error(status: u16, code: &str, message: &str) -> (u16, Value) {
    (status, json!({"error": {"code": code, "message": message}}))
}

fn handle(state: &Mutex<State>, mut request: tiny_http::Request) {
    let mut body = String::new();
    let _ = request.as_reader().read_to_string(&mut body);
    let header = |name: &'static str| {
        request
            .headers()
            .iter()
            .find(|h| h.field.equiv(name))
            .map(|h| h.value.as_str().to_string())
    };
    let seen = Seen {
        method: request.method().as_str().to_string(),
        url: request.url().to_string(),
        authorization: header("Authorization"),
        user_agent: header("User-Agent"),
    };
    let answer = {
        let mut s = state.lock().unwrap();
        s.requests.push(seen.clone());
        route(&mut s, &seen, &body)
    };
    match answer {
        Some((status, reply)) => {
            let content_type =
                tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..])
                    .unwrap();
            let text = if reply.is_null() {
                String::new()
            } else {
                reply.to_string()
            };
            let _ = request.respond(
                tiny_http::Response::from_string(text)
                    .with_status_code(status)
                    .with_header(content_type),
            );
        }
        // Dropped unanswered, tiny_http answers 500 with no body.
        None => drop(request),
    }
}

/// The answer to a request; None to lose it.
fn route(s: &mut State, seen: &Seen, body: &str) -> Option<(u16, Value)> {
    if s.down {
        return Some(error(503, "unavailable", "down for the test"));
    }
    if seen.authorization.as_deref() != Some(&format!("Bearer {TOKEN}")) {
        return Some(error(401, "unauthorized", "not a token this service knows"));
    }
    let (path, query) = seen.url.split_once('?').unwrap_or((&seen.url, ""));
    let Some(path) = path.strip_prefix("/v1/workspaces") else {
        return Some(error(404, "not_found", "no such path"));
    };
    let body: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        if seen.method != "POST" {
            return Some(error(400, "bad_request", "POST to make a replica"));
        }
        s.made += 1;
        let workspace = format!("ws-{}", s.made);
        s.replicas.insert(
            workspace.clone(),
            Replica {
                key: body["key"].as_str().unwrap().to_string(),
                name: body["name"].as_str().unwrap().to_string(),
                description: body["description"].as_str().unwrap().to_string(),
                created_at: body["created_at"].as_i64().unwrap(),
                counter: body["next_issue_number"].as_i64().unwrap(),
                ..Replica::default()
            },
        );
        let machine = s.machine(&workspace);
        let block = s.block(&workspace);
        return Some((
            201,
            json!({"workspace": workspace, "machine": machine, "block": block}),
        ));
    }
    let workspace = parts[0].to_string();
    if !s.replicas.contains_key(&workspace) {
        return Some(error(404, "not_found", "no such workspace"));
    }
    let known = |s: &State| {
        body["machine"]
            .as_str()
            .is_some_and(|m| s.replicas[&workspace].machines.iter().any(|k| k == m))
    };
    match (seen.method.as_str(), &parts[1..]) {
        ("DELETE", []) => {
            s.replicas.remove(&workspace);
            Some((204, Value::Null))
        }
        ("POST", ["machines"]) => {
            let machine = s.machine(&workspace);
            let block = s.block(&workspace);
            let r = &s.replicas[&workspace];
            Some((
                201,
                json!({"machine": machine, "block": block, "workspace": {"key": r.key, "name": r.name, "description": r.description, "created_at": r.created_at}}),
            ))
        }
        ("POST", ["blocks"]) => {
            if !known(s) {
                return Some(error(400, "bad_request", "unknown machine"));
            }
            Some((201, json!({"block": s.block(&workspace)})))
        }
        ("POST", ["changes"]) => {
            if !known(s) {
                return Some(error(400, "bad_request", "unknown machine"));
            }
            let machine = body["machine"].as_str().unwrap().to_string();
            let changes = body["changes"].as_array().unwrap().clone();
            let service_ms = now_ms() + s.skew_ms;
            let ahead = changes.iter().any(|c| {
                c["clock"].as_str().unwrap()[..13].parse::<i64>().unwrap() > service_ms + 60_000
            });
            if ahead {
                let (status, mut reply) = error(409, "clock_ahead", "a clock is ahead");
                reply["service_ms"] = service_ms.into();
                return Some((status, reply));
            }
            let r = s.replicas.get_mut(&workspace).unwrap();
            r.pushes
                .push(changes.iter().map(|c| c["id"].as_i64().unwrap()).collect());
            r.push_clocks.push(
                changes
                    .iter()
                    .map(|c| c["clock"].as_str().unwrap().to_string())
                    .collect(),
            );
            let accepted: Vec<Value> = changes
                .iter()
                .map(|c| {
                    let seq = accept(r, &machine, c);
                    let applied = r.log[seq as usize - 1].applied;
                    json!({"id": c["id"], "seq": seq, "applied": applied})
                })
                .collect();
            if std::mem::take(&mut s.lose_next_push_answer) {
                return None;
            }
            Some((200, json!({"accepted": accepted})))
        }
        ("GET", ["changes"]) => {
            let param = |name: &str| {
                query
                    .split('&')
                    .find_map(|kv| kv.strip_prefix(&format!("{name}=")))
                    .and_then(|v| v.parse::<i64>().ok())
            };
            let after = param("after").unwrap_or(0);
            let limit = param("limit").unwrap_or(500).clamp(1, 500) as usize;
            let log = &s.replicas[&workspace].log;
            let page: Vec<Value> = log
                .iter()
                .filter(|l| l.seq > after)
                .take(limit)
                .map(|l| {
                    let mut c = l.change.clone();
                    c["seq"] = l.seq.into();
                    c["applied"] = l.applied.into();
                    c
                })
                .collect();
            let last = page.last().and_then(|c| c["seq"].as_i64()).unwrap_or(after);
            let more = log.iter().any(|l| l.seq > last);
            Some((200, json!({"changes": page, "more": more})))
        }
        _ => Some(error(400, "bad_request", "no such endpoint")),
    }
}
