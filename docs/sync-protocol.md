# Sync protocol (design note)

Status: approved by the project's owner on 2026-11-24 (LOCL-30). Built so far: `lcl sync start`, `lcl sync` pushing and topping up numbers, `lcl sync stop`, and numbers from blocks. Not built yet: join, pull and applying what is pulled. It is the contract between the open client in this repository and the sync service, which lives in a separate closed repository. [sync.md](sync.md) says what sync does: number blocks, the change log, the later change wins per field, purge wins. This note says how the client and the service talk, and what each relies on the other for. Accounts, billing and the browser are the service's business, and not here.

## Transport

- HTTPS, JSON bodies, UTF-8. The client refuses a plain `http://` address except `localhost` and `127.0.0.1`, which the tests use.
- Every path starts with `/v1`. A later protocol is `/v2`, served beside it for as long as clients that speak only `/v1` are supported.
- Every request carries `Authorization: Bearer TOKEN`. lcl reads the token from `LCL_SYNC_TOKEN` and never stores it: a workspace file may be committed, copied or put in a synced folder. How an owner gets a token is the service's business.
- `User-Agent: lcl/VERSION`.
- The client waits 30 seconds for a response, then gives up. Nothing it sent is lost: see [Pushing twice](#pushing-twice).

An error is an HTTP status of 400 or more with this body:

```json
{"error": {"code": "string", "message": "string"}}
```

| Status | `code` | Meaning |
| --- | --- | --- |
| 400 | `bad_request` | The request doesn't follow this note. |
| 401 | `unauthorized` | No token, or not one the service knows. |
| 403 | `forbidden` | The token's account may not do this to this workspace. |
| 404 | `not_found` | No such workspace (or deleted). |
| 409 | `clock_ahead` | A change's clock is more than a minute ahead of the service's. The body also has `"service_ms"`, the service's time. |
| 410 | `gone` | This protocol version is no longer served. |
| 503 | `unavailable` | Try again later. |

## Names

- **Workspace id**: the service's name for a replica, a string it chooses. The owner gives it to another machine to join.
- **Machine id**: 16 lowercase hex digits, from the service, unique in the workspace. It is the last part of every clock the machine writes after opt-in. Before opt-in a clock ends in `local` (sync.md). The service's own changes, a browser edit or a correction, end in `service`.
- **seq**: the service's number for a change it has accepted, from 1, increasing, never reused, per workspace.

## A change on the wire

A row of the change log, as sync.md and schema 14 define it:

```json
{"id": 8812, "clock": "1795523737123-0000-a1b2c3d4e5f60718", "kind": "issue", "record": "UUID", "field": "status", "old": "Backlog", "new": "Done", "author": "ann"}
```

- `id` is the row's `seq` in the file of the machine that made it. A file never reuses one (AUTOINCREMENT), so a machine id and an `id` name a change once and for all.
- `old` and `new` are the stored JSON, parsed. Field `*` is a create when `new` is the record and a purge when `new` is null. Its `old` is null.
- Changes the service sends back also have `seq`, and `applied`: false when the service accepted the change into the log but did not apply it to the replica. That happens for an edit of a purged record, or the later of two changes that conflict on a rule, as below.

## Endpoints

### Opt in: `POST /v1/workspaces`

The first machine of a workspace makes the replica:

```json
{"key": "LOCL", "name": "lcl", "description": "", "created_at": 1795000000, "next_issue_number": 49}
```

The service answers `201`:

```json
{"workspace": "string", "machine": "string", "block": {"first": 49, "last": 148}}
```

The service's counter starts at `next_issue_number`, and the first block is the first 100 numbers from it. The machine then pushes its whole log. That log rebuilds the workspace: it starts with a create for every record (schema 14 wrote one for each record there was).

The name, key and description are set here. Changing them later is not synced in 1.0.

### Join: `POST /v1/workspaces/{workspace}/machines`

Another machine of the owner. No body. `201`:

```json
{"machine": "string", "block": {"first": 149, "last": 248}, "workspace": {"key": "LOCL", "name": "lcl", "description": "", "created_at": 1795000000}}
```

The machine then pulls from the start, into an empty workspace with that key.

### Push: `POST /v1/workspaces/{workspace}/changes`

```json
{"machine": "string", "changes": ["Change"]}
```

In `id` order, and in whole writes: a write's rows (one clock) all go in one push. At most 500 changes, unless one write has more; then that write goes alone. (An import is one write, and so is the upgrade that started the log, so a push can hold every record of a workspace.) `200`:

```json
{"accepted": [{"id": 8812, "seq": 1041, "applied": true}]}
```

One entry for each change sent, in the same order. Nothing is accepted from a push that a `409 clock_ahead` refuses.

### Pull: `GET /v1/workspaces/{workspace}/changes?after=SEQ&limit=N`

`limit` is at most 500. `200`:

```json
{"changes": ["Change"], "more": true}
```

The accepted changes with a `seq` after `SEQ`, in `seq` order. They include the asking machine's own; it skips those it has already. With `more` true, ask again after the last `seq`.

### Numbers: `POST /v1/workspaces/{workspace}/blocks`

```json
{"machine": "string"}
```

`201`: `{"block": {"first": 249, "last": 348}}`. The next 100 numbers from the counter. The browser's issues take numbers from the same counter.

### Delete the replica: `DELETE /v1/workspaces/{workspace}`

For LOCL-40. `204`. Every machine's next request about it is `404`. Local files are not touched.

## What the service guarantees

- **One order.** It gives each change it accepts the next `seq`. Every machine applies the others' changes in `seq` order.
- **Pushing twice.** A change it has already accepted, by machine id and `id`, is accepted again with the same `seq`, and not added twice. A client that lost a push's response pushes again.
- **Clocks.** It refuses a push with a change whose clock is more than a minute ahead of its own, and never sends a clock that goes back on a machine's.
- **The rules of sync.md**, applied to its replica in `seq` order: the later change wins per field; purge wins, and a later change to a purged record is accepted with `applied` false.
- **Corrections.** When a change it accepts conflicts with a rule that two changes, each fine alone, can break together, it does not apply the later one (`applied` false). Then it adds the changes that make the replica whole again, from machine `service`, with clocks later than both, right after it in `seq`. The rules: a ref given to two issues, two duplicate marks on one issue, a parent loop (an issue's or a doc's), two labels (projects, cycles, milestones in a project) with one name, an issue's milestone outside its project, and an issue's `started_at`, `completed_at` and `canceled_at` not matching its status (they follow the status that won, sync.md). Those are ordinary changes, so every machine applies them by the rules below. After a machine has applied every change up to a `seq`, its records are the replica's at that `seq`, with its own unpushed changes on top.
- **Numbers.** Blocks never overlap one another or the browser's numbers. A number handed out is never handed out again.

## The client

### Commands

- `lcl sync start URL` opts the workspace in: makes the replica, pushes the whole log, takes the first block. Then it prints the workspace id to join with.
- `lcl sync join URL ID`, in an empty workspace with the replica's key (made with `workspace create NAME --key KEY`, as for `lcl import`). It pulls everything and takes a block.
- `lcl sync` pushes this machine's changes, pulls the others', and takes a block when fewer than 50 numbers are left (a machine holds at most two). It reports any change of this machine's that was not applied, and any field where a change made here was overwritten by a later one from elsewhere.
- `lcl sync stop` forgets the service: the `meta` keys go, and the local file and the replica stay. Issues are numbered from the file's counter again, from past the highest number used.

Only these commands touch the network. A workspace that never ran `lcl sync start` or `join` makes no network call.

### What the file keeps

`meta` keys, absent until opt-in: `sync_server`, `sync_workspace`, `sync_machine`, `sync_seq` (the last `seq` applied), `sync_blocks` (the numbers held, as JSON `[[first, last], …]`, used from the front), and `sync_at` (the time of the last full sync). A pushed row's `synced` is its `seq`.

### Applying what it pulls

A sync pulls every page first, then applies them all in one transaction, with foreign keys deferred to the commit. So no reader sees a state between two changes that the service corrects together, as a parent loop, and an interrupted sync applies nothing; the next one pulls the same changes again.

1. Every change goes in the log as it came, with its `seq` in `synced`, applied or not, so that `lcl issue history` shows both sides of a conflict.
2. A change is applied only if `applied` is true, its record isn't purged here, and no row in the log for the same record and field has a later clock. (Rows of one write share a clock, and arrive in their order.)
3. A change that a rule of the file refuses (a unique label or project name, a ref, one duplicate mark per issue, a milestone in the issue's project) is tried again after the rest, by rule 2 again. A correction after it makes room for it, or makes it lose.
4. A create of a record that exists, or an add of a member already there, does nothing. A purge removes the record as `issue purge` does, and leaves its tombstone.
5. The machine's clock moves past the latest clock it received.

A change that still can't be applied after its retry stops the sync, rolled back whole, and names the change. That means the service broke its guarantee.

### Numbers

Once opted in, `issue create` takes the next number from the blocks held. With none left, it is refused: exit 1, kind `conflict`, "this machine has used the issue numbers it holds; run lcl sync to get more". Every other write works offline.

### Errors

Two new error kinds, for the sync commands only:

- `network`: the service couldn't be reached, or answered outside this note. Edits stay in the file, and go at the next `lcl sync`.
- `auth`: no `LCL_SYNC_TOKEN`, or the service answered `401` or `403`.

`clock_ahead` is a `conflict` that names how far ahead the clock is. `not_found` is `not_found`.

### `--json`

```json
lcl sync start  {"server": "string", "workspace": "string", "machine": "string", "sent": "integer", "numbers_left": "integer"}
lcl sync join   {"server": "string", "workspace": "string", "machine": "string", "received": "integer", "numbers_left": "integer"}
lcl sync        {"sent": "integer", "received": "integer", "numbers_left": "integer", "not_applied": ["SyncConflict"], "overwritten": ["SyncConflict"]}
lcl sync stop   {"server": "string", "workspace": "string"}
```

`SyncConflict` is `{"kind": "string", "record": "string", "identifier": "string|null", "field": "string"}`. `identifier` is the issue's `KEY-N` for an issue or one of its comments, else null.

`workspace show` gains `"sync"`: null, or `{"server": "string", "workspace": "string", "machine": "string", "last_sync": "integer|null", "numbers_left": "integer"}`.

### Dependencies

An HTTPS client: `ureq`, with `rustls` on `ring`, so no system TLS library is needed on any of the five release targets. `cargo deny` checks their licenses.

## Testing

The tests run a reference service, in Rust, on `127.0.0.1`. It does the service's part the client relies on: seq, pushing twice, clocks, blocks, purge wins, `applied` false. It sends corrections that the tests script, not ones it works out. The rules that make corrections are tested in the service's repository. The client's tests cover:

- Two machines opted in, creating and editing while apart, converge, with no number used twice (LOCL-30's criteria).
- A delete, a restore and a purge reach the other machine once, comments included.
- With the service stopped, the file opens and takes edits. They go at the next sync.
- A workspace that never opted in makes no connection.
- Each case of [Applying what it pulls](#applying-what-it-pulls), from scripted pages.

## Considered and not chosen

- **The client resolves every conflict itself.** Every machine, and the replica, would have to make the same choice from changes that arrive in different orders. The service sees one order, so it decides, and its corrections are ordinary changes.
- **Sending whole records instead of changes.** That loses the per-field rule, and the history.
- **Keeping the token in the workspace file or the registry.** The file travels. The registry would be a second place to protect. An environment variable is where tokens for command-line tools already live.

## Decisions

Made with the owner on 2026-11-24, when the note was approved:

1. The service is the arbiter. It orders every change, and fixes a conflict between two changes with ordinary correction changes. Clients apply the later change per field, purge first, and retry what a rule refuses.
2. The token comes from `LCL_SYNC_TOKEN` and is never stored.
3. The commands are `lcl sync start URL`, `lcl sync join URL ID`, `lcl sync` and `lcl sync stop`, with the error kinds `network` and `auth`.
4. The client uses `ureq` with `rustls` on `ring`.
5. A workspace's name, key and description are set at opt-in, and not synced in 1.0.
