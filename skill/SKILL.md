---
name: lcl
description: Use the `lcl` CLI (0.3.0-alpha.2), a local tracker that mirrors Linear with one SQLite file per workspace (issues, projects, milestones, cycles, labels, relations, comments, docs, and a trash for each). Use whenever the user asks to track work, create or manage issues, projects or documentation, look up a ticket (including by its Linear id such as BZR-1532), back up a workspace, run the TUI, or replace Linear with a local tool. Trigger on "lcl", "create an issue", "track this", "open a ticket", "project board", "local linear", or any request to manage tasks, issues or docs. Always pass `--json` when a program reads the output.
---

# lcl: local Linear-like tracker

This skill describes lcl 0.3.0-alpha.2: `lcl --version` prints `lcl 0.3.0-alpha.2`. If it prints another version, this skill may not match the binary; use the skill from the same release. Each workspace is one SQLite file. Never edit that file directly; every change goes through `lcl`.

## Which workspace

1. `-w NAME` (or `--workspace NAME`, or `LCL_WORKSPACE=NAME`): a registered workspace, found from any directory.
2. Otherwise, the nearest `.lcl/data.sqlite` at or above the current directory.

`lcl workspace list` shows the registered workspaces (`name`, `slug`, `kind`, `path`, `created_at`, `missing`). `lcl workspace show` shows the current one, with its `key`, `path` and `schema_version`.

To make one, ask the user first. The key is the prefix of every issue identifier (`DEMO` in `DEMO-12`): a letter, then up to 9 letters or digits. It is typed, never derived, and can't be changed later. Two workspaces may have the same key; names are what must differ.

```bash
lcl workspace create demo --key DEMO   # global: ~/.lcl/projects/demo.sqlite
lcl init --key DEMO                    # here: ./.lcl/data.sqlite, named after this directory
```

- `lcl init` also writes `.lcl/.gitignore` (`*`), so git ignores the workspace. To share a workspace, export it (see below); never commit the SQLite file.
- The first command a newer lcl runs on an older workspace upgrades it. It copies the file beside itself first, as `FILE.vN.snapshot` where N is the old schema version (`data.sqlite.v10.snapshot`), and prints one `lcl: upgraded …` notice on stderr naming the copy. That is not an error.

`LCL_PROJECT` still works but prints a deprecation notice; use `LCL_WORKSPACE`.

## The agent contract

- Pass `--json` on every call whose output you read. stdout is then one JSON value. The one exception is `lcl export` without `--output`, whose stdout is the export itself.
- Exit status: `0` success; `1` the command failed (not found, a conflict with what is stored, a newer schema, an I/O error); `2` the command line is wrong (a bad flag, value or combination).
  - Exit 2 means the command can't succeed as typed. Fix the arguments.
  - Exit 1 means it could succeed with other stored data, for example once a label exists.
- With `--json`, an error is the last line of stderr, and the only one starting with `{`:
  `{"error":{"kind":"usage|not_found|conflict|schema|io|network|auth|other","message":"..."}}`. Other stderr lines are notices starting with `lcl:`.
  - `conflict` includes naming something in the trash. The message names the `restore` command that brings it back.
- Writes are safe to run concurrently; each one is a single transaction. A write waits up to 30 s for another one; reads don't wait.
- For long or multi-line text, use `--description-file -`, `--content-file -` or `--body-file -` and pipe the text in. It is stored byte for byte, with no shell quoting to get wrong.

## Issues

An issue is named by `KEY-N` (`DEMO-12`), `#N`, `N` or its UUID. Numbers are never reused.

