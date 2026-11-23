#!/usr/bin/env bash
# Checks that TAG may be released from this checkout, and writes the
# release notes. Run from the repository root:
#
#   .github/scripts/release-check.sh TAG NOTES_FILE
#
# It fails, before anything is built, unless:
# - TAG is v followed by the workspace version in Cargo.toml;
# - CHANGELOG.md has a section for that version;
# - skill/SKILL.md states that version.
# On success it prints `version=V` and `prerelease=true|false`, for
# $GITHUB_OUTPUT, and writes the notes to NOTES_FILE.
set -euo pipefail

tag=${1:?usage: release-check.sh TAG NOTES_FILE}
notes=${2:?usage: release-check.sh TAG NOTES_FILE}

fail() {
  echo "release-check: $*" >&2
  exit 1
}

version=$(python3 -c 'import tomllib; print(tomllib.load(open("Cargo.toml", "rb"))["workspace"]["package"]["version"])')
[[ "$tag" == "v$version" ]] ||
  fail "the tag $tag is not v$version, the workspace version in Cargo.toml"

grep -qF "## [$version]" CHANGELOG.md ||
  fail "CHANGELOG.md has no section \"## [$version]\" for the release notes"
grep -qF "\`lcl --version\` prints \`lcl $version\`" skill/SKILL.md ||
  fail "skill/SKILL.md does not state version $version"

# The changelog's section for this version, without its heading: from the
# line after "## [V]" up to the next "## [".
section=$(awk -v head="## [$version]" '
  index($0, head) == 1 { on = 1; next }
  on && /^## \[/ { exit }
  on { print }
' CHANGELOG.md | sed -e '/[^[:space:]]/,$!d')
[[ -n "${section//[[:space:]]/}" ]] || fail "the CHANGELOG.md section for $version is empty"

{
  echo "The agent skill for this version is attached as \`SKILL.md\`; in the repository it is \`skill/SKILL.md\`. To install it for Claude Code, from where you downloaded it:"
  echo
  echo '```'
  echo 'mkdir -p ~/.claude/skills/lcl && cp SKILL.md ~/.claude/skills/lcl/'
  echo '```'
  echo
  echo "Every file is listed in \`SHA256SUMS\`. Check a download with \`sha256sum --check --ignore-missing SHA256SUMS\`."
  echo
  echo "The changes, from CHANGELOG.md. Every change to the command line and to the \`--json\` contract is listed; \`docs/json-contract.md\` has the shapes."
  echo
  printf '%s\n' "$section"
} >"$notes"

echo "version=$version"
if [[ "$version" == *-* ]]; then
  echo "prerelease=true"
else
  echo "prerelease=false"
fi
