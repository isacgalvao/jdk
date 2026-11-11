#!/usr/bin/env bash
# Copy a generated index into the publication checkout. No Git operations:
# callers may commit only after this succeeds.
set -euo pipefail

staged=$1
published=$2

cp "$staged/index.json" "$published/index.json"
rsync -a --delete "$staged/windows-x64/" "$published/windows-x64/"
if [ -d "$staged/windows-aarch64" ]; then
    rsync -a --delete "$staged/windows-aarch64/" "$published/windows-aarch64/"
else
    rm -rf "$published/windows-aarch64"
fi
