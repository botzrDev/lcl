//! The sync client (docs/sync-protocol.md, docs/sync.md).
//!
//! What travels is the change log (`crate::changes`). A workspace that
//! never opted in has none of the `meta` keys below, and nothing here is
//! called for it: only the sync commands reach the network.

use crate::db::{next_issue_number, put_meta, read_meta};
use crate::error::{LclError, Result};
use crate::models::now;
use crate::store::Store;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;

/// Where lcl reads the token for the service. It never stores one.
pub const TOKEN_VAR: &str = "LCL_SYNC_TOKEN";

/// The most changes a push carries, unless one write has more: a write
/// goes whole, alone if it must.
const PUSH_MAX: usize = 500;
/// The fewest numbers a machine holds before it asks for another block.
const LOW_WATER: i64 = 50;
/// The most blocks a machine holds.
const MAX_BLOCKS: usize = 2;
/// How long a request may take.
const TIMEOUT: Duration = Duration::from_secs(30);

const SERVER: &str = "sync_server";
const WORKSPACE: &str = "sync_workspace";
const MACHINE: &str = "sync_machine";
const SEQ: &str = "sync_seq";
const BLOCKS: &str = "sync_blocks";
const AT: &str = "sync_at";
const KEYS: &[&str] = &[SERVER, WORKSPACE, MACHINE, SEQ, BLOCKS, AT];

/// A row of the change log as it travels.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Change {
    /// The row's `seq` in the file of the machine that made it.
    pub id: i64,
    pub clock: String,
    pub kind: String,
    pub record: String,
    pub field: String,
    pub old: Value,
    pub new: Value,
    pub author: Option<String>,
    /// The service's number for it, on what the service sends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<i64>,
    /// Whether the service applied it, on what the service sends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applied: Option<bool>,
}

/// Issue numbers from `first` to `last`, both included.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Block {
    pub first: i64,
    pub last: i64,
}

/// What opting in tells the service about the workspace.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewReplica {
    pub key: String,
    pub name: String,
    pub description: String,
    pub created_at: i64,
    pub next_issue_number: i64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Created {
    pub workspace: String,
    pub machine: String,
    pub block: Block,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Accepted {
    pub id: i64,
    pub seq: i64,
    pub applied: bool,
}

#[derive(Debug, Clone, Serialize)]
struct PushBody<'a> {
    machine: &'a str,
    changes: &'a [Change],
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct PushReply {
    accepted: Vec<Accepted>,
}

#[derive(Debug, Clone, Serialize)]
struct MachineBody<'a> {
    machine: &'a str,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct BlockReply {
    block: Block,
}

/// The service, as the client uses it: `Http` for a real one.
pub trait Service {
    /// Its address, as the workspace keeps it.
    fn server(&self) -> &str;
    fn create(&self, replica: &NewReplica) -> Result<Created>;
    fn push(&self, workspace: &str, machine: &str, changes: &[Change]) -> Result<Vec<Accepted>>;
    fn block(&self, workspace: &str, machine: &str) -> Result<Block>;
}

/// The service over HTTPS.
pub struct Http {
    server: String,
    token: String,
    agent: ureq::Agent,
}

impl Http {
    /// A client for the service at `server` (`https://…`; `http://` only
    /// for localhost and 127.0.0.1), with the token for it: the value of
    /// `TOKEN_VAR`, which the caller reads. A bad address is checked first.
    pub fn new(server: &str, token: Option<String>) -> Result<Self> {
        let server = server.trim().trim_end_matches('/').to_string();
        let local = ["http://localhost", "http://127.0.0.1"].iter().any(|p| {
            server
                .strip_prefix(p)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with([':', '/']))
        });
        if !(server.starts_with("https://") && server.len() > "https://".len() || local) {
            return Err(LclError::SyncAddress(server));
        }
        let token = token
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .ok_or(LclError::SyncNoToken)?;
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(TIMEOUT))
            .http_status_as_error(false)
            .build()
            .into();
        Ok(Http {
            server,
            token,
            agent,
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}/v1{path}", self.server)
    }

    /// Reads a reply: `T` for a status under 400, else the service's error.
    fn reply<T: serde::de::DeserializeOwned>(
        &self,
        sent: std::result::Result<ureq::http::Response<ureq::Body>, ureq::Error>,
    ) -> Result<T> {
        let mut response = sent.map_err(|e| self.network(e.to_string()))?;
        let status = response.status().as_u16();
        if status < 400 {
            return response
                .body_mut()
                .read_json::<T>()
                .map_err(|e| self.network(format!("an answer this lcl can't read: {e}")));
        }
        let body: Value = response.body_mut().read_json().unwrap_or(Value::Null);
        let message = body["error"]["message"]
            .as_str()
            .unwrap_or("no message")
            .to_string();
        Err(match status {
            401 | 403 => LclError::SyncAuth {
                server: self.server.clone(),
                message,
            },
            404 => LclError::ReplicaNotFound {
                server: self.server.clone(),
            },
            409 if body["error"]["code"] == "clock_ahead" => {
                let ahead = body["service_ms"]
                    .as_i64()
                    .map(|ms| (crate::changes::now_ms() - ms) / 1000);
                LclError::ClockAhead { seconds: ahead }
            }
            500.. => self.network(format!("it answered {status}: {message}")),
            _ => LclError::SyncRefused {
                server: self.server.clone(),
                status,
                message,
            },
        })
    }

    fn network(&self, detail: String) -> LclError {
        LclError::SyncNetwork {
            server: self.server.clone(),
            detail,
        }
    }

    fn post<B: Serialize, T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T> {
        self.reply(
            self.agent
                .post(self.url(path))
                .header("Authorization", format!("Bearer {}", self.token))
                .header("User-Agent", user_agent())
                .send_json(body),
        )
    }
}

