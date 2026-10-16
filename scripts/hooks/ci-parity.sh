#!/usr/bin/env bash
#
# CI-parity brake: every ci.yml job declares how it is reproduced locally.
#
# The CI job matrix is compared here against a MANIFEST of local entrypoints
# (scripts/hooks/ci-parity-manifest.txt): a job without one is drift, and a
# manifest entry naming a `just` recipe that no longer exists is drift too.
# Notes claiming pre-push wiring are additionally checked against prek.toml's
# `stages = ["pre-push"]` entries -- the push-gate manifest `just pre-push`
# delegates to (ADR-074), so the claim cannot outlive the wiring.
#
# Exit codes:
#   0   parity
#   1   drift (prints the offending entries)
#   2   an extraction produced nothing -- fails closed, so a broken read can
#       never report a green that examined nothing
#
# Scope and blind spots:
#   - release.yml is not covered (tag lifecycle; its steps are the publish
#     recipes named in the manifest).
#   - It checks NAMES, not semantics: a job's `run:` body changing under an
#     unchanged step name is invisible here.
#   - Only `run: just <recipe>` lines are machine-checked against the
#     justfile. Raw `run:` commands are the manifest's prose.
#   - Manifest lines whose notes carry "(in just pre-push)" must name a recipe
#     that appears as a `stages = ["pre-push"]` entry in prek.toml, i.e. is
#     part of the push-gate manifest.
#
# Override the sources for the negative probes (tests/test_ci_parity.py):
#   MANIFEST=... CI=... JF=... PREK=...
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$repo_root"

ci="${CI:-.github/workflows/ci.yml}"
jf="${JF:-justfile}"
prek_toml="${PREK:-prek.toml}"
manifest_file="${MANIFEST:-scripts/hooks/ci-parity-manifest.txt}"

# The manifest: job id | kind | local entrypoint notes.
#   local    -- a named local entrypoint exists (named in the notes)
#   ci-only  -- nothing to run locally; the notes must say WHY
if [ ! -s "$manifest_file" ]; then
    echo "ci-parity: manifest ${manifest_file} is missing or empty -- refusing to pass" >&2
    exit 2
fi

# Job ids: the two-space keys of the `jobs:` section (nested keys are deeper).
mapfile -t jobs < <(awk '
    /^jobs:$/ { in_jobs=1; next }
    in_jobs && /^[^ ]/ { in_jobs=0 }
    in_jobs && /^  [A-Za-z0-9_-]+:$/ { line=$0; sub(/^  /, "", line); sub(/:$/, "", line); print line }
' "$ci")

if [ "${#jobs[@]}" -eq 0 ]; then
    echo "ci-parity: extracted 0 jobs from ${ci} -- refusing to pass" >&2
    exit 2
fi

# The push-gate manifest (ADR-074): prek.toml's `stages = ["pre-push"]` hook
# entries, one single-line `{ id = ... }` table each. The sed anchors on the
# table opener so the prose in the surrounding comment blocks cannot match --
# which also means a pre-push hook written as a multi-line TOML table would be
# invisible here. Keep the pre-push entries single-line.
mapfile -t pre_push_entries < <(
    sed -n 's/^[[:space:]]*{ id = .*entry = "\([^"]*\)".*stages = \["pre-push"\].*/\1/p' "$prek_toml"
)

if [ "${#pre_push_entries[@]}" -eq 0 ]; then
    echo "ci-parity: extracted 0 pre-push entries from ${prek_toml} -- refusing to pass" >&2
    exit 2
fi

fail=0

# Does the justfile define this recipe? (a recipe line starts with the name,
# then `:`, whitespace, or a parameter list)
recipe_exists() {
    grep -I -qE "^${1}(:|[[:space:]])" "$jf"
}

# The `just <recipe>` names a note carries, minus the "(in just pre-push)"
# marker itself -- its own `just pre-push` would otherwise read as a gate name.
note_recipes() {
    printf '%s' "$1" | sed 's/([^)]*in just pre-push[^)]*)//' \
        | grep -oE 'just [a-z0-9_-]+' | awk '{print $2}' | sort -u || true
}

in_push_manifest() {
    local entry
    for entry in "${pre_push_entries[@]}"; do
        [ "$entry" = "$1" ] && return 0
    done
    return 1
}

declare -A in_manifest=()

while IFS='|' read -r job kind notes; do
    [ -n "${job}" ] || continue
    if [ -z "${kind}" ] || [ -z "${notes}" ]; then
        echo "ci-parity: manifest line for '${job}' needs kind AND notes" >&2
        fail=1
        continue
    fi
    case "${kind}" in
        local|ci-only) ;;
        *)
            echo "ci-parity: manifest line for '${job}' has unknown kind '${kind}'" >&2
            fail=1
            ;;
    esac
    if [ "${kind}" = "ci-only" ] && ! printf '%s' "${notes}" | grep -qE '[a-z]{3,}'; then
        echo "ci-parity: ci-only job '${job}' must say why (notes)" >&2
        fail=1
    fi
    in_manifest["${job}"]=1
    # Every `just <recipe>` the notes name must exist ...
    for name in $(note_recipes "${notes}"); do
        if ! recipe_exists "${name}"; then
            echo "ci-parity: job '${job}' names just recipe '${name}' which does not exist in ${jf}" >&2
            fail=1
        fi
        # ... and, when the notes claim pre-push wiring, be wired there.
        if printf '%s' "${notes}" | grep -q 'in just pre-push'; then
            if ! in_push_manifest "just ${name}"; then
                echo "ci-parity: job '${job}' claims '${name}' is in just pre-push, but no stages = [\"pre-push\"] entry in ${prek_toml} runs 'just ${name}'" >&2
                fail=1
            fi
        fi
    done
