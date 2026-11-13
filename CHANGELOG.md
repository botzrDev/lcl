# Changelog

All notable changes to lcl are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.2.0] - 2026-11-12

Everything below is relative to 0.1.0. Builds between 0.1.0 and 0.2.0 were
development builds and were never released.

### Upgrading from 0.1.0

Before you upgrade:

- Back up `~/.lcl`. It holds the registry (`index.sqlite`) and every global workspace (`projects/*.sqlite`).
- Back up every `.lcl/` directory that `lcl init` made.
- 0.2.0 changes these files in place. 0.1.0 can't undo the change.

Each workspace:

- The first command that opens a 0.1.0 workspace upgrades it to schema 11.
- If the upgrade changed stored values, it prints one line on stderr saying what it changed. A later command prints nothing.
- The line reads like `lcl: upgraded workspace "old" to schema 11: moved 1 unreadable due date into a description, …`.
- A workspace with nothing to repair upgrades silently.
- A due date that isn't a real date written as `YYYY-MM-DD` is cleared.
- Its text moves to the end of the issue's description, as the line `Due (from 0.1.0): next week`.
- A cycle's start or end date that isn't a real date is cleared.
- Its text moves into the cycle's name, as in `Sprint 1 (start: soon)`.
- A blank due date or cycle date is cleared, with nothing added.
- An issue's parent, which 0.1.0 stored as typed, becomes that issue's UUID when it names one (as `N`, `#N`, `KEY-N` or a UUID), and is cleared otherwise.
- A doc's parent that names no doc is cleared.
- A parent link on a loop is cleared.
- Labels whose names match, ignoring case, merge into one.
- A label that an issue named but that didn't exist is created, with colour `#888888`.
- A blank label name becomes `unnamed`, or `unnamed-2` and so on when that is taken.
- A label colour that isn't `#rgb` or `#rrggbb` becomes `#888888`.
- A negative estimate is cleared.
- Assignees and comment authors are trimmed, and cleared when blank.

The registry:

- `~/.lcl/index.sqlite` is upgraded to registry v2 the first time lcl reads it.
- That happens on `lcl init`, `workspace create`, `workspace list`, and `--workspace`, `-w`, `LCL_WORKSPACE` or `LCL_PROJECT`.
- A command run inside a workspace directory, without one of those, leaves the registry alone.
- Every workspace 0.1.0 registered stays registered under its name.
- The registry keeps 0.1.0's `projects` table, so 0.1.0 still lists the workspaces it knew.

If you go back to 0.1.0:

- 0.1.0 opens an upgraded workspace without a warning.
- It shows a Duplicate issue as Backlog.
- It ignores the number high-water mark, so it can reuse a deleted issue's number.
- It doesn't set or clear `started_at` and `canceled_at`.
- `issue create --label` exits 1 with `FOREIGN KEY constraint failed`, but still writes the issue, without the label.
- `label delete NAME` deletes nothing, yet prints `Deleted NAME` and exits 0.
- `project list` doesn't show workspaces that 0.2.0 created.
- `--project NAME` still opens a global workspace that 0.2.0 created, by its slug.
- Values 0.1.0 writes, such as a due date of `tomorrow`, aren't repaired again when you return to 0.2.0.
- `lcl export` then refuses such a value, naming the command that fixes it.

Schema 11, applied on that same open:

- An issue status is stored as one of Backlog, Todo, InProgress, InReview, Done, Canceled, Duplicate.
- A parent, a comment, a doc parent and a cycle membership name a row that exists.
- Deleting an issue deletes its comments and its cycle memberships.
- Deleting an issue or a doc that still has children is refused. Deleting a cycle that still has issues is refused.
- Opening a workspace checks that the search indexes match their rows. `VACUUM` can break that; lcl does not run it.

### Changed

Workspaces:

