#!/usr/bin/env bash
# Attach to the degenbot devcontainer from a plain SSH terminal.
#
# Finds the container VSCode or `devcontainer up` created, starts it if
# stopped, and drops into a tmux session named "pi".
#
# Usage:
#   .devcontainer/ssh-attach.sh          # attach (creates tmux session if absent)
#   .devcontainer/ssh-attach.sh rebuild  # recreate from scratch via devcontainer CLI
#
# Requires: podman. rebuild path also requires `devcontainer` CLI
# (npm i -g @devcontainers/cli).
#
# NOTE: VSCode/devcontainer name the IMAGE `vsc-degenbot-<hash>-...` but Podman
# assigns the CONTAINER a random name (e.g. `priceless_roentgen`). So we find
# the container via its source image label, not its container name.
set -euo pipefail

# Resolve the workspace from the script's own location (repo root) rather than
# a hardcoded path — portable across machines / user homes.
WORKSPACE="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

if [ "${1:-}" = "rebuild" ]; then
  echo ">>> rebuilding container via devcontainer CLI + podman"
  exec devcontainer up --workspace-folder "$WORKSPACE" --docker-path podman \
    --remove-existing-container
fi

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
  echo "  $0 rebuild                   # build via devcontainer CLI (needs npm i -g @devcontainers/cli)" >&2
  echo "  VSCode 'Reopen in Container' # build via VSCode" >&2
  exit 1
fi

state="$(podman inspect -f '{{.State.Status}}' "$name")"
if [ "$state" = "running" ]; then
  echo ">>> $name already running"
else
  echo ">>> starting $name"
  podman start "$name"
fi

# Drop in as the container's regular user (remoteUser=dev, uid 1000) instead of
# root, so tmux's socket lands under /home/dev and files we touch are owned by
# the workspace user.
USER="dev"

# Propagate the host terminal's color capability through the podman exec
# boundary. Without this, COLORTERM is dropped and pi falls back to a dim
# 16-color palette (near-invisible grey, red accents); TERM is also needed so
# tmux's terminal-overrides match the outer terminal for truecolor pass-through.
# See .devcontainer/tmux.conf and the README "Terminal colors" section.

# tmux new -A attaches to an existing session or creates a new one.
exec podman exec -it --user "$USER" \
    --env HOME="/home/$USER" \
    --env TERM="${TERM:-xterm-256color}" \
    --env COLORTERM="${COLORTERM:-}" \
    "$name" tmux new -As pi