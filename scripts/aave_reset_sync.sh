#!/usr/bin/env bash
#
# Aave V3 reset-and-sync soak: the operator tool for a market's replay from its
# cold-boot block to a captured target head.
#
# Two paths, both on demand (never a default gate, pre-push, or CI):
#
#   Short cassette smoke - no node, no operator database write. The committed
#   capture suites replay the recorded spans:
#
#       just test-rust-nextest -p degenbot-aave -E 'test(aave_cassette_replay)'
#
#   Full live soak - this script, against a reachable archive node:
#
#       just aave-soak                       # resume to the chain head
#       just aave-soak -- --reset            # purge first, then sync
#       just aave-soak -- --to-block 20000000 --stop-after-one-chunk
#
# RESUME IS THE DEFAULT. The script never purges unless --reset is given: it
# captures the target head, syncs to that exact block, and asserts the market's
# cursor reached it. --reset purges the market through `degenbot aave reset`
# (one transaction, under the market's writer lock) before the sync.
#
# The target is captured BEFORE the sync and then used as the sync's own
# --to-block, so a moving `latest` cannot make a run assert against a head it
# never targeted. A `latest` target (the default) is resolved to its concrete
# block FIRST — `cast block-number` against `--node` when given, otherwise
# against the `nodes.http[<chain-id>]` entry the CLI configuration resolves. A
# missing node, an unreachable one, or an unnameable head refuses BEFORE any
# write. The resolved listing redacts userinfo credentials, so an endpoint that
# authenticates that way belongs in `--node`. The final report states the
# cursor, the applied event count, and the wall time.
#
# Each CLI command the soak runs (`aave reset`, `aave update`) takes the
# market's one writer lock for its own duration, so another writer of the same
# market (the PyO3 seam, a Rust consumer, a concurrent soak command) is refused
# rather than interleaved. The script adds no lock of its own and holds none
# across its commands: its plan reads are dry-runs, and a multi-command soak is
# several separately locked CLI invocations, not one atomic operation.
#
# Options:
#   --reset                  purge the market before syncing (default: resume)
#   --to-block BLOCK         target block identifier or number (default: latest)
#   --stop-after-one-chunk   stop after the first committed chunk
#   --market NAME            market name for --reset (default: the CLI's)
#   --database PATH          operator database (default: the CLI's resolution)
#   --node URI               RPC endpoint (default: the CLI's resolution)
#   --chain-id ID            chain (default: 1)
#   -h, --help               this text
#
# A single leading `--` (the separator `just` forwards from
# `just aave-soak -- --reset`) is consumed; the words after it parse normally.
#
# Environment:
#   DEGENBOT_CMD   the CLI invocation (default: `uv run --no-sync degenbot`,
#                  the repository's own console-script shim).
#   CAST_CMD       the chain-head reader (default: `cast`).
set -euo pipefail

read -r -a DEGENBOT <<<"${DEGENBOT_CMD:-uv run --no-sync degenbot}"
# The chain-head reader. `cast` is the repository's own toolchain binary; the
# seam exists so a test can record the resolution without a node.
read -r -a CAST <<<"${CAST_CMD:-cast}"

usage() { sed -n '2,57p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; }

reset_market=0
to_block="latest"
one_chunk=0
market=""
chain_id="1"
node_override=""
cli_args=()

# `just aave-soak -- --reset` forwards ONE literal `--` (just keeps the
# operator's separator in the variadic args). Consume that separator, then parse
# the remaining words exactly as a direct invocation would.
if [ "${1:-}" = "--" ]; then
    shift
fi

while [ $# -gt 0 ]; do
    case "$1" in
        --reset) reset_market=1 ;;
        --to-block)
            shift
            [ $# -gt 0 ] || { echo "aave-soak: --to-block needs a value" >&2; exit 2; }
            to_block="$1"
            ;;
        --stop-after-one-chunk) one_chunk=1 ;;
        --market)
            shift
            [ $# -gt 0 ] || { echo "aave-soak: --market needs a value" >&2; exit 2; }
            market="$1"
            ;;
        --database)
            shift
            [ $# -gt 0 ] || { echo "aave-soak: --database needs a value" >&2; exit 2; }
            cli_args+=(--database "$1")
            ;;
        --node)
            shift
            [ $# -gt 0 ] || { echo "aave-soak: --node needs a value" >&2; exit 2; }
            node_override="$1"
            cli_args+=(--node "$1")
            ;;
        --chain-id)
            shift
            [ $# -gt 0 ] || { echo "aave-soak: --chain-id needs a value" >&2; exit 2; }
            chain_id="$1"
            cli_args+=(--chain-id "$1")
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "aave-soak: unknown argument: $1 (see --help)" >&2
            exit 2
            ;;
    esac
    shift
done

cli() { "${DEGENBOT[@]}" "${cli_args[@]}" "$@"; }

# The cursor column of a dry-run plan line, or empty when the market is not
# stamped (the "needs bootstrapping" arm carries no cursor).
cursor_of() { sed -n 's/.* from block \([0-9][0-9]*\) to .*/\1/p'; }

# The resolved target column of a dry-run plan line.
target_of() { sed -n 's/.* to \([0-9][0-9]*\) (no changes committed).*/\1/p'; }

# The applied-events column of a committed chunk-loop line.
events_of() { sed -n 's/.* in [0-9][0-9]* chunks (\([0-9][0-9]*\) events applied).*/\1/p'; }

dry_run_plan() {
    cli aave update --dry-run --to-block "$1" 2>/dev/null | grep -m1 'Dry run:'
}

# The HTTP node this chain resolves through the CLI's own configuration, read
# off the resolved listing. Only the URL is taken from the line — never echoed:
# the listing redacts userinfo credentials, so an endpoint that authenticates
# that way belongs in `--node`.
configured_http_node() {
    local line value
    line="$(cli config show --resolved 2>/dev/null | grep -m1 "^nodes\.http\[$1\] = ")" || true
    [ -n "$line" ] || return 1
    value="${line#* = }"
    value="${value% (*}"
    [ -n "$value" ] || return 1
    printf '%s' "$value"
}

# The chain head, as the node reports it. A failure here refuses BEFORE any
# write, so a soak never runs against a target it could not name.
resolve_head() {
    local head
    head="$("${CAST[@]}" block-number --rpc-url "$1" 2>/dev/null | tr -d '[:space:]')" || {
        echo "aave-soak: chain-head resolution failed (cast block-number refused); check the node and the credential form" >&2
        exit 1
    }
    case "$head" in
        "" | *[!0-9]*)
            echo "aave-soak: chain-head resolution returned no block number" >&2
            exit 1
            ;;
    esac
    printf '%s' "$head"
}

