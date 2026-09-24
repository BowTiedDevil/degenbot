#!/usr/bin/env bash
# Report and reclaim rebuildable Rust target artifacts without touching the
# warm maturin link cache or the repository-root extension build receipt.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
target_dir="$(realpath -m -- "${GC_TARGET_DIR:-$repo_root/rust/target}")"
maturin_dir="$target_dir/maturin"
build_number="$(realpath -m -- "$target_dir/../../.build-number")"
age="${AGE:-7}"

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

declare -a family_names=(
    normal-cargo
    maturin
    coverage
    llvm-coverage
    criterion
    wheels
    documentation
)
declare -A family_policy=(
    [normal-cargo]="age+incremental+dedupe"
    [maturin]="protected"
    [coverage]="age+stale-top-level"
    [llvm-coverage]="age"
    [criterion]="age"
    [wheels]="age"
    [documentation]="age"
)
declare -A family_size=()
declare -A family_reclaimable=()
declare -A family_selected=()
for family in "${family_names[@]}"; do
    family_size["$family"]=0
    family_reclaimable["$family"]=0
    family_selected["$family"]=0
done

cargo_family_paths=(
    "$target_dir/debug"
    "$target_dir/release"
    "$target_dir/rust-analyzer"
)
family_size["normal-cargo"]="$(sum_apparent_bytes "${cargo_family_paths[@]}")"
family_size["maturin"]="$(apparent_bytes "$maturin_dir")"
family_size["coverage"]="$(apparent_bytes "$target_dir/coverage")"
family_size["llvm-coverage"]="$(apparent_bytes "$target_dir/llvm-cov-target")"
family_size["criterion"]="$(apparent_bytes "$target_dir/criterion")"
family_size["wheels"]="$(apparent_bytes "$target_dir/wheels")"
family_size["documentation"]="$(apparent_bytes "$target_dir/doc")"

declare -a candidate_paths=()
declare -A candidate_seen=()
declare -A candidate_family=()

add_candidate() {
    local family="$1"
    local path
    path="$(realpath -m -- "$2")"

    # The deletion boundary is the Cargo target root. In particular, the
    # repository-root build receipt is outside it and cannot enter this list.
    case "$path" in
        "$target_dir"/*) ;;
        *) return ;;
    esac
    case "$path" in
        "$maturin_dir"|"$maturin_dir"/*) return ;;
    esac
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

cargo_build_roots=(
    "$target_dir/debug"
    "$target_dir/release"
    "$target_dir/rust-analyzer/debug"
)
cargo_subtrees=(deps examples build .fingerprint)
for root in "${cargo_build_roots[@]}"; do
    [[ -d "$root" ]] || continue
    add_candidate normal-cargo "$root/incremental"
    for subtree in "${cargo_subtrees[@]}"; do
        select_stale_children normal-cargo "$root/$subtree"
    done
done

# Cargo hashes metadata into executable names. Feature and environment churn
# can therefore leave many old variants of the same test binary; retain the
# newest large extensionless executable in each basename group.
duplicates_file="$(mktemp)"
trap 'rm -f "$duplicates_file"' EXIT
for dir in \
    "$target_dir/debug/deps" \
    "$target_dir/debug/examples" \
    "$target_dir/release/deps" \
    "$target_dir/release/examples" \
    "$target_dir/rust-analyzer/debug/deps" \
    "$target_dir/rust-analyzer/debug/examples"
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

select_stale_children coverage "$target_dir/coverage"
select_stale_children llvm-coverage "$target_dir/llvm-cov-target"
select_stale_children criterion "$target_dir/criterion"
select_stale_children wheels "$target_dir/wheels"
select_stale_children documentation "$target_dir/doc"

# CRAP and ad-hoc LLVM runs also leave coverage files directly in the Cargo
# target root rather than under rust/target/coverage.
while IFS= read -r -d '' artifact; do
    add_candidate coverage "$artifact"
done < <(
    find "$target_dir" -mindepth 1 -maxdepth 1 -type f -mtime "+$age" \
        \( -name '*.info' -o -name '*.lcov' -o -name '*.profdata' \
        -o -name '*.profraw' -o -name 'coverage.xml' \) -print0 2>/dev/null
)

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

target_before="$(apparent_bytes "$target_dir")"
printf 'action=%s\n' "$action"
printf 'age-days=%s\n' "$age"
printf 'target-dir=%s\n' "$target_dir"
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
printf 'protected=%s\n' "$build_number"
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

target_after="$(apparent_bytes "$target_dir")"
printf 'deleted-paths=%s\n' "$deleted_paths"
printf 'target-size-after-bytes=%s\n' "$target_after"
