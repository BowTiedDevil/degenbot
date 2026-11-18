#!/usr/bin/env bash
# Require a Forge lane to execute tests and skip none.
#
# `forge test` exits 0 when a filter matches nothing and when a contract skips
# itself in `setUp`, so its exit status alone cannot certify that a lane ran the
# suites it names. Lane recipes capture their output and hand it here: a missing
# lane switch, an over-broad exclusion, or a filter that matched no suite then
# fails the recipe instead of reporting a green run with no coverage.
#
# Usage: forge_lane_summary.sh <lane-name> <log-file>
set -euo pipefail

lane="$1"
log="$2"

summary="$(grep -E '^Ran [0-9]+ test suites? in .*: [0-9]+ tests? passed, [0-9]+ failed, [0-9]+ skipped' "$log" | tail -n 1 || true)"
if [ -z "$summary" ]; then
    echo "forge lane '${lane}': no Forge test summary in ${log} -- refusing to pass" >&2
    exit 1
fi

counts="$(sed -E 's/^Ran [0-9]+ test suites? in .*: ([0-9]+) tests? passed, ([0-9]+) failed, ([0-9]+) skipped.*/\1 \2 \3/' <<<"${summary}")"
read -r passed failed skipped <<<"${counts}"

if [ "${passed}" -eq 0 ] || [ "${failed}" -ne 0 ] || [ "${skipped}" -ne 0 ]; then
    echo "forge lane '${lane}': ${passed} passed, ${failed} failed, ${skipped} skipped" >&2
    echo "  a lane must execute at least one test and skip none: check the lane switch and the" >&2
    echo "  suite selection (a skipped contract and an empty filter both exit forge with 0)." >&2
    exit 1
fi

echo "forge lane '${lane}': ${passed} passed, 0 failed, 0 skipped"
