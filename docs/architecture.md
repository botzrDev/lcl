# Architecture

lcl 0.3.0-alpha.2 is a local tracker. One process opens one SQLite file, does its work, and exits. There is no account, no network call, and no license check.

The command line is the interface. `lcl --help` lists every flag. This page is how the program is put together, and what this version does and does not do.

## Crates

| Crate | Path | Role |
| --- | --- | --- |
| `lcl` | `crates/lcl-cli` | The binary. Parses arguments, opens a workspace, prints text or JSON, and exits. |
| `lcl-core` | `crates/lcl-core` | The workspace file, the registry, search, and the export format. |
| `lcl-tui` | `crates/lcl-tui` | A read-only list of issues. Key handling and drawing are tested without a terminal. |

`unsafe` is forbidden in the workspace.

## Files

A workspace made with `lcl init` is `.lcl/data.sqlite` in that directory. Commands run there, or in a directory below it, use that file. Init also writes `.lcl/.gitignore` with `*`, unless something by that name is there, so git ignores everything in `.lcl/`. It writes it before it makes the workspace or registers it, and a failure stops the init with nothing made. `lcl workspace create` makes a global workspace at `~/.lcl/projects/<slug>.sqlite`. The slug is the name with ASCII letters lowercased and every other character turned into `-`.

`~/.lcl/index.sqlite` is the registry. It records each workspace's name, key, and path. It is not a workspace: its schema is its own, at registry version 2, stored in `registry_meta`. `--workspace` and `LCL_WORKSPACE` look a name up there.

The home directory is `HOME` when it is set and non-empty, on every platform. On Windows, when it is not, it is `USERPROFILE`. With neither, anything that uses `~/.lcl` is an error that names both; lcl never falls back to another directory. `registry::home_dir` is that rule, as a pure function. A workspace found from the current directory does not need `HOME`.

While a workspace is open it is in WAL mode. `data.sqlite-wal` and `data.sqlite-shm` may sit beside the file. Copy those with the database if you copy it by hand, and only while no lcl is running.

## A workspace

One file holds the issues, comments, labels, relations, projects, milestones, cycles, and docs.

An issue is named `KEY-N`. The key is chosen at creation: one ASCII letter, then up to nine ASCII letters or digits, stored uppercase. It does not change. The number comes from `meta.next_issue_number`, which only grows. Deleting an issue does not return its number. The counter stops at the largest `i64`; the next create says the workspace has run out of numbers.

A person can also name an issue by `#N`, `N`, or its UUID. A `--ref` is an id from somewhere else, unique ignoring case. It cannot be shaped like one of this workspace's own references (`N`, `#N`, its `KEY-N`, or a UUID). `issue show` of a ref exits 2 and names the issue that holds it. `search` finds it.

Issue statuses are Backlog, Todo, In Progress, In Review, Done, Canceled, and Duplicate. Input ignores case and drops spaces, `_`, and `-`. `cancelled` is Canceled. JSON and the database store the canonical spelling, so In Progress is `InProgress`. Priority is 0–4: None, Urgent, High, Medium, Low.

Project statuses are Backlog, Planned, Started, Paused, Completed, and Canceled. `In Progress` is Started.

These rules hold for every writer, including import:

- A label exists before an issue can wear it. Deleting a label takes it off every issue.
- A parent link or a blocks link that would close a cycle is refused. An issue is a duplicate of at most one issue, and `duplicate-of` sets its status to Duplicate. Removing that relation leaves the status as it is.
- A milestone belongs to one project for good. An issue's milestone is in that issue's project.
- Deleting an issue moves it to the trash: it sets `deleted_at` and changes nothing else. It is refused while the issue has sub-issues that are not in the trash. `issue restore` clears `deleted_at`, and is refused while the issue's parent is in the trash, so a live issue's parent is live.
- An issue in the trash is in no list, search, count, child list or relation list, and does not block. Every command but `issue restore` and `issue purge` refuses one by name. It keeps its number, its ref, and its place in its project, milestone and cycles. `lcl export` leaves it out. 
- `issue purge` removes an issue in the trash for good, in one transaction with its comments, relations, label links and cycle links. A live issue can't be purged. An issue is purged after its sub-issues, or with them under `--all`. Its number is never handed out again. Nothing purges by itself.
- Docs, comments and labels have the same trash: a `deleted_at`, and `delete`, `restore` and `purge`. A live doc's parent is live. A comment is frozen while its issue is in the trash, and is purged with it. A label in the trash keeps its name and its links to issues, hidden; rewriting an issue's labels replaces only its links to live labels, so `label restore` puts the label back on every issue that had it. `crates/lcl-core/src/trash.rs` states all these rules; `crates/lcl-cli/tests/trash.rs` and `trash_others.rs` test them.
- Deleting a project is refused while it has issues or milestones. Deleting a milestone is refused while an issue has it. Deleting a cycle is refused while an issue is in it. Deleting a doc is refused while a doc that is not in the trash sits under it.

