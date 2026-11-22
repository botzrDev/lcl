# Security

lcl keeps each workspace in one SQLite file on the machine that runs it. It does not send that file anywhere.

Report a vulnerability through [GitHub private vulnerability reporting](https://github.com/botzrDev/lcl/security/advisories/new) for this repository. Include the version (`lcl --version`), the operating system, and a workspace you can share, or the commands that produce the problem from `lcl init` on.

A lost write, a half-written workspace, or a file `PRAGMA integrity_check` does not call `ok` is a vulnerability. A crash on bad input is a bug; use a public issue for that.
