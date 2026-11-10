#!/usr/bin/env bash
# Fail unless a directory holds exactly the four release archives of one version and a matching
# .sha256 file for each, and nothing else (so no fault-injection archive can ride along). Each
# archive must hold exactly three entries at its root: the executable, LICENSE and
# THIRD-PARTY-NOTICES, and the two text files must be byte-identical to the ones in the checked-out
# commit (the repository this script lives in).
# Usage: verify-dist.sh <dir> <version>
set -euo pipefail
dir="${1:?usage: verify-dist.sh <dir> <version>}"
version="${2:?usage: verify-dist.sh <dir> <version>}"
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=targets.sh
. "$here/targets.sh"
root="$(cd "$here/../.." && pwd)"
license_files=(LICENSE THIRD-PARTY-NOTICES)

# Print the entry names of an archive, one per line.
list_entries() {
  if [ "$2" = zip ]; then unzip -Z1 "$1"; else tar -tzf "$1"; fi
}

# Print the bytes of one archive entry.
read_entry() {
  if [ "$2" = zip ]; then unzip -p "$1" "$3"; else tar -xzOf "$1" "$3"; fi
}

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
  IFS=: read -r target extension executable <<< "$entry"
  archive="abapreach-$version-$target.$extension"
  read -r recorded name < "$dir/$archive.sha256"
  computed="$(bash "$here/sha256.sh" "$dir/$archive")"
  if [ "$name" != "$archive" ] || [ "$recorded" != "$computed" ]; then
    echo "::error::SHA256 mismatch for $archive: recorded $recorded for $name, computed $computed" >&2
    exit 1
  fi

  expected_entries="$(printf '%s\n' "$executable" "${license_files[@]}" | LC_ALL=C sort)"
  actual_entries="$(list_entries "$dir/$archive" "$extension" | LC_ALL=C sort)" || actual_entries="(unreadable archive)"
  if [ "$actual_entries" != "$expected_entries" ]; then
    echo "::error::$archive must hold exactly $executable, ${license_files[0]} and ${license_files[1]} at its root" >&2
    diff <(printf '%s\n' "$expected_entries") <(printf '%s\n' "$actual_entries") | sed 's/^/  /' >&2 || true
    exit 1
  fi
  for file in "${license_files[@]}"; do
    if ! read_entry "$dir/$archive" "$extension" "$file" | cmp -s - "$root/$file"; then
      echo "::error::$archive: $file differs from the $file of the checked-out commit" >&2
      exit 1
    fi
  done
  echo "$archive: SHA256 $computed"
done
