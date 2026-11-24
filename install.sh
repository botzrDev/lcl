#!/bin/sh
# Installs lcl from a GitHub release, on Linux or macOS:
#
#   curl -fsSL https://raw.githubusercontent.com/botzrDev/lcl/main/install.sh | sh
#
# It picks the archive for this machine, downloads it with the release's
# SHA256SUMS, and checks the archive against them. It then runs the lcl in
# it, which must print the release's version, and only then puts it in
# place. Anything that fails before that installs nothing and exits 1.
#
# Settings, all optional, from the environment:
#
#   LCL_VERSION       The release to install: v0.3.0-alpha.2 or 0.3.0-alpha.2.
#                     Without it, the newest release. Until 1.0 that can be a
#                     pre-release.
#   LCL_INSTALL_DIR   Where lcl goes. Without it, ~/.local/bin.
#   LCL_RELEASES_URL  Where the release files are, as URL/TAG/FILE. Without
#                     it, https://github.com/botzrDev/lcl/releases/download.
#   GITHUB_TOKEN      If set, sent to GitHub's API when it is asked for the
#                     newest release, for a higher rate limit.
#
# Everything is in main, called on the last line, so a script cut short in
# the download runs nothing.

set -eu

REPO=botzrDev/lcl

say() {
  printf 'lcl install: %s\n' "$*" >&2
}

fail() {
  say "$*"
  exit 1
}

need() {
  command -v "$1" >/dev/null 2>&1 || fail "it needs $1, which is not installed"
}

# Prints the Rust target of the build for this machine.
target() {
  os=$(uname -s)
  arch=$(uname -m)
  case "$os" in
    Linux | Darwin) ;;
    MINGW* | MSYS* | CYGWIN*)
      fail "on Windows, install with PowerShell: irm https://raw.githubusercontent.com/$REPO/main/install.ps1 | iex"
      ;;
    *) fail "there is no lcl build for $os; build it from source: https://github.com/$REPO#source" ;;
  esac
  case "$arch" in
    x86_64 | amd64) arch=x86_64 ;;
    arm64 | aarch64) arch=aarch64 ;;
    *) fail "there is no lcl build for $os on $arch; build it from source: https://github.com/$REPO#source" ;;
  esac
  if [ "$os" = Linux ]; then
    # The Linux builds link glibc. musl systems, such as Alpine, lack it.
    if command -v ldd >/dev/null 2>&1 && ldd --version 2>&1 | grep -qi musl; then
      fail "the Linux builds need glibc 2.28 or later, and this system uses musl; build lcl from source: https://github.com/$REPO#source"
    fi
    echo "$arch-unknown-linux-gnu"
  else
    # A shell under Rosetta says x86_64 on Apple silicon: install the
    # native build.
    if [ "$arch" = x86_64 ] && [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || true)" = 1 ]; then
      arch=aarch64
    fi
    echo "$arch-apple-darwin"
  fi
}

# Prints the tag of the newest release, pre-releases included.
newest_tag() {
  url="https://api.github.com/repos/$REPO/releases?per_page=1"
  if [ -n "${GITHUB_TOKEN:-}" ]; then
    json=$(curl -fsSL --retry 3 -H "Accept: application/vnd.github+json" -H "Authorization: Bearer $GITHUB_TOKEN" "$url") ||
      fail "can't ask GitHub for the newest release of lcl; set LCL_VERSION to choose one"
  else
    json=$(curl -fsSL --retry 3 -H "Accept: application/vnd.github+json" "$url") ||
      fail "can't ask GitHub for the newest release of lcl; set LCL_VERSION to choose one"
  fi
  tag=$(printf '%s\n' "$json" | tr ',' '\n' |
    sed -n 's/^[[:space:]]*"tag_name":[[:space:]]*"\([^"]*\)".*/\1/p' | head -n 1)
  [ -n "$tag" ] || fail "GitHub lists no release of lcl"
  echo "$tag"
}

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d' ' -f1
  else
    shasum -a 256 "$1" | cut -d' ' -f1
  fi
}

main() {
  need curl
  need tar
  if ! command -v sha256sum >/dev/null 2>&1 && ! command -v shasum >/dev/null 2>&1; then
    fail "it needs sha256sum or shasum to check the download, and neither is installed"
  fi
  [ -n "${HOME:-}" ] || [ -n "${LCL_INSTALL_DIR:-}" ] || fail "HOME is not set; set LCL_INSTALL_DIR to say where lcl goes"

  t=$(target) || exit 1
  if [ -n "${LCL_VERSION:-}" ]; then
    tag=$LCL_VERSION
  else
    tag=$(newest_tag) || exit 1
  fi
  case "$tag" in
    v*) ;;
    *) tag="v$tag" ;;
  esac
  version=${tag#v}
  base=${LCL_RELEASES_URL:-https://github.com/$REPO/releases/download}
  name="lcl-$version-$t"
  archive="$name.tar.gz"
  dir=${LCL_INSTALL_DIR:-$HOME/.local/bin}

  tmp=$(mktemp -d 2>/dev/null || mktemp -d -t lcl) || fail "can't make a temporary directory"
  trap 'rm -rf "$tmp"' EXIT
  trap 'rm -rf "$tmp"; exit 1' HUP INT TERM

  say "downloading lcl $version for $t"
  curl -fsSL --retry 3 -o "$tmp/$archive" "$base/$tag/$archive" ||
    fail "can't download $base/$tag/$archive; nothing was installed"
  curl -fsSL --retry 3 -o "$tmp/SHA256SUMS" "$base/$tag/SHA256SUMS" ||
    fail "can't download $base/$tag/SHA256SUMS; nothing was installed"

  want=$(awk -v f="$archive" '$2 == f || $2 == "*" f { print $1; exit }' "$tmp/SHA256SUMS")
  [ -n "$want" ] || fail "SHA256SUMS of $tag has no line for $archive; nothing was installed"
  got=$(sha256 "$tmp/$archive")
  [ "$got" = "$want" ] ||
    fail "$archive does not match SHA256SUMS: its SHA-256 is $got, and SHA256SUMS says $want; nothing was installed"

  tar -xzf "$tmp/$archive" -C "$tmp" || fail "can't unpack $archive; nothing was installed"
  bin="$tmp/$name/lcl"
  [ -f "$bin" ] || fail "$archive holds no $name/lcl; nothing was installed"
  printed=$("$bin" --version 2>&1) || printed="${printed:-} (exit status $?)"
  [ "$printed" = "lcl $version" ] ||
    fail "the lcl in $archive does not run here as lcl $version; it printed: $printed; nothing was installed"

  mkdir -p "$dir" || fail "can't make $dir; nothing was installed"
  [ ! -d "$dir/lcl" ] || fail "$dir/lcl is a directory; nothing was installed"
  part="$dir/.lcl.install.$$"
  if ! { cp "$bin" "$part" && chmod 755 "$part" && mv -f "$part" "$dir/lcl"; }; then
    rm -f "$part"
    fail "can't write $dir/lcl; nothing was installed"
  fi

  say "installed lcl $version as $dir/lcl"
  case ":${PATH:-}:" in
    *":$dir:"*) ;;
    *) say "$dir is not on your PATH; add it in your shell's profile: export PATH=\"$dir:\$PATH\"" ;;
  esac
  say "the agent skill for this version: $base/$tag/SKILL.md"
}

main "$@"
