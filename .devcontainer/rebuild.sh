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

# Self-heal (devcontainers/cli#1236, unfixed as of CLI 0.89.0 + podman 5.8.7):
# on create/recreate the CLI can block FOREVER after "Container started". It
# spawns `podman events --filter event=start` and then `podman run`; when the
# container's start event lands before that listener is subscribed, the CLI
# waits for an event that already fired. The container is left Up but
# UNPROVISIONED (postCreateCommand never execs). Workaround: watchdog the
# create attempt; if the container is Up but the post-create venv (the first
# thing post-create.sh makes; mirrors UV_PROJECT_ENVIRONMENT) never appears
# within the grace window, kill the CLI and resume with a plain
# `devcontainer up` — the existing-container path is unaffected by the bug
# and re-runs postCreateCommand (verified 2026-10-01: full provisioning on
# resume after a killed hung create).
VENV_PY="/home/dev/.venvs/degenbot/bin/python"
GRACE_SECONDS=45

dc_up() {
  devcontainer up --workspace-folder "$WORKSPACE" --docker-path podman "$@"
}

dc_up --remove-existing-container &
dc_pid=$!

container_up_at=""
hang=false
while kill -0 "$dc_pid" 2>/dev/null; do
  cid="$(podman ps -q --filter "$SELECTOR" | head -1)"
  if [ -n "$cid" ] && podman exec "$cid" test -x "$VENV_PY" 2>/dev/null; then
    break  # post-create started — the hang can only strike before this point
  fi
  now=$(date +%s)
  if [ -n "$cid" ]; then
    container_up_at="${container_up_at:-$now}"
    if [ $((now - container_up_at)) -ge "$GRACE_SECONDS" ]; then
      hang=true
      break
    fi
  fi
  sleep 2
done

if $hang; then
  echo ">>> detected devcontainers/cli#1236 hang (container Up, post-create never started)"
  kill "$dc_pid" 2>/dev/null || true
  pkill -TERM -P "$dc_pid" 2>/dev/null || true
  sleep 2
  kill -9 "$dc_pid" 2>/dev/null || true
  # The orphaned `podman events` child would otherwise linger; no other
  # process on this host runs the CLI's exact argument vector.
  pkill -f 'podman events --format json --filter event=start' 2>/dev/null || true
  echo ">>> resuming with existing-container up (runs postCreateCommand)"
  dc_up
else
  wait "$dc_pid"
fi

echo ">>> rebuild complete — attach with: .devcontainer/attach.sh"