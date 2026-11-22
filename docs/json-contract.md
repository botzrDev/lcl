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
{"error": {"kind": "'usage'|'not_found'|'conflict'|'schema'|'io'|'other'", "message": "string"}}
```

- `usage` comes with exit 2. Every other kind comes with exit 1.
- `not_found`: a named issue, label, project, milestone, cycle, doc, comment or workspace does not exist.
- `conflict`: the change is refused because of what is stored, as deleting a project that still has issues.
- `schema`: the file's schema is newer than this build reads, or is not a schema lcl wrote.
- `io`: a file could not be read or written.

Other stderr lines are notices. Each starts with `lcl:`. Without `--json`, an error is `Error: MESSAGE`.

## How to read a shape

A shape is JSON. A string in it names what stands at that place:

- `"string"`, `"integer"`, `"boolean"` and `"null"` are JSON types.
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
  "canceled_at": "integer|null"
}
```

### `RelatedIssue`

An issue as a relation names it.

```json
{"identifier": "string", "status": "Status", "title": "string"}
```

### `Comment`

`issue_id` is the issue's `id`, not its `KEY-N`.

```json
{"id": "string", "issue_id": "string", "body": "string", "author": "string|null", "created_at": "integer"}
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

### `Label`

```json
{"id": "string", "name": "string", "color": "string"}
```

### `Doc`

```json
{"id": "string", "title": "string", "content": "string", "parent_id": "string|null", "created_at": "integer", "updated_at": "integer"}
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

```json
{"name": "string", "key": "string", "description": "string", "path": "string", "schema_version": "integer", "created_at": "integer"}
```

## Issues

### `lcl issue create`

```json
"Issue"
```

### `lcl issue list`

`total` counts every issue the filters match. `truncated` is true when `issues` holds fewer than that.

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

`deleted` is the identifier.

```json
{"deleted": "string", "id": "string"}
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

```json
["Comment"]
```

## Comments

### `lcl comment edit`

```json
"Comment"
```

### `lcl comment delete`

`deleted` is the comment's id and `issue` the issue's identifier.

```json
{"deleted": "string", "issue": "string"}
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

```json
["Label"]
```

### `lcl label delete`

`deleted` is the name.

```json
{"deleted": "string", "id": "string", "removed_from_issues": "integer"}
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

`deleted` is the id.

```json
{"deleted": "string"}
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

What the file holds, as counts, and the number the next new issue gets. With `--dry-run` nothing was written and `dry_run` is true.

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
  "dry_run": "boolean"
}
```

## The terminal UI

### `lcl tui`

No JSON output on success: it takes the terminal. An error under `--json` is the error line, as for any command.
