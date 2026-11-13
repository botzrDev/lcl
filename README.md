# lcl

lcl is a local tracker with one SQLite file per workspace. Issues, projects, milestones, cycles, labels, relations, comments and docs live in that file. Every write runs in one immediate transaction, together with the reads it depends on. Issue numbers come from a counter that only grows, so a deleted number is never reused.

The command line is the interface. `lcl --json` prints one JSON value on stdout, and on failure one JSON error on stderr. Exit 0 is success, 1 is a conflict or a failure against stored data, and 2 is a wrong command line.

## Schema

`PRAGMA user_version` is the schema version. Opening a workspace applies the pending migrations in one transaction. Migrations are append-only: a shipped step is never edited. A file from a newer lcl is refused and left untouched.

Schema 11 holds the ownership rules in the tables. An issue status is one of the seven canonical spellings. Parents, comments, doc parents and cycle memberships name rows that exist. Deleting an issue deletes its comments and cycle memberships. Deleting a parent, a doc that still has children, or a cycle that still has issues is refused. A new parent link or blocks link that would close a cycle is refused in the write transaction.

Search uses FTS5 tables keyed by rowid. lcl does not run `VACUUM`. On open it checks that each index still matches its rows.

## Boundaries

lcl is one process and one file. It is not a multi-user server. It does not reuse issue numbers. It does not vacuum a workspace.
w