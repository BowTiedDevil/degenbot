#!/usr/bin/env bash
# pytest spray tripwire.
#
# The historical "exception spray" defect class was invisible to CI: after the
# pytest session ended, `--- Logging error ---` blocks and raw
# `Reconnection attempt` lines printed to the real stdout/stderr of the test
# process, while the exit code stayed 0. Post-session output never reaches
# pytest plugins, so the tripwire cannot live inside the suite: it captures the
# whole child-process output to a file and scans that file after the run.
#
# Usage:
#   pytest_noise_gate.sh run  [pytest args...]   # run the suite, capture, scan
#   pytest_noise_gate.sh scan <logfile>          # scan an existing capture
#   pytest_noise_gate.sh --help
#
# Exit codes:
#   0   suite passed and the capture is free of spray signatures
#   N   the suite's own nonzero exit code, propagated unchanged (a clean noise
#       scan never masks a genuine test failure)
#   3   a spray signature was found in the captured output
#   2   usage error (unknown mode, missing/absent logfile)
#
# The `run` exit code and the signature scan are captured separately, so each
# failure mode keeps its own reason.
set -euo pipefail

# live-invariant gate (ADR-087 D2): watches the captured pytest session output for the spray signatures below, both emitted by live code; not tripwire precedent.
signatures=('--- Logging error ---' 'Reconnection attempt')

usage() {
    cat <<'EOF'
pytest spray tripwire.

Usage:
  pytest_noise_gate.sh run  [pytest args...]   # run the suite, capture, scan
  pytest_noise_gate.sh scan <logfile>          # scan an existing capture
  pytest_noise_gate.sh --help

Exit codes:
  0   suite passed and the capture is free of spray signatures
  N   the suite's own nonzero exit code, propagated unchanged (a clean noise
      scan never masks a genuine test failure)
  3   a spray signature was found in the captured output
  2   usage error (unknown mode, missing/absent logfile)
EOF
}

# Print every offending line and return nonzero when any signature matched.
scan_log() {
    local log="$1"
    local found=0
    local sig matches
    for sig in "${signatures[@]}"; do
        matches="$(grep -n -F -e "$sig" "$log" || true)"
        if [ -n "$matches" ]; then
            found=1
            printf 'noise-gate: spray signature -> %s\n' "$sig" >&2
            printf '%s\n' "$matches" >&2
        fi
    done
    return "$found"
}

run_suite() {
    local log
    log="$(mktemp -t pytest-noise.XXXXXXXXXX.log)"
    printf 'noise-gate: capturing suite output to %s\n' "$log" >&2

    # run and scan are captured separately: the pipeline status of pytest is
    # read from PIPESTATUS before the scan can touch the exit code.
    local run_rc=0
    set +e
    uv run --no-sync pytest -x -q --no-header "$@" 2>&1 | tee "$log"
    run_rc="${PIPESTATUS[0]}"
    set -e

    if scan_log "$log"; then
        printf 'noise-gate: clean capture (suite exit %s)\n' "$run_rc" >&2
        # Remove the capture on the SUCCESS path only. A spray failure keeps
        # the log because the FAIL message below prints its path for
        # debugging, and a nonzero suite exit keeps it too: both are failure
        # outcomes whose captured output is worth inspecting.
        if [ "$run_rc" -eq 0 ]; then
            rm -f "$log"
        fi
    else
        printf 'noise-gate: FAIL - spray signatures in %s (suite exit was %s)\n' \
            "$log" "$run_rc" >&2
        return 3
    fi
    return "$run_rc"
}

case "${1:-}" in
    run)
        shift
        run_suite "$@" || exit $?
        ;;
    scan)
        if [ "$#" -lt 2 ]; then
            printf 'noise-gate: scan requires a logfile argument\n' >&2
            exit 2
        fi
        log="$2"
        if [ ! -f "$log" ]; then
            printf 'noise-gate: no such logfile: %s\n' "$log" >&2
            exit 2
        fi
        if scan_log "$log"; then
            printf 'noise-gate: clean capture - no spray signatures in %s\n' "$log"
        else
            printf 'noise-gate: FAIL - spray signatures in %s\n' "$log" >&2
            exit 3
        fi
        ;;
    -h | --help | help)
        usage
        ;;
    *)
        usage >&2
        exit 2
        ;;
esac
