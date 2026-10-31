#!/usr/bin/env bash
# Provision the LLVM 23 toolchain the evm2 JIT link requires.
#
# `evm2-jit-llvm` links `llvm-sys`, which discovers LLVM in exactly two places:
# `$LLVM_SYS_<crate-major>_PREFIX/bin` (llvm-sys 231.0.2 -> `LLVM_SYS_231_PREFIX`
# — the number is the crate's major version, not `23`), or a compatible
# `llvm-config` on PATH (`llvm-config-23`, `llvm-config23`, `llvm23-config`,
# `llvm-config-23.1`, `llvm-config231`, `llvm-config`, accepting LLVM >= 23.1).
# Nothing else: no pkg-config, no CMake, no distro tag.
#
# Two properties of the official prebuilt tarball shape this script:
#
#   * It is minutes versus hours for a source build, but its binaries are built
#     on a glibc 2.34-era host, so the container running them needs
#     glibc >= 2.34 — manylinux_2_28 (glibc 2.28) cannot run it. Its static
#     archives also reference `__libc_single_threaded` (glibc 2.32+), so the
#     same floor applies to linking.
#   * `llvm-config --system-libs` reports the absolute path of the static zstd
#     archive from its own build host (`/usr/lib/x86_64-linux-gnu/libzstd.a`),
#     and llvm-sys panics unless that path is a file where the build runs.
#     --system-deps installs the -devel packages, then links every recorded
#     path at the archive this machine actually ships.
#
# Modes:
#   scripts/provision-llvm23.sh                 print the exports (caller evals)
#   scripts/provision-llvm23.sh --github-env    append them to $GITHUB_ENV
#   scripts/provision-llvm23.sh --check         assert an LLVM 23 is discoverable
#
# Flags:
#   --system-deps   install the static/dev libraries the LLVM archives link
#                   against (dnf/yum/apt); needs root or passwordless sudo, so it
#                   is for CI containers and runners
#
# Env overrides:
#   LLVM23_ROOT     extraction root (default: $RUNNER_TEMP or /tmp, keyed by
#                   version and arch, so a warm runner reuses one extraction)
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

llvm_version="23.1.4"
mode="print"
system_deps=0

for arg in "$@"; do
    case "$arg" in
        --github-env) mode="github-env" ;;
        --check) mode="check" ;;
        --system-deps) system_deps=1 ;;
        -h|--help) sed -n '2,38p' "${BASH_SOURCE[0]}"; exit 0 ;;
        *) echo "provision-llvm23: unknown argument '$arg'" >&2; exit 2 ;;
    esac
done

log() { printf 'llvm23: %s\n' "$*" >&2; }
die() {
    printf 'llvm23: %s\n' "$*" >&2
    exit 1
}
as_root() {
    if [ "$(id -u)" -eq 0 ]; then "$@"; else sudo "$@"; fi
}

# The prefix variable is derived from the locked llvm-sys version so a crate
# bump cannot silently point at a stale variable name; the LLVM major it targets
# must match the pinned tarball, so a bump is a deliberate edit here.
crate_major=$(awk '/^name = "llvm-sys"/{getline; gsub(/[^0-9.]/, "", $0); split($0, v, "."); print v[1]; exit}' rust/Cargo.lock)
[ -n "$crate_major" ] || die "llvm-sys is not pinned in rust/Cargo.lock"
prefix_var="LLVM_SYS_${crate_major}_PREFIX"
llvm_major="${llvm_version%%.*}"
llvm_minor="${llvm_version#*.}"
llvm_minor="${llvm_minor%%.*}"
[ "$((crate_major / 10))" -eq "$llvm_major" ] || die "llvm-sys ${crate_major}.* targets LLVM $((crate_major / 10)), but this script pins LLVM ${llvm_version} — bump llvm_version and its digests"

# llvm-sys's own candidate order, restricted to binaries that answer --version.
check_discovery() {
    local names=("llvm-config-${llvm_major}" "llvm-config${llvm_major}" "llvm${llvm_major}-config"
                 "llvm-config-${llvm_major}.${llvm_minor}" "llvm-config${llvm_major}${llvm_minor}" "llvm-config")
    local name dir candidate version found=""
    local -a search=()
    [ -n "${!prefix_var:-}" ] && search+=("${!prefix_var}/bin")
    local -a path_dirs=()
    IFS=: read -r -a path_dirs <<<"$PATH"
    search+=("${path_dirs[@]}")
    for dir in "${search[@]}"; do
        for name in "${names[@]}"; do
            candidate="$dir/$name"
            [ -x "$candidate" ] || continue
            version=$("$candidate" --version 2>/dev/null) || continue
            case "$version" in
                "${llvm_major}."*)
                    log "discovered $candidate (LLVM $version)"
                    found="$candidate"
                    break 2
                    ;;
                *) log "ignoring $candidate (LLVM $version)" ;;
            esac
        done
    done
    [ -n "$found" ] || die "no LLVM ${llvm_major}.${llvm_minor}+ llvm-config discoverable via \$$prefix_var or PATH"
}

if [ "$mode" = "check" ]; then
    check_discovery
    exit 0
fi

case "$(uname -s)" in
    Linux) ;;
    *) die "this script provisions the Linux prebuilt only; other platforms need a toolchain this script cannot supply (docs/llvm-release-ci.md)" ;;
