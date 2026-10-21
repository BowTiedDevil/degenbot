#!/usr/bin/env bash

# Report which publishable workspace members are live on crates.io at the
# workspace version, and which are not — the exact set `cargo publish` accepts.
#
# The oracle is the crates.io SPARSE INDEX, because that is the source
# `cargo publish` itself gates on. The alternative, `cargo search`, answers from a
# separate search backend that lags publication by minutes and is rate-limited
# per IP, so a member published moments ago can still read as absent there. A
# caller that treats "absent" as "publishable" then re-uploads a version crates.io
# already holds — which the registry rejects with a hard error rather than a
# no-op, so a resumable publish loop turns into a permanent failure.
#
# Output: one crate name per line on stdout, diagnostics on stderr. Exit 0 means
# every member's state was read conclusively; exit 2 means the index could not be
# read for at least one member, which callers must treat as "unknown state" and
# never as "not published".
#
# Usage:
#   scripts/crates_io_pending.sh              # members NOT yet live
#   scripts/crates_io_pending.sh --live       # members already live
#   scripts/crates_io_pending.sh --classify   # "<live|pending> <member>" lines
#
# Options: --version <semver>, --manifest-path <path>, --index-url <url>.
# Env: CRATES_IO_INDEX_URL overrides the index root (mirrors, local fixtures).

set -uo pipefail

cd "$(dirname "$0")/.."

usage() {
  cat <<'USAGE'
Print which publishable workspace members are live on crates.io at the
workspace version, and which are not.

Usage:
  crates_io_pending.sh              # members not yet live
  crates_io_pending.sh --live       # members already live
  crates_io_pending.sh --classify   # "<live|pending> <member>" lines

Options:
  --version <semver>       version to test (default: the workspace version)
  --manifest-path <path>   workspace manifest (default: rust/Cargo.toml)
  --index-url <url>        index root (default: $CRATES_IO_INDEX_URL or
                           https://index.crates.io)
  -h, --help               this text

Exit: 0 conclusive, 2 index state unknown, 64 usage error.
USAGE
}

manifest="rust/Cargo.toml"
version=""
mode="pending"
index_url="${CRATES_IO_INDEX_URL:-https://index.crates.io}"

while [ $# -gt 0 ]; do
  case "$1" in
    --version)        version="${2:?--version needs a value}"; shift 2 ;;
    --version=*)      version="${1#*=}"; shift ;;
    --manifest-path)  manifest="${2:?--manifest-path needs a value}"; shift 2 ;;
    --index-url)      index_url="${2:?--index-url needs a value}"; shift 2 ;;
    --live)           mode="live"; shift ;;
    --classify)       mode="classify"; shift ;;
    -h|--help)        usage; exit 0 ;;
    *)                echo "crates_io_pending: unknown argument '$1'" >&2; exit 64 ;;
  esac
done

index_url="${index_url%/}"

if [ -z "$version" ]; then
  version=$(sed -n 's/^version = //p' "$manifest" | head -n 1 | tr -d '"' | tr -d ' ')
fi
if [ -z "$version" ]; then
  echo "crates_io_pending: no version literal found in $manifest" >&2
  exit 1
fi

# Publishable members come from cargo metadata (publish == null), so a crate
# declaring publish = false is skipped without a hand-maintained list drifting
# out of date.
# Reachability probe first: a down or misconfigured index must fail the audit once,
# in seconds, instead of costing a retry ladder per member. config.json is the
# index's own root document, so a 200 here means the rest of the audit can trust
# per-member status codes.
if ! curl -fsS -o /dev/null --max-time 15 --retry 3 --retry-connrefused "$index_url/config.json" 2>/dev/null; then
  echo "crates_io_pending: index unreachable at $index_url; state unknown" >&2
  exit 2
fi

members=$(cargo metadata --format-version 1 --no-deps --manifest-path "$manifest" 2>/dev/null \
  | python3 -c 'import json, sys
meta = json.load(sys.stdin)
print("\n".join(p["name"] for p in meta["packages"] if p["publish"] is None))')
if [ -z "$members" ]; then
  echo "crates_io_pending: could not read publishable members from $manifest" >&2
  exit 1
fi

# crates.io sparse-index path layout: fixed-width prefix directories keyed on the
# (lowercase) name length, so every crate name maps to one deterministic URL.
index_path() {
  local name="$1"
  local len=${#1}
  case "$len" in
    1) printf '1/%s' "$name" ;;
    2) printf '2/%s' "$name" ;;
    3) printf '3/%s/%s' "${name:0:1}" "$name" ;;
    *) printf '%s/%s/%s' "${name:0:2}" "${name:2:2}" "$name" ;;
  esac
}

version_re=$(printf '%s' "$version" | sed 's/[.]/\\./g')

body_file=$(mktemp)
trap 'rm -f "$body_file"' EXIT

inconclusive=0
while IFS= read -r member; do
  [ -n "$member" ] || continue
  path=$(index_path "$(printf '%s' "$member" | tr '[:upper:]' '[:lower:]')")
  url="$index_url/$path"
  code=$(curl -sS -o "$body_file" -w '%{http_code}' \
    --max-time 15 --retry 2 --retry-delay 1 "$url" 2>/dev/null) || code="000"
  case "$code" in
    200)
      # Exact `"vers": "<version>"` match, dots escaped and the closing quote
      # required: a bare substring test would also accept 0.6.0-alpha.1 as
      # 0.6.0-alpha.15. Whitespace tolerance covers index renderings that pretty
      # print the line instead of emitting it compact.
      if grep -q "\"vers\":[[:space:]]*\"$version_re\"" "$body_file"; then state="live"; else state="pending"; fi
      ;;
    404) state="pending" ;;
    *)
      echo "crates_io_pending: index read failed for $member (HTTP $code at $url)" >&2
      inconclusive=$((inconclusive + 1))
      continue
      ;;
  esac
  case "$mode" in
    live)     [ "$state" = "live" ] && printf '%s\n' "$member" ;;
    pending)  [ "$state" = "pending" ] && printf '%s\n' "$member" ;;
    classify) printf '%s %s\n' "$state" "$member" ;;
  esac
done <<< "$members"

if [ "$inconclusive" -gt 0 ]; then
  echo "crates_io_pending: $inconclusive member(s) unreadable at $index_url; state unknown" >&2
  exit 2
fi

exit 0
