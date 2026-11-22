# Contributions

The source of lcl is public so it can be read and built. Pull requests are not accepted, and neither are feature requests.

A bug found while trying the alpha can be reported as [the README](README.md) describes, under Alpha testing. A vulnerability is reported as [SECURITY.md](SECURITY.md) describes.

## The `--json` contract

[docs/json-contract.md](docs/json-contract.md) holds the shape of every command's `--json` output, the exit statuses and the error line. Programs read that output, so a change to it is a change to an API.

- A change to an output shape, an exit status or an error kind changes that note in the same commit. The tests in `crates/lcl-cli/tests/contract.rs` fail until it does.
- A new command gets a section in the note and a test in that file.
- Every contract change is listed in `CHANGELOG.md`, under the version that ships it.
