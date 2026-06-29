#!/usr/bin/env bash
# Rebuild the degenbot devcontainer from scratch via the devcontainer CLI.
#
# Recreates the image + container and runs postCreateCommand (uv sync + git
# hooks) via `devcontainer up` — the same engine VSCode's "Rebuild Container"
# uses, so mounts, containerEnv, postCreate/postStart all apply identically.
# Returns to the host shell once the container is up; does NOT attach (use
# ssh-attach.sh for that).
#
# Usage:
#   .devcontainer/rebuild.sh
#
# Requires: podman + `devcontainer` CLI (npm i -g @devcontainers/cli).
set -euo pipefail

# Resolve the workspace from the script's own location (repo root) rather than
# a hardcoded path — portable across machines / user homes.
WORKSPACE="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

echo ">>> rebuilding container via devcontainer CLI + podman"
devcontainer up --workspace-folder "$WORKSPACE" --docker-path podman \
  --remove-existing-container
echo ">>> rebuild complete — attach with: .devcontainer/ssh-attach.sh"