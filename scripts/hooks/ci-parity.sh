#!/usr/bin/env bash
#
# CI-parity brake: every ci.yml job declares how it is reproduced locally.
#
# The CI job matrix is compared here against a MANIFEST of local entrypoints
# (scripts/hooks/ci-parity-manifest.txt): a job without one is drift, and a
# manifest entry naming a `just` recipe that no longer exists is drift too.
# The manifest's push_gates column is additionally checked against prek.toml's
# `stages = ["pre-push"]` entries -- the push-gate manifest `just pre-push`
# delegates to (ADR-074), so the declaration cannot outlive the wiring.
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
#   - The manifest's push_gates column names entry strings that must appear as
#     `stages = ["pre-push"]` entries in prek.toml, i.e. must be part of the
#     push-gate manifest; notes carry no wiring claim.
#
# Override the sources for the negative probes (tests/test_ci_parity.py):
#   MANIFEST=... CI=... JF=... PREK=... TOOLS=...
#
# live-invariant gate (ADR-087 D2): watches the live ci-parity manifest and the prek.toml `stages = ["pre-push"]` entries it is cross-checked against; not tripwire precedent.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$repo_root"

ci="${CI:-.github/workflows/ci.yml}"
jf="${JF:-justfile}"
prek_toml="${PREK:-prek.toml}"
manifest_file="${MANIFEST:-scripts/hooks/ci-parity-manifest.txt}"

# The manifest: job id | kind | push_gates | local entrypoint notes.
#   local    -- a named local entrypoint exists (named in the notes)
#   ci-only  -- nothing to run locally; the notes must say WHY
# push_gates is a comma-separated list of prek.toml `entry` strings that must
# be `stages = ["pre-push"]` entries; empty when the job is not push-wired.
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

trim() {
    local value="$1"
    value="${value#"${value%%[![:space:]]*}"}"
    value="${value%"${value##*[![:space:]]}"}"
    printf '%s' "${value}"
}

in_push_manifest() {
    local entry
    for entry in "${pre_push_entries[@]}"; do
        [ "$entry" = "$1" ] && return 0
    done
    return 1
}

declare -A in_manifest=()

while IFS='|' read -r job kind push_gates notes; do
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
    for name in $(printf '%s' "${notes}" | grep -oE 'just [a-z0-9_-]+' | awk '{print $2}' | sort -u || true); do
        if ! recipe_exists "${name}"; then
            echo "ci-parity: job '${job}' names just recipe '${name}' which does not exist in ${jf}" >&2
            fail=1
        fi
    done
    # ... and every entry string in the push_gates column must be wired into the
    # push-gate manifest (prek.toml's `stages = ["pre-push"]` entries).
    IFS=',' read -ra gates <<< "${push_gates}"
    for gate in "${gates[@]}"; do
        gate="$(trim "${gate}")"
        [ -n "${gate}" ] || continue
        if ! in_push_manifest "${gate}"; then
            echo "ci-parity: job '${job}' declares push gate '${gate}' which is not a stages = [\"pre-push\"] entry in ${prek_toml}" >&2
            fail=1
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

# --- Toolchain parity -------------------------------------------------------
# A `just` recipe whose tool the running job never installs fails at shell level
# -- `no such command: nextest`, `Failed to spawn: vyper`, `uv: command not
# found` -- while every local gate stays green, because a developer environment
# has the tool. The requirement table names, per recipe ci.yml invokes, the
# tools the recipe needs and the ones it provisions itself:
#
#   recipe|requires|provides
#
# Reconciled in both directions like the manifest above: a recipe a job runs
# with no row is drift, and a row no job runs is a stale claim.
tools_file="${TOOLS:-scripts/hooks/ci-recipe-tools.txt}"

if [ ! -s "${tools_file}" ]; then
    echo "ci-parity: toolchain table ${tools_file} is missing or empty -- refusing to pass" >&2
    exit 2
fi

# The job's own YAML text, comments stripped: prose never provisions a tool,
# and a comment mentioning `cargo nextest` must not read as an install.
job_steps() {
    awk -v want="$1" '
        /^jobs:$/ { in_jobs=1; next }
        in_jobs && /^[^ ]/ { in_jobs=0 }
        in_jobs && /^  [A-Za-z0-9_-]+:$/ {
            line=$0; sub(/^  /, "", line); sub(/:$/, "", line); cur=line; next
        }
        in_jobs && cur==want && !/^[[:space:]]*#/ { print }
    ' "${ci}"
}

# The recipes a job invokes, read from its `run:` bodies only (single-line and
# `run: |` blocks), so a recipe named in a comment or a step name is not a call.
job_recipes() {
    job_steps "$1" | awk '
        /^[[:space:]]*-?[[:space:]]*run:[[:space:]]*[|>]-?[[:space:]]*$/ { in_run=1; next }
        /^[[:space:]]*-?[[:space:]]*run:/ { in_run=0; print; next }
        in_run && /^[[:space:]]{10,}[^[:space:]]/ { print; next }
        in_run { in_run=0 }
    ' | grep -oE '\bjust [a-z0-9_-]+' | awk '{print $2}' | sort -u || true
}

# One predicate per tool name: does this job's step text provision it?
provisions_tool() {
    local block="$1" tool="$2"
    case "${tool}" in
        uv) printf '%s\n' "${block}" | grep -qE 'uses:[[:space:]]*astral-sh/setup-uv@' ;;
        cargo-nextest) printf '%s\n' "${block}" | grep -qE 'tool:[[:space:]]*nextest|cargo install[^|]*cargo-nextest' ;;
        dev-venv) printf '%s\n' "${block}" | grep -qE 'uv sync[^|]*--dev' ;;
        *)
            echo "ci-parity: unknown tool '${tool}' in ${tools_file}" >&2
            return 2
            ;;
    esac
}