- What 0.1.0 called a project is now a workspace.
- `lcl project` and `lcl milestone` now manage projects and milestones inside a workspace, as in Linear.
- `--project NAME` before the subcommand is now `--workspace NAME`, or `-w NAME`.
- `lcl --project NAME …` exits 2 with a hint naming the new form.
- `LCL_PROJECT` is now `LCL_WORKSPACE`.
- `LCL_PROJECT` still works, with a notice on stderr: `lcl: LCL_PROJECT is deprecated; use LCL_WORKSPACE`.
- An empty `LCL_WORKSPACE` or `LCL_PROJECT` counts as unset.
- `lcl project create NAME --global` is now `lcl workspace create NAME --key KEY`.
- `lcl project create NAME --global` exits 2 with a hint.
- 0.1.0's `project list` and `project show` are now `workspace list` and `workspace show`.
- A global workspace's path comes from the current `HOME`, so moving `HOME` keeps every global workspace.
- With no `HOME`, a command that needs it exits 1. 0.1.0 used `/tmp/.lcl`.

`lcl init`:

- `lcl init` needs `--key KEY`. 0.1.0 made the key from the name.
- A key is a letter, then up to 9 letters or digits. lcl uppercases it.
- Without a name, `init` still uses the current directory's name.
- In a directory with no name, such as `/`, `init` exits 2. 0.1.0 named the workspace `project`.
- Run again with the same name and key, `init` does nothing.
- `init` never renames or re-keys a workspace. A different name or key exits 1.
- `init` exits 1 for a name another workspace has.

`issue list`:

- `issue list --json` prints an object, `{"issues": […], "total", "limit", "offset", "truncated"}`, not an array.
- It still shows 50 issues by default.
- When more issues match, the text output ends with a line like `showing 1–50 of 60; use --all, or --limit/--offset`.
- `--limit` must be 1 or more.
- `--limit=-1` no longer means all. Use `--all`.
- An unknown `--status` exits 2. 0.1.0 ignored it and listed every issue.

`lcl search`:

- `lcl search` takes plain words, not an FTS5 query.
- Every word must match.
- `AND`, `OR`, `NOT`, `NEAR`, quotes, parentheses and punctuation are plain text.
- Several words need no quotes: `lcl search login bug`. 0.1.0 refused that with exit 2.
- A word that starts with `-` goes after `--`, as in `lcl search -- -x`.
- A word ending in `*` matches as a prefix.
- Search shows at most 50 issues and 50 docs. `--limit N` changes that.
- `search --json` adds `query`, `issue_total`, `doc_total`, `limit` and `truncated`.

Values that 0.1.0 accepted and 0.2.0 refuses with exit 2:

- `--status` accepts Backlog, Todo, In Progress, In Review, Done, Canceled or Duplicate, ignoring case, spaces, `_` and `-`.
- 0.1.0 turned an unknown status into Backlog on create, and ignored it on update.
- `--priority` accepts 0–4, or none, urgent, high, medium or low.
- `--due`, and `cycle create --start` and `--end`, accept a real date written as `YYYY-MM-DD`.
- `--estimate` accepts a whole number, 0 or more.
- `label create --color` accepts `#rgb` or `#rrggbb`.
- `cycle create --end` before `--start` is refused.
- A blank label name, cycle name or comment is refused.
- `--assignee ""` is refused. Unassign with `issue update --no-assignee`.
- An inline `-` as a description, content or comment is refused. To read stdin, use `--description-file -`, `--content-file -` or `--body-file -`.
- `--parent` accepts an issue id: `KEY-N`, `#N`, `N` or a UUID. 0.1.0 stored any text.

Commands that exited 0 in 0.1.0 and now exit 1:

- `--label` naming a label that doesn't exist.
- `--parent` naming an issue that doesn't exist.
- `doc create --parent` naming a doc that doesn't exist.
- `label delete` or `doc delete` of something that doesn't exist. 0.1.0 printed `Deleted`.
- `issue delete` of an issue with sub-issues.
- `label create` of a name that exists, ignoring case.
- `cycle create` of a name that exists, trimmed and ignoring case.

