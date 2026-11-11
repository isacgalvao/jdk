#!/usr/bin/env bash
# Copy a generated index into the publication checkout, then validate the
# destination. No Git operations: callers may commit only after this succeeds.
set -euo pipefail

staged=$1
published=$2
generator=$3

cp "$staged/index.json" "$published/index.json"
# Generation and checkout can finish in the same second. Equal size and mtime
# do not imply equal bytes, so rsync's default quick check is insufficient.
rsync -a --checksum --delete "$staged/windows-x64/" "$published/windows-x64/"
if [ -d "$staged/windows-aarch64" ]; then
    rsync -a --checksum --delete "$staged/windows-aarch64/" "$published/windows-aarch64/"
else
    rm -rf "$published/windows-aarch64"
fi

# Generation validated the source, not these copied bytes. A failed copy must
# never be published with an index.json that promises a different checksum.
"$generator" --out "$published" --validate-only