`--cycle` puts an issue in that cycle and takes it out of every other. `--no-cycle` clears them. An imported file may name more than one cycle on an issue; the table allows it. A comment's author defaults to `LCL_AUTHOR`, then `USER`. Adding, editing, or deleting a comment does not change the issue's `updated_at`.

`issue update` with no flags changes nothing but `updated_at`.

## Writes

Every write runs in one `BEGIN IMMEDIATE` transaction, together with the reads it depends on. The write lock is taken before those reads, so another lcl cannot change the rows between the read and the write. A writer waits up to 30 seconds. An error rolls the transaction back.

Opening a database retries while SQLite reports it busy, including the moment after a killed lcl when another connection is recovering the WAL. A command that finds the database locked waits. It does not fail at once.

lcl does not run `VACUUM`. The search indexes are FTS5 tables keyed by rowid (`issues_fts`, `docs_fts`, `comments_fts`), and a vacuum, or a dump loaded into a new file, can renumber those rowids. lcl checks each index against its rows in two ways. A mismatch is a schema error.

- On every open, read-only: the index lists exactly the rowids of its table's rows. That shows a renumbering. An open of an up-to-date workspace takes no write lock, so a read runs while another lcl writes. A file this process cannot write is checked too.
- In a process's first write, and in an upgrade before it commits: FTS5's `integrity-check`, which also compares the words. It is a write statement, so it runs only under the write lock the write or the upgrade holds already. A mismatch fails the write before it changes anything, and an upgrade is not committed.

## The change log

Every write to a record appends rows to the `changes` table in its own transaction: one row per field changed, with the old and new values as JSON, the author, and the write's clock. Triggers on each table write them, so no write path can leave one out, and a write that fails leaves none. `lcl issue history` reads an issue's rows; sync (docs/sync.md) will send them.

- A create is one row with field `*` whose value is the whole record. An issue's labels, cycles and relations are fields of the issue, one per member: `label:UUID`, `cycle:UUID`, `blocks:UUID`.
- `updated_at` is not logged.
- The clock is a hybrid logical clock, `MS-COUNTER-MACHINE`, which orders as text and never goes back. The machine is `local` until sync gives it an id. One write's rows share its clock.
- A write puts its clock and author in `meta` (`clock`, `author`) for the triggers, since a trigger in the file can't read the connection's temp tables. `author` leaves when the write ends. A write that logged nothing puts the clock back, so it leaves the log and the clock as they were.
- Deleting a record row is a purge. The record's rows, and the memberships that name it, leave the log. One row with no values and a tombstone in `purged` (kind, UUID, issue number, clock) take their place. Rows are never changed or deleted otherwise.
- A hand edit to the file is logged too, under the clock of lcl's last write and with no author.

## Schema

`PRAGMA user_version` is the schema version. This build's is 14, the length of the migration list. Opening a file applies every pending migration in one transaction, then checks foreign keys. A file from a newer lcl is refused and left untouched. A migration that has shipped is never edited. A shipped step runs once; editing it would fork the files that already ran it.

An open that changes stored values prints one line on stderr and does not print it again.

| Version | What it added |
| --- | --- |
| 1 | The 0.1.0 tables: issues, labels, comments, docs, cycles, and the issue and doc search indexes. |
| 2 | The issue-number counter, seeded one past the highest number already stored. |
| 3 | `started_at` and `canceled_at`. |
| 4 | Parents stored as UUIDs. A dangling parent, or a parent loop, is cleared. |
| 5 | Labels as their own rows. Blank assignees cleared. Duplicate label names merged. |
| 6 | A search index on comment bodies. |
| 7 | Relations: `blocks`, `related`, `duplicate`. |
| 8 | Projects and milestones, and an issue's place in them. |
| 9 | An issue's `ref` and `creator`. |
| 10 | Values 0.1.0 stored that this lcl could not read, repaired where they can be read. |
| 11 | Status, parent, comment, doc-parent, and cycle-membership rules held in the tables. |
| 12 | The trash for issues: `deleted_at`. |
| 13 | The trash for docs, comments, and labels. |
| 14 | The change log: `changes`, `purged`, and the triggers that write them. Every record there is gets a create row, so the file grows by about its records' size. |

## Search

Search matches issues by identifier, ref, title, description, and comments, and docs by title and content. Every word must match. A word is plain text: `AND`, `OR`, and `NOT` are words, and quotes are dropped. A word ending in `*` is a prefix. Case and diacritics are ignored. A query that is exactly an issue's `KEY-N` or `#N` lists that issue first. Results come 50 issues and 50 docs at a time.

An issue matches when every word is in its title and description together, or every word is in one comment. Words split between the issue and a comment, or across two comments, do not match.

## Export

`lcl export` writes the whole workspace as `lcl-export/1`: one JSON object per line. The workspace header is first, then labels, projects, milestones, cycles, issues, comments, relations, and docs. The same workspace exports to the same bytes. Issues are named by number in the file, not by the UUID stored on disk. Cycles and docs keep their ids, because a name is not unique for them.

An export carries no history. It is a snapshot of the records, and an import begins each record's history with its values as imported.

