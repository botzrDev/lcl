## What

## Checklist

- [ ] `cargo fmt --check`
- [ ] `cargo clippy --workspace --all-targets --locked -- -D warnings`
- [ ] `cargo test --workspace --locked`
- [ ] `cargo deny check`
- [ ] User-visible changes are under `[Unreleased]` in `CHANGELOG.md`
- [ ] A schema change is a new migration, and no shipped migration was edited
