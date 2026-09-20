#!/usr/bin/env bash
# Rebuild the degenbot devcontainer from scratch via the devcontainer CLI.
#
# Recreates the image + container and runs postCreateCommand (uv sync + git
# hooks) via `devcontainer up` — the same engine VSCode's "Rebuild Container"
# uses, so mounts, containerEnv, postCreate/postStart all apply identically.
# Returns to the host shell once the container is up; does NOT attach (use
# attach.sh for that).
#
# Usage:
#   .devcontainer/rebuild.sh
#
# Requires: podman + `devcontainer` CLI (npm i -g @devcontainers/cli).
set -euo pipefail

# Resolve the workspace from the script's own location (repo root) rather than
# a hardcoded path — portable across machines / user homes.
WORKSPACE="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# Self-heal (post-mortem 2026-09-08): a failed `devcontainer up` can leave
# TWO artifacts that break the NEXT rebuild:
#   1. zombie containers in `Created` state (the CLI retries its create with a
#      second podman run, and can then exec into the dead one — "can only
#      create exec sessions on running containers" — leaving the surviving
#      container Up but UNPROVISIONED: no postCreateCommand, no venv).
#   2. an orphaned rootless-podman pasta network helper still holding the
#      published ports (9464 metrics, 6772 hotpath exporter — see runArgs in
#      devcontainer.json). The leak is invisible in `podman ps` (its container
#      is gone) and makes the next create fail with "Listen failed for HOST
#      TCP port */6772: Address already in use";
# pasta binds the host side before wiring the container netns, so the loser
# comes up network-less and postCreateCommand (uv sync) dies on DNS. No other
# container on this host publishes 6772/9464, so both cleanups are safe.
SELECTOR="label=devcontainer.local_folder=$WORKSPACE"
if podman ps -aq --filter "$SELECTOR" | grep -q .; then
  echo ">>> removing leftover devcontainer containers"
  podman ps -aq --filter "$SELECTOR" | xargs -r podman rm -f >/dev/null
fi
if fuser -k 6772/tcp 9464/tcp 2>/dev/null; then
  echo ">>> killed leaked pasta forwarder on 6772/9464"
  sleep 1
fi

# Mount sources must exist before `podman run` — a missing host dir
# (e.g. ~/.local/state/degenbot on a fresh checkout) fails the create with
# "statfs ...: no such file or directory". Mirror every localEnv:HOME mount
# from devcontainer.json here.
for d in .agents .config/degenbot .local/state/degenbot .foundry .pi; do
  mkdir -p "$HOME/$d"
done

echo ">>> rebuilding container via devcontainer CLI + podman"
devcontainer up --workspace-folder "$WORKSPACE" --docker-path podman \
  --remove-existing-container
echo ">>> rebuild complete — attach with: .devcontainer/attach.sh"