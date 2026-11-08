#!/usr/bin/env bash
# Fail when a binary holds the name of the private custom function module, the experimental
# feature name, or the private crate name.
# Usage: check-no-custom-module.sh <binary>
set -euo pipefail
binary="${1:?usage: check-no-custom-module.sh <binary>}"

# Each needle is built from two pieces, so the public boundary test does not trip on this file.
needles=("YFM_UTILS_""SOURCE_DEPLOY" "experimental""-ext" "abapreach""-ext")

if [ ! -f "$binary" ]; then
  echo "::error::$binary is not a file" >&2
  exit 1
fi

for needle in "${needles[@]}"; do
  if grep -qaF -- "$needle" "$binary"; then
    echo "::error::$binary holds a name that belongs to the private side ($needle); the public release must not" >&2
    exit 1
  fi
done
echo "no private name in $binary"