```bash
lcl --json label create bug
lcl --json label create urgent
lcl --json project create Parser
lcl --json issue create --title "Fix the parser" --priority high --label bug --project Parser --estimate 3
lcl --json issue create --title "Port the tests" --parent DEMO-1 --status todo --assignee ann
lcl --json issue list --status todo --status "in progress" --assignee ann
lcl --json issue list --project Parser --label bug --all
lcl --json issue show DEMO-1
lcl --json issue update DEMO-1 --status "in progress" --add-label urgent --due 2026-12-01
printf 'Steps:\n1. run it\n2. see it fail\n' | lcl --json issue update DEMO-1 --description-file -
```

- Statuses: Backlog, Todo, In Progress, In Review, Done, Canceled, Duplicate. Case, spaces, `_` and `-` are ignored on input. JSON prints `Backlog`, `Todo`, `InProgress`, `InReview`, `Done`, `Canceled` or `Duplicate`.
- Priority: `0`-`4`, or `none`, `urgent`, `high`, `medium`, `low` (0 is none). JSON prints the name: `None`, `Urgent`, `High`, `Medium` or `Low`.
- A blank `--title` is refused with exit 2.
- `issue list` returns `{"issues":[…],"total":N,"limit":50,"offset":0,"truncated":bool}`: 50 at a time, newest number first. Check `truncated`, and pass `--all` or `--limit`/`--offset` to see more. Filters apply before the limit. Other filters: `--parent`, `--blocked`, `--unblocked`, `--milestone`, `--cycle`, `--project none`, `--assignee none`, `--trashed`; and `--sort number|created|updated|priority`.
- `--blocked` lists an issue only while one of its blockers is Backlog, Todo, In Progress or In Review. An issue whose blockers are all Done, Canceled or Duplicate is listed by `--unblocked`.
- `issue update` returns the issue object. `issue create` returns it with `similar` beside its fields: up to five open issues like it, best first, each `{"identifier","status","title"}`. Before filing a second copy of something, look there: relate the new issue (`issue relate NEW related KEY-N`) or mark it a duplicate (`issue relate NEW duplicate KEY-N`). The list never stops or changes the create.
- `issue similar --title T [--description D]` runs the same check without creating anything: `{"words":[…],"issues":[…]}`. It compares the title's words, less common English words, tracker verbs (fix, add, update…) and words in more than a tenth of the issues; an open issue holding more than half of them is similar, and the description only ranks them. A title made only of common words finds nothing (`words` empty).
- `issue show` returns `{"issue":{…},"comments":[…],"children":["KEY-N",…],"relations":{"blocks":[],"blocked_by":[],"related":[],"duplicate_of":null,"duplicates":[]}}`. `children` holds identifiers; each relation entry, and `duplicate_of` when set, is `{"identifier","status","title"}`.
- Each issue object has `id`, `number`, `identifier`, `title`, `description`, `status`, `priority`, `assignee`, `parent_id`, `parent_identifier`, `project`, `milestone`, `ref`, `creator`, `estimate`, `due_date`, `labels`, `cycles`, `created_at`, `updated_at`, `started_at`, `completed_at`, `canceled_at` (Unix seconds, or null), and `deleted_at` (null unless it is in the trash).
- `issue update` changes only what its flags name.
  - Each `--x` has a `--no-x` that clears it: `--no-assignee`, `--no-estimate`, `--no-due`, `--no-parent`, `--no-project`, `--no-milestone`, `--no-cycle`, `--no-ref`.
  - `--label` replaces every label. `--add-label` and `--remove-label` change one, and `--clear-labels` removes them all.
  - `--description ""` clears the description.
- A label must exist before an issue can have it: `lcl label create NAME` first. Otherwise you get exit 1, kind not_found.

### Refs: ids from elsewhere

`--ref BZR-1532` records the id an issue has elsewhere, such as its Linear id. A ref is unique, ignoring case.

- `issue show` prints it.
- `lcl search BZR-1532` finds the issue.
- `lcl issue show BZR-1532` exits 2, and its message names the issue that has that ref.

```bash
lcl --json issue create --title "Imported ticket" --ref BZR-1532
lcl --json search BZR-1532
```

## The trash

