# The `--json` contract (draft)

This note is a draft. Through 0.x a shape here can change between versions, and `CHANGELOG.md` lists every such change. At 1.0 it becomes the rule that a shape does not change inside a major version.

The shapes below are checked. `crates/lcl-cli/tests/contract.rs` reads this file, runs each command, and fails when the output and the shape differ, when a command has no section here, or when a section names a command that does not exist.

## Exit status

- `0`: success. A closed stdout also exits 0.
- `1`: the command failed. It could succeed with other stored data: a row was not found, the change conflicts with what is stored, the file has a schema this build will not read, or there was an I/O error.
- `2`: the command line is wrong: a bad flag, a bad value, or a combination that cannot work. It cannot succeed as typed.

The exit status is the same with and without `--json`.

## stdout

With `--json`, a command that succeeds prints one JSON value on stdout and nothing else. A command that fails prints nothing on stdout.

There are two exceptions. `lcl export` without `--output` prints the export itself, which is `lcl-export/1`: one JSON object per line. `lcl tui` takes the terminal and prints no JSON.

The order of an object's keys is not part of the contract. A timestamp is Unix seconds. A date is `YYYY-MM-DD`.

## stderr

With `--json`, an error is one line: the last line of stderr, and the only one that starts with `{`. `--json` counts when it comes before any `--`.

### `Error`

```json
{"error": {"kind": "'usage'|'not_found'|'conflict'|'schema'|'io'|'network'|'auth'|'other'", "message": "string"}}
```

- `usage` comes with exit 2. Every other kind comes with exit 1.
- `not_found`: a named issue, label, project, milestone, cycle, doc, comment or workspace does not exist.
- `conflict`: the change is refused because of what is stored, as deleting a project that still has issues. A command that names an issue, a doc, a comment or a label in the trash is a conflict, and its message names the `restore` command.
- `schema`: the file's schema is newer than this build reads, or is not a schema lcl wrote.
- `io`: a file could not be read or written.
- `network`: the sync service could not be reached, or answered outside [the protocol](sync-protocol.md). Edits stay in the file and go at the next `lcl sync`.
- `auth`: `LCL_SYNC_TOKEN` is not set, or the sync service refused the token.

Other stderr lines are notices. Each starts with `lcl:`. Without `--json`, an error is `Error: MESSAGE`.

## How to read a shape

A shape is JSON. A string in it names what stands at that place:

