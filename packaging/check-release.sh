#!/usr/bin/env bash
# Checks a release as Moonglow's updater does before installing it: its
# SHA256SUMS signed by the release key, with the tag in the signature's
# trusted comment, and every package (Moonglow-*) listed there with its
# checksum. Run by .github/workflows/release-signed.yml as a release is
# published; runs by hand too:
#
#   packaging/check-release.sh v3.3.0
#
#   MOONGLOW_MINISIGN_PUB  the public key (default as sign-release.sh's)
#   MOONGLOW_REPO          the repository (default as sign-release.sh's)
set -euo pipefail

tag=${1:?usage: packaging/check-release.sh vX.Y.Z}
here=$(cd "$(dirname "$0")" && pwd)
source_field() {
    [[ -f $here/release-source ]] || return 0
    sed -n "s/^[[:space:]]*$1[[:space:]]*=[[:space:]]*\"\{0,1\}\([^\"]*\)\"\{0,1\}[[:space:]]*$/\1/p" \
        "$here/release-source" | head -n 1
}
fork_repo=$(source_field repo)
fork_key=$(source_field key)
if [[ -n $fork_repo ]]; then
    repo=${MOONGLOW_REPO:-$fork_repo}
    pub=${MOONGLOW_MINISIGN_PUB:-$here/${fork_key:-no-key-named-in-release-source.pub}}
else
    repo=${MOONGLOW_REPO:-jadzziaa/moonglow-toolset}
    pub=${MOONGLOW_MINISIGN_PUB:-$here/minisign.pub}
fi
pub=$(cd "$(dirname "$pub")" && pwd)/$(basename "$pub")

[[ -f $pub ]] || { echo "no public key at $pub" >&2; exit 1; }
command -v minisign > /dev/null || { echo "minisign isn't installed" >&2; exit 1; }

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
cd "$work"

names=$(gh release view "$tag" --repo "$repo" --json assets --jq '.assets[].name')
grep -qx SHA256SUMS <<< "$names" && grep -qx SHA256SUMS.minisig <<< "$names" ||
    { echo "$tag isn't signed: no SHA256SUMS and SHA256SUMS.minisig" >&2; exit 1; }
gh release download "$tag" --repo "$repo" --pattern 'Moonglow-*' --pattern 'SHA256SUMS*'

said=$(minisign -V -p "$pub" -m SHA256SUMS) ||
    { echo "$tag's signature isn't the release key's ($pub)" >&2; exit 1; }
grep -qx "Trusted comment: Moonglow Toolset $tag" <<< "$said" ||
    { echo "$tag's signature is for another release: $said" >&2; exit 1; }

shopt -s nullglob
files=(Moonglow-*)
(( ${#files[@]} )) || { echo "$tag has no packages" >&2; exit 1; }
for f in "${files[@]}"; do
    grep -q "  $f\$" SHA256SUMS || { echo "$f isn't in $tag's SHA256SUMS: sign again" >&2; exit 1; }
done
sha256sum -c --strict SHA256SUMS || { echo "a package of $tag isn't the one signed: sign again" >&2; exit 1; }
echo "$tag is signed: ${files[*]}"