Deleting moves to the trash; nothing is removed until you purge it. This holds for issues, docs, comments and labels.

```bash
lcl --json issue delete DEMO-3      # into the trash
lcl --json issue list --trashed     # what is there
lcl --json issue restore DEMO-3     # back, exactly as it was
lcl --json issue purge DEMO-3       # gone for good: ask the user first
```

- `issue delete` returns `{"deleted":"KEY-N","id":…}`; `issue restore` returns the issue; `issue purge` returns `{"purged":["KEY-N",…],"issues":N,"comments":N,"relations":N}`.
- An issue in the trash keeps its comments, labels, cycles, relations, number and ref. It is left out of lists, search, export, the TUI and counts, and doesn't block. A command that names it exits 1, kind `conflict`.
- `issue delete` is refused while the issue has sub-issues that aren't in the trash. `issue restore` is refused while its parent is in the trash.
- `issue purge` removes one issue in the trash with its comments and relations; `issue purge --all` empties the issue trash. It can't be undone. Only something in the trash can be purged. Purge only when the user asks.
- The same for the others: `doc delete|restore|purge ID`, `comment delete|restore|purge ID` and `label delete|restore|purge NAME`, each `purge` also with `--all`. `doc list --trashed`, `label list --trashed` and `issue comments ID --trashed` list what is there.
- No issue shows a label in the trash, but lcl remembers which had it: `label restore` puts it back on each. Its name stays taken while it is in the trash.

## Relations and parents

```bash
lcl --json issue relate DEMO-2 blocks DEMO-1
lcl --json issue relate DEMO-3 related DEMO-1
lcl --json issue unrelate DEMO-2 blocks DEMO-1
```

- The kinds are `blocks`, `blocked-by`, `related` and `duplicate-of`. `A blocked-by B` is the same relation as `B blocks A`.
- `relate` returns `{"issue","kind","related","created"}`; `created` is false when the relation was there already. `unrelate` returns the same with `removed`.
- `duplicate-of` sets A's status to Duplicate. An issue is a duplicate of at most one issue.
- A loop of blocks, of duplicates or of parents is refused (exit 1).
- `issue update --parent DEMO-1` moves an issue under another; `--no-parent` makes it top-level.

## Comments

```bash
lcl --json issue comment DEMO-1 "Reproduced on main."
printf 'Root cause:\nthe tokenizer drops "-".\n' | lcl --json issue comment DEMO-1 --body-file - --author ann
lcl --json issue comments DEMO-1
```

- `issue comments` lists comments oldest first. Each has `id`, `issue_id`, `body`, `author`, `created_at` and `deleted_at`.
- The author defaults to `LCL_AUTHOR`, else `USER`.
- Edit or delete a comment by its `id` from that list:
  - `lcl --json comment edit ID "new text"` (or `--body-file -`). The author and date stay.
  - `lcl --json comment delete ID` moves it to the trash; `comment restore ID` brings it back.
- Adding, editing or deleting a comment doesn't change the issue's `updated_at`.

## History

```bash
lcl --json issue history DEMO-1
```

- Lists what changed in an issue, oldest first, one entry per write: `{"at", "author", "changes"}`. Each change is `{"field", "comment_id", "old", "new"}`.
- Fields are the `Issue` keys, but `parent` is the parent's `KEY-N`. `priority` is shown by name, and so are a project, a milestone, a label and a cycle.
- `labels`, `cycles` and the relations (`blocks`, `blocked_by`, `related`, `duplicate_of`, `duplicates`) change one member at a time. `old` null means the member was added; `new` null means it was removed.
- A comment's changes carry its `comment_id`, with field `body` (`old` null when it was added) or `deleted_at`.
- The first entry sets every field that isn't null. It is the creation, or for an issue made before lcl kept history, or brought in by `lcl import`, the fields as they were then.
- `author` is `--author`, else `LCL_AUTHOR`, else `USER`.
- History keeps old values, a description's included. Only a purge removes them: a purged issue, comment or label takes its history with it.
- An issue in the trash has a history too. `lcl export` doesn't carry history.