Commands that exited 1 in 0.1.0 and now exit 2:

- An issue id from another workspace, such as `OTHER-1` in workspace `WS`. 0.1.0 said `issue not found`.
- An issue id that isn't `KEY-N`, `#N`, `N` or a UUID, such as `abc`.
- An empty search, or one with no words, such as `"`. 0.1.0 printed an FTS5 error.

A command that exited 101 in 0.1.0 and now exits 0:

- Output into a closed pipe, as in `lcl issue list | head -1`. 0.1.0 panicked.

Errors:

- Exit 2 means the command can't succeed against any state of the workspace: a bad flag, value or combination.
- Exit 1 means it could, given other stored data: not found, a conflict, a newer schema, an I/O error.
- With `--json`, an error is one line on stderr: `{"error":{"kind":"…","message":"…"}}`. 0.1.0 printed text.
- It is the last line of stderr, and the only one that starts with `{`.
- `kind` is one of `usage`, `not_found`, `conflict`, `schema`, `io` or `other`.
- clap's own errors follow the same format under `--json`.
- Messages name what is missing, as in `issue WS-999 not found` and `doc nope not found`.

Names and case:

- Where a name is matched ignoring case (labels, keys, assignees, projects, milestones, cycles), only ASCII letters fold.
- So `Bug` matches `BUG`, but `Été` doesn't match `été`.
- `issue list --assignee` ignores ASCII case. 0.1.0 matched it exactly.

Output:

- `issue comments` prints each comment as a header, `AUTHOR · YYYY-MM-DD HH:MM · ID`, then its body indented two spaces.
- A blank line separates comments.
- The time is local, as `TZ` sets it.
- 0.1.0 printed `[SECONDS] author: body`.
- `issue show` prints comments the same way, without the id.
- Comment JSON is unchanged.
- `issue show` prints `Assignee: ann` or `Assignee: Unassigned`. 0.1.0 printed `Some("ann")` or `None`.
- `issue delete --json` prints `{"deleted": "WS-2", "id": "…"}`. 0.1.0 echoed the id as typed.
- `label delete` takes a name or an id. 0.1.0 took only an id.
- An issue's `parent_id` is always its parent's UUID. 0.1.0 stored what was typed.
- `workspace show --json` gives `created_at` as a number. 0.1.0's `project show --json` gave a string.
- Without `--author`, a comment's author is `LCL_AUTHOR`, else `USER`. 0.1.0 stored none.

### Added

Linear's structure:

- Projects in a workspace: `lcl project create|list|show|update|delete`.
- Milestones in a project: `lcl milestone create|list|show|update|delete`.
- `--project` and `--milestone` on `issue create`, `issue update` and `issue list`.
- `--no-project` and `--no-milestone` on `issue create` and `issue update`.
- Relations: `lcl issue relate A KIND B` and `lcl issue unrelate A KIND B`.
- `KIND` is `blocks`, `blocked-by`, `related` or `duplicate-of`.
- `duplicate-of` sets the issue's status to Duplicate.
- An issue is a duplicate of at most one issue.
- `issue list --blocked` and `--unblocked`.
- The Duplicate status.
- Sub-issues: `issue update --parent` and `--no-parent`, and `issue list --parent`.
- An issue can't be its own ancestor.
- `issue show` lists relations and sub-issues. Its JSON adds `relations` and `children`.
- Refs: `--ref` on `issue create` and `issue update`, and `--no-ref`.
- A ref is the id an issue has elsewhere, such as `BZR-1532` for a Linear issue.
- `lcl search BZR-1532` finds the issue with that ref.
- Cycles hold issues: `--cycle` on `issue create`, `issue update` and `issue list`, and `issue update --no-cycle`.
- `issue update --cycle` puts the issue in that cycle and takes it out of any other.
- Issue JSON adds `cycles`.
- Cycle list JSON adds `issues`, the number of issues in the cycle.
- `lcl cycle update`, with `--name`, `--start`, `--no-start`, `--end` and `--no-end`.
- `lcl cycle delete`, refused while an issue is in the cycle.
- `lcl comment edit ID` replaces a comment's text. Its author and date stay.
- `lcl comment delete ID`.