done < "$manifest_file"

if [ "${#in_manifest[@]}" -eq 0 ]; then
    echo "ci-parity: the manifest produced 0 entries -- refusing to pass" >&2
    exit 2
fi

# Structural floor: a job without `runs-on:` is an invalid workflow -- it
# fails at schedule time on the runner, where nothing local can see it.
# (2026-10-14: a seeded-teeth restore deleted every runs-on line in this
# file and only a manual diff review caught it. This check makes that
# damage class visible here instead.)
mapfile -t jobs_with_runner < <(awk '
    /^jobs:$/ { in_jobs=1; next }
    in_jobs && /^[^ ]/ { in_jobs=0 }
    in_jobs && /^  [A-Za-z0-9_-]+:$/ { line=$0; sub(/^  /, "", line); sub(/:$/, "", line); job=line }
    in_jobs && /^    runs-on:/ { print job }
' "${ci}")
for job in "${jobs[@]}"; do
    found=0
    for j in "${jobs_with_runner[@]}"; do
        [ "${j}" = "${job}" ] && found=1
    done
    if [ "${found}" -eq 0 ]; then
        echo "ci-parity: ci.yml job '${job}' has no runs-on: line (the workflow would not schedule)" >&2
        fail=1
    fi
done

# Direction 1: every ci.yml job has a manifest line.
for job in "${jobs[@]}"; do
    if [ -z "${in_manifest[${job}]:-}" ]; then
        echo "ci-parity: ci.yml job '${job}' has no manifest line in ${manifest_file}" >&2
        fail=1
    fi
done

# Direction 2: no stale manifest lines.
for job in "${!in_manifest[@]}"; do
    found=0
    for j in "${jobs[@]}"; do
        [ "${j}" = "${job}" ] && found=1
    done
    if [ "${found}" -eq 0 ]; then
        echo "ci-parity: manifest line for '${job}' but no such job in ${ci}" >&2
        fail=1
    fi
done

# Every recipe CI invokes by name must exist locally.
mapfile -t ci_just < <(grep -oE 'run: just [a-z0-9_-]+' "${ci}" | awk '{print $3}' | sort -u)

if [ "${#ci_just[@]}" -eq 0 ]; then
    echo "ci-parity: extracted 0 'run: just' calls from ${ci} -- refusing to pass" >&2
    exit 2
fi
for name in "${ci_just[@]}"; do
    if ! recipe_exists "${name}"; then
        echo "ci-parity: ${ci} runs 'just ${name}' which does not exist in ${jf}" >&2
        fail=1
    fi
done

if [ "${fail}" -ne 0 ]; then
    echo
    echo "ci-parity FAILED: give each ci.yml job a local entrypoint (or record why it is CI-only), and keep the named just recipes alive."
    exit 1
fi

echo "ci-parity: ${#jobs[@]} ci.yml jobs covered; ${#ci_just[@]} CI-named just recipes resolve."
