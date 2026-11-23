# lcl

lcl is a local tracker with one SQLite file per workspace. Issues, projects, milestones, cycles, labels, relations, comments and docs live in that file. Every write runs in one immediate transaction, together with the reads it depends on. Issue numbers come from a counter that only grows, so a deleted number is never reused.

The command line is the interface. `lcl --json` prints one JSON value on stdout, and on failure one JSON error on stderr. Exit 0 is success, 1 is a conflict or a failure against stored data, and 2 is a wrong command line.

## Install

lcl needs Rust 1.98 or later.

    cargo install --path crates/lcl-cli --locked

That puts `lcl` in `~/.cargo/bin`. `lcl --version` prints the version.

## Quick start

    cd my-project
    lcl init --key MP
    lcl label create bug
    lcl issue create --title "Login fails" --priority high --label bug
    lcl issue update MP-1 --status "in progress" --assignee ann
    lcl issue comment MP-1 "Reproduced on main"
    lcl issue list
    lcl search login
    lcl tui

`lcl init` makes the workspace at `.lcl/data.sqlite` and registers its name in `~/.lcl/index.sqlite`. Commands run in that directory or below it use that workspace. From anywhere else, name it with `--workspace NAME`. `lcl workspace create NAME --key KEY` makes a workspace under `~/.lcl/projects` instead.

`lcl init` also writes `.lcl/.gitignore`, so git leaves `.lcl/` alone. A SQLite file can't be merged, and one committed while lcl writes to it can be torn. To share a workspace, run `lcl export`; the file it writes is what another person imports with `lcl import`. An existing `.lcl/.gitignore` is left as it is. In a workspace an earlier lcl made, run `lcl init` again to add the file.

`lcl help`, and `--help` on any command, list every flag.

## For agents

The skill an agent reads to use lcl is [`skill/SKILL.md`](skill/SKILL.md). It describes the binary built from the same commit, and names its version: `lcl --version` prints the version the skill states. A test fails when they differ, and when an example in the skill names a command or flag the binary lacks.

To install it for Claude Code, copy the directory where Claude Code looks for skills:

    mkdir -p ~/.claude/skills
    cp -r skill ~/.claude/skills/lcl

Take the skill from the release whose binary you run. Another agent can be given `SKILL.md` as instructions as it is.

## Alpha testing

This is an alpha. Try to break it: odd values, huge files, many commands at once, killed processes, full disks, hand-edited files.

Keep real data safe while you do:

- Test with a throwaway `HOME`, so lcl can't reach your own workspaces: `export HOME=$(mktemp -d)`. `HOME` wins on Windows too; without it lcl uses `USERPROFILE` there.
- A backup is `lcl export --output backup.jsonl`. `lcl import backup.jsonl` restores it into an empty workspace with the same key.
- `lcl backup PATH` copies the workspace's file to PATH, also while lcl is in use. To restore it, put the copy where the workspace's file is while no lcl is running; `lcl workspace show` prints that path.

What counts as a bug:

- A panic, or any exit status other than 0, 1 or 2.
- A hang, or a command that takes seconds on a workspace of a few thousand issues.
- An error that doesn't say what to do, or that names no file when a file is at fault.
- A write that is lost, half done, or done twice. `sqlite3 .lcl/data.sqlite "PRAGMA integrity_check; PRAGMA foreign_key_check"` should print only `ok`.
- A workspace that `lcl export` refuses, or whose export does not import and export again to the same bytes.
- `--json` output that isn't one JSON value on stdout, or an error that isn't one JSON line on stderr.
- Anything the help text or `CHANGELOG.md` says that the program doesn't do.

A report needs:

- `lcl --version`, the operating system and the terminal.
- The commands, in order, from `lcl init` on. A script is best.
- What happened, with the whole of stdout and stderr and the exit status, and what you expected.
- `lcl workspace show --json`, and the export if the workspace can be shared.

Known limits:

- Text sizes and name lengths have no limit. A value larger than the system lets a command line carry goes in through `--description-file`, `--content-file` or `--body-file`.
- `lcl import` and `lcl export` take a blank title, which the command line refuses: 0.1.0 stored them.
- `issue update` with no flags changes nothing but the issue's `updated_at`.
- A hand-edited value of the wrong type (text where a number goes) stops the commands that read it, with SQLite's own message.
- The TUI is read-only.

## Schema

`PRAGMA user_version` is the schema version. Opening a workspace applies the pending migrations in one transaction. Migrations are append-only: a shipped step is never edited. A file from a newer lcl is refused and left untouched.

Schema 11 holds the ownership rules in the tables. An issue status is one of the seven canonical spellings. Parents, comments, doc parents and cycle memberships name rows that exist. Deleting a parent, a doc that still has children, or a cycle that still has issues is refused. A new parent link or blocks link that would close a cycle is refused in the write transaction.

Schemas 12 and 13 add the trash, for issues and then for docs, comments and labels. `lcl issue delete` sets the issue's `deleted_at` and changes nothing else; `lcl issue restore` clears it. `doc`, `comment` and `label` have the same `delete`, `restore` and `purge`. An issue in the trash is left out of every list, search and count, and keeps its number and its ref. `lcl issue purge` is the only command that removes an issue for good, and only from the trash. Nothing is purged by itself.

Search uses FTS5 tables keyed by rowid. lcl does not run `VACUUM`. On open it checks that each index still matches its rows.

## Boundaries

lcl is one process and one file. It is not a multi-user server. It does not reuse issue numbers. It does not vacuum a workspace.

## Source

The source is public so it can be read and built. Pull requests are not accepted. [CONTRIBUTING.md](CONTRIBUTING.md) says what is accepted. How the program is put together is in [docs/architecture.md](docs/architecture.md). Building it is in [docs/building.md](docs/building.md). The license is [Apache-2.0](LICENSE).
