#!/usr/bin/env bash
#
# Lint/format tool pins: exact for reviewability, TRACKED for freshness.
#
# Every tool the python gates run carries ONE exact pin -- `ruff_version` and
# `ty_version` in the justfile, `markdownlint-cli2@X` in the markdown recipes,
# and the `@commitlint/cli@X` pair in prek.toml + the justfile -- read from a
# single place so no gate's verdict moves without a reviewable diff. An exact
# pin nobody refreshes is the other half of that deal: it ages out. This script
# keeps the pins fresh instead of frozen.
#
#   --report   pinned vs what this repo can install TODAY, per tool. Exits 1 when
#              any pin is behind (the pre-push `lint-tools` gate). An
#              unreachable registry is a WARNING, never a red: the check must
#              not turn "offline" into a broken push.
#   --bump     rewrite every behind pin to the probed version, in place. The
#              `just bump-lint-tools` recipe runs this and then re-runs the
#              gates, so the churn lands as one reviewable diff.
#
# The probe asks uv (`uvx --with P P --version` / `uv run --no-sync --with P
# P --version`) rather than a package index, because uv is the resolver the
# gates themselves use: a pin has to be the newest version THIS repo can
# install. `[tool.uv] exclude-newer` applies to the project-venv form (`ty`), so
# a bare index lookup would name a version uv then refuses -- an un-pushable
# tree. The npm tools have no such cutoff, so their registry is the honest
# source.
#
# live-invariant gate (ADR-087 D2): watches the live ruff/ty/markdownlint/commitlint
# pins and the justfile recipes that read them; not tripwire precedent.
set -euo pipefail

repo_root="${LINT_TOOL_PINS_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
cd "$repo_root"

# tool | file | pin regex (ONE capture group = the version) | probe
#
# probe forms: uvx:PACKAGE (the gate's `uvx --with` form), uv:PACKAGE (the
# gate's `uv run --no-sync --with` form, so exclude-newer applies), npm:PACKAGE
# (the registry's `latest` dist-tag).
pins=(
    'ruff|justfile|^ruff_version := "ruff==([0-9][^"]*)"$|uvx:ruff'
    'ty|justfile|^ty_version := "ty==([0-9][^"]*)"$|uv:ty'
    'markdownlint-cli2|justfile|markdownlint-cli2@([0-9]+\.[0-9]+\.[0-9]+)|npm:markdownlint-cli2'
    '@commitlint/cli (prek.toml)|prek.toml|@commitlint/cli@([0-9]+\.[0-9]+\.[0-9]+)|npm:@commitlint/cli'
    '@commitlint/cli (justfile)|justfile|@commitlint/cli@([0-9]+\.[0-9]+\.[0-9]+)|npm:@commitlint/cli'
)

# read_pin FILE REGEX -> "<version>\t<line>\t<line text>"; fails closed when the
# pin is gone, so a vanished pin can never read as "current".
read_pin() {
    local file="$1" regex="$2" hit line text
    hit="$(grep -m1 -nE -- "$regex" "$file")" || {
        echo "lint-tool-pins: no line matches /${regex}/ in ${file}" >&2
        return 2
    }
    line="${hit%%:*}"
    text="${hit#*:}"
    # BASH_REMATCH reads the pattern's ONE capture group: an unanchored pin
    # (markdownlint sits mid-line in a recipe) yields just the version.
    if [[ ! "$text" =~ $regex ]]; then
        echo "lint-tool-pins: /${regex}/ matched no version in ${file}" >&2
        return 2
    fi
    printf '%s\t%s\t%s\n' "${BASH_REMATCH[1]}" "$line" "$text"
}

# probe_version PROBE -> the version this repo can install today; empty when the
# probe cannot answer (offline, registry down) -- the caller warns, never fails.
probe_version() {
    local probe="$1" kind="${1%%:*}" package="${1#*:}" out
    case "$kind" in
        uvx)
            out="$(uvx --with "$package" "$package" --version 2>/dev/null || true)"
            ;;
        uv)
            out="$(uv run --no-sync --with "$package" "$package" --version 2>/dev/null || true)"
            ;;
        npm)
            out="$(curl -fsS --max-time 15 "https://registry.npmjs.org/${package}/latest" 2>/dev/null \
                | jq -r '.version' 2>/dev/null || true)"
            ;;
        *)
            echo "lint-tool-pins: unknown probe ${probe}" >&2
            return 2
            ;;
    esac
    printf '%s\n' "$out" | grep -oE '[0-9]+\.[0-9]+\.[0-9]+[0-9A-Za-z.-]*' | head -n1 || true
}

mode_report() {
    local spec tool file regex probe pinned line text latest state behind=0 pin_out
    printf '%-26s %-12s %-12s %s\n' tool pinned installable state
    for spec in "${pins[@]}"; do
        IFS='|' read -r tool file regex probe <<<"$spec"
        # A vanished pin is drift the gate cannot paper over: fail closed.
        pin_out="$(read_pin "$file" "$regex")" || return 2
        IFS=$'\t' read -r pinned line text <<<"$pin_out"
        latest="$(probe_version "$probe")"
        if [ -z "$latest" ]; then
            printf '%-26s %-12s %-12s %s\n' "$tool" "$pinned" "?" "unknown (no registry answer)"
            continue
        fi
        if [ "$latest" = "$pinned" ]; then state="current"; else state="BEHIND"; behind=1; fi
        printf '%-26s %-12s %-12s %s\n' "$tool" "$pinned" "$latest" "$state"
    done
    if [ "$behind" -ne 0 ]; then
        {
            echo ""
            echo "lint-tool-pins: at least one pin is behind what this repo can install."
            echo "Refresh them all in one reviewable diff (the recipe re-runs the gates):"
            echo "  just bump-lint-tools"
        } >&2
        return 1
    fi
    echo ""
    echo "lint-tool-pins: every pin is at the newest version this repo can install."
}

mode_bump() {
    local spec tool file regex probe pinned line text latest changed=0 tmp replacement pin_out
    tmp="$(mktemp)"
    trap 'rm -f "${tmp:-}"' EXIT
    for spec in "${pins[@]}"; do
        IFS='|' read -r tool file regex probe <<<"$spec"
        pin_out="$(read_pin "$file" "$regex")" || return 2
        IFS=$'\t' read -r pinned line text <<<"$pin_out"
        latest="$(probe_version "$probe")"
        if [ -z "$latest" ]; then
            echo "lint-tool-pins: ${tool}: no registry answer; leaving ${pinned} alone" >&2
            continue
        fi
        if [ "$latest" = "$pinned" ]; then
            echo "lint-tool-pins: ${tool}: current (${pinned})"
            continue
        fi
        # Swap only the captured version, leaving the rest of the line
        # byte-identical (quoting, trailing comment, surrounding recipe text).
        replacement="${text/"$pinned"/"$latest"}"
        awk -v n="$line" -v repl="$replacement" 'NR==n{print repl; next}{print}' "$file" >"$tmp"
        cat "$tmp" >"$file"
        echo "lint-tool-pins: ${tool}: ${pinned} -> ${latest} (${file}:${line})"
        changed=1
    done
    if [ "$changed" -eq 0 ]; then
        echo "lint-tool-pins: nothing to bump."
    fi
    return 0
}

case "${1:---report}" in
    --report) mode_report ;;
    --bump) mode_bump ;;
    *)
        echo "usage: lint-tool-pins.sh [--report | --bump]" >&2
        exit 2
        ;;
esac