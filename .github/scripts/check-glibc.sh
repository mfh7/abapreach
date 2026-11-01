#!/usr/bin/env bash
# Fail when a binary references a GLIBC_ symbol version newer than the limit.
# Usage: objdump -T <binary> | check-glibc.sh <limit>      (for example 2.28)
set -euo pipefail
limit="${1:?usage: objdump -T <binary> | check-glibc.sh <limit>}"

# Numeric versions only: GLIBC_PRIVATE and GLIBCXX_* are not part of the baseline.
highest="$( (grep -oE 'GLIBC_[0-9]+(\.[0-9]+)*' || true) | sed 's/^GLIBC_//' | sort -V | tail -n 1)"
if [ -z "$highest" ]; then
  echo "::error::no GLIBC_ symbol version found; the input is not the dynamic symbol table of a glibc binary" >&2
  exit 1
fi
echo "highest GLIBC symbol version: $highest (limit $limit)"

# Version order, not text order: 2.9 < 2.28 < 2.34.
newest="$(printf '%s\n%s\n' "$highest" "$limit" | sort -V | tail -n 1)"
if [ "$newest" != "$limit" ]; then
  echo "::error::the binary needs GLIBC_$highest, newer than the GLIBC_$limit baseline" >&2
  exit 1
fi
