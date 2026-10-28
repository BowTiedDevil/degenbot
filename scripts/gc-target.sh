#!/usr/bin/env bash

# Report and reclaim rebuildable Rust target artifacts without touching the
# warm maturin link cache or anything outside the Cargo target roots.

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
active_target_dir="$(realpath -m -- "${GC_TARGET_DIR:-${CARGO_TARGET_DIR:-$repo_root/rust/target}}")"

# This repo builds cargo artifacts in two isolated environments against the
# same checkout: the devcontainer keeps the in-tree rust/target, while the
# host (via .envrc) redirects builds to ~/.cache/degenbot-target. Sweep the
# other environment's target root too whenever it exists and differs from the
# active one — devcontainer.json binds the host cache into the container at
# the same path — so one `just gc-target` reclaims both sides from either
# side. Roots are deduped by realpath and are disjoint in practice; an
# explicitly set GC_TARGET_DIR narrows the sweep to exactly that root (tests
# and targeted sweeps).
declare -a root_candidates=(
    "$active_target_dir"
    "${HOME:-}/.cache/degenbot-target"
    "$repo_root/rust/target"
)
declare -a target_roots=()
declare -A root_seen=()
if [[ -n "${GC_TARGET_DIR:-}" ]]; then
    target_roots=("$active_target_dir")
