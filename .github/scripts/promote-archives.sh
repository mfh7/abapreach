#!/usr/bin/env bash
# Promote a tested rc release set to the final version without rebuilding: copy each rc archive
# under its final name, then prove the archive and the executable inside it are unchanged. Each
# archive holds the executable, LICENSE and THIRD-PARTY-NOTICES; verify-dist.sh proves the rc set
# and the promoted set hold exactly those three entries, with the two text files identical to the
# ones in the checked-out tag commit.
# Usage: promote-archives.sh <rc-dir> <rc-version> <out-dir> <final-version>
set -euo pipefail
rc_dir="${1:?usage: promote-archives.sh <rc-dir> <rc-version> <out-dir> <final-version>}"
rc_version="${2:?}"
out_dir="${3:?}"
final_version="${4:?}"
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=targets.sh
. "$here/targets.sh"

# The rc set must be exactly what the rc job published, must match its own checksums, and must
# carry the tag commit's LICENSE and THIRD-PARTY-NOTICES.
bash "$here/verify-dist.sh" "$rc_dir" "$rc_version" > /dev/null

mkdir -p "$out_dir"
if [ -n "$(ls -A "$out_dir")" ]; then
  echo "::error::$out_dir is not empty; refusing to mix promoted files with leftovers" >&2
  exit 1
fi

# Hash the executable straight out of an archive, without writing it anywhere.
executable_hash() {
  local archive="$1" extension="$2" executable="$3"
  if [ "$extension" = zip ]; then
    unzip -p "$archive" "$executable" | bash "$here/sha256.sh"
  else
    tar -xzOf "$archive" "$executable" | bash "$here/sha256.sh"
  fi
}

for entry in "${RELEASE_TARGETS[@]}"; do
  IFS=: read -r target extension executable <<< "$entry"
  source_archive="$rc_dir/abapreach-$rc_version-$target.$extension"
  name="abapreach-$final_version-$target.$extension"
  promoted="$out_dir/$name"

  cp "$source_archive" "$promoted"
  archive_hash="$(bash "$here/sha256.sh" "$promoted")"
  rc_executable="$(executable_hash "$source_archive" "$extension" "$executable")"
  promoted_executable="$(executable_hash "$promoted" "$extension" "$executable")"
  if [ -z "$rc_executable" ] || [ "$rc_executable" != "$promoted_executable" ] \
    || [ "$archive_hash" != "$(bash "$here/sha256.sh" "$source_archive")" ]; then
    echo "::error::$target: the promoted executable differs from the tested rc executable" >&2
    exit 1
  fi
  printf '%s  %s\n' "$archive_hash" "$name" > "$promoted.sha256"
  echo "$target: binary SHA256 identical to the rc ($promoted_executable), archive $name"
done

bash "$here/verify-dist.sh" "$out_dir" "$final_version" > /dev/null
