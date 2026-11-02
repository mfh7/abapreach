#!/usr/bin/env bash
# Print the SHA256 of each file argument, or of standard input without arguments, as a bare hash.
# GNU coreutils (Linux, Git for Windows) ship sha256sum; macOS ships shasum.
set -euo pipefail
if command -v sha256sum >/dev/null 2>&1; then
  sha256sum "$@" | cut -d' ' -f1
else
  shasum -a 256 "$@" | cut -d' ' -f1
fi
