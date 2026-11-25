#!/bin/sh
# Writes the winget manifests for one lcl release, in winget-pkgs' layout:
#
#   packaging/winget/manifests.sh v0.3.0-alpha.3 OUT
#
# makes OUT/manifests/b/botzrDev/lcl/0.3.0-alpha.3/ with the version, the
# installer and the en-US locale manifest. The installer is the release's
# Windows zip, checked against the SHA256 its SHA256SUMS gives, holding a
# portable lcl.exe that winget puts on PATH as `lcl`. Submitting the folder
# to microsoft/winget-pkgs is the owner's step (LOCL-15).
#
# Arguments:
#
#   TAG        The release: v0.3.0-alpha.3 (or 0.3.0-alpha.3).
#   OUT        The directory to write under. Made if missing.
#   SHA256SUMS Optional: a local copy of the release's SHA256SUMS. Without
#              it, the release's own is downloaded.
#
# Settings, from the environment:
#
#   LCL_RELEASES_URL  Where the release files are, as URL/TAG/FILE. Without
#                     it, https://github.com/botzrDev/lcl/releases/download.
#
# A release without the Windows zip, or a sum that isn't 64 hex digits,
# writes nothing and exits 1.

set -eu

REPO=botzrDev/lcl
ID=botzrDev.lcl
MANIFEST_VERSION=1.10.0
TARGET=x86_64-pc-windows-msvc

fail() {
  printf 'manifests.sh: %s\n' "$*" >&2
  exit 1
}

main() {
  if [ $# -lt 2 ] || [ $# -gt 3 ]; then
    fail "usage: manifests.sh TAG OUT [SHA256SUMS]"
  fi
  version=${1#v}
  tag=v$version
  case $version in
    [0-9]*.[0-9]*.[0-9]*) ;;
    *) fail "$1 is not a release tag, as v0.3.0-alpha.3" ;;
  esac
  out=$2
  releases=${LCL_RELEASES_URL:-https://github.com/$REPO/releases/download}

  sums=$(mktemp)
  trap 'rm -f "$sums"' EXIT
  if [ $# -eq 3 ]; then
    cp "$3" "$sums" || fail "can't read $3"
  else
    curl -fsSL "$releases/$tag/SHA256SUMS" -o "$sums" ||
      fail "can't download $releases/$tag/SHA256SUMS"
  fi
  name="lcl-$version-$TARGET"
  sum=$(awk -v f="$name.zip" '$2 == f || $2 == "*" f { print $1 }' "$sums")
  [ -n "$sum" ] || fail "$tag's SHA256SUMS has no $name.zip"
  printf '%s' "$sum" | grep -Eqx '[0-9a-f]{64}' ||
    fail "$tag's SHA256SUMS gives $name.zip a sum that isn't 64 hex digits: $sum"
  sum=$(printf '%s' "$sum" | tr 'a-f' 'A-F')

  dir="$out/manifests/b/botzrDev/lcl/$version"
  mkdir -p "$dir"
  schema=https://aka.ms/winget-manifest

  cat >"$dir/$ID.yaml" <<EOF
# yaml-language-server: \$schema=$schema.version.$MANIFEST_VERSION.schema.json
# Made by packaging/winget/manifests.sh from $tag's SHA256SUMS.
PackageIdentifier: $ID
PackageVersion: $version
DefaultLocale: en-US
ManifestType: version
ManifestVersion: $MANIFEST_VERSION
EOF

  cat >"$dir/$ID.installer.yaml" <<EOF
# yaml-language-server: \$schema=$schema.installer.$MANIFEST_VERSION.schema.json
# Made by packaging/winget/manifests.sh from $tag's SHA256SUMS.
PackageIdentifier: $ID
PackageVersion: $version
InstallerType: zip
NestedInstallerType: portable
NestedInstallerFiles:
  - RelativeFilePath: $name\\lcl.exe
    PortableCommandAlias: lcl
Commands:
  - lcl
Installers:
  - Architecture: x64
    InstallerUrl: $releases/$tag/$name.zip
    InstallerSha256: $sum
ManifestType: installer
ManifestVersion: $MANIFEST_VERSION
EOF

  cat >"$dir/$ID.locale.en-US.yaml" <<EOF
# yaml-language-server: \$schema=$schema.defaultLocale.$MANIFEST_VERSION.schema.json
# Made by packaging/winget/manifests.sh from $tag's SHA256SUMS.
PackageIdentifier: $ID
PackageVersion: $version
PackageLocale: en-US
Publisher: botzrDev
PublisherUrl: https://github.com/botzrDev
PackageName: lcl
PackageUrl: https://github.com/$REPO
License: Apache-2.0
LicenseUrl: https://github.com/$REPO/blob/main/LICENSE
ShortDescription: Local issue tracker with one SQLite file per workspace
Moniker: lcl
Tags:
  - issue-tracker
  - cli
  - sqlite
ReleaseNotesUrl: https://github.com/$REPO/releases/tag/$tag
ManifestType: defaultLocale
ManifestVersion: $MANIFEST_VERSION
EOF
  printf '%s\n' "$dir"
}

main "$@"