Moving data:

- `lcl export` writes the whole workspace in the format `lcl-export/1`, one JSON record per line.
- `export --output PATH` writes to a file, and `--force` replaces one.
- The same workspace always exports to the same bytes.
- `lcl import PATH` restores an export into an empty workspace with the file's key.
- `import --dry-run` checks the file and writes nothing.
- Import keeps numbers, refs, creators, comment authors and timestamps.

Workspaces:

- `lcl workspace create NAME --key KEY` makes a global workspace.
- `lcl workspace list`. Its JSON adds `kind`, `global` or `cwd`, and `missing`, true when the workspace's file is gone.
- `lcl workspace show`. Its JSON adds `path` and `schema_version`.
- `init --json` adds `created`, false when the workspace was already there.
- lcl refuses, with exit 1, a workspace or registry written by a newer lcl.

Issues:

- Commands accept an issue as `KEY-N` (such as `WS-12`) or `#N`, as well as `N` or its UUID.
- Issue JSON adds `identifier`, `parent_identifier`, `started_at`, `canceled_at`, `creator`, `ref`, `project`, `milestone` and `cycles`.
- Entering In Progress or In Review sets `started_at`. Moving back to Backlog or Todo clears it.
- Entering Canceled or Duplicate sets `canceled_at`. Leaving both clears it.
- Leaving Done clears `completed_at`.
- An issue records its creator: `issue create --author`, else `LCL_AUTHOR`, else `USER`.
- `issue update --add-label`, `--remove-label` and `--clear-labels`. `--label` still replaces every label.
- `issue update --no-assignee`, `--no-estimate` and `--no-due`.
- `issue list --label`, repeated to require every label.
- `issue list --status`, repeated to match any status.
- `issue list --assignee none` for unassigned issues.
- `issue list --sort number|created|updated|priority`, and `--offset N`.

Text:

- `--description-file`, `--content-file` and `--body-file` read text from a file, or from stdin when the path is `-`.
- The text is stored byte for byte.
- A terminal as stdin is refused.
- A stdin that sends nothing for `LCL_STDIN_WAIT` seconds is refused. The default is 10; 0 waits forever.
- `doc update --parent` and `--no-parent`.

Search:

- Search covers comments too.
- Search JSON gives each issue a `matched_in`, naming where it matched.
- A query that is exactly an issue's `KEY-N` or `#N` lists that issue first.

Help:

- `lcl --help` states the exit codes.
- Every flag and argument has help text.

### Fixed

- Parallel `issue create` runs all succeed, with distinct numbers. In 0.1.0, 19 of 20 run at once exited 1 with `UNIQUE constraint failed`.
- Every write now runs in one transaction, so concurrent updates, comments and label changes don't overwrite each other.
- A write waits up to 30 seconds for another lcl to finish.
- An issue's number is never reused, even after the newest issue is deleted.
- `issue list --assignee` no longer filters after the limit, which showed fewer issues than matched.
- `issue list` no longer cuts the list at 50 without saying so.
- Search for text with punctuation, such as `BZR-1532` or `v1.3.1`, matches it. 0.1.0 exited 1 with an FTS5 error.
- Output into a closed pipe no longer panics.
- In the TUI, `j` no longer moves past the end of a filtered list.
- In the TUI, `r` reloads the issues.
- In the TUI, Ctrl-C quits.
- The TUI restores the terminal on every way out, an error included.
- The TUI reads the issues before taking the terminal, so a database error ends it with that error. 0.1.0 showed an empty list.
- The TUI shows every issue. 0.1.0 showed the newest 200.