esac

case "$(uname -m)" in
    x86_64|amd64)
        tar_arch="X64"
        # sha256 of LLVM-${llvm_version}-Linux-X64.tar.xz, from the release's
        # published attestation (llvmorg-${llvm_version}).
        tar_sha256="25d814fda50d5fb0084b42be09605d8b1595d49d5e2e03a3345d7e112ff6f9f3"
        ;;
    aarch64|arm64)
        tar_arch="ARM64"
        tar_sha256="9b27cf263d711aa6ebfd4045ab241d947bd09b3cd19ffa5728f48a80c9eb3a91"
        ;;
    *) die "no prebuilt LLVM ${llvm_version} for $(uname -m)" ;;
esac

root="${LLVM23_ROOT:-${RUNNER_TEMP:-/tmp}/llvm23-${llvm_version}-${tar_arch}}"
archive="$root/LLVM-${llvm_version}-Linux-${tar_arch}.tar.xz"

install_system_deps() {
    if command -v dnf >/dev/null 2>&1; then
        as_root dnf install -y curl tar xz zlib-devel libzstd-devel libzstd-static ncurses-devel libxml2-devel libffi-devel
    elif command -v yum >/dev/null 2>&1; then
        # Some EL repos keep the zstd static archive out of libzstd-devel; a
        # missing libzstd-static surfaces as an unresolved `zstd` at link time.
        as_root yum install -y curl tar xz zlib-devel libzstd-devel libzstd-static ncurses-devel libxml2-devel libffi-devel
    elif command -v apt-get >/dev/null 2>&1; then
        as_root apt-get update -qq
        as_root apt-get install -y curl tar xz-utils zlib1g-dev libzstd-dev libncurses-dev libxml2-dev libffi-dev
    else
        log "no dnf/yum/apt-get here: skipping system dependency installation"
    fi
}

# llvm-sys panics on any absolute path `llvm-config --system-libs` prints that is
# not a file on this machine; the prebuilt only knows its own build host's paths.
repair_recorded_paths() {
    local token base actual
    for token in $("$root/bin/llvm-config" --system-libs); do
        case "$token" in
            /*) ;;
            *) continue ;;
        esac
        [ -e "$token" ] && continue
        base=$(basename "$token")
        actual=$(find /usr/lib64 /usr/lib /lib64 /lib -maxdepth 3 -name "$base" \( -type f -o -type l \) 2>/dev/null | head -n 1)
        [ -n "$actual" ] || die "$root/bin/llvm-config records the system library $token and $base is not installed (see --system-deps)"
        as_root mkdir -p "$(dirname "$token")"
        as_root ln -sfn "$actual" "$token"
        log "recorded system library $token -> $actual"
    done
}

# Only the files llvm-sys reads: the config tool, the headers its --cxxflags
# name, and the archives its --libs name. Polly is not part of the LLVM
# component set, so `libLLVM*.a` alone links short. The full tarball extracts to
# ~8 GiB; this subset is ~0.6 GiB.
extract_subset() {
    log "extracting to $root"
    tar -xf "$archive" -C "$root" --wildcards --strip-components=1 \
        "LLVM-${llvm_version}-Linux-${tar_arch}/bin/llvm-config" \
        "LLVM-${llvm_version}-Linux-${tar_arch}/include/*" \
        "LLVM-${llvm_version}-Linux-${tar_arch}/lib/libLLVM*.a" \
        "LLVM-${llvm_version}-Linux-${tar_arch}/lib/libPolly*.a"
}

fetch() {
    command -v curl >/dev/null 2>&1 || die "curl is required to download the LLVM tarball"
    mkdir -p "$root"
    local url="https://github.com/llvm/llvm-project/releases/download/llvmorg-${llvm_version}/LLVM-${llvm_version}-Linux-${tar_arch}.tar.xz"
    log "downloading $url"
    curl -fsSL --retry 3 --retry-delay 5 -o "$archive" "$url"
    log "verifying sha256 of $archive"
    printf '%s  %s\n' "$tar_sha256" "$archive" | sha256sum -c - >/dev/null || die "sha256 mismatch for $archive"
}

# stdout carries only the exports the caller evals: a package manager's
# "Nothing to do." on stdout would otherwise be evaluated as a command.
exec 3>&1 1>&2

if [ "$system_deps" -eq 1 ]; then
    install_system_deps
fi

if [ ! -x "$root/bin/llvm-config" ]; then
    fetch
    extract_subset
fi
[ -x "$root/bin/llvm-config" ] || die "extraction did not produce $root/bin/llvm-config"
log "LLVM $("$root/bin/llvm-config" --version) at $root"

repair_recorded_paths

if [ "$mode" = "github-env" ]; then
    [ -n "${GITHUB_ENV:-}" ] || die "--github-env needs \$GITHUB_ENV set"
    {
        printf '%s=%s\n' "$prefix_var" "$root"
        printf 'PATH=%s:%s\n' "$root/bin" "$PATH"
    } >>"$GITHUB_ENV"
    log "exported $prefix_var=$root through \$GITHUB_ENV"
else
    printf 'export %s=%q\n' "$prefix_var" "$root" >&3
    printf 'export PATH=%q\n' "$root/bin:$PATH" >&3
fi
