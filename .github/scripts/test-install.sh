#!/usr/bin/env bash
# Tests install.sh against fake releases on disk, without the network.
# Run from the repository root:
#
#   .github/scripts/test-install.sh
#
# Each fake release has an archive for every Linux and macOS target. Its
# lcl is a script that prints `lcl VERSION` and names its target, so a test
# can tell which archive was installed. Stubs for uname, sysctl and ldd
# make install.sh see each platform in turn.
set -euo pipefail

root=$(pwd)
script="$root/install.sh"
[[ -f "$script" ]] || { echo "run from the repository root" >&2; exit 2; }
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
rel="$work/releases"
failures=0
cases=0

sha256() {
  if command -v sha256sum >/dev/null; then sha256sum "$@"; else shasum -a 256 "$@"; fi
}

# make_release TAG [PRINTED]: a fake release whose lcl prints PRINTED,
# by default `lcl VERSION`.
make_release() {
  local tag=$1 version=${1#v}
  local printed=${2:-lcl $version}
  mkdir -p "$rel/$tag" "$work/build/$tag"
  for t in x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu aarch64-apple-darwin x86_64-apple-darwin; do
    local name="lcl-$version-$t"
    mkdir -p "$work/build/$tag/$name"
    printf '#!/bin/sh\n# built for %s\necho "%s"\n' "$t" "$printed" >"$work/build/$tag/$name/lcl"
    chmod 755 "$work/build/$tag/$name/lcl"
    tar -C "$work/build/$tag" -czf "$rel/$tag/$name.tar.gz" "$name"
  done
  (cd "$rel/$tag" && sha256 -- *.tar.gz >SHA256SUMS)
}

# platform OS ARCH [TRANSLATED [LIBC]]: a directory of stubs that make
# install.sh see that platform. TRANSLATED 1 is a shell under Rosetta.
# LIBC musl makes ldd say so.
platform() {
  local d="$work/stubs/$1-$2-${3:-0}-${4:-glibc}"
  mkdir -p "$d"
  # shellcheck disable=SC2016 # $1 is the stub's own argument.
  printf '#!/bin/sh\ncase "$1" in -s) echo %s ;; -m) echo %s ;; *) echo %s ;; esac\n' "$1" "$2" "$1" >"$d/uname"
  printf '#!/bin/sh\necho %s\n' "${3:-0}" >"$d/sysctl"
  if [[ "${4:-glibc}" == musl ]]; then
    printf '#!/bin/sh\necho "musl libc (%s)" >&2\nexit 1\n' "$2" >"$d/ldd"
  else
    printf '#!/bin/sh\necho "ldd (GNU libc) 2.28"\n' >"$d/ldd"
  fi
  chmod 755 "$d"/*
  echo "$d"
}

# run STUBS [VAR=VALUE…]: install.sh, with a fresh install directory
# $work/bin, and the stubs first on PATH. Sets $out and $code.
run() {
  local stubs=$1
  shift
  rm -rf "${work:?}/bin" "${work:?}/home"
  mkdir -p "$work/home"
  code=0
  out=$(env PATH="$stubs:$PATH" HOME="$work/home" LCL_INSTALL_DIR="$work/bin" \
    LCL_RELEASES_URL="file://$rel" LCL_VERSION=v1.0.0 "$@" sh "$script" 2>&1) || code=$?
}

pass() { cases=$((cases + 1)); echo "ok: $1"; }
bad() {
  cases=$((cases + 1)) failures=$((failures + 1))
  printf 'FAILED: %s\n  exit %s, output:\n%s\n' "$1" "$code" "    ${out//$'\n'/$'\n    '}"
}

# expect_installed NAME TARGET: the run exited 0 and installed TARGET's lcl.
expect_installed() {
  if [[ $code -eq 0 ]] && grep -qx "# built for $2" "$work/bin/lcl" 2>/dev/null && [[ -x "$work/bin/lcl" ]] &&
    [[ "$(ls -A "$work/bin")" == lcl ]]; then
    pass "$1"
  else
    bad "$1: expected $2 installed"
  fi
}

# expect_refused NAME WORDS: the run exited 1, said WORDS, and the install
# directory holds nothing new.
expect_refused() {
  local before=${3:-}
  if [[ $code -eq 1 ]] && grep -qF -- "$2" <<<"$out" && [[ "$(ls -A "$work/bin" 2>/dev/null)" == "$before" ]]; then
    pass "$1"
  else
    bad "$1: expected exit 1, '$2', and nothing installed"
  fi
}

make_release v1.0.0

# The right build for each platform.
run "$(platform Linux x86_64)"
expect_installed "Linux x86_64" x86_64-unknown-linux-gnu
run "$(platform Linux aarch64)"
expect_installed "Linux aarch64" aarch64-unknown-linux-gnu
run "$(platform Linux arm64)"
expect_installed "Linux arm64" aarch64-unknown-linux-gnu
run "$(platform Darwin arm64)"
expect_installed "macOS on Apple silicon" aarch64-apple-darwin
run "$(platform Darwin x86_64)"
expect_installed "macOS on Intel" x86_64-apple-darwin
run "$(platform Darwin x86_64 1)"
expect_installed "macOS, a shell under Rosetta" aarch64-apple-darwin

# No build for the platform.
run "$(platform Linux x86_64 0 musl)"
expect_refused "Linux with musl" "this system uses musl"
run "$(platform Linux riscv64)"
expect_refused "Linux on riscv64" "no lcl build for Linux on riscv64"
run "$(platform FreeBSD amd64)"
expect_refused "FreeBSD" "no lcl build for FreeBSD"
run "$(platform MINGW64_NT-10.0 x86_64)"
expect_refused "Git Bash on Windows" "install with PowerShell"

# The version, with or without its v.
run "$(platform Linux x86_64)" LCL_VERSION=1.0.0
expect_installed "LCL_VERSION without the v" x86_64-unknown-linux-gnu

# A failed download installs nothing.
run "$(platform Linux x86_64)" LCL_VERSION=v2.0.0
expect_refused "a release that doesn't exist" "can't download"
make_release v1.1.0
rm "$rel/v1.1.0/lcl-1.1.0-x86_64-unknown-linux-gnu.tar.gz"
run "$(platform Linux x86_64)" LCL_VERSION=v1.1.0
expect_refused "a release without the archive" "can't download"
make_release v1.2.0
rm "$rel/v1.2.0/SHA256SUMS"
run "$(platform Linux x86_64)" LCL_VERSION=v1.2.0
expect_refused "a release without SHA256SUMS" "SHA256SUMS; nothing was installed"

# A checksum that doesn't match installs nothing.
make_release v1.3.0
tampered=$(awk '{ print ($2 ~ /x86_64-unknown-linux-gnu/ ? "0000000000000000000000000000000000000000000000000000000000000000" : $1) "  " $2 }' "$rel/v1.3.0/SHA256SUMS")
printf '%s\n' "$tampered" >"$rel/v1.3.0/SHA256SUMS"
run "$(platform Linux x86_64)" LCL_VERSION=v1.3.0
expect_refused "an archive that doesn't match SHA256SUMS" "does not match SHA256SUMS"
make_release v1.4.0
grep -v x86_64-unknown-linux-gnu "$rel/v1.4.0/SHA256SUMS" >"$work/sums" && mv "$work/sums" "$rel/v1.4.0/SHA256SUMS"
run "$(platform Linux x86_64)" LCL_VERSION=v1.4.0
expect_refused "SHA256SUMS without the archive" "has no line for"

# A binary that doesn't print the release's version installs nothing.
make_release v1.5.0 "lcl 1.4.9"
run "$(platform Linux x86_64)" LCL_VERSION=v1.5.0
expect_refused "a binary that prints another version" "it printed: lcl 1.4.9"

# Over an earlier install: a refused one leaves it as it was, and a good
# one replaces it.
installed_over() {
  rm -rf "${work:?}/bin" "${work:?}/home"
  mkdir -p "$work/bin" "$work/home"
  printf '#!/bin/sh\necho "lcl 0.9.0"\n' >"$work/bin/lcl"
  chmod 755 "$work/bin/lcl"
  code=0
  out=$(env PATH="$(platform Linux x86_64):$PATH" HOME="$work/home" LCL_INSTALL_DIR="$work/bin" \
    LCL_RELEASES_URL="file://$rel" LCL_VERSION="$1" sh "$script" 2>&1) || code=$?
}
installed_over v1.3.0
if [[ $code -eq 1 ]] && [[ "$("$work/bin/lcl")" == "lcl 0.9.0" ]] && [[ "$(ls -A "$work/bin")" == lcl ]]; then
  pass "a refused install leaves the earlier lcl"
else
  bad "a refused install leaves the earlier lcl"
fi
installed_over v1.0.0
if [[ $code -eq 0 ]] && [[ "$("$work/bin/lcl")" == "lcl 1.0.0" ]] && [[ "$(ls -A "$work/bin")" == lcl ]]; then
  pass "an install replaces the earlier lcl"
else
  bad "an install replaces the earlier lcl"
fi

# An install directory that can't be written: nothing is left in it.
if [[ $(id -u) -ne 0 ]]; then
  rm -rf "${work:?}/bin"
  mkdir -p "$work/bin"
  chmod 555 "$work/bin"
  code=0
  out=$(env PATH="$(platform Linux x86_64):$PATH" HOME="$work/home" LCL_INSTALL_DIR="$work/bin" \
    LCL_RELEASES_URL="file://$rel" LCL_VERSION=v1.0.0 sh "$script" 2>&1) || code=$?
  chmod 755 "$work/bin"
  if [[ $code -eq 1 ]] && grep -qF "can't write $work/bin/lcl" <<<"$out" && [[ -z "$(ls -A "$work/bin")" ]]; then
    pass "an install directory that can't be written"
  else
    bad "an install directory that can't be written"
  fi
fi

# An lcl in the install directory that is a directory, which mv would
# move the binary into.
rm -rf "${work:?}/bin"
mkdir -p "$work/bin/lcl"
code=0
out=$(env PATH="$(platform Linux x86_64):$PATH" HOME="$work/home" LCL_INSTALL_DIR="$work/bin" \
  LCL_RELEASES_URL="file://$rel" LCL_VERSION=v1.0.0 sh "$script" 2>&1) || code=$?
if [[ $code -eq 1 ]] && grep -qF "$work/bin/lcl is a directory" <<<"$out" && [[ -z "$(ls -A "$work/bin/lcl")" ]] &&
  [[ "$(ls -A "$work/bin")" == lcl ]]; then
  pass "an lcl that is a directory"
else
  bad "an lcl that is a directory"
fi

# The PATH hint, and the directory made when missing.
run "$(platform Linux x86_64)" LCL_INSTALL_DIR="$work/bin/deeper"
if [[ $code -eq 0 ]] && [[ -x "$work/bin/deeper/lcl" ]] && grep -qF "is not on your PATH" <<<"$out"; then
  pass "a missing install directory is made, and the PATH hint given"
else
  bad "a missing install directory is made, and the PATH hint given"
fi

echo "$cases cases, $failures failed"
[[ $failures -eq 0 ]]
