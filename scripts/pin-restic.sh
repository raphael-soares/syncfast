#!/usr/bin/env bash
# Usage: scripts/pin-restic.sh <version>
# Points the Flatpak manifest, the CI workflow and RESTIC_VERSION at a restic
# release, with the sha256 of both architectures taken from the release's SHA256SUMS.
set -euo pipefail

version="${1:?usage: pin-restic.sh <version, for example 0.19.1>}"
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "not a version: $version" >&2; exit 1; }

root="$(cd "$(dirname "$0")/.." && pwd)"
manifest="$root/packaging/flatpak/io.github.raphael_soares.syncfast.yml"
constant="$root/crates/syncfast-core/src/restic.rs"
workflow="$root/.github/workflows/ci.yml"
release="https://github.com/restic/restic/releases/download/v$version"

sums="$(curl -fsSL "$release/SHA256SUMS")"
sum_of() {
  local asset="restic_${version}_linux_$1.bz2" sum
  sum="$(awk -v asset="$asset" '$2 == asset { print $1 }' <<<"$sums")"
  [[ "$sum" =~ ^[0-9a-f]{64}$ ]] || { echo "no sha256 for $asset in SHA256SUMS" >&2; exit 1; }
  printf '%s' "$sum"
}
amd64="$(sum_of amd64)"
arm64="$(sum_of arm64)"

sed -i -E \
  -e "s#(restic/releases/download/)v[0-9.]+(/restic_)[0-9.]+(_linux_)#\1v$version\2$version\3#" \
  -e "/restic_${version}_linux_amd64\.bz2/{n;s/sha256: .*/sha256: $amd64/}" \
  -e "/restic_${version}_linux_arm64\.bz2/{n;s/sha256: .*/sha256: $arm64/}" \
  "$manifest"
sed -i -E \
  -e "s/^( *RESTIC_VERSION: ).*/\\1$version/" \
  -e "s/^( *RESTIC_SHA256: ).*/\\1$amd64/" \
  "$workflow"
sed -i -E "s/^(pub const RESTIC_VERSION: &str = )\"[0-9.]+\";/\1\"$version\";/" "$constant"
echo "restic $version pinned: amd64 $amd64, arm64 $arm64"
