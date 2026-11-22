# Contributing

lcl builds with Rust 1.98, pinned in `rust-toolchain.toml`.

```
cargo fmt --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo deny check
```

`cargo test` is the check that matters. A change that only passes on one operating system is not done: CI runs the same clippy and test commands on Linux, macOS, and Windows.

## What a change includes

- Behavior a user can see goes in `CHANGELOG.md` under `[Unreleased]`.
- A schema change is a new migration. A migration that has shipped is never edited. `PRAGMA user_version` only goes up.
- `--json` output, exit codes, and `lcl-export/1` are the interface agents use. A change to any of them is called out in the changelog.
- The crates stay `publish = false` until the crates.io name is chosen. The name `lcl` is already taken there. The binary name stays `lcl`.
- A version bump changes `workspace.package.version` and the `lcl-core` and `lcl-tui` versions under `workspace.dependencies` together. Those path dependencies carry a version so they are not wildcards.

## Reporting a bug

Use the bug template. A useful report has `lcl --version`, the operating system, the commands from `lcl init` on, and the full stdout, stderr, and exit status. The README lists what counts as a bug.
