# Sync (design note)

Status: approved by the project's owner on 2026-11-24 (LOCL-22). Nothing here is built yet. The service lives in a separate closed repository; this note says what it needs from the data, and what the client changes. LOCL-30 builds sync from it, and LOCL-34 builds the change log.

## What sync is

- The owner's local file is the source of truth. If the service's replica is lost, the owner's file rebuilds it. If the owner deletes the replica (LOCL-40), the local file stays as it is. The service changes a local file only through the changes `lcl sync` brings.
- Sync is opt-in, per workspace. A workspace that never runs `lcl sync` makes no network call, and nothing in it shows an account or a price.
- The service keeps a replica of an opted-in workspace. An invited person reads and edits that replica in a browser, and has no local file (LOCL-37, LOCL-38).
- Another machine of the owner opts in and gets a local file of its own, kept in step through the replica.
- A local file opens and takes edits when the service can't be reached. The edits go up at the next `lcl sync`.

## Records are named by UUID

Every record already has a UUID: issues, comments, labels, projects, milestones, cycles and docs. Sync names records by UUID, on the wire and in the change log. Numbers are for people. A link row (an issue's label, an issue's cycle, a relation) is named by its two UUIDs, and a relation by its kind too.

`lcl-export/1` names issues by number, and an import gives every issue a new UUID. So an imported workspace is a new workspace to sync: it can't be attached to the replica its export came from.

## How issues are numbered across machines

Today the next number comes from one counter in the file, so two machines creating issues offline would both take `KEY-42`.

**The model: number blocks.** The service holds the workspace's counter. It gives each opted-in machine a block of numbers, 100 at a time. A machine numbers the issues it creates from its blocks, offline or not. The browser takes numbers from the service's counter directly; it is always online.

- A number, once shown, never changes. `KEY-N` stays the only identifier shape.
- At opt-in, the service's counter starts at the file's `next_issue_number`, the counter it keeps today, and the machine gets its first block from there.
- A machine holds at most two blocks. At each sync it asks for another when fewer than 50 numbers are left, so it can create 50 to 200 issues offline.
- A machine that has used every number it holds refuses to create an issue: exit 1, kind `conflict`, "this machine has used the issue numbers it holds; run lcl sync to get more". Every other edit still works offline.
- Numbers are never reused: not a purged issue's, not the rest of a block a machine never used.
- Numbers no longer follow creation order across machines. An issue made on the web as `KEY-101` can be older than one a laptop makes later as `KEY-61` from an earlier block. `issue list` sorts by number as before.

Considered and not chosen:

- **The service numbers at sync, with a provisional number until then.** Sync is a command, not a background process, so every issue made between syncs would be provisional: most of them. Its identifier would change at the next sync, after it may have gone into a commit message, an agent's notes or a script reading `--json`.
- **An operation log merged at sync, renumbering the loser of a collision.** The same identifier change, after the fact, for whichever side loses.
- **A device suffix, as `KEY-42-b`.** Stable, but a second identifier shape for every reader, and the plain `KEY-N` stops being unique.

## The change log

Every write appends rows to a `changes` table, in the same transaction as the write. One log serves two uses:

- Sync sends a machine's changes the service hasn't seen, and applies the changes it hasn't seen from others.
- `lcl issue history` (LOCL-34) lists an issue's changes. LOCL-34 builds this log, not a second one.

A row is one change to one field of one record: the record's kind and UUID, the field, the old and new values as JSON, the author, and a clock. A create is one row whose field is `*` and whose value is the whole record. A purge is one row too, and so is each membership added to or removed from a set (a label on an issue, an issue in a cycle, a relation).

The clock is a hybrid logical clock: wall-clock milliseconds, a counter, and the machine's id, written so that comparing two clocks as text orders them. A machine's clock never goes back. On receiving a change with a later clock, it moves its own past it. The service refuses a change whose clock is more than a minute ahead of its own, so a machine with a wrong clock can't win every conflict; that machine is told to fix its clock.

Rows are never updated or deleted, except when the record itself is purged.

## Two machines edit the same field

**The later change wins, per field.** Each field of each record keeps the value of the change with the latest clock; a tie can't happen, because the machine id is part of the clock. Both machines end with the same value, whichever order the changes arrive in.

