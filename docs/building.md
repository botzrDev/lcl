# Building

The source is public so it can be read and built. Pull requests are not accepted.

lcl builds with Rust 1.98, pinned in `rust-toolchain.toml`.

```
cargo install --path crates/lcl-cli --locked
```

That puts `lcl` in `~/.cargo/bin`.

The checks this repository runs:

```
cargo fmt --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo deny check
```

CI runs `cargo fmt --check` on Linux, and clippy and the tests on Linux, macOS, and Windows. `cargo deny` checks advisories, licenses, and where dependencies come from. One advisory is ignored on purpose: `chrono` 0.4 (`RUSTSEC-2026-0543`) parses and prints dates, and it is marked unmaintained. `deny.toml` records that.

The tests run the `lcl` binary. Each one gets its own `HOME` and its own directory, so it cannot see a real `~/.lcl`.

A schema change is a new entry at the end of `MIGRATIONS` in `crates/lcl-core/src/migrations.rs`. `SCHEMA_VERSION` is the length of that list. A migration that has shipped is never edited.

The crates stay `publish = false`. A version bump changes `workspace.package.version` and the versions of `lcl-core` and `lcl-tui` under `workspace.dependencies` together. Behavior a person can see goes in `CHANGELOG.md` under `[Unreleased]`.