## Projects and milestones

```bash
lcl --json project create Lexer --status planned --target-date 2026-12-31
lcl --json milestone create Beta --project Lexer --target-date 2026-12-15
lcl --json issue update DEMO-1 --project Lexer --milestone Beta
lcl --json project show Lexer
lcl --json milestone list --project Lexer
```

- Project statuses: Backlog, Planned, Started (or In Progress), Paused, Completed, Canceled.
- A milestone belongs to one project for good, and its name is unique within that project. An issue's milestone must be in the issue's project. `issue create --milestone` needs `--project`.
- A milestone name used in several projects needs `--project` to pick one.
- `project delete` is refused while the project has issues or milestones, and `milestone delete` while issues have the milestone. Issues in the trash count; the message says how many are there.

## Labels and cycles

```bash
lcl --json label create needs-review --color "#d73a4a"
lcl --json label list
lcl --json cycle create "Sprint 1" --start 2026-11-16 --end 2026-11-29
lcl --json issue update DEMO-1 --cycle "Sprint 1"
lcl --json issue list --cycle "Sprint 1"
lcl --json cycle update "Sprint 1" --end 2026-11-30
lcl --json cycle list
```

- Label and cycle names are unique, ignoring case; a cycle name is also trimmed.
- `--cycle` puts an issue in exactly that cycle, and `--no-cycle` takes it out. A cycle name that several cycles share (possible in workspaces from before 0.2.0) needs the cycle's id.
- `cycle delete` is refused while any issue is in the cycle.
- `cycle report "Sprint 1"` returns the cycle's scope and velocity in issues and points (estimates): `scope` with `planned` (held at its start), `added` and `removed` (after it), `completed`, `unestimated` (issues with no estimate, counted as issues and in no point total), and `velocity`, the completed counts of up to five cycles before it and of it. Membership is read from each issue's history. `scope` is null, and `scope_unavailable` says why, when the cycle has no start date, or started before lcl kept history and held issues then.

## Docs

```bash
lcl --json doc create --title "Design notes" --content "First draft."
lcl --json doc list
```

- `doc show ID`, `doc update ID [--title] [--content | --content-file -] [--parent ID | --no-parent]` and `doc delete ID` take the id from `doc list`.
- `doc create --parent ID` nests a doc under another. `doc delete` is refused while docs that aren't in the trash are under it.

## Search

```bash
lcl --json search parser tokenizer
lcl --json search 'tok*'
lcl --json search -- -x
```

- Search matches issues by identifier (`DEMO-12`, `#12`), ref, title, description and comments, and docs by title and content. A comment matches when it holds every word. Each issue hit lists where it matched in `matched_in`: `identifier`, `ref`, `title`, `description` or `comments`. Nothing in the trash matches.
- Results come 50 issues and 50 docs at a time (`--limit`), with `issue_total`, `doc_total` and `truncated`.
- Every word must match. Words are plain text, and there are no operators: `AND`, `OR` and `NOT` are words that must match like any other, ignoring case, and quotes are dropped, so `"the parser"` searches for `the` and `parser` separately. A word ending in `*` matches as a prefix (`tok*`).
- A query that is exactly an issue's `KEY-N` or `#N` lists that issue first.
- Put `--` before any word that starts with `-`.

## Insights

```bash
lcl --json insights
lcl --json insights --since 2026-10-01 --until 2026-10-31 --project api --label bug
```

- Of the issues completed (moved to Done) in the range: lead time (created to completed) and cycle time (started to completed), each as `median` and `p90` in seconds, and throughput: `completed`, `per_week`, and `weeks` (each Monday with its count). With no `--since`/`--until`, the range is the four weeks to today, in local time.
- `--project` (or `none`), `--cycle` and `--label` (repeatable) narrow it, as `issue list`'s do.
- Canceled, Duplicate and trashed issues are left out. `left_out` counts the rest it couldn't use: `not_completed` (in neither time) and `not_started` (completed with no start time, so in lead time only).
- A range with nothing in it exits 0 with zero counts, and `median` and `p90` null. A range that ends before it starts exits 2.

