#!/usr/bin/env bash

# Print `--exclude <name>` for every vendored workspace member (rust/vendor/).

#

# The vendored evm2 family (ADR-096) is upstream's code, shipped through the

# feature sets our members request: workspace-wide lanes that select members as

# primary packages must not select it. Its in-crate test modules are upstream's

# (test harnesses, and a revm differential check, that never run here), and its

# feature matrix carries nightly-only switches that cannot compile on the

# pinned stable toolchain. The family still compiles as a dependency of every

# consumer — which is exactly what ships.

#

# The list is derived from cargo metadata, so it cannot drift from the tree.

#

# live-invariant gate (ADR-087 D2): watches the vendored workspace members under rust/vendor/ against the workspace-wide lane invocations that must not select them; not tripwire precedent.

set -euo pipefail

cargo metadata --format-version 1 --no-deps --manifest-path "$(dirname "${BASH_SOURCE[0]}")/../rust/Cargo.toml" |

    python3 -c 'import json, sys

for p in json.load(sys.stdin)["packages"]:

    if "/vendor/" in p["manifest_path"]:

        print(f'"'"'--exclude {p["name"]}'"'"', end=" ")

print()'