fn user_agent() -> String {
    format!("lcl/{}", env!("CARGO_PKG_VERSION"))
}

impl Service for Http {
    fn server(&self) -> &str {
        &self.server
    }

    fn create(&self, replica: &NewReplica) -> Result<Created> {
        self.post("/workspaces", replica)
    }

    fn push(&self, workspace: &str, machine: &str, changes: &[Change]) -> Result<Vec<Accepted>> {
        let reply: PushReply = self.post(
            &format!("/workspaces/{workspace}/changes"),
            &PushBody { machine, changes },
        )?;
        Ok(reply.accepted)
    }

    fn block(&self, workspace: &str, machine: &str) -> Result<Block> {
        let reply: BlockReply = self.post(
            &format!("/workspaces/{workspace}/blocks"),
            &MachineBody { machine },
        )?;
        Ok(reply.block)
    }
}

/// An opted-in workspace's place in sync, for `workspace show`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SyncStatus {
    pub server: String,
    pub workspace: String,
    pub machine: String,
    /// When the last `lcl sync` (or start) finished, in epoch seconds.
    pub last_sync: Option<i64>,
    /// The issue numbers this machine still holds.
    pub numbers_left: i64,
}

/// What `lcl sync start` did.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SyncStarted {
    pub server: String,
    pub workspace: String,
    pub machine: String,
    pub sent: usize,
    pub numbers_left: i64,
}

/// One change, of this machine's, that the service didn't apply, or that a
/// later one from elsewhere overwrote.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SyncConflict {
    pub kind: String,
    pub record: String,
    /// The issue's `KEY-N`, for an issue or one of its comments.
    pub identifier: Option<String>,
    pub field: String,
}

/// What `lcl sync` did.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SyncReport {
    pub sent: usize,
    pub numbers_left: i64,
    pub not_applied: Vec<SyncConflict>,
}

/// What `lcl sync stop` forgot.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SyncStopped {
    pub server: String,
    pub workspace: String,
}

/// The machine id the clocks of this file's writes end in: the service's
/// once opted in, else `local`.
pub(crate) fn machine(conn: &Connection) -> Result<String> {
    Ok(read_meta(conn, MACHINE)?.unwrap_or_else(|| crate::changes::LOCAL_MACHINE.to_string()))
}

fn blocks(conn: &Connection) -> Result<Vec<Block>> {
    let text = read_meta(conn, BLOCKS)?.unwrap_or_else(|| "[]".into());
    let pairs: Vec<(i64, i64)> =
        serde_json::from_str(&text).map_err(|_| LclError::BadMetaNumber {
            key: BLOCKS,
            value: text.clone(),
        })?;
    Ok(pairs
        .into_iter()
        .map(|(first, last)| Block { first, last })
        .collect())
}

fn put_blocks(conn: &Connection, blocks: &[Block]) -> Result<()> {
    let pairs: Vec<(i64, i64)> = blocks.iter().map(|b| (b.first, b.last)).collect();
    let text = serde_json::to_string(&pairs).map_err(|e| LclError::Encode(e.to_string()))?;
    put_meta(conn, BLOCKS, &text)
}