## Export, import and backup

```bash
lcl export --output demo.jsonl
lcl -w other import demo.jsonl --dry-run
lcl --json backup demo-copy.sqlite
```

- `lcl export` writes the whole workspace as `lcl-export/1` text (one JSON record per line), to `--output PATH` or to stdout. It leaves out what is in the trash. This is how a workspace is shared: another person imports the file. With `--output` and `--json` it prints `path` and a count of each kind of record (`labels`, `projects`, `milestones`, `cycles`, `issues`, `comments`, `relations`, `docs`).
- `lcl -w NAME import FILE` restores such a file into an empty workspace that has the file's key (make one with `workspace create NAME --key KEY`). `--dry-run` checks everything and writes nothing. With `--json` it prints the same counts, `next_issue_number`, `dry_run`, `skipped` and `notes`. A workspace that isn't empty is refused with exit 1, kind `conflict`.
- `lcl import --linear TEAM` (instead of FILE) brings in Linear team TEAM's issues, by its key, from Linear's API, with the personal API key in `LINEAR_API_KEY`. It is the same import, into the same kind of empty workspace: Linear's ENG-123 becomes issue 123, with ref `ENG-123` when the workspace has another key. `notes` (also in a plain import's `--json`, empty there) lists what the conversion left out or changed: Linear's trash, cycles, documents, comment threads, links to other teams. A value lcl refuses exits 2 naming the Linear issue, and nothing is written. Exit 1, kind `auth`: no key, or a refused one; `not_found`: no such team; `network`: Linear out of reach, or its rate limit used up. Run it only when the owner asks: it reads their Linear workspace.
- A record whose `type` this lcl doesn't know (from a later lcl) is skipped, not refused: `skipped` lists each such type with its count (`[{"type": "attachment", "records": 2}]`), and the text output names them. An unknown key in a known record is still refused, exit 2.
- `lcl backup PATH` copies the workspace's SQLite file to PATH, safely while other lcl commands run. PATH must not exist; its directory must. With `--json` it prints `path`, `bytes` and `schema_version`. To restore it, put the copy where the workspace's file is (`lcl workspace show` prints that path) while no lcl is running.

## Sync

```bash
lcl --json sync
```

- Sync is opt-in, per workspace, and only the owner opts in: `lcl sync start URL` (it needs `LCL_SYNC_TOKEN`). Don't run it unless asked. `lcl workspace show` has `sync`: null for a workspace that doesn't sync.
- In a workspace that syncs, `lcl sync` sends this machine's changes, receives the others', and prints `sent`, `received`, `numbers_left`, `not_applied` (changes the service didn't apply) and `overwritten` (fields where a later change from elsewhere won). `lcl sync join URL ID` joins from another machine, into an empty workspace with the same key. `lcl sync leave --output PATH` exports, then deletes the service's replica (only when the owner asks). Exit 1, kind `network`: the service is out of reach; the edits are kept and go next time. Kind `auth`: no token, or a refused one.
- A synced workspace numbers new issues from blocks the service hands out. When they run out, `issue create` exits 1, kind `conflict`, until `lcl sync` gets more. Every other write works offline.

## TUI

`lcl tui` is an issue browser for people: j/k to move, 1-7 to filter by status, 0 for all, Enter to open an issue's detail (c there to comment) and Esc to go back, / to search, p for the next project, c to create an issue (title, then description), s and ! to set the selected issue's status and priority, r to reload, q or Ctrl-C to quit. Its writes go through the same code as `issue create`, `issue update` and `issue comment`. Agents use the CLI.
