# lcl

lcl is a local tracker with one SQLite file per workspace. Issues, projects, milestones, cycles, labels, relations, comments and docs live in that file. Every write runs in one immediate transaction, together with the reads it depends on. Issue numbers come from a counter that only grows, so a deleted number is never reused.

The command line is the interface. `lcl --json` prints one JSON value on stdout, and on failure one JSON error on stderr. Exit 0 is success, 1 is a conflict or a failure against stored data, and 2 is a wrong command line.

## Install

On Linux or macOS:

    curl -fsSL https://raw.githubusercontent.com/botzrDev/lcl/main/install.sh | sh

On Windows, in PowerShell:

    irm https://raw.githubusercontent.com/botzrDev/lcl/main/install.ps1 | iex

It installs the newest [release](https://github.com/botzrDev/lcl/releases), which until 1.0 can be an alpha. It checks the download against the release's `SHA256SUMS` and installs nothing if they differ. `lcl` goes in `~/.local/bin`, or `%LOCALAPPDATA%\Programs\lcl` on Windows, which is added to your user `PATH`. `lcl --version` prints the version. To install a given release, set `LCL_VERSION`, as `LCL_VERSION=v0.3.0-alpha.2`; to choose the directory, `LCL_INSTALL_DIR`. The other settings are at the top of [`install.sh`](install.sh) and [`install.ps1`](install.ps1).

The builds are for Linux (x86_64 and arm64, glibc 2.28 or later), macOS (Apple silicon and Intel) and Windows (x86_64). Elsewhere, [build from source](#source).

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

`lcl init` also writes `.lcl/.gitignore`, so git leaves `.lcl/` alone. A SQLite file can't be merged, and one committed while lcl writes to it can be torn. To share a workspace, export it, as the next section says. An existing `.lcl/.gitignore` is left as it is. In a workspace an earlier lcl made, run `lcl init` again to add the file.

`lcl help`, and `--help` on any command, list every flag.

## Moving a workspace

`lcl export` and `lcl import` move a workspace to another machine, or hand it to another person. They need no account and no network; lcl shows no banner and asks for no license. It reaches the network only when told to: `lcl sync`, and `lcl import --linear` (below).

    lcl export --output issues.jsonl

The file is text, one JSON record per line: the workspace's labels, projects, milestones, cycles, issues, comments, relations and docs. On the other machine, make a workspace with the same key and import it:

    lcl init --key MP
    lcl import issues.jsonl

The other person then sees the same issues, under the same numbers, with the same comments: MP-12 is MP-12 there too. The next issue either of you creates gets the same number.

An import fills an empty workspace; it doesn't merge. Changes made on two machines after the hand-over stay apart. To read a newer export, import it into a new workspace, as `lcl workspace create mp-next --key MP` then `lcl -w mp-next import issues.jsonl`. `lcl export --output` refuses a file that exists; `--force` replaces it.

### Coming from Linear

`lcl import --linear TEAM` brings a Linear team's issues into an empty workspace: titles, descriptions, statuses, priorities, estimates, due dates, labels, assignees, projects and milestones, parents, comments and relations. It reads them from Linear's API with a personal API key, made in Linear's settings, which lcl never stores:

    lcl init --key ENG
    LINEAR_API_KEY=lin_api_... lcl import --linear ENG --dry-run
    LINEAR_API_KEY=lin_api_... lcl import --linear ENG

ENG-123 stays ENG-123. Under another key, issue 123 keeps ENG-123 as its ref. Cycles, documents and issues in Linear's trash are not brought in, and the import says so. A value lcl can't take, such as an estimate of 0.5, stops the import before anything is written, naming the Linear issue.

### Not in a synced folder

Keep `.lcl/`, and `~/.lcl`, out of Dropbox, iCloud Drive, OneDrive, Google Drive and network drives. On Windows that includes Documents and Desktop when OneDrive backs them up.

lcl writes in SQLite's WAL mode. A change goes first to `data.sqlite-wal`, beside the file, and lcl processes take turns through file locks and a shared-memory file, `data.sqlite-shm`.

- A sync service copies each of those files on its own, when it sees it change. The copy on another machine can be the file without its latest changes, or a torn file.
- It doesn't carry the locks. Two machines writing at once each keep their own changes, and the service keeps one file and drops the other's, or makes a conflicted copy.
- A network drive breaks the locks and the shared memory, which work only between processes on one machine.

To keep a copy in a synced folder, put an export or an `lcl backup` there. Each is one file, and it gets its name only when it is complete.

### For your project's README

A project whose issues are in lcl can tell people how to get them. Paste this into its README, with your key for `MP`:

```markdown
## Issues

This project's issues are in [lcl](https://github.com/botzrDev/lcl), a local issue tracker. `issues.jsonl` is an export of them.

Install lcl on Linux or macOS:

    curl -fsSL https://raw.githubusercontent.com/botzrDev/lcl/main/install.sh | sh

On Windows, in PowerShell:

    irm https://raw.githubusercontent.com/botzrDev/lcl/main/install.ps1 | iex

In your clone, make the workspace and import the export:

    lcl init --key MP
    lcl import issues.jsonl
    lcl issue list

To hand the issues on, export them over the file:

    lcl export --output issues.jsonl --force
```

## For agents

The skill an agent reads to use lcl is [`skill/SKILL.md`](skill/SKILL.md). It describes the binary built from the same commit, and names its version: `lcl --version` prints the version the skill states. A test fails when they differ, and when an example in the skill names a command or flag the binary lacks.

To install it for Claude Code, copy the directory where Claude Code looks for skills:

    mkdir -p ~/.claude/skills
    cp -r skill ~/.claude/skills/lcl

Take the skill from the release whose binary you run. Each release has it as `SKILL.md`; from the directory you downloaded it to:

    mkdir -p ~/.claude/skills/lcl && cp SKILL.md ~/.claude/skills/lcl/

Another agent can be given `SKILL.md` as instructions as it is.

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
- The TUI creates an issue with a title and a one-line description, sets an issue's status and priority, and adds a one-line comment. Every other write is made on the command line.

## Schema

`PRAGMA user_version` is the schema version. Opening a workspace applies the pending migrations in one transaction. Migrations are append-only: a shipped step is never edited. A file from a newer lcl is refused and left untouched.

Schema 11 holds the ownership rules in the tables. An issue status is one of the seven canonical spellings. Parents, comments, doc parents and cycle memberships name rows that exist. Deleting a parent, a doc that still has children, or a cycle that still has issues is refused. A new parent link or blocks link that would close a cycle is refused in the write transaction.

Schemas 12 and 13 add the trash, for issues and then for docs, comments and labels. `lcl issue delete` sets the issue's `deleted_at` and changes nothing else; `lcl issue restore` clears it. `doc`, `comment` and `label` have the same `delete`, `restore` and `purge`. An issue in the trash is left out of every list, search and count, and keeps its number and its ref. `lcl issue purge` is the only command that removes an issue for good, and only from the trash. Nothing is purged by itself.

Search uses FTS5 tables keyed by rowid. lcl does not run `VACUUM`. On open it checks that each index still matches its rows.

## Boundaries

lcl is one process and one file. It is not a multi-user server. It does not reuse issue numbers. It does not vacuum a workspace.

## Source

To build lcl from source, with Rust 1.98 or later:

    cargo install --path crates/lcl-cli --locked

That puts `lcl` in `~/.cargo/bin`.

The source is public so it can be read and built. Pull requests are not accepted. [CONTRIBUTING.md](CONTRIBUTING.md) says what is accepted. How the program is put together is in [docs/architecture.md](docs/architecture.md). Building it is in [docs/building.md](docs/building.md). The license is [Apache-2.0](LICENSE).