else
    for root in "${root_candidates[@]}"; do
        [[ -n "$root" && -e "$root" ]] || continue
        root="$(realpath -m -- "$root")"
        [[ -n "${root_seen["$root"]+present}" ]] && continue
        root_seen["$root"]=1
        target_roots+=("$root")
    done
    if ((${#target_roots[@]} == 0)); then
        target_roots=("$active_target_dir")
    fi
fi

maturin_dir="$active_target_dir/maturin"
age="${AGE:-3}"

if [[ ! "$age" =~ ^[0-9]+$ ]]; then
    echo "ERROR: AGE must be a non-negative integer (got '$age')" >&2
    exit 2
fi

case "${DRY_RUN:-0}" in
    1|true|TRUE|yes|YES) action="dry-run" ;;
    *) action="cleanup" ;;
esac

apparent_bytes() {
    local path="$1"
    if [[ -e "$path" || -L "$path" ]]; then
        du -sb -- "$path" 2>/dev/null | awk '{print $1}'
    else
        echo 0
    fi
}

sum_apparent_bytes() {
    local path
    local -a existing_paths=()
    for path in "$@"; do
        if [[ -e "$path" || -L "$path" ]]; then
            existing_paths+=("$path")
        fi
    done
    if ((${#existing_paths[@]} == 0)); then
        echo 0
        return
    fi
    printf '%s\0' "${existing_paths[@]}" \
        | xargs -0 -r du -cb 2>/dev/null \
        | awk '$2 == "total" { total += $1 } END { print total + 0 }'
}

family_size_for() {
    # Apparent bytes of a family's per-root subdirectory across every target
    # root (missing roots contribute zero).
    local -a paths=()
    local root sub
    for root in "${target_roots[@]}"; do
        for sub in "$@"; do
            paths+=("$root/$sub")
        done
    done
    sum_apparent_bytes "${paths[@]}"
}

declare -a family_names=(
    normal-cargo
    maturin
    coverage
    llvm-coverage
    criterion
    wheels
    documentation
    stubgen-inspect
)
declare -A family_policy=(
    [normal-cargo]="age+incremental+dedupe"
    [maturin]="protected"
    [coverage]="age+stale-top-level"
    [llvm-coverage]="age"
    [criterion]="age"
    [wheels]="age"
    [documentation]="age"
    [stubgen-inspect]="age"
)
declare -A family_size=()
declare -A family_reclaimable=()
declare -A family_selected=()
for family in "${family_names[@]}"; do
    family_size["$family"]=0
    family_reclaimable["$family"]=0
    family_selected["$family"]=0
done

declare -a candidate_paths=()
declare -A candidate_seen=()
declare -A candidate_family=()

_path_in_union() {
    # True when $1 is strictly inside one of the target roots.
    local path="$1" root
    for root in "${target_roots[@]}"; do
        case "$path" in
            "$root"/?*) return 0 ;;
        esac
    done
    return 1
}

_path_shelters_root() {
    # True when $1 is one of the target roots or an ancestor of one, so it
    # must never be selected for deletion.
    local path="$1" root
    for root in "${target_roots[@]}"; do
        case "$root" in
            "$path"|"$path"/?*) return 0 ;;
        esac
    done
    return 1
}

add_candidate() {
    local family="$1"
    local path root
    path="$(realpath -m -- "$2")"

    # The deletion boundary is the union of the target roots: anything
    # outside them cannot enter this list.
    _path_in_union "$path" || return
    # A hand-nested root (GC_TARGET_DIR inside another root) must never be
    # selected, nor may an ancestor of one be.
    _path_shelters_root "$path" && return
    # Every root's maturin/ link cache is protected.
    for root in "${target_roots[@]}"; do
        case "$path" in
            "$root"/maturin|"$root"/maturin/*) return ;;
        esac
    done
    [[ -n "${candidate_seen["$path"]+present}" ]] && return

    candidate_seen["$path"]=1
    candidate_family["$path"]="$family"
    candidate_paths+=("$path")
    family_selected["$family"]=$((family_selected["$family"] + 1))
}

select_stale_children() {
    local family="$1"
    local root="$2"
    [[ -d "$root" ]] || return 0
    local path
    while IFS= read -r -d '' path; do
        add_candidate "$family" "$path"
    done < <(find "$root" -mindepth 1 -maxdepth 1 -mtime "+$age" -print0 2>/dev/null)
}

cargo_subtrees=(deps examples build .fingerprint)
for root in "${target_roots[@]}"; do
    for build_root in "$root/debug" "$root/release" "$root/rust-analyzer/debug"; do
        [[ -d "$build_root" ]] || continue
        add_candidate normal-cargo "$build_root/incremental"
        for subtree in "${cargo_subtrees[@]}"; do
            select_stale_children normal-cargo "$build_root/$subtree"
        done
    done
done

# Cargo hashes metadata into executable names. Feature and environment churn
# can therefore leave many old variants of the same test binary; retain the
# newest large extensionless executable in each basename group.
duplicates_file="$(mktemp)"
trap 'rm -f "$duplicates_file"' EXIT
for root in "${target_roots[@]}"; do
    for dir in \
        "$root/debug/deps" \
        "$root/debug/examples" \
        "$root/release/deps" \
        "$root/release/examples" \
        "$root/rust-analyzer/debug/deps" \
        "$root/rust-analyzer/debug/examples"
    do
        [[ -d "$dir" ]] || continue
        : >"$duplicates_file"
        find "$dir" -maxdepth 1 -type f -size +10M -printf '%T@ %f\n' 2>/dev/null \
            | awk '{
                stamp = $1
                name = $2
                basename = name
                sub(/-[0-9a-f]{16}$/, "", basename)
                if (basename in newest) {
                    if (stamp > newest[basename]) {
                        print previous[basename]
                        newest[basename] = stamp
                        previous[basename] = name
                    } else {
                        print name
                    }
                } else {
                    newest[basename] = stamp
                    previous[basename] = name
                }
            }' >"$duplicates_file"
        while IFS= read -r duplicate; do
            [[ -n "$duplicate" ]] || continue
            add_candidate normal-cargo "$dir/$duplicate"
        done <"$duplicates_file"
    done
done

for root in "${target_roots[@]}"; do
    select_stale_children coverage "$root/coverage"
    select_stale_children llvm-coverage "$root/llvm-cov-target"
    select_stale_children criterion "$root/criterion"
    select_stale_children wheels "$root/wheels"
    select_stale_children documentation "$root/doc"
    select_stale_children stubgen-inspect "$root/stubgen-inspect"
done

# Ad-hoc LLVM coverage runs also leave coverage files directly in a Cargo
# target root rather than under that root's coverage/.
for root in "${target_roots[@]}"; do
    while IFS= read -r -d '' artifact; do
        add_candidate coverage "$artifact"
    done < <(
        find "$root" -mindepth 1 -maxdepth 1 -type f -mtime "+$age" \
            \( -name '*.info' -o -name '*.lcov' -o -name '*.profdata' \
            -o -name '*.profraw' -o -name 'coverage.xml' \) -print0 2>/dev/null
    )
done

family_size["normal-cargo"]="$(family_size_for debug release rust-analyzer)"
family_size["maturin"]="$(family_size_for maturin)"
family_size["coverage"]="$(family_size_for coverage)"
family_size["llvm-coverage"]="$(family_size_for llvm-cov-target)"
family_size["criterion"]="$(family_size_for criterion)"
family_size["wheels"]="$(family_size_for wheels)"
family_size["documentation"]="$(family_size_for doc)"
family_size["stubgen-inspect"]="$(family_size_for stubgen-inspect)"

for family in "${family_names[@]}"; do
    family_paths=()
    for path in "${candidate_paths[@]}"; do
        if [[ "${candidate_family["$path"]}" == "$family" ]]; then
            family_paths+=("$path")
        fi
    done
    family_reclaimable["$family"]="$(sum_apparent_bytes "${family_paths[@]}")"
done

reclaimable_bytes=0
for family in "${family_names[@]}"; do
    reclaimable_bytes=$((reclaimable_bytes + family_reclaimable["$family"]))
done

target_before="$(sum_apparent_bytes "${target_roots[@]}")"
printf 'action=%s\n' "$action"
printf 'age-days=%s\n' "$age"
printf 'target-dir=%s\n' "$active_target_dir"
if ((${#target_roots[@]} > 1)); then
    for root in "${target_roots[@]}"; do
        [[ "$root" == "$active_target_dir" ]] && continue
        printf 'extra-target-dir=%s\n' "$root"
    done
fi
for family in "${family_names[@]}"; do
    printf 'family=%s size-bytes=%s reclaimable-bytes=%s selected-paths=%s policy=%s\n' \
        "$family" \
        "${family_size["$family"]}" \
        "${family_reclaimable["$family"]}" \
        "${family_selected["$family"]}" \
        "${family_policy["$family"]}"
done
printf 'reclaimable-bytes=%s\n' "$reclaimable_bytes"
printf 'target-size-before-bytes=%s\n' "$target_before"
printf 'protected=%s\n' "$maturin_dir"

deleted_paths=0
if [[ "$action" == "cleanup" ]]; then
    for path in "${candidate_paths[@]}"; do
        if rm -rf -- "$path" 2>/dev/null; then
            deleted_paths=$((deleted_paths + 1))
        else
            echo "WARN: could not fully remove $path (a build may be writing into it)" >&2
        fi
    done
fi

target_after="$(sum_apparent_bytes "${target_roots[@]}")"
printf 'deleted-paths=%s\n' "$deleted_paths"
printf 'target-size-after-bytes=%s\n' "$target_after"
