#!/usr/bin/env bash
# Attach to the degenbot devcontainer. 
#
# Finds the container VSCode or `devcontainer up` created, starts it if
# stopped, and drops into a tmux session named "dev".
#
# Usage:
#   .devcontainer/attach.sh  # attach to an existing tmux session, creating it first if absent
#
# To rebuild the container first, use .devcontainer/rebuild.sh.
#
# Requires: podman.
#
# NOTE: VSCode/devcontainer name the IMAGE `vsc-degenbot-<hash>-...` but Podman
# assigns the CONTAINER a random name (e.g. `priceless_roentgen`). So we find
# the container via its source image label, not its container name.
set -euo pipefail

# Resolve the workspace from the script's own location (repo root) rather than
# a hardcoded path — portable across machines / user homes.
WORKSPACE="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# Discover the container by its image name (the devcontainer runtime tags the
# image `vsc-degenbot-<hash>-features[-uid]` and the container references it).
name="$(podman ps -a --format '{{.Names}}' \
  | while read -r n; do
      img="$(podman inspect -f '{{.ImageName}}' "$n" 2>/dev/null || true)"
      case "$img" in
        *vsc-degenbot*) echo "$n"; break ;;
      esac
    done | head -1)"

if [ -z "$name" ]; then
  echo "ERROR: no degenbot devcontainer found." >&2
  echo "Run one of:" >&2
  echo "  .devcontainer/rebuild.sh     # build via devcontainer CLI (needs npm i -g @devcontainers/cli)" >&2
  echo "  VSCode 'Reopen in Container'  # build via VSCode" >&2
  exit 1
fi

state="$(podman inspect -f '{{.State.Status}}' "$name")"
if [ "$state" != "running" ]; then
  echo ">>> starting $name"
  podman start "$name"
fi

# Resource-cap invariant check (post-incident 2026-08-21): runArgs caps
# (--cpus/--memory/--pids-limit) apply only at container CREATION. A container
# created before the caps landed — or recreated without them — runs unbounded,
# and attach.sh is the last line of defense that can say so. Warn (don't block)
# when the running container lacks the CPU cap; NanoCpus is 0 when unset.
nano_cpus="$(podman inspect -f '{{.HostConfig.NanoCpus}}' "$name" 2>/dev/null || echo 0)"
if [ "$nano_cpus" = "0" ]; then
  echo "⚠️  container has NO CPU/memory cap (NanoCpus=0). Runaway builds/tests can" >&2
  echo "    starve the host. Fix by recreating: .devcontainer/rebuild.sh (runArgs in" >&2
  echo "    devcontainer.json apply at creation only, never on 'podman start')." >&2
fi

# Guard against the plain-attach path landing in a container that was created
# but never had postCreateCommand run. The attach path does podman start + exec
# — it has no knowledge of devcontainer.json, so postCreateCommand (which runs
# `uv sync` in the container-local venv via post-create.sh) is NOT invoked
# here. The venv lives at $UV_PROJECT_ENVIRONMENT inside the container's
# writable layer (not the bind-mounted repo), so we check it via podman exec.
# Warn (don't block — the user may want to inspect) and point at the fix;
# .devcontainer/rebuild.sh goes through `devcontainer up`, which DOES run postCreate.
CONTAINER_VENV="/home/dev/.venvs/degenbot"
if ! podman exec --user dev "$name" test -d "$CONTAINER_VENV/bin" 2>/dev/null; then
  echo "⚠️  container venv missing — post-create.sh has not run on this container." >&2
  echo "    Tests (just test-*) and 'uv run' will fail until the venv + PyO3" >&2
  echo "    extension are built. Fix with:" >&2
  echo "      .devcontainer/rebuild.sh" >&2
  echo "    (attaching anyway; Ctrl-b d to detach, then rebuild.)" >&2
fi

# Drop in as the container's regular user (remoteUser=dev, uid 1000) instead of
# root, so tmux's socket lands under /home/dev and files we touch are owned by
# the workspace user.
USER="dev"

