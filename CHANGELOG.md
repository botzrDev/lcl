# Changelog

All notable changes to lcl are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- One command installs lcl from the newest release: `curl -fsSL https://raw.githubusercontent.com/botzrDev/lcl/main/install.sh | sh` on Linux and macOS, `irm https://raw.githubusercontent.com/botzrDev/lcl/main/install.ps1 | iex` in PowerShell on Windows. It picks the build for the machine, checks it against `SHA256SUMS`, and checks that it runs and prints the release's version before it puts it in place. A failed download, a checksum that doesn't match, or a binary that doesn't run installs nothing and exits 1. `LCL_VERSION` installs a given release and `LCL_INSTALL_DIR` chooses the directory. Until 1.0 the newest release can be a pre-release.
- The README says how to move a workspace to another machine or person with `lcl export` and `lcl import`, why `.lcl/` must stay out of synced folders and network drives, and gives a snippet for a project's own README: the install line, `lcl init --key`, and how to export.

### Changed

- `lcl import` skips a record whose `type` it doesn't know, and says so: `Skipped 3 records of types this lcl doesn't know: attachment (2), initiative (1)`. 0.3.0-alpha.2 refused the file, exit 2. With `--json`, the output adds `skipped`, a list of `{"type", "records"}`, empty when nothing was skipped. A later `lcl-export/1` adds what it adds as new record types, so an older lcl reads the rest of a newer file. An unknown key in a known record is still refused, exit 2. So is a type that is a known one in other case, as `"Issue"`, a blank type, and an unknown type on the first line, which must be the workspace header.

## [0.3.0-alpha.2] - 2026-11-23

The second alpha, and the first with binaries: for Linux, macOS and Windows, on the GitHub release.

### Added

- `lcl backup PATH` copies the workspace's file to PATH, safely while other lcl commands read and write it. The copy is one committed state. It is written under a temporary name and gets its name only when complete, so PATH is never half a file. A PATH that exists is refused with exit 1. So is a PATH whose directory is missing or can't be written, with kind `io` and the reason. A backup that can't get a lock in 30 seconds exits 1 with kind `other`, as a write does. The copy has the workspace file's permissions, as does the snapshot taken before an upgrade. With `--json` it prints `path`, `bytes` and `schema_version`.

- `lcl issue restore ID` brings an issue back from the trash, exactly as it was: its comments, labels, cycles and relations never left.
- `lcl issue list --trashed` lists the trash, and only the trash.
- `lcl issue purge ID` removes one issue in the trash for good, with its comments, relations, label links and cycle links. `lcl issue purge --all` empties the trash. It can't be undone, and it is the only command that removes an issue permanently. A live issue can't be purged: delete it first. An issue whose sub-issues are in the trash is purged after them, or with them under `--all`. A purged issue's number is never used again. Nothing is purged by itself. With `--json` it prints `purged`, `issues`, `comments` and `relations`.
- An issue's JSON has `deleted_at`: when it went to the trash, or null.
- Docs, comments and labels have the same trash. `lcl doc restore ID`, `lcl comment restore ID` and `lcl label restore NAME` bring one back as it was. `lcl doc purge`, `lcl comment purge` and `lcl label purge` remove one in the trash for good, or every one with `--all`. `lcl doc list --trashed`, `lcl label list --trashed` and `lcl issue comments ID --trashed` list what is there. Their JSON has `deleted_at` too.

- Pushing a version tag publishes a GitHub release with `lcl` for `x86_64` and `aarch64` Linux, `aarch64` and `x86_64` macOS, and `x86_64` Windows, with `SHA256SUMS` and the agent skill. The Linux binaries need glibc 2.28 or later. The Windows one needs no Visual C++ redistributable. `docs/building.md` says how a release is cut and dry-run.
- The agent skill is in the repo, at `skill/SKILL.md`, and describes this version. It covers the trash, `lcl backup` and the `.lcl/.gitignore`. The README says how to install it.
- `lcl init` writes `.lcl/.gitignore` containing `*`, so git ignores the workspace's file, its WAL and its snapshots without a step from you. An existing `.lcl/.gitignore` is left as it is. If the file can't be written, `lcl init` exits 1, kind `io`, and makes no workspace.

### Upgrading from 0.3.0-alpha.1

- The first command that opens a workspace upgrades it to schema 13. Schema 12 adds the trash for issues and schema 13 for docs, comments and labels. Everything starts out of it.
- A workspace made by an earlier `lcl init` has no `.lcl/.gitignore`. Run `lcl init` again in its directory, with its name and key, to add one. That changes nothing else. A file git already tracks stays tracked: `git rm -r --cached .lcl` stops tracking it.

You no longer need to back up by hand before an upgrade.

- Before an upgrade changes a workspace file, lcl copies it beside itself. `data.sqlite` at schema 10 is copied to `data.sqlite.v10.snapshot`.
- The registry, `~/.lcl/index.sqlite`, is copied the same way before its own upgrade, as `index.sqlite.v0.snapshot` for one 0.1.0 wrote.
- The copy is made the way `lcl backup` makes one, while lcl holds the lock the upgrade runs under. It is the file exactly as the upgrade found it.
- The upgrade line on stderr names the copy: `lcl: upgraded workspace "old" to schema 11; the file as it was is PATH`. What the upgrade changed follows, if anything.
- If the copy can't be written, as on a full disk, the upgrade does not run. The command exits 1 and the file is as it was.
- A snapshot is never overwritten. If the name is taken, the next one is `…snapshot.2`.
- An upgrade that fails changes nothing and keeps no snapshot.
- To go back, put the snapshot where the file is, under the file's name, while no lcl is running, and use the older lcl.
- lcl never deletes a snapshot. Delete one when you no longer want it.

### Changed

- `lcl issue delete` moves the issue to the trash and no longer removes it. It prints `Moved DEMO-3 to the trash; restore it with: lcl issue restore DEMO-3`. Its `--json` output is unchanged.
- An issue in the trash is left out of `issue list`, `search`, the TUI, a parent's sub-issues, other issues' relations, and the counts of projects, milestones and cycles. It does not block: `--blocked` ignores it.
- A command that names an issue in the trash exits 1, kind `conflict`, with `DEMO-3 is in the trash; restore it with: lcl issue restore DEMO-3`. 0.3.0-alpha.1 said `issue DEMO-3 not found`.
- `issue delete` is refused only while the issue has sub-issues that are not in the trash. `issue restore` is refused while the issue's parent is in the trash.
- An issue in the trash keeps its number and its ref, and stays in its project, milestone and cycles. `project delete`, `milestone delete` and `cycle delete` still count it, and say how many of the issues they count are in the trash.
- `lcl export` leaves out issues in the trash, with their comments and relations. `lcl-export/1` is unchanged. `lcl import` refuses a workspace whose only issues are in the trash, as it refuses any workspace that isn't empty.
- `lcl doc delete`, `lcl comment delete` and `lcl label delete` move to the trash and no longer remove. Each prints the command that restores. Their `--json` output is unchanged.
- `lcl label delete` no longer takes the label off its issues for good. No issue shows it while it is in the trash, and `lcl label restore` puts it back on every issue that had it. Its name stays taken: `lcl label create` with that name exits 1 and names `lcl label restore`.
- A doc, comment or label in the trash is left out of lists, `issue show`, search and `lcl export`. A command that names one exits 1, kind `conflict`, and names the restore command. 0.3.0-alpha.1 said `not found`.
- `doc delete` is refused only while docs that are not in the trash are under the doc. `doc restore` is refused while the doc's parent is in the trash.
- A comment of an issue in the trash can't be edited, deleted or restored until the issue is restored. `lcl issue purge` removes all of an issue's comments.
- Every upgrade of a workspace prints one line on stderr, naming the snapshot. Before, an upgrade with nothing to repair printed nothing.
- An upgrade of the registry prints one line on stderr too.

### Fixed

- A read, such as `lcl issue list`, no longer waits while another lcl holds the write lock. Opening a workspace checked the search indexes with a write statement, so a read behind a long write waited 30 seconds and then failed with kind `schema`, saying the index did not match its rows. The check on open is now read-only. The full check, which also compares the words, runs in a process's first write and in an upgrade, under the lock they hold. A busy database is never reported as a mismatch.
- On Windows, lcl finds the home directory through `USERPROFILE` when `HOME` is not set or is empty, so `lcl init` and `lcl workspace create` work in cmd and PowerShell. 0.3.0-alpha.1 exited 1 there. `HOME` still wins when it is set, on every platform, so a registry made under it stays where it is. With neither, the error names both; lcl never falls back to another directory.

## [0.3.0-alpha.1] - 2026-11-23

The first alpha. Found by rough handling before it.

### Upgrading from 0.2.0

- A 0.2.0 built before schema 11 was added leaves a workspace at schema 10. The first command from this version that opens it upgrades it to schema 11, in place.
- That older build then refuses the workspace as one from a newer lcl, and leaves it untouched. Back up `~/.lcl` and every `.lcl/` directory first.
- `lcl workspace show --json` prints `schema_version`.

### Changed

- The local tool is licensed Apache-2.0. The crates stay unpublished.
- `issue create`, `issue update`, `doc create` and `doc update` refuse a blank `--title` with exit 2. `lcl import` and `lcl export` still take one, because 0.1.0 stored them.
- An empty `.lcl/data.sqlite` is refused with exit 1 and left empty. 0.2.0 opened it as a workspace with no name and no key. `lcl init` replaces it.
- An error prints its cause once. 0.2.0 printed a SQLite error three times, as `database error: X`, `X` and `Error code N: X`.
- An error from the registry that names no file now starts `open the workspace registry PATH`.
- `doc update` of a doc that doesn't exist says `doc ID not found`, as `doc show` and `doc delete` do. 0.2.0 said `not found: ID`.
- A cycle named in an error shows at most its first five and last five steps, as in `W-1 -> … 91 more … -> W-1`.
- On a terminal, a control character in text lcl prints shows as its picture, so ESC is `␛`. A newline and a tab stay. Output to a pipe or a file is unchanged, byte for byte, and so is `--json`.

### Fixed

- A command no longer fails at once with `database is locked` when it starts while another lcl is opening the same workspace, as after a killed lcl. It waits, as a write does.
- `lcl import` and `lcl export` take time in proportion to the workspace. A chain of 50,000 sub-issues took over ten minutes to import; it takes about a second. A long chain of blocks or duplicate-of relations is checked in one pass too.
- `lcl tui` without a terminal says so. 0.2.0 said `No such device or address (os error 6)`.
- The TUI lays out only the rows on screen. With 20,000 issues each key took 45 ms; it takes under 1 ms.
- Creating an issue after number 9223372036854775807 says the workspace has run out of numbers. 0.2.0 printed a SQLite type error.
- On Windows, usage and error text, `--json`'s too, name the program `lcl`. 0.2.0 said `lcl.exe`.
- On Windows, upgrading a registry from before 0.2.0 keeps a global workspace global. 0.2.0 took its path, recorded with `\`, for a cwd workspace's.

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