started=$SECONDS

# (0) A `latest` target is resolved to its concrete block BEFORE anything writes.
# The CLI's dry-run states an identifier target as `None` (it plans against the
# identifier the operator typed), so a soak that pinned the sync to `latest`
# would have no block to assert against. Only the identifier `latest` needs
# this: a numeric target is its own answer, and the other tags keep the CLI's
# resolution.
if [ "$to_block" = "latest" ]; then
    if [ -n "$node_override" ]; then
        rpc_url="$node_override"
        echo "aave-soak: resolving latest through the --node endpoint"
    else
        rpc_url="$(configured_http_node "$chain_id")" || {
            echo "aave-soak: latest needs a node, and the CLI configuration declares no nodes.http[$chain_id]; pass --node" >&2
            exit 1
        }
        echo "aave-soak: resolving latest through the configured HTTP node"
    fi
    to_block="$(resolve_head "$rpc_url")"
    echo "aave-soak: resolved latest to block $to_block"
fi

# (1) Capture the target BEFORE anything writes. The plan resolves the target
# identifier to the ONE block this soak syncs to and asserts it; a plan with no
# target (an unstamped market) refuses here rather than syncing blind.
plan="$(dry_run_plan "$to_block")"
[ -n "$plan" ] || {
    echo "aave-soak: the pre-capture plan is empty - every active market needs a stamped cursor" >&2
    exit 1
}
target="$(printf '%s\n' "$plan" | target_of)"
start_cursor="$(printf '%s\n' "$plan" | cursor_of)"
[ -n "$target" ] || {
    echo "aave-soak: the pre-capture plan names no resolved target: $plan" >&2
    exit 1
}
echo "aave-soak: captured target block $target (cursor ${start_cursor:-none})"

# (2) The purge, only when asked. `aave reset` purges and re-seeds the market's
# cold-boot substrate in ONE transaction under the market's writer lock.
if [ "$reset_market" -eq 1 ]; then
    reset_args=(aave reset)
    if [ -n "$market" ]; then
        reset_args+=(--name "$market")
    fi
    echo "aave-soak: resetting the market (explicit --reset)"
    cli "${reset_args[@]}"
    # The reset rewinds the market to its cold-boot cursor, so the advancement a
    # one-chunk run is measured against starts THERE, not at the pre-reset
    # cursor. The target stays the block captured before any write.
    reset_plan="$(dry_run_plan "$target")"
    start_cursor="$(printf '%s\n' "$reset_plan" | cursor_of)"
    [ -n "$start_cursor" ] || {
        echo "aave-soak: the post-reset plan names no cursor - the reset left no stamp" >&2
        exit 1
    }
    echo "aave-soak: reset left the market at $start_cursor"
fi

# (3) The sync, pinned to the captured target. --one-chunk is the CLI's
# stop-after-first-commit flag; a resumed soak continues from the market cursor.
update_args=(aave update --to-block "$target")
if [ "$one_chunk" -eq 1 ]; then
    update_args+=(--one-chunk)
fi
report="$(cli "${update_args[@]}")"
printf '%s\n' "$report"
events=0
while read -r count; do
    if [ -n "$count" ]; then
        events=$((events + count))
    fi
done < <(printf '%s\n' "$report" | events_of)

# (4) Verify the cursor reached the captured target. The post-run plan reads the
# market row the run committed to, so the assertion is about persisted state,
# not the run's own claim.
final_cursor="$(dry_run_plan "$target" | cursor_of)"
[ -n "$final_cursor" ] || {
    echo "aave-soak: the post-run plan names no cursor - the market row lost its stamp" >&2
    exit 1
}
if [ "$one_chunk" -eq 1 ]; then
    if [ "$final_cursor" -le "${start_cursor:-0}" ]; then
        echo "aave-soak: the one-chunk run did not advance the cursor past ${start_cursor:-none}" >&2
        exit 1
    fi
    echo "aave-soak: one-chunk run stopped at $final_cursor (captured target $target not reached by design)"
else
    if [ "$final_cursor" -ne "$target" ]; then
        echo "aave-soak: cursor $final_cursor never reached the captured target $target" >&2
        exit 1
    fi
fi

elapsed=$((SECONDS - started))
echo "aave-soak: cursor $final_cursor events $events duration ${elapsed}s"
