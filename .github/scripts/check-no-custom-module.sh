#!/usr/bin/env bash
# Fail when a binary holds the name of the private custom function module.
# Usage: check-no-custom-module.sh <binary>
set -euo pipefail
binary="${1:?usage: check-no-custom-module.sh <binary>}"

# The needle is built from two pieces, so the public boundary test does not trip on this file.
needle="YFM_UTILS_""SOURCE_DEPLOY"

if [ ! -f "$binary" ]; then
  echo "::error::$binary is not a file" >&2
  exit 1
fi

if grep -qaF -- "$needle" "$binary"; then
  echo "::error::$binary names the private custom function module; the public release must not" >&2
  exit 1
fi
echo "no custom function module name in $binary"
