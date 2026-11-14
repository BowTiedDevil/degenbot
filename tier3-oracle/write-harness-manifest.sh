#!/usr/bin/env bash
# Recompute `artifacts/manifest.json`: every row of `harness-artifacts.tsv` mapped
# to the sha256 of its git-tracked source .sol, so the two never come from
# separate lists. The Rust tier-3 tests read this manifest (via
# `tier3_harness_artifacts.rs`) at test time and reject any artifact whose
# tracked source changed without a rebuild — the checked-in bytecode can never
# silently drift from the harness sources.
#
# Sources only: the canonical vendored libs are version-pinned and immutable
# during the oracle's lifetime, and the pinned-version bump case is covered by
# the CI tier3-oracle rebuild job.
set -euo pipefail

TD="$(cd "$(dirname "$0")" && pwd)"
cd "${TD}"

python3 "${TD}/harness-artifacts.py" manifest
