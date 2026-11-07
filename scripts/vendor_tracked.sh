#!/usr/bin/env bash

# Assert every file under rust/vendor/ is tracked by git.

#

# The vendored evm2 family (ADR-096) is a byte-for-byte source copy, and that

# claim only holds when the copy in the index is the copy in the tree. A

# silently untracked vendored file is worse than a missing one: it builds

# locally, breaks every clean checkout, and `cargo package` would ship a crate

# whose tree differs from the repository's.

#

# Prints the untracked paths and fails on any; exits 2 when the tree or the git

# read is unusable — never certifies a tree it did not examine.

#

# live-invariant gate (ADR-087 D2): watches the vendored source tree under rust/vendor/ against the git index; not tripwire precedent.

set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

[ -d rust/vendor ] || { echo "vendor_tracked: rust/vendor is missing; refusing to certify" >&2; exit 2; }
command -v git >/dev/null 2>&1 || { echo "vendor_tracked: git not found; refusing to certify" >&2; exit 2; }

tracked=$(git ls-files rust/vendor | wc -l)
[ "$tracked" -gt 0 ] || { echo "vendor_tracked: the index holds no vendored files; refusing to certify" >&2; exit 2; }

missing=$(comm -23 \
    <(cd rust/vendor && find . -type f | sed 's|^\./||' | sort) \
    <(git ls-files rust/vendor | sed 's|^rust/vendor/||' | sort))

if [ -n "$missing" ]; then
    echo "vendor_tracked: vendored files missing from the index (git add rust/vendor):" >&2
    while IFS= read -r path; do
        echo "  $path" >&2
    done <<<"$missing"
    exit 1
fi

echo "vendor_tracked: $tracked vendored files, all tracked"