declare -A requires_of=()
declare -A provides_of=()
declare -A declared=()

while IFS='|' read -r recipe requires provides; do
    [ -n "${recipe}" ] || continue
    case "${recipe}" in \#*) continue ;; esac
    declared["${recipe}"]=1
    requires_of["${recipe}"]="${requires}"
    provides_of["${recipe}"]="${provides}"
done < "${tools_file}"

if [ "${#declared[@]}" -eq 0 ]; then
    echo "ci-parity: ${tools_file} produced 0 rows -- refusing to pass" >&2
    exit 2
fi

tool_list() { printf '%s' "$1" | tr ',' ' '; }

declare -A ci_recipes=()

for job in "${jobs[@]}"; do
    steps="$(job_steps "${job}")"
    mapfile -t recipes_of_job < <(job_recipes "${job}")
    [ "${#recipes_of_job[@]}" -gt 0 ] || continue
    for recipe in "${recipes_of_job[@]}"; do
        ci_recipes["${recipe}"]=1
        if [ -z "${declared[${recipe}]:-}" ]; then
            echo "ci-parity: job '${job}' runs 'just ${recipe}', which has no row in ${tools_file}" >&2
            fail=1
            continue
        fi
        for tool in $(tool_list "${requires_of[${recipe}]}"); do
            if provisions_tool "${steps}" "${tool}"; then
                continue
            fi
            provided=0
            for other in "${recipes_of_job[@]}"; do
                for given in $(tool_list "${provides_of[${other}]}"); do
                    [ "${given}" = "${tool}" ] && provided=1
                done
            done
            if [ "${provided}" -eq 0 ]; then
                echo "ci-parity: job '${job}' runs 'just ${recipe}', which needs '${tool}', and nothing in that job provisions it" >&2
                fail=1
            fi
        done
    done
done

if [ "${#ci_recipes[@]}" -eq 0 ]; then
    echo "ci-parity: extracted 0 'just <recipe>' calls from ${ci} -- refusing to pass" >&2
    exit 2
fi

for recipe in "${!declared[@]}"; do
    if [ -z "${ci_recipes[${recipe}]:-}" ]; then
        echo "ci-parity: ${tools_file} declares '${recipe}', which no ci.yml job runs" >&2
        fail=1
    fi
done

if [ "${fail}" -ne 0 ]; then
    echo
    echo "ci-parity FAILED: give each ci.yml job a local entrypoint (or record why it is CI-only), keep the named just recipes alive, and provision the tools those recipes need."
    exit 1
fi

echo "ci-parity: ${#jobs[@]} ci.yml jobs covered; ${#ci_just[@]} CI-named just recipes resolve; ${#ci_recipes[@]} recipe toolchains provisioned."
