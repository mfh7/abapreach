#!/usr/bin/env bash
# Fail when a Windows executable imports the dynamic Visual C++ runtime.
# Usage: llvm-readobj --coff-imports <exe> | check-imports.sh
set -euo pipefail

dlls="$( (tr -d '\r' | grep -iE '^[[:space:]]*Name:[[:space:]]*[^[:space:]]+\.dll[[:space:]]*$' || true) \
  | sed -E 's/^[[:space:]]*Name:[[:space:]]*//' | sort -fu)"
if [ -z "$dlls" ]; then
  echo "::error::no imported DLL found; the input is not the import table of a Windows executable" >&2
  exit 1
fi
echo "imported DLLs:"
printf '%s\n' "$dlls" | sed 's/^/  /'

dynamic="$(printf '%s\n' "$dlls" | grep -iE '^(vcruntime|msvcp)' || true)"
if [ -n "$dynamic" ]; then
  echo "::error::the executable imports the dynamic VC++ runtime, so it is not built with crt-static: $(printf '%s' "$dynamic" | tr '\n' ' ')" >&2
  exit 1
fi