fn numbers_left(blocks: &[Block]) -> i64 {
    blocks.iter().map(|b| b.last - b.first + 1).sum()
}

/// The number for an issue this machine creates, when it is opted in: the
/// first of its blocks, which it then leaves out. None when it isn't. An
/// opted-in machine with no numbers left can't create an issue.
pub(crate) fn take_number(conn: &Connection) -> Result<Option<i64>> {
    if read_meta(conn, SERVER)?.is_none() {
        return Ok(None);
    }
    let mut held = blocks(conn)?;
    let Some(block) = held.first_mut() else {
        return Err(LclError::NumbersUsed);
    };
    let n = block.first;
    if block.first == block.last {
        held.remove(0);
    } else {
        block.first += 1;
    }
    put_blocks(conn, &held)?;
    Ok(Some(n))
}

fn status(conn: &Connection) -> Result<Option<SyncStatus>> {
    let Some(server) = read_meta(conn, SERVER)? else {
        return Ok(None);
    };
    let at = read_meta(conn, AT)?.and_then(|t| t.parse().ok());
    Ok(Some(SyncStatus {
        server,
        workspace: read_meta(conn, WORKSPACE)?.unwrap_or_default(),
        machine: read_meta(conn, MACHINE)?.unwrap_or_default(),
        last_sync: at,
        numbers_left: numbers_left(&blocks(conn)?),
    }))
}

/// This machine's changes the service doesn't have yet, from the oldest,
/// in whole writes (a write's rows share its clock): at most `PUSH_MAX`, or
/// one write alone when it has more.
fn pending(conn: &Connection) -> Result<Vec<Change>> {
    let mut stmt = conn.prepare(
        "SELECT seq, clock, kind, record, field, old, new, author FROM changes
         WHERE synced IS NULL ORDER BY seq",
    )?;
    let mut rows = stmt.query([])?;
    let json = |text: Option<String>| -> Value {
        text.and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or(Value::Null)
    };
    let (mut out, mut write): (Vec<Change>, Vec<Change>) = (Vec::new(), Vec::new());
    while let Some(r) = rows.next()? {
        let change = Change {
            id: r.get(0)?,
            clock: r.get(1)?,
            kind: r.get(2)?,
            record: r.get(3)?,
            field: r.get(4)?,
            old: json(r.get(5)?),
            new: json(r.get(6)?),
            author: r.get(7)?,
            seq: None,
            applied: None,
        };
        if write.first().is_some_and(|w| w.clock != change.clock) {
            if !out.is_empty() && out.len() + write.len() > PUSH_MAX {
                return Ok(out);
            }
            out.append(&mut write);
        }
        write.push(change);
    }
    if out.is_empty() || out.len() + write.len() <= PUSH_MAX {
        out.append(&mut write);
    }
    Ok(out)
}

/// A change as `SyncConflict` reports it.
fn conflict(conn: &Connection, c: &Change) -> Result<SyncConflict> {
    let issue: Option<String> = match c.kind.as_str() {
        "issue" => Some(c.record.clone()),
        "comment" => conn
            .query_row(
                "SELECT issue_id FROM comments WHERE id = ?1",
                params![c.record],
                |r| r.get(0),
            )
            .ok(),
        _ => None,
    };
    let key = crate::db::read_key(conn)?;
    let identifier = match issue {
        Some(id) => conn
            .query_row(
                "SELECT number FROM issues WHERE id = ?1
                 UNION ALL SELECT number FROM purged WHERE record = ?1 AND number IS NOT NULL
                 LIMIT 1",
                params![id],
                |r| r.get::<_, i64>(0),
            )
            .ok()
            .map(|n| crate::models::identifier(&key, n)),
        None => None,
    };
    Ok(SyncConflict {
        kind: c.kind.clone(),
        record: c.record.clone(),
        identifier,
        field: c.field.clone(),
    })
}

impl Store {
    /// The workspace's place in sync; None if it never opted in.
    pub fn sync_status(&self) -> Result<Option<SyncStatus>> {
        self.read(status)
    }