- `"string"`, `"integer"`, `"boolean"` and `"null"` are JSON types. `"number"` is any JSON number: an integer, or one with a fraction.
- `"'cwd'|'global'"` is a string with one of the quoted values.
- `"string|null"` is either of the two.
- A capitalised name, as `"Issue"`, is the object of that name under [Objects](#objects).
- `["Issue"]` is an array of them. It can be empty.
- An object has exactly the keys shown. The key `"..."` means every key of the object it names, with the other keys beside them.

## Objects

### `Status`

```json
"'Backlog'|'Todo'|'InProgress'|'InReview'|'Done'|'Canceled'|'Duplicate'"
```

### `Priority`

```json
"'None'|'Urgent'|'High'|'Medium'|'Low'"
```

### `Issue`

`parent_id` is the parent's `id` and `parent_identifier` its `KEY-N`. `project` and `milestone` are names. `labels` are label names.

```json
{
  "id": "string",
  "number": "integer",
  "identifier": "string",
  "title": "string",
  "description": "string",
  "status": "Status",
  "priority": "Priority",
  "assignee": "string|null",
  "parent_id": "string|null",
  "parent_identifier": "string|null",
  "project": "string|null",
  "milestone": "string|null",
  "ref": "string|null",
  "creator": "string|null",
  "estimate": "integer|null",
  "due_date": "string|null",
  "labels": ["string"],
  "cycles": [{"id": "string", "name": "string"}],
  "created_at": "integer",
  "updated_at": "integer",
  "started_at": "integer|null",
  "completed_at": "integer|null",
  "canceled_at": "integer|null",
  "deleted_at": "integer|null"
}
```

`deleted_at` is when `lcl issue delete` moved the issue to the trash, and null for every issue that is not there. Only `lcl issue list --trashed` prints issues that are.

### `RelatedIssue`

An issue as a relation, or a list of similar issues, names it.

```json
{"identifier": "string", "status": "Status", "title": "string"}
```

### `HistoryEntry`

One write's changes to an issue, for `lcl issue history`. `at` is when, in epoch seconds; `author` who, from `--author`, `LCL_AUTHOR` or `USER` (null for none, and for the entries an upgrade or import began a history with).

```json
{"at": "integer", "author": "string|null", "changes": ["FieldChange"]}
```

### `FieldChange`

One field's change, `old` to `new`. `field` is a key of `Issue` but for `parent`, which is the parent's `KEY-N`, and the relations, which are `blocks`, `blocked_by`, `related`, `duplicate_of` and `duplicates` as in `lcl issue show`. Values are as `Issue` shows them: `priority` by name, and `project`, `milestone`, a label and a cycle by name (by UUID once deleted for good). `labels`, `cycles` and the relations change one member at a time: `new` is the member added, with `old` null, or `old` the member removed, with `new` null. A change to one of the issue's comments has its `comment_id`, and `field` `body` (`old` null when the comment was added) or `deleted_at`.

```json
{"field": "string", "comment_id": "string|null", "old": "string|integer|null", "new": "string|integer|null"}
```

### `SkippedRecords`

The records of one type an import didn't know, in type order. A later `lcl-export/1` adds new record types, not new keys, so an older lcl reads the rest of the file.

```json
{"type": "string", "records": "integer"}
```

### `Comment`

`issue_id` is the issue's `id`, not its `KEY-N`. `deleted_at` is when the comment went to the trash, and null otherwise; so too for a label and a doc. Only a `--trashed` listing prints one that is there.

```json
{"id": "string", "issue_id": "string", "body": "string", "author": "string|null", "created_at": "integer", "deleted_at": "integer|null"}
```

### `Project`

```json
{
  "id": "string",
  "name": "string",
  "description": "string",
  "status": "'Backlog'|'Planned'|'Started'|'Paused'|'Completed'|'Canceled'",
  "target_date": "string|null",
  "created_at": "integer",
  "updated_at": "integer"
}
```

### `Milestone`

`project` is the project's name.

```json
{
  "id": "string",
  "name": "string",
  "project": "string",
  "description": "string",
  "target_date": "string|null",
  "sort_order": "integer",
  "created_at": "integer",
  "updated_at": "integer"
}
```

### `IssueCounts`

`unreadable` counts issues whose stored status this build can't read.

```json
{
  "total": "integer",
  "by_status": {
    "Backlog": "integer",
    "Todo": "integer",
    "InProgress": "integer",
    "InReview": "integer",
    "Done": "integer",
    "Canceled": "integer",
    "Duplicate": "integer",
    "unreadable": "integer"
  }
}
```

### `Cycle`

```json
{"id": "string", "name": "string", "starts_at": "string|null", "ends_at": "string|null", "created_at": "integer"}
```

### `CycleCount`

Issues, and the sum of their estimates. An issue with no estimate counts in `issues` and adds nothing to `points`.

```json
{"issues": "integer", "points": "integer"}
```

### `CycleScope`

What a cycle held at its start, what entered after (up to its end), and what it held at or after its start but not at its end.

```json
{"planned": "CycleCount", "added": "CycleCount", "removed": "CycleCount"}
```

### `Label`

```json
{"id": "string", "name": "string", "color": "string", "deleted_at": "integer|null"}
```

### `Doc`

```json
{"id": "string", "title": "string", "content": "string", "parent_id": "string|null", "created_at": "integer", "updated_at": "integer", "deleted_at": "integer|null"}
```

### `SyncStatus`

A workspace's place in sync. `workspace` is the service's id for its replica, and `machine` this machine's. `last_sync` is when the last `lcl sync` (or `lcl sync start`) finished, null until one has. `numbers_left` counts the issue numbers this machine holds.

```json
{"server": "string", "workspace": "string", "machine": "string", "last_sync": "integer|null", "numbers_left": "integer"}
```

### `SyncConflict`

A change of this machine's that the service did not apply (its record was purged there, or the change conflicted with another), or that a later change from elsewhere overwrote. `kind` and `record` name the record, `field` the field (`*` for a create). `identifier` is the issue's `KEY-N`, for an issue or one of its comments.

```json
{"kind": "string", "record": "string", "identifier": "string|null", "field": "string"}
```

## Workspaces

### `lcl init`

`created` is false when the workspace was already there with that name and key.

```json
{"path": "string", "name": "string", "key": "string", "created": "boolean"}
```

### `lcl workspace create`

```json
{"name": "string", "slug": "string", "key": "string", "path": "string", "created_at": "integer"}
```

### `lcl workspace list`

`missing` is true when the file is gone.

```json
[{"name": "string", "slug": "string", "kind": "'cwd'|'global'", "path": "string", "created_at": "integer", "missing": "boolean"}]
```

### `lcl workspace show`

`sync` is null when the workspace doesn't sync.

```json
{"name": "string", "key": "string", "description": "string", "path": "string", "schema_version": "integer", "created_at": "integer", "sync": "SyncStatus|null"}
```

## Issues

### `lcl issue create`

The issue, and `similar`: up to five open issues like it, best first, as `lcl issue similar` finds them. The list never stops or changes the create: it is made after the issue is written, and if it can't be, it is empty, with a notice on stderr.

```json
{"...": "Issue", "similar": ["RelatedIssue"]}
```

### `lcl issue similar`

Writes nothing. `words` are the title's words it compared, lowercased, less the common ones: empty when the title has only common words, and then `issues` is empty too. `issues` are the open issues (Backlog, Todo, InProgress or InReview, not in the trash) holding more than half of `words`, best first, at most five.

```json
{"words": ["string"], "issues": ["RelatedIssue"]}
```

### `lcl issue list`

`total` counts every issue the filters match. `truncated` is true when `issues` holds fewer than that. With `--trashed` the issues are the ones in the trash, and no others; without it, none of them.

```json
{"issues": ["Issue"], "total": "integer", "limit": "integer", "offset": "integer", "truncated": "boolean"}
```

### `lcl issue show`

`children` are the sub-issues' identifiers.

```json
{
  "issue": "Issue",
  "comments": ["Comment"],
  "children": ["string"],
  "relations": {
    "blocks": ["RelatedIssue"],
    "blocked_by": ["RelatedIssue"],
    "related": ["RelatedIssue"],
    "duplicate_of": "RelatedIssue|null",
    "duplicates": ["RelatedIssue"]
  }
}
```

### `lcl issue update`

```json
"Issue"
```

### `lcl issue delete`

`deleted` is the identifier. The issue is in the trash, and `lcl issue restore` brings it back.

```json
{"deleted": "string", "id": "string"}
```

### `lcl issue restore`

The issue, live again: `deleted_at` is null.

```json
"Issue"
```

### `lcl issue purge`

What was removed for good: `purged` holds the identifiers, lowest number first, and the rest are counts. `lcl issue purge --all` with an empty trash prints zeros and exits 0.

```json
{"purged": ["string"], "issues": "integer", "comments": "integer", "relations": "integer"}
```

### `lcl issue relate`

`issue` and `related` are identifiers, and `kind` is as typed. `created` is false when the relation was already there.

```json
{"issue": "string", "kind": "'blocks'|'blocked-by'|'related'|'duplicate-of'", "related": "string", "created": "boolean"}
```

### `lcl issue unrelate`

```json
{"issue": "string", "kind": "'blocks'|'blocked-by'|'related'|'duplicate-of'", "related": "string", "removed": "boolean"}
```

### `lcl issue comment`

```json
"Comment"
```

### `lcl issue comments`

With `--trashed`, the issue's comments that are in the trash, and no others.

```json
["Comment"]
```

### `lcl issue history`

Oldest first, one entry per write. The first entry sets every field that isn't null, `old` null: the issue's creation, or for an issue made before lcl kept history, or brought in by `lcl import`, its fields when lcl began to record them (its `created_at` change says when the issue was made). An issue in the trash has a history too; a purged one has none.

```json
["HistoryEntry"]
```

## Comments

### `lcl comment edit`

```json
"Comment"
```

### `lcl comment delete`

`deleted` is the comment's id and `issue` the issue's identifier. The comment is in the trash, and `lcl comment restore` brings it back.

```json
{"deleted": "string", "issue": "string"}
```

### `lcl comment restore`

```json
"Comment"
```

### `lcl comment purge`

What was removed for good: `purged` holds the ids, oldest comment first.

```json
{"purged": ["string"], "comments": "integer"}
```

## Projects and milestones

### `lcl project create`

```json
"Project"
```

### `lcl project list`

```json
["Project"]
```

### `lcl project show`

```json
{
  "...": "Project",
  "issues": "IssueCounts",
  "milestones": [{"id": "string", "name": "string", "target_date": "string|null", "sort_order": "integer", "issues": "integer"}]
}
```

### `lcl project update`

```json
"Project"
```

### `lcl project delete`

`deleted` is the name.

```json
{"deleted": "string", "id": "string"}
```

### `lcl milestone create`

```json
"Milestone"
```

### `lcl milestone list`

```json
["Milestone"]
```

### `lcl milestone show`

```json
{"...": "Milestone", "issues": "IssueCounts"}
```

### `lcl milestone update`

```json
"Milestone"
```

### `lcl milestone delete`

`deleted` is the name, and `project` the project's name.

```json
{"deleted": "string", "id": "string", "project": "string"}
```

## Labels and cycles

### `lcl label create`

```json
"Label"
```

### `lcl label list`

With `--trashed`, the labels in the trash, and no others.

```json
["Label"]
```

### `lcl label delete`

`deleted` is the name. `removed_from_issues` counts the issues that showed the label. The label is in the trash, and `lcl label restore` puts it back on them.

```json
{"deleted": "string", "id": "string", "removed_from_issues": "integer"}
```

### `lcl label restore`

`restored` is the name. `restored_to_issues` counts the issues that show the label again.

```json
{"restored": "string", "id": "string", "restored_to_issues": "integer"}
```

### `lcl label purge`

What was removed for good: `purged` holds the names, and `removed_from_issues` counts the links to issues that went with them.

```json
{"purged": ["string"], "labels": "integer", "removed_from_issues": "integer"}
```

### `lcl cycle create`

```json
"Cycle"
```

### `lcl cycle list`

`issues` counts the issues in the cycle.

```json
[{"...": "Cycle", "issues": "integer"}]
```

### `lcl cycle report`

A cycle runs from the first moment of its first day to the first moment after its last, in the machine's local time; one not over yet runs to now. Membership comes from each issue's history. `scope` is null, and `scope_unavailable` says why, for a cycle with no start date, or one that started before lcl kept history and held issues then (from an upgrade to schema 14 or an import): when those issues went in is not recorded. `completed` counts the issues the cycle held at its end that were completed (are Done, and were completed before its end). `unestimated` counts the issues in `scope` or `completed` with no estimate. `velocity` is `completed` for up to five cycles before this one, by start date (cycles with none after those with one, by creation), then this one. Issues in the trash are left out.

```json
{
  "cycle": "Cycle",
  "scope": "CycleScope|null",
  "scope_unavailable": "string|null",
  "completed": "CycleCount",
  "unestimated": "integer",
  "velocity": [{"...": "Cycle", "completed": "CycleCount"}]
}
```

### `lcl cycle update`

```json
"Cycle"
```

### `lcl cycle delete`

`deleted` is the name.

```json
{"deleted": "string", "id": "string"}
```

## Docs

### `lcl doc create`

```json
"Doc"
```

### `lcl doc list`

With `--trashed`, the docs in the trash, and no others.

```json
["Doc"]
```

### `lcl doc show`

```json
"Doc"
```

### `lcl doc update`

```json
"Doc"
```

### `lcl doc delete`

`deleted` is the id. The doc is in the trash, and `lcl doc restore` brings it back.

```json
{"deleted": "string"}
```

### `lcl doc restore`

```json
"Doc"
```

### `lcl doc purge`

What was removed for good: `purged` holds the ids.

```json
{"purged": ["string"], "docs": "integer"}
```

## Search

### `lcl search`

`matched_in` lists where an issue matched. `issue_total` and `doc_total` count every match, and `truncated` is true when either list holds fewer.

```json
{
  "query": "string",
  "issues": [{"...": "Issue", "matched_in": ["'identifier'|'ref'|'title'|'description'|'comments'"]}],
  "docs": ["Doc"],
  "issue_total": "integer",
  "doc_total": "integer",
  "limit": "integer",
  "truncated": "boolean"
}
```

## Insights

### `lcl insights`

The range is `since` to `until`, both included, as YYYY-MM-DD; with neither flag, the four weeks to today. Days are in the machine's local time. Of the issues completed in the range: `completed` counts them, `per_week` is `completed` over the range's days, times 7, to two decimals, and `weeks` has each week (Monday to Sunday, `week` its Monday) the range touches, oldest first, with those completed in it and in the range. Lead time is from `created_at` to `completed_at`, cycle time from `started_at` to `completed_at`, in whole seconds: `median` and `p90` (the 90th percentile, interpolated as a spreadsheet's PERCENTILE.INC does) are null when `issues` is 0. Canceled, Duplicate and trashed issues are in nothing. `left_out.not_completed` counts the other issues made by the range's end that aren't Done, or are Done with no `completed_at`; `left_out.not_started` counts the issues completed in the range with no `started_at`, which are in lead time only. A range with nothing in it exits 0 with zero counts; one that ends before it starts is refused with exit 2.

```json
{
  "since": "string",
  "until": "string",
  "completed": "integer",
  "per_week": "number",
  "weeks": [{"week": "string", "completed": "integer"}],
  "lead_time": {"issues": "integer", "median": "integer|null", "p90": "integer|null"},
  "cycle_time": {"issues": "integer", "median": "integer|null", "p90": "integer|null"},
  "left_out": {"not_completed": "integer", "not_started": "integer"}
}
```

## Backup

### `lcl backup`

`path` is as typed. `bytes` is the copy's size, and `schema_version` its schema.

```json
{"path": "string", "bytes": "integer", "schema_version": "integer"}
```

## Export and import

### `lcl export`

With `--output PATH`, what was written, as counts. Without it, stdout is the export itself and not this value.

```json
{
  "path": "string",
  "labels": "integer",
  "projects": "integer",
  "milestones": "integer",
  "cycles": "integer",
  "issues": "integer",
  "comments": "integer",
  "relations": "integer",
  "docs": "integer"
}
```

### `lcl import`

What the file holds, as counts, and the number the next new issue gets. With `--dry-run` nothing was written and `dry_run` is true. `skipped` lists the records whose type this lcl doesn't know, which it left out: empty when there were none.

With `--linear TEAM` the file is the Linear team's issues, read from Linear's API with the key in `LINEAR_API_KEY` and converted. `notes` says what the conversion left out or changed, one sentence each (issues in Linear's trash, cycles, documents, comment threads, links to other teams' issues, names trimmed of whitespace): empty for a file. A value lcl would refuse is a `usage` error naming the Linear issue, and nothing is written. A missing or refused key is `auth`; an unknown team `not_found`; Linear out of reach or limiting the rate, `network`.

```json
{
  "labels": "integer",
  "projects": "integer",
  "milestones": "integer",
  "cycles": "integer",
  "issues": "integer",
  "comments": "integer",
  "relations": "integer",
  "docs": "integer",
  "next_issue_number": "integer",
  "dry_run": "boolean",
  "skipped": ["SkippedRecords"],
  "notes": ["string"]
}
```

## Sync

See [the protocol](sync-protocol.md). The token is read from `LCL_SYNC_TOKEN`.

### `lcl sync start`

`workspace` is the service's id for the new replica, and `machine` this machine's. `sent` counts the changes pushed: the workspace's whole history.

```json
{"server": "string", "workspace": "string", "machine": "string", "sent": "integer", "numbers_left": "integer"}
```

### `lcl sync join`

`machine` is this machine's id. `received` counts the changes pulled: the replica's whole history.

```json
{"server": "string", "workspace": "string", "machine": "string", "received": "integer", "numbers_left": "integer"}
```

### `lcl sync`

`sent` counts the changes pushed and `received` those pulled from elsewhere. `not_applied` lists this machine's changes that the service accepted but did not apply, and `overwritten` the fields where a change made here gave way to a later one.

```json
{"sent": "integer", "received": "integer", "numbers_left": "integer", "not_applied": ["SyncConflict"], "overwritten": ["SyncConflict"]}
```

### `lcl sync leave`

`path` and the counts are as for `lcl export --output`. `server` and `workspace` name the replica deleted (or found gone); both are null when the workspace didn't sync, and only the export was written.

```json
{
  "path": "string",
  "labels": "integer",
  "projects": "integer",
  "milestones": "integer",
  "cycles": "integer",
  "issues": "integer",
  "comments": "integer",
  "relations": "integer",
  "docs": "integer",
  "server": "string|null",
  "workspace": "string|null"
}
```

### `lcl sync stop`

The service and the replica the workspace synced with, which is left as it is.

```json
{"server": "string", "workspace": "string"}
```

## The terminal UI

### `lcl tui`

No JSON output on success: it takes the terminal. An error under `--json` is the error line, as for any command.
