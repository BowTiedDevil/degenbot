#!/usr/bin/env bash

#

# Run pytest with `anvil` NOT reachable -- the no-anvil simulation shared by

# `just test-python-no-anvil` (full suite) and

# `just test-python-no-anvil-spawn-surface` (the pre-push slice).

#

# The simulation FAILS CLOSED: if anvil is still reachable after filtering

# foundry-style install dirs out of PATH, this errors rather than silently

# running the ordinary suite. Extend the filter below if anvil lives

# somewhere unexpected.

#

# Usage: scripts/no_anvil_run.sh [pytest args...]

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

cd "$repo_root"

stripped=""

IFS=:

for dir in $PATH; do

    case "$dir" in

        *[Ff][Oo][Uu][Nn][Dd][Rr][Yy]*) continue ;;

    esac

    stripped="${stripped:+$stripped:}$dir"

done

unset IFS

if PATH="$stripped" command -v anvil >/dev/null 2>&1; then

    echo "ERROR: cannot simulate a no-anvil run: anvil is still reachable after the foundry PATH filter" >&2

    echo "Extend the filter in scripts/no_anvil_run.sh to cover its install dir." >&2

    exit 1

fi

exec env PATH="$stripped" bash scripts/pytest_noise_gate.sh run "$@"
