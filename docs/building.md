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

The crates stay `publish = false`. A version bump changes `workspace.package.version` and the versions of `lcl-core` and `lcl-tui` under `workspace.dependencies` together. Behavior a person can see goes in `CHANGELOG.md` under `[Unreleased]`. So does every change to [the `--json` contract](json-contract.md).

## Releasing

`.github/workflows/release.yml` builds and publishes a release when a tag `vX.Y.Z` (or `vX.Y.Z-pre`) is pushed. Before anything is built, `.github/scripts/release-check.sh` fails the run unless:

- the tag is `v` and the workspace version;
- `CHANGELOG.md` has a section for that version, which becomes the release notes;
- `skill/SKILL.md` states that version.

It builds `lcl` for five targets, each with the bundled SQLite:

| Target | Runner | Version checked by running it |
|---|---|---|
| `x86_64-unknown-linux-gnu` | `manylinux_2_28` container on `ubuntu-24.04` | yes |
| `aarch64-unknown-linux-gnu` | `manylinux_2_28` container on `ubuntu-24.04-arm` | yes |
| `aarch64-apple-darwin` | `macos-latest` | yes |
| `x86_64-apple-darwin` | `macos-latest`, cross-compiled | when the runner has Rosetta |
| `x86_64-pc-windows-msvc` | `windows-latest` | yes |

The Linux builds run in PyPA's `manylinux_2_28` images (AlmaLinux 8), so the binaries need glibc 2.28 or later: RHEL 8, Debian 10, Ubuntu 20.04 and newer. The Windows build links the C runtime in, so `lcl.exe` runs without the Visual C++ redistributable.

The release gets `lcl-VERSION-TARGET.tar.gz` (`.zip` on Windows) for each target, each holding the binary, `LICENSE` and `README.md`. It also gets `SKILL.md`, the agent skill, and `SHA256SUMS`, which covers every other file. A version with a `-` is published as a pre-release.

To cut one: set the version in `Cargo.toml` (see above) and in `skill/SKILL.md`, rename `[Unreleased]` in `CHANGELOG.md` to `[X.Y.Z] - DATE`, land that on `main`, then tag it and push the tag:

```
git tag vX.Y.Z
git push origin vX.Y.Z
```

A dry run does the same checks and builds and publishes nothing. The files it would release, and the notes, are uploaded as an artifact of the run, and the notes are shown in its summary. Start one with `gh workflow run release.yml -f tag=vX.Y.Z`. Any push to a branch that changes the workflow or the check script starts one too.

## Installing

`install.sh` (Linux and macOS) and `install.ps1` (Windows) install a release; the README gives the line that runs each. `.github/workflows/install.yml` tests them. It runs when they change, after a release is published, and by hand:

- It installs with the install line on `ubuntu-24.04`, `ubuntu-24.04-arm`, `macos-latest`, `macos-15-intel`, and on `windows-latest` in PowerShell 7 and Windows PowerShell 5.1. Each then runs `lcl --version`, `lcl init` and `lcl issue create`. The line fetches the script of the commit under test, not main's.
- `.github/scripts/test-install.sh` and `.github/scripts/test-install.ps1` run the scripts against fake releases on disk, through `LCL_RELEASES_URL`: the build each platform gets, and every failure that must install nothing. Both run locally. The PowerShell one needs Windows PowerShell 5.1 and a release's Windows archive:

```
.github/scripts/test-install.sh
powershell -NoProfile -ExecutionPolicy Bypass -File .github/scripts/test-install.ps1 -Zip lcl-VERSION-x86_64-pc-windows-msvc.zip
```
