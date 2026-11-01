#!/usr/bin/env bash
# Fail unless every file that differs between two commits is README.md or under docs/.
# Usage: check-doc-only.sh <base-commit> <head-commit>
set -euo pipefail
base="${1:?usage: check-doc-only.sh <base-commit> <head-commit>}"
head="${2:?usage: check-doc-only.sh <base-commit> <head-commit>}"

list="$(mktemp)"
trap 'rm -f "$list"' EXIT
# reports both sides of a move, so a file moved out of docs/ is still seen.
git diff --name-only --no-renames -z "$base" "$head" -- > "$list"

changed=0
rejected=0
while IFS= read -r -d '' path; do
  changed=$((changed + 1))
  case "$path" in
    README.md | docs/*) echo "documentation: $path" ;;
    *)
      echo "::error::$path is not README.md or under docs/; a final tag may only add documentation to the tested rc" >&2
      rejected=$((rejected + 1))
      ;;
  esac
done < "$list"

if [ "$rejected" -gt 0 ]; then
  echo "$rejected of $changed changed files are not documentation" >&2
  exit 1
fi
if [ "$changed" -eq 0 ]; then
  echo "no changed files: the final commit is the rc commit"
else
  echo "$changed changed files, all documentation"
fi
