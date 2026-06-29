#!/usr/bin/env bash
# Attach to the degenbot devcontainer from a plain SSH terminal.
#
# Finds the container VSCode or `devcontainer up` created, starts it if
# stopped, and drops into a tmux session named "dev".
#
# Usage:
#   .devcontainer/ssh-attach.sh          # attach (creates tmux session if absent)
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
if [ "$state" = "running" ]; then
  echo ">>> $name already running"
else
  echo ">>> starting $name"
  podman start "$name"
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
exec podman exec -it --user "$USER" \
    --env HOME="/home/$USER" \
    --env TERM="${TERM}" \
    --env COLORTERM="${COLORTERM:-}" \
    "$name" tmux new -As dev