`lcl import` restores that file into an empty workspace with the same key. Numbers, refs, creators, authors, and timestamps are kept as the file gives them. The file is checked whole before anything is written, and the write is one transaction. `--dry-run` checks and counts, and writes nothing. A record of a type this lcl doesn't know is skipped and counted by type, so a file from a later lcl, which adds new record types rather than new keys, still imports; an unknown key in a known record is refused. A stored value the import would refuse stops an export as well, and the export writes nothing.

## Backup

`lcl backup PATH` copies the workspace's file with SQLite's online backup API, through the open connection, so it is safe while other lcl processes write. It never uses `VACUUM INTO`: the search indexes name rows by rowid, and a vacuum may renumber them.

The pages are read in one read transaction, so the copy is one committed state. It is written to a temporary file in PATH's directory, checked with `PRAGMA integrity_check`, synced, and then hard-linked to PATH. On Unix the directory is synced after that, so a copy reported as written is still there after a power loss. PATH is a complete workspace or is not there. A PATH that exists is never replaced. A backup that is killed can leave its temporary file, `.NAME.lcl-backup-PID-RANDOM.tmp`, beside PATH. A backup that can't get a lock in 30 seconds, as a write can't, is kind `other`; one that can't write the file is kind `io`. `crates/lcl-core/src/backup.rs` states these rules and tests them.

## The snapshot before an upgrade

Before a migration changes a workspace file or the registry, lcl copies the file beside itself, with the copy function `lcl backup` uses. `FILE` at version `N` is copied to `FILE.vN.snapshot`. The copy is taken while the migration holds the write lock and has written nothing, so it is the state the first step starts from. If it can't be written, no step runs and the command exits 1. An existing snapshot is never overwritten: the next one is `FILE.vN.snapshot.2`. A migration that fails rolls back and removes the snapshot it took. A current file and a new, empty file get none. The line on stderr that reports the upgrade names the snapshot.

## The terminal UI

`lcl tui` lists issues. `j` and `k` move, `1` through `7` filter by status, `0` shows every status, `r` reloads, and `q` or Ctrl-C quits. Enter opens the selected issue's detail (fields, description, comments), which `j`/`k`, PgDn/PgUp, `g`/`G` scroll, and Esc goes back to the list. Which of the two is shown is one enum, `Mode`; the detail's scroll lives in it. It needs a terminal on both stdin and stdout. It does not create, edit, search, or comment. The command line does that.

## The command contract

Exit 0 is success. Exit 1 is a missing row, a conflict with what is stored, a schema this build will not read, or an I/O error. Exit 2 is a wrong command line: a bad flag, a bad value, or a combination of flags that cannot work. A closed stdout exits 0.

With `--json`, stdout is one JSON value. On failure the error is the last line of stderr, and the only line that starts with `{`:

```json
{"error":{"kind":"usage|not_found|conflict|schema|io|network|auth|other","message":"..."}}
```

Other stderr lines are notices and start with `lcl:`.

Sync is being built: [the sync note](sync.md) says how issues are numbered across machines, how conflicts and purges resolve, and what it changes in the schema, the `--json` contract and `lcl-export/1`. [The protocol note](sync-protocol.md) says how the client and the service talk. The client is built, in `lcl-core/src/sync/`: opting in (`lcl sync start`), joining from another machine (`lcl sync join`), pushing the change log, pulling and applying the others' changes (`apply.rs`), issue numbers from the service's blocks, `lcl sync stop`, and `lcl sync leave`, which exports, then deletes the replica. A pull is applied whole, in one transaction, by its net effect; the writes run with the log's triggers on, and the rows they log are replaced by the changes as they came, so nothing received goes back as this machine's. The client speaks HTTPS through `ureq` on `rustls`, and only the sync commands reach the network. The tests run it against a reference service, `crates/lcl-cli/tests/sync_service`.

The CLI's arguments are parsed by clap's derive. Each subcommand's arguments are a `#[derive(Args)]` struct of their own, and `issue update`'s 26 are three flattened parts (LOCL-46). The derive builds all of a struct's arguments in one function, and a debug build gives each temporary its own stack slot: with every argument inline in one enum, `lcl --version` needed about 900 KB of stack, and now needs about 400. The unit test `the_command_builds_on_a_small_stack` builds the whole command on a 512 KiB thread, so a subcommand with many arguments inline fails it. `crates/lcl-cli/build.rs` also gives the main thread 8 MiB on Windows, as Linux and macOS have.

[The `--json` contract](json-contract.md) has the shape of every command's output. It is a draft through 0.x, and the tests in `crates/lcl-cli/tests/contract.rs` check the commands against it.

Text on a terminal shows a control character as its picture, so ESC is `␛`. A newline and a tab stay. Output to a pipe or a file is unchanged, and so is `--json`.

## What this version does not do

Deleting a project, a milestone or a cycle is immediate, and refused while anything is in it. They have no trash.

There is no install other than building this repository. The crates are unpublished. The name `lcl` on crates.io is someone else's crate. The binary in this repository is named `lcl`.

There is no sync, no shared workspace, and no copy of the file off the machine. lcl does not send the workspace anywhere.