- Edits to different fields of the same issue don't conflict: a status changed on one machine and a title on another both stay.
- Text fields (title, description, a doc's content, a comment's body) follow the same rule. 1.0 merges no text. The losing text is still in the change log: `lcl sync` names each field where a change made here was overwritten by a later one from elsewhere, and `lcl issue history` shows both values.
- Sets: each membership is its own field. Two machines adding different labels to an issue both keep them. An add and a remove of the same label: the later wins.
- `started_at`, `completed_at` and `canceled_at` are written by the status change, in the same change set, and follow it.
- `updated_at` is the time of the latest change applied to the record.
- Parents: if two changes, each fine alone, would make an issue its own ancestor, the later one is not applied, and sync reports it. The same holds for a doc's parent.
- Names that must be unique (a label's, a project's or a cycle's name, a milestone's name in its project): two machines that each create `bug` end with one label. The one created first keeps its UUID; what used the other (issues, a project's milestones) moves to it, and the other is removed. Both are recorded as changes.
- Other values that must be unique: an issue's `ref`, and the one issue a duplicate points to. If two machines give the same `ref` to different issues, the later change is not applied, and sync reports it. If two machines mark one issue a duplicate of two different issues, the later mark wins and the earlier is removed, as a recorded change.

## Delete, restore and purge

- **Delete** (to the trash) is a change to `deleted_at`. **Restore** is a change of it back to null. Between a delete and a restore made on two machines, the later wins on both.
- **Purge** is final, and it wins over everything. It removes the record and what hangs off it, as it does today, and writes a tombstone: the kind, the UUID, the issue number if any, and the clock. Every machine that receives the purge removes the same rows, whatever state they are in there. A later edit or restore of that UUID, from a machine that hadn't heard of the purge, is dropped, and sync reports it. Purge wins because it is the only change that can't be undone: a row kept on one machine and gone from another would never converge.
- Tombstones are kept for good, so a machine that comes back after months still learns of the purge. They hold no content.
- Purging an issue purges its comments, relations, and label and cycle links, on every machine.
- Projects, milestones and cycles have no trash. Deleting one is refused while it is in use, as today. A delete that does go through writes a tombstone, and syncs as a purge does.
- A purged issue's number is never used again, on any machine.

## The service's part

What the client relies on. How the service is built is not this note's business.

- It gives each change it accepts a sequence number. A machine syncs from the last sequence number it has seen.
- It hands out number blocks, and numbers the browser's issues.
- It applies changes to the replica by the rules above, so the browser shows what a synced machine would.
- A browser edit is a change from the service's own machine id, with the invited person as author.
- It never copies a workspace that has not opted in (LOCL-39).

## What this changes

### The schema

One migration:

- `changes`: `seq INTEGER PRIMARY KEY`, `clock TEXT NOT NULL`, `kind TEXT NOT NULL`, `record TEXT NOT NULL` (the UUID, or the pair for a link), `field TEXT NOT NULL`, `old TEXT`, `new TEXT`, `author TEXT`, `synced INTEGER` (the service's sequence number, null until the service has it). An index on `(record, field, clock)`. It is written for every workspace, synced or not, because it is the history too. The migration writes one create row for each record that exists, with its current values.
- `purged`: `record TEXT PRIMARY KEY`, `kind TEXT NOT NULL`, `number INTEGER`, `clock TEXT NOT NULL`.
- `meta` keys written at opt-in, and absent until then: the machine's id, the service's address and workspace id, the last sequence number seen, and the blocks held.
- The existing tables and columns stay as they are.

### The `--json` contract

- Every existing output keeps its shape. Issue, comment, doc, label, project, milestone and cycle objects do not change.
- New commands with their own shapes: `lcl sync`, and `lcl issue history` (LOCL-34).
- `workspace show --json` gains `sync`: null for a workspace that has not opted in, else an object with the service's address, the time of the last sync, and the issue numbers left in this machine's blocks.
- One new refusal, only in an opted-in workspace: an issue create with no numbers left, kind `conflict`. No new error kind.

### `lcl-export/1`

- The records and their keys do not change. An export is a snapshot. It carries no change log, no tombstones, and no sync settings.
- **One change before the freeze, made in LOCL-48:** `lcl import` refused a record type it didn't know (exit 2). Then no record type could be added after 1.0 without a new format: a 1.0 reader would refuse every newer file. Now `lcl import` skips a record whose type it doesn't know, and says how many it skipped, of which types. An unknown key in a known record stays an error, so a converter's typo is still caught. Anything added after 1.0 comes as a new record type, not a new key.

## Out of 1.0

From LOCL-31:

- Initiatives: after the first 10,000. Projects already group issues. A later record can sit beside them.
- Initiative nesting: after the first 10,000, with initiatives.
- Releases: after the first 10,000. Milestones and cycles cover the first teams.
- Attachments: after the first 10,000. The browser does not upload files. A file comes later as a record of its own; it does not change an existing issue's keys.
- Teams: never, for this product. A workspace has one key. A second team is a second workspace.

LOCL-31 says each of these can be added later because readers skip unknown records. That holds once the `lcl-export/1` change above lands, before the freeze.

## Decisions

Made with the owner on 2026-11-24, when the note was approved:

1. Issues are numbered from blocks, as above, not provisionally until sync and not with a device suffix.
2. `lcl import` skips record types it doesn't know and says so; an unknown key stays an error. This lands before 1.0.
3. The rest as written: the later change wins per field, with no text merged; purge wins over a concurrent edit or restore; tombstones are kept for good; history and sync share one change log, written for every workspace.