    /// Opts the workspace in: the service makes a replica of it, and this
    /// machine pushes its whole log and gets its first block of numbers.
    /// Refused for a workspace that syncs already.
    ///
    /// The replica is made first, then the file records it: if the push
    /// fails after, `lcl sync` sends the rest.
    pub fn sync_start(&mut self, svc: &dyn Service) -> Result<SyncStarted> {
        if let Some(s) = self.sync_status()? {
            return Err(LclError::SyncsAlready { server: s.server });
        }
        let replica = self.read(|conn| {
            let meta = |k: &str| -> Result<String> { Ok(read_meta(conn, k)?.unwrap_or_default()) };
            Ok(NewReplica {
                key: meta("key")?,
                name: meta("name")?,
                description: meta("description")?,
                created_at: meta("created_at")?.parse().unwrap_or(0),
                next_issue_number: next_issue_number(conn)?,
            })
        })?;
        let created = svc.create(&replica)?;
        self.write(|tx| {
            put_meta(tx, SERVER, svc.server())?;
            put_meta(tx, WORKSPACE, &created.workspace)?;
            put_meta(tx, MACHINE, &created.machine)?;
            put_meta(tx, SEQ, "0")?;
            put_blocks(tx, &[created.block])?;
            // A new replica needs the whole log, whatever an earlier one had.
            tx.execute("UPDATE changes SET synced = NULL", [])?;
            Ok(())
        })?;
        let (sent, _) = self.push_pending(svc, &created.workspace, &created.machine)?;
        let numbers_left = self.top_up(svc, &created.workspace, &created.machine)?;
        Ok(SyncStarted {
            server: svc.server().to_string(),
            workspace: created.workspace,
            machine: created.machine,
            sent,
            numbers_left,
        })
    }

    /// Sends this machine's changes the service doesn't have, and tops up
    /// its numbers. Refused for a workspace that never opted in.
    pub fn sync(&mut self, svc: &dyn Service) -> Result<SyncReport> {
        let Some(s) = self.sync_status()? else {
            return Err(LclError::NotSyncing);
        };
        let (sent, not_applied) = self.push_pending(svc, &s.workspace, &s.machine)?;
        let numbers_left = self.top_up(svc, &s.workspace, &s.machine)?;
        Ok(SyncReport {
            sent,
            numbers_left,
            not_applied,
        })
    }

    /// Forgets the service. The file keeps everything else; the replica is
    /// left as it is. Issues are numbered from the file's counter again.
    pub fn sync_stop(&mut self) -> Result<SyncStopped> {
        let Some(s) = self.sync_status()? else {
            return Err(LclError::NotSyncing);
        };
        self.write(|tx| {
            for k in KEYS {
                tx.execute("DELETE FROM meta WHERE key = ?1", params![k])?;
            }
            Ok(())
        })?;
        Ok(SyncStopped {
            server: s.server,
            workspace: s.workspace,
        })
    }

    /// Pushes every pending change, a batch at a time, and marks each with
    /// its `seq`. Returns how many it sent, and the ones not applied.
    fn push_pending(
        &mut self,
        svc: &dyn Service,
        workspace: &str,
        machine: &str,
    ) -> Result<(usize, Vec<SyncConflict>)> {
        let (mut sent, mut not_applied) = (0, Vec::new());
        loop {
            let batch = self.read(pending)?;
            if batch.is_empty() {
                break;
            }
            let accepted = svc.push(workspace, machine, &batch)?;
            if accepted.len() != batch.len()
                || accepted.iter().zip(&batch).any(|(a, c)| a.id != c.id)
            {
                return Err(LclError::SyncNetwork {
                    server: svc.server().to_string(),
                    detail: "it accepted other changes than this machine sent".into(),
                });
            }
            not_applied.extend(self.write(|tx| {
                let mut refused = Vec::new();
                for (a, c) in accepted.iter().zip(&batch) {
                    tx.execute(
                        "UPDATE changes SET synced = ?1 WHERE seq = ?2",
                        params![a.seq, a.id],
                    )?;
                    if !a.applied {
                        refused.push(conflict(tx, c)?);
                    }
                }
                put_meta(tx, AT, &now().to_string())?;
                Ok(refused)
            })?);
            sent += batch.len();
        }
        Ok((sent, not_applied))
    }

    /// Asks for blocks while this machine holds fewer than `LOW_WATER`
    /// numbers and fewer than `MAX_BLOCKS` blocks. Returns the numbers left.
    fn top_up(&mut self, svc: &dyn Service, workspace: &str, machine: &str) -> Result<i64> {
        loop {
            let held = self.read(blocks)?;
            if numbers_left(&held) >= LOW_WATER || held.len() >= MAX_BLOCKS {
                self.write(|tx| put_meta(tx, AT, &now().to_string()))?;
                return Ok(numbers_left(&held));
            }
            let block = svc.block(workspace, machine)?;
            self.write(|tx| {
                let mut held = blocks(tx)?;
                held.push(block);
                put_blocks(tx, &held)
            })?;
        }
    }
}