# ---------------------------------------------------------------------------
# Timezone: forward the HOST's timezone into the container.
#
# attach.sh does `podman start` + `podman exec` — it has no knowledge of
# devcontainer.json, so the `mounts` entry (bind-mount host /etc/localtime)
# and the `containerEnv` TZ = ${localEnv:TZ} declared there are NEVER applied
# on this launch path. (They only take effect when the container is created via
# VSCode's 'Reopen in Container' or `devcontainer up` — see rebuild.sh.) Worse,
# `podman exec` does not inherit the caller's env, so even an exported host TZ
# wouldn't reach the exec'd shell without --env forwarding (same reason TERM and
# COLORTERM are forwarded below).
#
# So we resolve the host's current zone here — preferring an exported TZ, then
# reading the host's /etc/localtime symlink — and do two things with it:
#   1. Self-heal the container's /etc/localtime to match (a plain `podman start`
#      reuses whatever /etc/localtime the container was CREATED with, which on
#      this launch path is the image's default Etc/UTC; a travel/rebuild can
#      leave them out of sync). Self-healing means every attach is correct
#      without a rebuild, and picks up a changed host zone (laptop travel).
#   2. Forward TZ=... on the `podman exec` so tools that read the env var
#      directly (date, some loggers) are correct even before/without the
#      symlink, and so the inner shell has TZ set for child processes.
#
# Guard: only override if the resolved zone maps to a real file under
# /usr/share/zoneinfo/. If the host has no TZ exported and /etc/localtime isn't
# a zoneinfo symlink (e.g. a copied POSIX tz file, or missing entirely), we
# SKIP entirely and let the container keep its image default (Etc/UTC). Per the
# design decision in AGENTS discussion: default to UTC, only override when the
# host zone is positively resolvable. Making the override *conditional* on a
# resolvable zone is what makes the travel case (Q3) work: a host that moves
# to a new zone just updates its /etc/localtime, and the next attach picks it
# up; a host whose zone can't be resolved at all degrades safely to UTC
# instead of forwarding garbage.
host_zone=""
# Prefer an exported TZ (validates it resolves to a zone file before trusting).
if [ -n "${TZ:-}" ]; then
  if [ -f "/usr/share/zoneinfo/${TZ}" ]; then
    host_zone="$TZ"
  fi
fi
# Otherwise read the host's /etc/localtime symlink. readlink -f follows the
# chain to the final target; if it ends under .../zoneinfo/<name>, <name> is a
# valid IANA zone (e.g. America/Los_Angeles). A regular (non-symlink) file or a
# broken link yields nothing here -> skip, fall back to UTC.
if [ -z "$host_zone" ]; then
  lt="$(readlink -f /etc/localtime 2>/dev/null || true)"
  case "$lt" in
    */zoneinfo/*)
      z="${lt##*/zoneinfo/}"
      [ -n "$z" ] && [ -f "/usr/share/zoneinfo/${z}" ] && host_zone="$z"
      ;;
  esac
fi

if [ -n "$host_zone" ]; then
  # 1. Self-heal the container's /etc/localtime if it doesn't already match the
  #    host zone. Done as root (--user 0); the symlink target lives in the
  #    read-only image layer so it's safe and idempotent. Cheap: one inspect +
  #    one exec only when the target differs, which is rare (mostly the first
  #    attach after a create/rebuild/travel).
  #
  #    Guard against /etc/localtime being a MOUNT POINT: on the rebuild.sh
  #    path, `devcontainer up` applies the devcontainer.json mounts entry that
  #    bind-mounts the host's /etc/localtime read-only. That bind is already
  #    correct (it IS the host zone), so self-healing is unnecessary; and
  #    `ln -sf` over a read-only bind would fail noisily. We detect a mount
  #    point via `findmnt` (mounted) and skip the self-heal entirely in that
  #    case — the bind already carries the right zone. Only when /etc/localtime
  #    is NOT a mount point (the plain podman-start path, image's own file) do
  #    we rewrite the symlink.
  want="/usr/share/zoneinfo/${host_zone}"
  is_mount="$(podman exec --user 0 "$name" findmnt -n -o TARGET /etc/localtime 2>/dev/null || true)"
  if [ -z "$is_mount" ]; then
    cur="$(podman exec --user 0 "$name" readlink -f /etc/localtime 2>/dev/null || true)"
    if [ "$cur" != "$want" ]; then
      podman exec --user 0 "$name" ln -sf "$want" /etc/localtime >/dev/null 2>&1 || \
        echo "⚠️  could not set container /etc/localtime to ${host_zone} (logs above)" >&2
    fi
  fi
  TZ_FWD="$host_zone"
else
  # Unresolvable -> don't forward TZ; container keeps its image default (UTC).
  TZ_FWD=""
fi

# Propagate the host terminal's color capability through the podman exec
# boundary. `podman exec` does NOT inherit the caller's env — only vars named
# via --env are injected into the exec process — so TERM and COLORTERM must be
# forwarded explicitly:
#   - TERM      : tmux's terminal-overrides (",*:Tc") wildcard matches whatever
#                 host TERM is propagated to decide the outer terminal supports
#                 truecolor pass-through.
#   - COLORTERM : pi reads it to decide whether to EMIT 24-bit RGB escapes;
#                 without it pi falls back to a dim 16-color palette (grey text,
#                 red accents). See tmux.conf + README "Terminal colors".
# This matches VSCode, whose integrated terminal injects COLORTERM into the
# in-container shell via the remote server.
#
# tmux new -A attaches to an existing session or creates a new one.
exec podman exec -it \
    --user "$USER" \
    --env TERM="${TERM}" \
    --env COLORTERM="${COLORTERM:-}" \
    --env TZ="${TZ_FWD}" \
    "$name" tmux new -As dev
