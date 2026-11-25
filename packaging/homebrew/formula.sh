#!/bin/sh
# Writes the Homebrew formula for one lcl release, on stdout:
#
#   packaging/homebrew/formula.sh v0.3.0-alpha.3 > lcl.rb
#
# The formula installs the release's archive for macOS or Linux, arm64 or
# x86_64, checked against the SHA256 the release's SHA256SUMS gives it. The
# tap (botzrDev/homebrew-tap, LOCL-13) holds it as Formula/lcl.rb, and
# `brew install botzrDev/tap/lcl` installs it.
#
# Arguments:
#
#   TAG        The release: v0.3.0-alpha.3 (or 0.3.0-alpha.3).
#   SHA256SUMS Optional: a local copy of the release's SHA256SUMS. Without
#              it, the release's own is downloaded.
#
# Settings, from the environment:
#
#   LCL_RELEASES_URL  Where the release files are, as URL/TAG/FILE. Without
#                     it, https://github.com/botzrDev/lcl/releases/download.
#
# A release missing one of the four archives, or a sum that isn't 64 hex
# digits, writes nothing and exits 1.

set -eu

REPO=botzrDev/lcl
TARGETS="aarch64-apple-darwin x86_64-apple-darwin aarch64-unknown-linux-gnu x86_64-unknown-linux-gnu"

fail() {
  printf 'formula.sh: %s\n' "$*" >&2
  exit 1
}

main() {
  if [ $# -lt 1 ] || [ $# -gt 2 ]; then
    fail "usage: formula.sh TAG [SHA256SUMS]"
  fi
  version=${1#v}
  tag=v$version
  case $version in
    [0-9]*.[0-9]*.[0-9]*) ;;
    *) fail "$1 is not a release tag, as v0.3.0-alpha.3" ;;
  esac
  releases=${LCL_RELEASES_URL:-https://github.com/$REPO/releases/download}

  sums=$(mktemp)
  trap 'rm -f "$sums"' EXIT
  if [ $# -eq 2 ]; then
    cp "$2" "$sums" || fail "can't read $2"
  else
    curl -fsSL "$releases/$tag/SHA256SUMS" -o "$sums" ||
      fail "can't download $releases/$tag/SHA256SUMS"
  fi

  for target in $TARGETS; do
    file="lcl-$version-$target.tar.gz"
    sum=$(awk -v f="$file" '$2 == f || $2 == "*" f { print $1 }' "$sums")
    case $sum in
      "") fail "$tag's SHA256SUMS has no $file" ;;
    esac
    printf '%s' "$sum" | grep -Eqx '[0-9a-f]{64}' ||
      fail "$tag's SHA256SUMS gives $file a sum that isn't 64 hex digits: $sum"
    eval "sum_$(printf '%s' "$target" | tr '-' '_')=\$sum"
  done

  # shellcheck disable=SC2154 # set by the eval above
  cat <<EOF
# Made by packaging/homebrew/formula.sh from $tag's SHA256SUMS. Don't edit:
# run the script for the next release instead.
class Lcl < Formula
  desc "Local issue tracker with one SQLite file per workspace"
  homepage "https://github.com/$REPO"
  license "Apache-2.0"

  on_macos do
    on_arm do
      url "$releases/$tag/lcl-$version-aarch64-apple-darwin.tar.gz"
      sha256 "$sum_aarch64_apple_darwin"
    end
    on_intel do
      url "$releases/$tag/lcl-$version-x86_64-apple-darwin.tar.gz"
      sha256 "$sum_x86_64_apple_darwin"
    end
  end

  on_linux do
    on_arm do
      url "$releases/$tag/lcl-$version-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "$sum_aarch64_unknown_linux_gnu"
    end
    on_intel do
      url "$releases/$tag/lcl-$version-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "$sum_x86_64_unknown_linux_gnu"
    end
  end

  def install
    bin.install "lcl"
  end

  test do
    assert_equal "lcl #{version}", shell_output("#{bin}/lcl --version").strip
    system bin/"lcl", "init", "demo", "--key", "DEMO"
    assert_match "DEMO-1", shell_output("#{bin}/lcl issue create --title 'from brew test'")
  end
end
EOF
}

main "$@"
