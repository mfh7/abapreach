#!/usr/bin/env bash
# Fail unless a directory holds exactly the four release archives of one version and a matching
# .sha256 file for each, and nothing else (so no fault-injection archive can ride along).
# Usage: verify-dist.sh <dir> <version>
set -euo pipefail
dir="${1:?usage: verify-dist.sh <dir> <version>}"
version="${2:?usage: verify-dist.sh <dir> <version>}"
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=targets.sh
. "$here/targets.sh"

wanted=()
for entry in "${RELEASE_TARGETS[@]}"; do
  IFS=: read -r target extension _ <<< "$entry"
  wanted+=("abapreach-$version-$target.$extension" "abapreach-$version-$target.$extension.sha256")
done

expected="$(printf '%s\n' "${wanted[@]}" | LC_ALL=C sort)"
actual="$(cd "$dir" && ls -A1 | LC_ALL=C sort)"
if [ "$actual" != "$expected" ]; then
  echo "::error::$dir must hold exactly the four archives of $version and their .sha256 files" >&2
  diff <(printf '%s\n' "$expected") <(printf '%s\n' "$actual") | sed 's/^/  /' >&2 || true
  exit 1
fi

for entry in "${RELEASE_TARGETS[@]}"; do
  IFS=: read -r target extension _ <<< "$entry"
  archive="abapreach-$version-$target.$extension"
  read -r recorded name < "$dir/$archive.sha256"
  computed="$(bash "$here/sha256.sh" "$dir/$archive")"
  if [ "$name" != "$archive" ] || [ "$recorded" != "$computed" ]; then
    echo "::error::SHA256 mismatch for $archive: recorded $recorded for $name, computed $computed" >&2
    exit 1
  fi
  echo "$archive: SHA256 $computed"
done
